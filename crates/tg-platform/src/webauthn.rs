//! Windows `webauthn.dll` FIDO2 / Passkeys integration, WebAuthn protocol codecs,
//! and cross-device caBLE / hybrid QR support.
//!
//! Port origin:
//! - `Telegram/SourceFiles/platform/platform_webauthn.h`
//! - `Telegram/SourceFiles/platform/win/webauthn_win.cpp`
//! - `Telegram/SourceFiles/data/data_passkey_deserialize.h` / `.cpp`
//! - `Telegram/SourceFiles/webauthn/webauthn_common.h` / `.cpp`
//! - `Telegram/SourceFiles/webauthn/cable_core.h` / `.cpp`
//! - `Telegram/SourceFiles/webauthn/cable_ceremony.cpp`

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::Generate;
use p256::{PublicKey as P256PublicKey, SecretKey as P256SecretKey};
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Default Telegram WebAuthn origin (`data_passkey_deserialize.cpp:26`).
pub const DEFAULT_ORIGIN: &str = "https://telegram.org";
/// Default Telegram Relying Party ID.
pub const DEFAULT_RP_ID: &str = "telegram.org";
/// Default ceremony timeout in milliseconds (`data_passkey_deserialize.h:34`).
pub const DEFAULT_TIMEOUT_MS: u32 = 60_000;
/// Maximum bare Telegram DC ID (`MTP::kDcShift = 1000`, `passkeys.cpp:174`).
pub const MAX_USER_DC_ID: i32 = 1000;

/// COSE algorithm identifier for ECDSA w/ SHA-256 on P-256 (`ES256`).
pub const COSE_ES256: i32 = -7;
/// COSE algorithm identifier for EdDSA (`Ed25519`).
pub const COSE_EDDSA: i32 = -8;
/// COSE algorithm identifier for RSASSA-PKCS1-v1_5 w/ SHA-256 (`RS256`).
pub const COSE_RS256: i32 = -257;

/// CTAP2 status byte indicating success (`cable_core.h:293`).
pub const CTAP_SUCCESS: u8 = 0x00;
/// CTAP2 status byte for operation denied (`cable_core.h:294`).
pub const CTAP_ERR_OPERATION_DENIED: u8 = 0x27;
/// CTAP2 status byte for no matching credentials (`cable_core.h:295`).
pub const CTAP_ERR_NO_CREDENTIALS: u8 = 0x2E;
/// CTAP2 status byte for user action timeout (`cable_core.h:296`).
pub const CTAP_ERR_USER_ACTION_TIMEOUT: u8 = 0x2F;

/// Assigned caBLEv2 tunnel server domains (`cable_core.cpp:31-34`).
pub const CABLE_ASSIGNED_TUNNEL_DOMAINS: [&str; 2] = ["cable.ua5v.com", "cable.auth.com"];

/// Size of an uncompressed P-256 SEC1 point (`0x04 || X || Y`).
pub const P256_X962_LEN: usize = 65;
/// Size of a compressed P-256 SEC1 point (`0x02/0x03 || X`).
pub const P256_COMPRESSED_LEN: usize = 33;
/// Size of a caBLEv2 QR secret in bytes.
pub const CABLE_QR_SECRET_LEN: usize = 16;
/// Size of a caBLEv2 BLE service data advert (`16` encrypted EID + `4` HMAC tag).
pub const CABLE_ADVERT_LEN: usize = 20;
/// Size of a decrypted caBLEv2 EID in bytes.
pub const CABLE_EID_LEN: usize = 16;
/// Size of the derived caBLEv2 EID key (`32` AES key + `32` HMAC key).
pub const CABLE_EID_KEY_LEN: usize = 64;
/// Size of the caBLEv2 EID nonce in bytes.
pub const CABLE_NONCE_LEN: usize = 10;
/// Size of the caBLEv2 routing ID in bytes.
pub const CABLE_ROUTING_ID_LEN: usize = 3;
/// Size of the derived caBLEv2 tunnel ID in bytes.
pub const CABLE_TUNNEL_ID_LEN: usize = 16;
/// Size of the derived caBLEv2 Noise PSK in bytes.
pub const CABLE_PSK_LEN: usize = 32;
/// Size of a caBLEv2 Noise handshake response (`65`-byte peer point + `16`-byte AES-GCM tag).
pub const CABLE_HANDSHAKE_RESPONSE_LEN: usize = P256_X962_LEN + 16;

const MAX_CBOR_DEPTH: usize = 16;
const CABLE_MAX_SEQUENCE: u32 = (1 << 24) - 1;
const CABLE_PADDING_GRANULARITY: usize = 32;

// ============================================================================
// Core WebAuthn / Passkey Types
// ============================================================================

/// WebAuthn Relying Party descriptor (`Data::Passkey::RelyingParty`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelyingParty {
    /// Domain identifier (e.g. `"telegram.org"`).
    pub id: String,
    /// Human-readable display name (e.g. `"Telegram"`).
    pub name: String,
}

/// WebAuthn user entity descriptor (`Data::Passkey::User`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyUser {
    /// Raw opaque user handle bytes (Telegram encodes `"<dc_id>:<user_id>"`).
    pub id: Vec<u8>,
    /// Account handle / username or phone.
    pub name: String,
    /// User's friendly display name.
    pub display_name: String,
}

/// Supported public-key credential algorithm parameter (`Data::Passkey::CredentialParameter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialParameter {
    /// Credential type (typically `"public-key"`).
    pub cred_type: String,
    /// COSE algorithm identifier (e.g. `-7` for ES256, `-257` for RS256).
    pub alg: i32,
}

/// Deserialized `account.passkeyRegistrationOptions` payload (`Data::Passkey::RegisterData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterData {
    /// Relying party information.
    pub rp: RelyingParty,
    /// Account user entity.
    pub user: PasskeyUser,
    /// Raw server challenge bytes.
    pub challenge: Vec<u8>,
    /// Acceptable COSE algorithms in preference order.
    pub pub_key_cred_params: Vec<CredentialParameter>,
    /// Operation timeout in milliseconds.
    pub timeout_ms: u32,
}

/// Descriptor for an allowed credential during assertion (`Data::Passkey::Credential`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowCredential {
    /// Raw credential ID bytes.
    pub id: Vec<u8>,
    /// Credential type (typically `"public-key"`).
    pub cred_type: String,
}

/// WebAuthn user verification requirement (`userVerification`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserVerification {
    /// User verification is required.
    Required,
    /// User verification is preferred when available.
    #[default]
    Preferred,
    /// User verification is discouraged.
    Discouraged,
}

impl UserVerification {
    /// Parses a WebAuthn `userVerification` string (`"required"`, `"preferred"`, `"discouraged"`).
    #[must_use]
    pub fn from_option_str(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("required") => Self::Required,
            Some("preferred") => Self::Preferred,
            Some("discouraged") => Self::Discouraged,
            _ => Self::Preferred,
        }
    }

    /// Returns the canonical lowercase WebAuthn string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Preferred => "preferred",
            Self::Discouraged => "discouraged",
        }
    }
}

/// Deserialized `auth.passkeyLoginOptions` payload (`Data::Passkey::LoginData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginData {
    /// Raw server challenge bytes.
    pub challenge: Vec<u8>,
    /// Relying party ID (e.g. `"telegram.org"`).
    pub rp_id: String,
    /// Optional list of allowed credentials (empty for discoverable passkeys).
    pub allow_credentials: Vec<AllowCredential>,
    /// User verification requirement.
    pub user_verification: UserVerification,
    /// Operation timeout in milliseconds.
    pub timeout_ms: u32,
}

/// Categorized error returned by a WebAuthn provider (`Platform::WebAuthn::Error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebAuthnErrorKind {
    /// User dismissed or cancelled the OS / security-key ceremony.
    Cancelled,
    /// Platform WebAuthn (`webauthn.dll`) is unavailable or unsupported on this OS version.
    NotSupported,
    /// Unsigned build restriction (macOS/platform parity).
    UnsignedBuild,
    /// No matching passkey credential was found on the authenticator.
    NoCredentials,
    /// No authenticator or security key was detected.
    NoDevice,
    /// Invalid RP ID, origin, challenge, or JSON options.
    InvalidOptions(String),
    /// Any other platform or protocol error.
    Other(String),
}

impl fmt::Display for WebAuthnErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("passkey operation cancelled"),
            Self::NotSupported => f.write_str("WebAuthn is not supported on this system"),
            Self::UnsignedBuild => f.write_str("passkeys are not available in unsigned builds"),
            Self::NoCredentials => f.write_str("no matching passkey found"),
            Self::NoDevice => f.write_str("no security key or authenticator found"),
            Self::InvalidOptions(msg) => write!(f, "invalid WebAuthn options: {msg}"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for WebAuthnErrorKind {}

/// Output of a successful passkey registration (`Platform::WebAuthn::RegisterResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterResult {
    /// Raw credential ID bytes.
    pub credential_id: Vec<u8>,
    /// CBOR-encoded WebAuthn attestation object.
    pub attestation_object: Vec<u8>,
    /// UTF-8 JSON bytes of `clientDataJSON`.
    pub client_data_json: Vec<u8>,
}

/// Output of a successful passkey authentication (`Platform::WebAuthn::LoginResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginResult {
    /// UTF-8 JSON bytes of `clientDataJSON`.
    pub client_data_json: Vec<u8>,
    /// Raw credential ID bytes.
    pub credential_id: Vec<u8>,
    /// Raw authenticator data bytes.
    pub authenticator_data: Vec<u8>,
    /// Raw assertion signature bytes.
    pub signature: Vec<u8>,
    /// Raw user handle bytes (Telegram encodes `"<dc_id>:<user_id>"`).
    pub user_handle: Vec<u8>,
}

/// Capabilities probed from the host OS WebAuthn subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AuthenticatorCapabilities {
    /// Whether `webauthn.dll` loaded and resolved all required entry points.
    pub dll_available: bool,
    /// Value returned by `WebAuthNGetApiVersionNumber()` (`0` if unavailable).
    pub api_version: u32,
    /// Result of `WebAuthNIsUserVerifyingPlatformAuthenticatorAvailable`.
    pub platform_authenticator_available: bool,
    /// Whether Windows hybrid (caBLE QR) transport is natively supported (`api_version >= 6`).
    pub hybrid_transport_supported: bool,
}

impl AuthenticatorCapabilities {
    /// Returns `true` when native `webauthn.dll` is usable on this host.
    #[must_use]
    pub const fn is_native_supported(self) -> bool {
        self.dll_available && self.api_version >= 1
    }
}

// ============================================================================
// Base64 / Base64URL Helpers
// ============================================================================

const B64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encodes `input` using Base64URL without `=` padding (`QByteArray::Base64UrlEncoding | OmitTrailingEquals`).
#[must_use]
pub fn encode_base64url(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i];
        let b1 = if i + 1 < input.len() { input[i + 1] } else { 0 };
        let b2 = if i + 2 < input.len() { input[i + 2] } else { 0 };

        out.push(B64URL_ALPHABET[(b0 >> 2) as usize] as char);
        out.push(B64URL_ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if i + 1 < input.len() {
            out.push(B64URL_ALPHABET[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        }
        if i + 2 < input.len() {
            out.push(B64URL_ALPHABET[(b2 & 0x3F) as usize] as char);
        }
        i += 3;
    }
    out
}

/// Encodes `input` using Base64URL with `=` padding (`QByteArray::Base64UrlEncoding`).
#[must_use]
pub fn encode_base64url_padded(input: &[u8]) -> String {
    let mut out = encode_base64url(input);
    while !out.len().is_multiple_of(4) {
        out.push('=');
    }
    out
}

/// Decodes standard or URL-safe Base64 (with or without `=` padding).
#[must_use]
pub fn decode_base64_any(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    let mut vals = Vec::with_capacity(trimmed.len());
    let mut saw_pad = false;
    for byte in trimmed.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            saw_pad = true;
            continue;
        }
        if saw_pad {
            return None;
        }
        let v = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        vals.push(v);
    }
    if vals.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity((vals.len() * 3) / 4);
    let mut i = 0;
    while i < vals.len() {
        let a = vals[i];
        let b = vals[i + 1];
        out.push((a << 2) | (b >> 4));
        if i + 2 < vals.len() {
            let c = vals[i + 2];
            out.push((b << 4) | (c >> 2));
            if i + 3 < vals.len() {
                let d = vals[i + 3];
                out.push((c << 6) | d);
            }
        }
        i += 4;
    }
    Some(out)
}

// ============================================================================
// Origin, RP ID, User Handle & ClientDataJSON
// ============================================================================

/// Validates and normalizes a WebAuthn Relying Party ID (domain name without scheme/port/path).
pub fn validate_rp_id(rp_id: &str) -> Result<String, WebAuthnErrorKind> {
    let trimmed = rp_id.trim();
    if trimmed.is_empty() || trimmed.len() > 253 {
        return Err(WebAuthnErrorKind::InvalidOptions(
            "RP ID must be 1..=253 characters".to_owned(),
        ));
    }
    let normalized = trimmed.to_ascii_lowercase();
    if normalized.starts_with('.') || normalized.ends_with('.') {
        return Err(WebAuthnErrorKind::InvalidOptions(
            "RP ID cannot start or end with a dot".to_owned(),
        ));
    }
    for label in normalized.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(WebAuthnErrorKind::InvalidOptions(format!(
                "invalid RP ID label in '{rp_id}'"
            )));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(WebAuthnErrorKind::InvalidOptions(format!(
                "RP ID label cannot start/end with hyphen in '{rp_id}'"
            )));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(WebAuthnErrorKind::InvalidOptions(format!(
                "RP ID contains invalid character in '{rp_id}'"
            )));
        }
    }
    Ok(normalized)
}

/// Builds the canonical HTTPS origin for a validated RP ID (e.g. `"telegram.org"` -> `"https://telegram.org"`).
pub fn origin_for_rp_id(rp_id: &str) -> Result<String, WebAuthnErrorKind> {
    let normalized = validate_rp_id(rp_id)?;
    Ok(format!("https://{normalized}"))
}

/// Checks whether a WebAuthn `origin` is permitted for `rp_id` (exact match or valid subdomain
/// over `https://`, or `http://localhost` for localhost RP IDs).
#[must_use]
pub fn origin_matches_rp_id(origin: &str, rp_id: &str) -> bool {
    let Ok(norm_rp) = validate_rp_id(rp_id) else {
        return false;
    };
    let trimmed = origin.trim();
    let host_port = if let Some(rest) = trimmed.strip_prefix("https://") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        // WebAuthn allows http:// only for localhost.
        if norm_rp != "localhost" {
            return false;
        }
        rest
    } else {
        return false;
    };

    // Origin must not contain a path, query, fragment, or userinfo.
    if host_port.contains('/')
        || host_port.contains('?')
        || host_port.contains('#')
        || host_port.contains('@')
    {
        return false;
    }

    let host = match host_port.split_once(':') {
        Some((h, port_str)) => {
            if port_str.is_empty() || !port_str.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            if port_str.parse::<u16>().is_err() {
                return false;
            }
            h
        }
        None => host_port,
    };

    let Ok(norm_host) = validate_rp_id(host) else {
        return false;
    };

    if norm_host == norm_rp {
        return true;
    }
    norm_host
        .strip_suffix(&norm_rp)
        .is_some_and(|prefix| prefix.ends_with('.'))
}

/// Parsed Telegram passkey user handle (`"<dc_id>:<user_id>"`, `passkeys.cpp:168-177`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedUserHandle {
    /// Bare Telegram DC ID (`1..1000`).
    pub dc_id: i32,
    /// Telegram user ID (`> 0`).
    pub user_id: i64,
}

/// Parses Telegram's passkey `userHandle` (`"<dc_id>:<user_id>"`), matching `FinishPasskeyLogin`.
pub fn parse_user_handle(raw: &[u8]) -> Result<ParsedUserHandle, WebAuthnErrorKind> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| WebAuthnErrorKind::InvalidOptions("PASSKEY_HANDLE_INVALID".to_owned()))?;
    let mut parts = text.split(':');
    let Some(dc_str) = parts.next() else {
        return Err(WebAuthnErrorKind::InvalidOptions(
            "PASSKEY_HANDLE_INVALID".to_owned(),
        ));
    };
    let Some(uid_str) = parts.next() else {
        return Err(WebAuthnErrorKind::InvalidOptions(
            "PASSKEY_HANDLE_INVALID".to_owned(),
        ));
    };
    if parts.next().is_some() {
        return Err(WebAuthnErrorKind::InvalidOptions(
            "PASSKEY_HANDLE_INVALID".to_owned(),
        ));
    }
    let dc_id = dc_str
        .parse::<i32>()
        .map_err(|_| WebAuthnErrorKind::InvalidOptions("PASSKEY_HANDLE_INVALID".to_owned()))?;
    if dc_id <= 0 || dc_id >= MAX_USER_DC_ID {
        return Err(WebAuthnErrorKind::InvalidOptions(
            "PASSKEY_HANDLE_INVALID".to_owned(),
        ));
    }
    let user_id = uid_str
        .parse::<i64>()
        .map_err(|_| WebAuthnErrorKind::InvalidOptions("PASSKEY_HANDLE_INVALID".to_owned()))?;
    if user_id <= 0 {
        return Err(WebAuthnErrorKind::InvalidOptions(
            "PASSKEY_HANDLE_INVALID".to_owned(),
        ));
    }
    Ok(ParsedUserHandle { dc_id, user_id })
}

/// Serializes compact WebAuthn `clientDataJSON` for `webauthn.create` using [`DEFAULT_ORIGIN`].
#[must_use]
pub fn serialize_client_data_create(challenge: &[u8]) -> String {
    serialize_client_data_create_with_origin(challenge, DEFAULT_ORIGIN)
}

/// Serializes compact WebAuthn `clientDataJSON` for `webauthn.create` with an explicit `origin`.
#[must_use]
pub fn serialize_client_data_create_with_origin(challenge: &[u8], origin: &str) -> String {
    serialize_client_data(challenge, "webauthn.create", origin)
}

/// Serializes compact WebAuthn `clientDataJSON` for `webauthn.get` using [`DEFAULT_ORIGIN`].
#[must_use]
pub fn serialize_client_data_get(challenge: &[u8]) -> String {
    serialize_client_data_get_with_origin(challenge, DEFAULT_ORIGIN)
}

/// Serializes compact WebAuthn `clientDataJSON` for `webauthn.get` with an explicit `origin`.
#[must_use]
pub fn serialize_client_data_get_with_origin(challenge: &[u8], origin: &str) -> String {
    serialize_client_data(challenge, "webauthn.get", origin)
}

fn serialize_client_data(challenge: &[u8], kind: &str, origin: &str) -> String {
    let obj = json!({
        "type": kind,
        "challenge": encode_base64url(challenge),
        "origin": origin,
        "crossOrigin": false,
    });
    obj.to_string()
}

/// Parsed fields of a WebAuthn `clientDataJSON` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedClientData {
    /// `"webauthn.create"` or `"webauthn.get"`.
    pub kind: String,
    /// Decoded raw challenge bytes.
    pub challenge: Vec<u8>,
    /// Origin URI string (e.g. `"https://telegram.org"`).
    pub origin: String,
    /// Whether the ceremony was cross-origin.
    pub cross_origin: bool,
}

/// Parses and validates a UTF-8 `clientDataJSON` buffer.
pub fn parse_client_data(json_bytes: &[u8]) -> Result<ParsedClientData, WebAuthnErrorKind> {
    let val: JsonValue = serde_json::from_slice(json_bytes)
        .map_err(|e| WebAuthnErrorKind::InvalidOptions(format!("invalid clientDataJSON: {e}")))?;
    let obj = val.as_object().ok_or_else(|| {
        WebAuthnErrorKind::InvalidOptions("clientDataJSON is not an object".to_owned())
    })?;
    let kind = obj
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| WebAuthnErrorKind::InvalidOptions("missing clientDataJSON.type".to_owned()))?
        .to_owned();
    let challenge_b64 = obj
        .get("challenge")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            WebAuthnErrorKind::InvalidOptions("missing clientDataJSON.challenge".to_owned())
        })?;
    let challenge = decode_base64_any(challenge_b64).ok_or_else(|| {
        WebAuthnErrorKind::InvalidOptions("invalid base64url challenge".to_owned())
    })?;
    let origin = obj
        .get("origin")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            WebAuthnErrorKind::InvalidOptions("missing clientDataJSON.origin".to_owned())
        })?
        .to_owned();
    let cross_origin = obj
        .get("crossOrigin")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    Ok(ParsedClientData {
        kind,
        challenge,
        origin,
        cross_origin,
    })
}

/// Computes `SHA-256(client_data_json)` used as CTAP2 `clientDataHash`.
#[must_use]
pub fn client_data_hash(client_data_json: &[u8]) -> [u8; 32] {
    sha256_digest(client_data_json)
}

/// Deserializes `account.passkeyRegistrationOptions.options` JSON (`DeserializeRegisterData`).
pub fn deserialize_register_data(json_bytes: &[u8]) -> Result<RegisterData, WebAuthnErrorKind> {
    let root: JsonValue = serde_json::from_slice(json_bytes)
        .map_err(|e| WebAuthnErrorKind::InvalidOptions(format!("invalid register JSON: {e}")))?;
    let public_key = root
        .as_object()
        .and_then(|o| o.get("publicKey"))
        .and_then(JsonValue::as_object)
        .ok_or_else(|| {
            WebAuthnErrorKind::InvalidOptions("missing publicKey object in register data".into())
        })?;

    let rp_obj = public_key.get("rp").and_then(JsonValue::as_object);
    let rp_id_raw = rp_obj
        .and_then(|o| o.get("id"))
        .and_then(JsonValue::as_str)
        .unwrap_or(DEFAULT_RP_ID);
    let rp_id = validate_rp_id(rp_id_raw)?;
    let rp_name = rp_obj
        .and_then(|o| o.get("name"))
        .and_then(JsonValue::as_str)
        .unwrap_or("Telegram")
        .to_owned();

    let user_obj = public_key.get("user").and_then(JsonValue::as_object);
    let user_id_b64 = user_obj
        .and_then(|o| o.get("id"))
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let user_id = decode_base64_any(user_id_b64)
        .ok_or_else(|| WebAuthnErrorKind::InvalidOptions("invalid base64 user.id".into()))?;
    let user_name = user_obj
        .and_then(|o| o.get("name"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned();
    let user_display_name = user_obj
        .and_then(|o| o.get("displayName"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned();

    let challenge_b64 = public_key
        .get("challenge")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| WebAuthnErrorKind::InvalidOptions("missing register challenge".into()))?;
    let challenge = decode_base64_any(challenge_b64)
        .ok_or_else(|| WebAuthnErrorKind::InvalidOptions("invalid register challenge".into()))?;

    let mut pub_key_cred_params = Vec::new();
    if let Some(arr) = public_key
        .get("pubKeyCredParams")
        .and_then(JsonValue::as_array)
    {
        for entry in arr {
            if let Some(obj) = entry.as_object() {
                let cred_type = obj
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("public-key")
                    .to_owned();
                let alg = obj
                    .get("alg")
                    .and_then(JsonValue::as_i64)
                    .and_then(|v| i32::try_from(v).ok())
                    .unwrap_or(COSE_ES256);
                pub_key_cred_params.push(CredentialParameter { cred_type, alg });
            }
        }
    }
    if pub_key_cred_params.is_empty() {
        pub_key_cred_params.push(CredentialParameter {
            cred_type: "public-key".to_owned(),
            alg: COSE_ES256,
        });
    }

    let timeout_ms = public_key
        .get("timeout")
        .and_then(JsonValue::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    Ok(RegisterData {
        rp: RelyingParty {
            id: rp_id,
            name: rp_name,
        },
        user: PasskeyUser {
            id: user_id,
            name: user_name,
            display_name: user_display_name,
        },
        challenge,
        pub_key_cred_params,
        timeout_ms,
    })
}

/// Deserializes `auth.passkeyLoginOptions.options` JSON (`DeserializeLoginData`).
pub fn deserialize_login_data(json_bytes: &[u8]) -> Result<LoginData, WebAuthnErrorKind> {
    let root: JsonValue = serde_json::from_slice(json_bytes)
        .map_err(|e| WebAuthnErrorKind::InvalidOptions(format!("invalid login JSON: {e}")))?;
    let public_key = root
        .as_object()
        .and_then(|o| o.get("publicKey"))
        .and_then(JsonValue::as_object)
        .ok_or_else(|| {
            WebAuthnErrorKind::InvalidOptions("missing publicKey object in login data".into())
        })?;

    let challenge_b64 = public_key
        .get("challenge")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| WebAuthnErrorKind::InvalidOptions("missing login challenge".into()))?;
    let challenge = decode_base64_any(challenge_b64)
        .ok_or_else(|| WebAuthnErrorKind::InvalidOptions("invalid login challenge".into()))?;

    let rp_id_raw = public_key
        .get("rpId")
        .and_then(JsonValue::as_str)
        .unwrap_or(DEFAULT_RP_ID);
    let rp_id = validate_rp_id(rp_id_raw)?;

    let timeout_ms = public_key
        .get("timeout")
        .and_then(JsonValue::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    let user_verification = UserVerification::from_option_str(
        public_key
            .get("userVerification")
            .and_then(JsonValue::as_str),
    );

    let mut allow_credentials = Vec::new();
    if let Some(arr) = public_key
        .get("allowCredentials")
        .and_then(JsonValue::as_array)
    {
        for entry in arr {
            if let Some(obj) = entry.as_object() {
                let id_b64 = obj.get("id").and_then(JsonValue::as_str).unwrap_or("");
                let id = decode_base64_any(id_b64).ok_or_else(|| {
                    WebAuthnErrorKind::InvalidOptions("invalid allowCredential id".into())
                })?;
                let cred_type = obj
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("public-key")
                    .to_owned();
                allow_credentials.push(AllowCredential { id, cred_type });
            }
        }
    }

    Ok(LoginData {
        challenge,
        rp_id,
        allow_credentials,
        user_verification,
        timeout_ms,
    })
}

/// Selects the preferred COSE algorithm from `params` (`PreferredAlgorithm` in `webauthn_common.cpp:243`).
#[must_use]
pub fn preferred_cose_algorithm(params: &[CredentialParameter]) -> i32 {
    if params.iter().any(|p| p.alg == COSE_ES256) {
        return COSE_ES256;
    }
    for param in params {
        if param.alg == COSE_EDDSA || param.alg == COSE_RS256 {
            return param.alg;
        }
    }
    COSE_ES256
}

// ============================================================================
// Minimal Canonical CBOR Codec (port of `webauthn/cable_core.cpp:36-271`)
// ============================================================================

/// Subset of CBOR values used by CTAP2, WebAuthn attestation objects, and caBLEv2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CborValue {
    /// Signed 64-bit integer (CBOR major types 0 and 1).
    Int(i64),
    /// Byte string (CBOR major type 2).
    Bytes(Vec<u8>),
    /// UTF-8 text string (CBOR major type 3).
    Text(String),
    /// Array of CBOR values (CBOR major type 4).
    Array(Vec<CborValue>),
    /// Map of `(key, value)` pairs preserving insertion order (CBOR major type 5).
    Map(Vec<(CborValue, CborValue)>),
    /// Boolean value (`0xF4` = false, `0xF5` = true).
    Bool(bool),
}

impl CborValue {
    /// Looks up an integer key in a CBOR map.
    #[must_use]
    pub fn find_int(&self, key: i64) -> Option<&Self> {
        let Self::Map(entries) = self else {
            return None;
        };
        entries.iter().find_map(|(k, v)| match k {
            Self::Int(n) if *n == key => Some(v),
            _ => None,
        })
    }

    /// Looks up a text string key in a CBOR map.
    #[must_use]
    pub fn find_text(&self, key: &str) -> Option<&Self> {
        let Self::Map(entries) = self else {
            return None;
        };
        entries.iter().find_map(|(k, v)| match k {
            Self::Text(s) if s == key => Some(v),
            _ => None,
        })
    }

    /// Returns inner bytes if this is `CborValue::Bytes`.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// Returns inner text if this is `CborValue::Text`.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Returns inner slice if this is `CborValue::Array`.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns inner integer if this is `CborValue::Int`.
    #[must_use]
    pub const fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns inner bool if this is `CborValue::Bool`.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

fn cbor_append_unsigned(out: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    if value < 24 {
        out.push(prefix | value as u8);
    } else if value <= 0xFF {
        out.push(prefix | 24);
        out.push(value as u8);
    } else if value <= 0xFFFF {
        out.push(prefix | 25);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= 0xFFFF_FFFF {
        out.push(prefix | 26);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        out.push(prefix | 27);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn cbor_encode_to(out: &mut Vec<u8>, value: &CborValue) {
    match value {
        CborValue::Int(v) => {
            if *v >= 0 {
                cbor_append_unsigned(out, 0, *v as u64);
            } else {
                // Avoid overflow for i64::MIN via i128.
                let mag = (-1i128 - i128::from(*v)) as u64;
                cbor_append_unsigned(out, 1, mag);
            }
        }
        CborValue::Bytes(bytes) => {
            cbor_append_unsigned(out, 2, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        CborValue::Text(text) => {
            cbor_append_unsigned(out, 3, text.len() as u64);
            out.extend_from_slice(text.as_bytes());
        }
        CborValue::Array(items) => {
            cbor_append_unsigned(out, 4, items.len() as u64);
            for item in items {
                cbor_encode_to(out, item);
            }
        }
        CborValue::Map(entries) => {
            cbor_append_unsigned(out, 5, entries.len() as u64);
            for (k, v) in entries {
                cbor_encode_to(out, k);
                cbor_encode_to(out, v);
            }
        }
        CborValue::Bool(b) => out.push(if *b { 0xF5 } else { 0xF4 }),
    }
}

/// Encodes a [`CborValue`] into canonical CBOR bytes.
#[must_use]
pub fn cbor_encode(value: &CborValue) -> Vec<u8> {
    let mut out = Vec::new();
    cbor_encode_to(&mut out, value);
    out
}

struct CborReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> CborReader<'a> {
    fn read_byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }

    fn read_be(&mut self, count: usize) -> Option<u64> {
        if self.data.len().saturating_sub(self.pos) < count {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..count {
            value = (value << 8) | u64::from(self.data[self.pos]);
            self.pos += 1;
        }
        Some(value)
    }
}

fn cbor_decode_one(reader: &mut CborReader<'_>, depth: usize) -> Option<CborValue> {
    if depth > MAX_CBOR_DEPTH {
        return None;
    }
    let initial = reader.read_byte()?;
    let major = initial >> 5;
    let additional = initial & 0x1F;
    let argument = match additional {
        0..=23 => u64::from(additional),
        24 => reader.read_be(1)?,
        25 => reader.read_be(2)?,
        26 => reader.read_be(4)?,
        27 => reader.read_be(8)?,
        _ => return None,
    };
    match major {
        0 => {
            let v = i64::try_from(argument).ok()?;
            Some(CborValue::Int(v))
        }
        1 => {
            let v = i64::try_from(argument).ok()?;
            Some(CborValue::Int(-1 - v))
        }
        2 | 3 => {
            let len = usize::try_from(argument).ok()?;
            let remaining = reader.data.len().saturating_sub(reader.pos);
            if len > remaining {
                return None;
            }
            let slice = &reader.data[reader.pos..reader.pos + len];
            reader.pos += len;
            if major == 2 {
                Some(CborValue::Bytes(slice.to_vec()))
            } else {
                let text = std::str::from_utf8(slice).ok()?.to_owned();
                Some(CborValue::Text(text))
            }
        }
        4 => {
            let count = usize::try_from(argument).ok()?;
            if count > reader.data.len().saturating_sub(reader.pos) {
                return None;
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(cbor_decode_one(reader, depth + 1)?);
            }
            Some(CborValue::Array(items))
        }
        5 => {
            let count = usize::try_from(argument).ok()?;
            if count.saturating_mul(2) > reader.data.len().saturating_sub(reader.pos) {
                return None;
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let k = cbor_decode_one(reader, depth + 1)?;
                let v = cbor_decode_one(reader, depth + 1)?;
                entries.push((k, v));
            }
            Some(CborValue::Map(entries))
        }
        7 => match initial {
            0xF4 => Some(CborValue::Bool(false)),
            0xF5 => Some(CborValue::Bool(true)),
            _ => None,
        },
        _ => None,
    }
}

/// Decodes a complete CBOR buffer into a [`CborValue`], rejecting trailing bytes or excessive depth.
#[must_use]
pub fn cbor_decode(data: &[u8]) -> Option<CborValue> {
    let mut reader = CborReader { data, pos: 0 };
    let val = cbor_decode_one(&mut reader, 0)?;
    if reader.pos != reader.data.len() {
        return None;
    }
    Some(val)
}

// ============================================================================
// Attestation Object, AuthenticatorData & CTAP2 Codecs
// ============================================================================

/// Builds a `"none"` attestation object CBOR map (`{"fmt": "none", "attStmt": {}, "authData": authData}`),
/// matching `NoneAttestationObject` in `webauthn_common.cpp:211-232`.
#[must_use]
pub fn build_none_attestation_object(auth_data: &[u8]) -> Vec<u8> {
    cbor_encode(&CborValue::Map(vec![
        (
            CborValue::Text("fmt".to_owned()),
            CborValue::Text("none".to_owned()),
        ),
        (
            CborValue::Text("attStmt".to_owned()),
            CborValue::Map(Vec::new()),
        ),
        (
            CborValue::Text("authData".to_owned()),
            CborValue::Bytes(auth_data.to_vec()),
        ),
    ]))
}

/// Parsed fields of WebAuthn `authenticatorData` (`rpIdHash || flags || signCount [|| attestedCredentialData]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAuthenticatorData {
    /// `SHA-256(rpId)`.
    pub rp_id_hash: [u8; 32],
    /// Authenticator flags byte (`0x01` = UP, `0x04` = UV, `0x40` = AT, `0x80` = ED).
    pub flags: u8,
    /// Big-endian signature counter.
    pub sign_count: u32,
    /// Optional 16-byte AAGUID when the AT (`0x40`) flag is set.
    pub aaguid: Option<[u8; 16]>,
    /// Optional credential ID extracted from attested credential data when AT is set.
    pub credential_id: Option<Vec<u8>>,
    /// Trailing COSE public key CBOR bytes (plus optional extensions) when AT is set.
    pub credential_public_key_cbor: Option<Vec<u8>>,
}

impl ParsedAuthenticatorData {
    /// Returns `true` if the User Present (UP) bit (`0x01`) is set.
    #[must_use]
    pub const fn user_present(&self) -> bool {
        (self.flags & 0x01) != 0
    }

    /// Returns `true` if the User Verified (UV) bit (`0x04`) is set.
    #[must_use]
    pub const fn user_verified(&self) -> bool {
        (self.flags & 0x04) != 0
    }

    /// Returns `true` if Attested Credential Data (AT) bit (`0x40`) is set.
    #[must_use]
    pub const fn has_attested_credential_data(&self) -> bool {
        (self.flags & 0x40) != 0
    }
}

/// Parses raw WebAuthn `authenticatorData` bytes.
#[must_use]
pub fn parse_authenticator_data(auth_data: &[u8]) -> Option<ParsedAuthenticatorData> {
    const FIXED_PREFIX: usize = 32 + 1 + 4;
    if auth_data.len() < FIXED_PREFIX {
        return None;
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&auth_data[..32]);
    let flags = auth_data[32];
    let sign_count =
        u32::from_be_bytes([auth_data[33], auth_data[34], auth_data[35], auth_data[36]]);

    let (aaguid, credential_id, credential_public_key_cbor) = if (flags & 0x40) != 0 {
        if auth_data.len() < FIXED_PREFIX + 16 + 2 {
            return None;
        }
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&auth_data[FIXED_PREFIX..FIXED_PREFIX + 16]);
        let id_len = usize::from(u16::from_be_bytes([
            auth_data[FIXED_PREFIX + 16],
            auth_data[FIXED_PREFIX + 17],
        ]));
        let id_start = FIXED_PREFIX + 18;
        let id_end = id_start.checked_add(id_len)?;
        if auth_data.len() < id_end {
            return None;
        }
        let cred_id = auth_data[id_start..id_end].to_vec();
        let pub_key = if id_end < auth_data.len() {
            Some(auth_data[id_end..].to_vec())
        } else {
            None
        };
        (Some(guid), Some(cred_id), pub_key)
    } else {
        (None, None, None)
    };

    Some(ParsedAuthenticatorData {
        rp_id_hash,
        flags,
        sign_count,
        aaguid,
        credential_id,
        credential_public_key_cbor,
    })
}

/// Extracts the credential ID from attested `authenticatorData` (`CredentialIdFromAuthData`).
#[must_use]
pub fn credential_id_from_auth_data(auth_data: &[u8]) -> Option<Vec<u8>> {
    parse_authenticator_data(auth_data)?.credential_id
}

/// Parsed CBOR WebAuthn attestation object (`{"fmt", "authData", "attStmt"}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAttestationObject {
    /// Attestation statement format identifier (e.g. `"none"`, `"packed"`).
    pub fmt: String,
    /// Raw `authData` bytes.
    pub auth_data: Vec<u8>,
    /// Raw CBOR `attStmt` value.
    pub att_stmt: CborValue,
    /// Parsed `authData` structure.
    pub parsed_auth_data: ParsedAuthenticatorData,
}

/// Decodes and validates a CBOR attestation object.
#[must_use]
pub fn parse_attestation_object(cbor: &[u8]) -> Option<ParsedAttestationObject> {
    let val = cbor_decode(cbor)?;
    let fmt = val.find_text("fmt")?.as_text()?.to_owned();
    let auth_data = val.find_text("authData")?.as_bytes()?.to_vec();
    let att_stmt = val.find_text("attStmt")?.clone();
    let parsed_auth_data = parse_authenticator_data(&auth_data)?;
    Some(ParsedAttestationObject {
        fmt,
        auth_data,
        att_stmt,
        parsed_auth_data,
    })
}

/// Encodes a CTAP2 `authenticatorMakeCredential` (`0x01`) request (`BuildMakeCredentialRequest`).
#[must_use]
pub fn build_ctap_make_credential_request(
    client_data_hash_32: &[u8; 32],
    rp_id: &str,
    rp_name: &str,
    user_id: &[u8],
    user_name: &str,
    user_display_name: &str,
    algorithms: &[i32],
) -> Vec<u8> {
    let alg_list: Vec<i32> = if algorithms.is_empty() {
        vec![COSE_ES256]
    } else {
        algorithms.to_vec()
    };
    let params = CborValue::Array(
        alg_list
            .into_iter()
            .map(|alg| {
                CborValue::Map(vec![
                    (
                        CborValue::Text("alg".to_owned()),
                        CborValue::Int(i64::from(alg)),
                    ),
                    (
                        CborValue::Text("type".to_owned()),
                        CborValue::Text("public-key".to_owned()),
                    ),
                ])
            })
            .collect(),
    );
    let rp = CborValue::Map(vec![
        (
            CborValue::Text("id".to_owned()),
            CborValue::Text(rp_id.to_owned()),
        ),
        (
            CborValue::Text("name".to_owned()),
            CborValue::Text(rp_name.to_owned()),
        ),
    ]);
    let user = CborValue::Map(vec![
        (
            CborValue::Text("id".to_owned()),
            CborValue::Bytes(user_id.to_vec()),
        ),
        (
            CborValue::Text("name".to_owned()),
            CborValue::Text(user_name.to_owned()),
        ),
        (
            CborValue::Text("displayName".to_owned()),
            CborValue::Text(user_display_name.to_owned()),
        ),
    ]);
    let options = CborValue::Map(vec![
        (CborValue::Text("rk".to_owned()), CborValue::Bool(true)),
        (CborValue::Text("uv".to_owned()), CborValue::Bool(true)),
    ]);
    let body = CborValue::Map(vec![
        (
            CborValue::Int(1),
            CborValue::Bytes(client_data_hash_32.to_vec()),
        ),
        (CborValue::Int(2), rp),
        (CborValue::Int(3), user),
        (CborValue::Int(4), params),
        (CborValue::Int(7), options),
    ]);
    let mut out = vec![0x01];
    out.extend_from_slice(&cbor_encode(&body));
    out
}

/// Encodes a CTAP2 `authenticatorGetAssertion` (`0x02`) request (`BuildGetAssertionRequest`).
#[must_use]
pub fn build_ctap_get_assertion_request(
    rp_id: &str,
    client_data_hash_32: &[u8; 32],
    allow_credential_ids: &[Vec<u8>],
) -> Vec<u8> {
    let mut entries = vec![
        (CborValue::Int(1), CborValue::Text(rp_id.to_owned())),
        (
            CborValue::Int(2),
            CborValue::Bytes(client_data_hash_32.to_vec()),
        ),
    ];
    if !allow_credential_ids.is_empty() {
        let allow = CborValue::Array(
            allow_credential_ids
                .iter()
                .map(|id| {
                    CborValue::Map(vec![
                        (
                            CborValue::Text("id".to_owned()),
                            CborValue::Bytes(id.clone()),
                        ),
                        (
                            CborValue::Text("type".to_owned()),
                            CborValue::Text("public-key".to_owned()),
                        ),
                    ])
                })
                .collect(),
        );
        entries.push((CborValue::Int(3), allow));
    }
    let options = CborValue::Map(vec![
        (CborValue::Text("up".to_owned()), CborValue::Bool(true)),
        (CborValue::Text("uv".to_owned()), CborValue::Bool(true)),
    ]);
    entries.push((CborValue::Int(5), options));

    let mut out = vec![0x02];
    out.extend_from_slice(&cbor_encode(&CborValue::Map(entries)));
    out
}

/// Parsed CTAP2 `authenticatorMakeCredential` response (`MakeCredentialResponse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtapMakeCredentialResponse {
    /// Raw authenticator data bytes.
    pub auth_data: Vec<u8>,
    /// Extracted credential ID bytes.
    pub credential_id: Vec<u8>,
}

/// Parsed CTAP2 `authenticatorGetAssertion` response (`GetAssertionResponse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtapGetAssertionResponse {
    /// Selected credential ID bytes (may be empty if single-credential allowlist omitted it).
    pub credential_id: Vec<u8>,
    /// Raw authenticator data bytes.
    pub auth_data: Vec<u8>,
    /// Assertion signature bytes.
    pub signature: Vec<u8>,
    /// User handle bytes (if returned in CTAP2 map key `4`).
    pub user_handle: Vec<u8>,
}

/// Parses a CTAP2 `authenticatorMakeCredential` response with leading status byte (`ParseMakeCredentialResponse`).
#[must_use]
pub fn parse_ctap_make_credential_response(
    payload_with_status: &[u8],
) -> Option<CtapMakeCredentialResponse> {
    if payload_with_status.first().copied() != Some(CTAP_SUCCESS) {
        return None;
    }
    let parsed = cbor_decode(&payload_with_status[1..])?;
    let auth_data = parsed.find_int(2)?.as_bytes()?.to_vec();
    let credential_id = credential_id_from_auth_data(&auth_data)?;
    Some(CtapMakeCredentialResponse {
        auth_data,
        credential_id,
    })
}

/// Parses a CTAP2 `authenticatorGetAssertion` response with leading status byte (`ParseGetAssertionResponse`).
#[must_use]
pub fn parse_ctap_get_assertion_response(
    payload_with_status: &[u8],
) -> Option<CtapGetAssertionResponse> {
    if payload_with_status.first().copied() != Some(CTAP_SUCCESS) {
        return None;
    }
    let parsed = cbor_decode(&payload_with_status[1..])?;
    let credential_id = parsed
        .find_int(1)
        .and_then(|c| c.find_text("id"))
        .and_then(CborValue::as_bytes)
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    let auth_data = parsed.find_int(2)?.as_bytes()?.to_vec();
    let signature = parsed.find_int(3)?.as_bytes()?.to_vec();
    let user_handle = parsed
        .find_int(4)
        .and_then(|u| u.find_text("id"))
        .and_then(CborValue::as_bytes)
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    Some(CtapGetAssertionResponse {
        credential_id,
        auth_data,
        signature,
        user_handle,
    })
}

// ============================================================================
// Cross-Device caBLEv2 / Hybrid QR Protocol (port of `webauthn/cable_core.cpp`)
// ============================================================================

/// caBLEv2 framing message type (`Platform::WebAuthn::Cable::MessageType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CableMessageType {
    /// Tunnel shutdown notice.
    Shutdown = 0,
    /// Encapsulated CTAP2 request/response payload.
    Ctap = 1,
    /// Linking / authenticator metadata update.
    Update = 2,
    /// JSON control message.
    Json = 3,
}

/// Key derivation purpose tag for caBLEv2 HKDF (`DerivedValueType` in `cable_core.h:136`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CableDerivedValueType {
    /// 64-byte BLE EID decryption + HMAC key.
    EidKey = 1,
    /// 16-byte WebSocket tunnel path identifier.
    TunnelId = 2,
    /// 32-byte Noise protocol pre-shared key.
    Psk = 3,
}

/// Computes `SHA-256(data)`.
#[must_use]
pub fn sha256_digest(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Computes `HMAC-SHA256(key, data)`.
#[must_use]
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Computes RFC 5869 HKDF-SHA256 matching `HkdfSha256` in `cable_core.cpp:301-336`.
#[must_use]
pub fn hkdf_sha256(secret: &[u8], salt: &[u8], info: &[u8], out_len: usize) -> Vec<u8> {
    let effective_salt = if salt.is_empty() {
        &[0u8; 32][..]
    } else {
        salt
    };
    let prk = hmac_sha256(effective_salt, secret);
    let mut out = Vec::with_capacity(out_len);
    let mut prev: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < out_len {
        let mut mac =
            <HmacSha256 as Mac>::new_from_slice(&prk).expect("HMAC-SHA256 accepts 32-byte PRK");
        mac.update(&prev);
        mac.update(info);
        mac.update(&[counter]);
        let block: [u8; 32] = mac.finalize().into_bytes().into();
        let take = (out_len - out.len()).min(block.len());
        out.extend_from_slice(&block[..take]);
        prev.clear();
        prev.extend_from_slice(&block);
        counter = counter.wrapping_add(1);
    }
    out
}

/// Derives a fixed-size caBLEv2 subkey (`Derive<Size>` in `cable_core.h:143`).
#[must_use]
pub fn cable_derive<const N: usize>(
    secret: &[u8],
    nonce: &[u8],
    kind: CableDerivedValueType,
) -> [u8; N] {
    let info = (kind as u32).to_le_bytes();
    let derived = hkdf_sha256(secret, nonce, &info, N);
    let mut out = [0u8; N];
    out.copy_from_slice(&derived);
    out
}

/// Encodes raw bytes into CTAP2 hybrid QR decimal digits (`BytesToDigits` in `cable_core.cpp:755`).
#[must_use]
pub fn cable_bytes_to_digits(mut bytes: &[u8]) -> String {
    const WIDTHS: [usize; 8] = [0, 3, 5, 8, 10, 13, 15, 17];
    let mut out = String::with_capacity(bytes.len().div_ceil(7) * 17);
    while !bytes.is_empty() {
        let take = bytes.len().min(7);
        let mut value = 0u64;
        for (i, &b) in bytes[..take].iter().enumerate() {
            value |= u64::from(b) << (8 * i);
        }
        let width = WIDTHS[take];
        out.push_str(&format!("{value:0width$}"));
        bytes = &bytes[take..];
    }
    out
}

/// Encodes a `FIDO:/...` hybrid QR payload (`EncodeQRContents` in `cable_core.cpp:741`).
#[must_use]
pub fn cable_encode_qr_contents(
    compressed_pubkey_33: &[u8; P256_COMPRESSED_LEN],
    qr_secret_16: &[u8; CABLE_QR_SECRET_LEN],
    make_credential_hint: bool,
    now_unix_seconds: i64,
) -> String {
    let cbor = cbor_encode(&CborValue::Map(vec![
        (
            CborValue::Int(0),
            CborValue::Bytes(compressed_pubkey_33.to_vec()),
        ),
        (CborValue::Int(1), CborValue::Bytes(qr_secret_16.to_vec())),
        (
            CborValue::Int(2),
            CborValue::Int(CABLE_ASSIGNED_TUNNEL_DOMAINS.len() as i64),
        ),
        (CborValue::Int(3), CborValue::Int(now_unix_seconds)),
        (CborValue::Int(4), CborValue::Bool(false)),
        (
            CborValue::Int(5),
            CborValue::Text(if make_credential_hint { "mc" } else { "ga" }.to_owned()),
        ),
    ]));
    format!("FIDO:/{}", cable_bytes_to_digits(&cbor))
}

/// Decoded components of a decrypted 16-byte caBLEv2 BLE EID (`EidComponents`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CableEidComponents {
    /// 10-byte nonce (`eid[1..11]`).
    pub nonce: [u8; CABLE_NONCE_LEN],
    /// 3-byte tunnel routing ID (`eid[11..14]`).
    pub routing_id: [u8; CABLE_ROUTING_ID_LEN],
    /// Encoded tunnel server domain (`u16::from_le_bytes(eid[14..16])`).
    pub tunnel_server_domain: u16,
}

/// Verifies and decrypts a 20-byte caBLEv2 BLE service data advert (`DecryptAdvert`).
#[must_use]
pub fn cable_decrypt_advert(
    service_data: &[u8],
    eid_key: &[u8; CABLE_EID_KEY_LEN],
) -> Option<[u8; CABLE_EID_LEN]> {
    if service_data.len() < CABLE_ADVERT_LEN {
        return None;
    }
    let aes_key = &eid_key[..32];
    let hmac_key = &eid_key[32..];
    let body = &service_data[..16];
    let tag = &service_data[16..20];
    let expected = hmac_sha256(hmac_key, body);
    if expected[..4] != *tag {
        return None;
    }
    let cipher = Aes256::new_from_slice(aes_key).ok()?;
    let mut block = aes::Block::clone_from_slice(body);
    cipher.decrypt_block(&mut block);
    let mut plaintext = [0u8; CABLE_EID_LEN];
    plaintext.copy_from_slice(&block);
    if plaintext[0] != 0 {
        return None;
    }
    let domain = u16::from_le_bytes([plaintext[14], plaintext[15]]);
    if domain >= 256 || usize::from(domain) < CABLE_ASSIGNED_TUNNEL_DOMAINS.len() {
        Some(plaintext)
    } else {
        None
    }
}

/// Encrypts a 16-byte EID into a 20-byte BLE service-data advert (inverse of [`cable_decrypt_advert`]).
#[must_use]
pub fn cable_encrypt_advert(
    eid: &[u8; CABLE_EID_LEN],
    eid_key: &[u8; CABLE_EID_KEY_LEN],
) -> [u8; CABLE_ADVERT_LEN] {
    let cipher = Aes256::new_from_slice(&eid_key[..32]).expect("AES-256 accepts 32-byte key");
    let mut block = aes::Block::clone_from_slice(eid);
    cipher.encrypt_block(&mut block);
    let tag = hmac_sha256(&eid_key[32..], &block);
    let mut out = [0u8; CABLE_ADVERT_LEN];
    out[..16].copy_from_slice(&block);
    out[16..20].copy_from_slice(&tag[..4]);
    out
}

/// Extracts nonce, routing ID, and tunnel domain from a decrypted 16-byte EID (`ToEidComponents`).
#[must_use]
pub fn cable_to_eid_components(eid: &[u8; CABLE_EID_LEN]) -> CableEidComponents {
    let mut nonce = [0u8; CABLE_NONCE_LEN];
    nonce.copy_from_slice(&eid[1..1 + CABLE_NONCE_LEN]);
    let mut routing_id = [0u8; CABLE_ROUTING_ID_LEN];
    routing_id
        .copy_from_slice(&eid[1 + CABLE_NONCE_LEN..1 + CABLE_NONCE_LEN + CABLE_ROUTING_ID_LEN]);
    let tunnel_server_domain = u16::from_le_bytes([eid[14], eid[15]]);
    CableEidComponents {
        nonce,
        routing_id,
        tunnel_server_domain,
    }
}

/// Decodes a caBLEv2 tunnel server domain ID (`DecodeTunnelServerDomain` in `cable_core.cpp:821`).
#[must_use]
pub fn cable_decode_tunnel_server_domain(domain: u16) -> Option<String> {
    if domain < 256 {
        return CABLE_ASSIGNED_TUNNEL_DOMAINS
            .get(usize::from(domain))
            .map(|s| (*s).to_owned());
    }
    let mut input = b"caBLEv2 tunnel server domain".to_vec();
    input.push((domain & 0xFF) as u8);
    input.push((domain >> 8) as u8);
    input.push(0);
    let digest = sha256_digest(&input);
    let mut value = u64::from_le_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);
    const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    const TLDS: [&str; 4] = [".com", ".org", ".net", ".info"];
    let tld = TLDS[(value & 3) as usize];
    value >>= 2;
    let mut result = String::from("cable.");
    while value != 0 {
        result.push(BASE32[(value & 31) as usize] as char);
        value >>= 5;
    }
    result.push_str(tld);
    Some(result)
}

/// Formats bytes as lowercase hexadecimal (`HexLower` in `cable_core.cpp:852`).
#[must_use]
pub fn cable_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Builds the WebSocket tunnel connect path (`"/cable/connect/<routing_hex>/<tunnel_hex>"`).
#[must_use]
pub fn cable_tunnel_connect_path(
    routing_id: &[u8; CABLE_ROUTING_ID_LEN],
    tunnel_id: &[u8; CABLE_TUNNEL_ID_LEN],
) -> String {
    format!(
        "/cable/connect/{}/{}",
        cable_hex_lower(routing_id),
        cable_hex_lower(tunnel_id)
    )
}

/// Compresses an uncompressed 65-byte SEC1 P-256 point into its 33-byte compressed representation.
#[must_use]
pub fn compress_p256_sec1(uncompressed: &[u8]) -> Option<[u8; P256_COMPRESSED_LEN]> {
    if uncompressed.len() != P256_X962_LEN || uncompressed[0] != 0x04 {
        return None;
    }
    let mut out = [0u8; P256_COMPRESSED_LEN];
    out[0] = if (uncompressed[64] & 1) == 0 {
        0x02
    } else {
        0x03
    };
    out[1..].copy_from_slice(&uncompressed[1..33]);
    Some(out)
}

/// Computes the 32-byte ECDH shared X coordinate for a P-256 secret key and peer SEC1 point.
#[must_use]
pub fn p256_ecdh_shared_x(secret: &P256SecretKey, peer_sec1: &[u8]) -> Option<[u8; 32]> {
    let peer_pub = P256PublicKey::from_sec1_bytes(peer_sec1).ok()?;
    let shared = diffie_hellman(secret.to_nonzero_scalar(), peer_pub.as_affine());
    let mut out = [0u8; 32];
    out.copy_from_slice(shared.raw_secret_bytes().as_slice());
    Some(out)
}

/// Seals `plaintext` with AES-256-GCM and `aad`, returning `ciphertext || 16-byte tag`.
#[must_use]
pub fn aes_gcm_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
    aad: &[u8],
) -> Option<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .ok()
}

/// Opens `ciphertext || 16-byte tag` with AES-256-GCM and `aad`.
#[must_use]
pub fn aes_gcm_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
) -> Option<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .ok()
}

/// Noise handshake pattern used by caBLEv2 (`Noise::HandshakeType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CableHandshakeType {
    /// Initiator's static key is known to responder (QR flow).
    KNpsk0,
    /// Responder's static key is known to initiator (linked device flow).
    NKpsk0,
}

/// caBLEv2 Noise symmetric state (`Platform::WebAuthn::Cable::Noise`).
#[derive(Debug, Clone)]
pub struct CableNoise {
    chaining_key: [u8; 32],
    h: [u8; 32],
    symmetric_key: [u8; 32],
    symmetric_nonce: u32,
}

impl CableNoise {
    /// Initializes Noise state for `kind` (`Noise::init` in `cable_core.cpp:530`).
    #[must_use]
    pub fn new(kind: CableHandshakeType) -> Self {
        let name = match kind {
            CableHandshakeType::KNpsk0 => b"Noise_KNpsk0_P256_AESGCM_SHA256",
            CableHandshakeType::NKpsk0 => b"Noise_NKpsk0_P256_AESGCM_SHA256",
        };
        let mut ck = [0u8; 32];
        ck[..name.len()].copy_from_slice(name);
        Self {
            chaining_key: ck,
            h: ck,
            symmetric_key: [0u8; 32],
            symmetric_nonce: 0,
        }
    }

    /// Mixes `input` into the handshake hash `h = SHA-256(h || input)`.
    pub fn mix_hash(&mut self, input: &[u8]) {
        let mut buf = Vec::with_capacity(self.h.len() + input.len());
        buf.extend_from_slice(&self.h);
        buf.extend_from_slice(input);
        self.h = sha256_digest(&buf);
    }

    /// Mixes key material into the chaining key and resets the symmetric cipher key.
    pub fn mix_key(&mut self, ikm: &[u8]) {
        let out = hkdf_sha256(ikm, &self.chaining_key, &[], 64);
        self.chaining_key.copy_from_slice(&out[..32]);
        self.symmetric_key.copy_from_slice(&out[32..64]);
        self.symmetric_nonce = 0;
    }

    /// Mixes PSK material into chaining key, handshake hash, and symmetric key.
    pub fn mix_key_and_hash(&mut self, ikm: &[u8]) {
        let out = hkdf_sha256(ikm, &self.chaining_key, &[], 96);
        self.chaining_key.copy_from_slice(&out[..32]);
        self.mix_hash(&out[32..64]);
        self.symmetric_key.copy_from_slice(&out[64..96]);
        self.symmetric_nonce = 0;
    }

    /// Encrypts `plaintext` with AAD `h`, increments nonce, and mixes ciphertext into `h`.
    #[must_use]
    pub fn encrypt_and_hash(&mut self, plaintext: &[u8]) -> Option<Vec<u8>> {
        let mut nonce = [0u8; 12];
        nonce[0..4].copy_from_slice(&self.symmetric_nonce.to_be_bytes());
        self.symmetric_nonce = self.symmetric_nonce.wrapping_add(1);
        let ct = aes_gcm_seal(&self.symmetric_key, &nonce, plaintext, &self.h)?;
        self.mix_hash(&ct);
        Some(ct)
    }

    /// Decrypts `ciphertext` with AAD `h`, increments nonce, and mixes ciphertext into `h`.
    #[must_use]
    pub fn decrypt_and_hash(&mut self, ciphertext: &[u8]) -> Option<Vec<u8>> {
        let mut nonce = [0u8; 12];
        nonce[0..4].copy_from_slice(&self.symmetric_nonce.to_be_bytes());
        self.symmetric_nonce = self.symmetric_nonce.wrapping_add(1);
        let pt = aes_gcm_open(&self.symmetric_key, &nonce, ciphertext, &self.h)?;
        self.mix_hash(ciphertext);
        Some(pt)
    }

    /// Splits the chaining key into `(first_key, second_key)` traffic keys.
    #[must_use]
    pub fn traffic_keys(&self) -> ([u8; 32], [u8; 32]) {
        let out = hkdf_sha256(&[], &self.chaining_key, &[], 64);
        let mut first = [0u8; 32];
        let mut second = [0u8; 32];
        first.copy_from_slice(&out[..32]);
        second.copy_from_slice(&out[32..64]);
        (first, second)
    }

    /// Returns the current handshake transcript hash `h`.
    #[must_use]
    pub const fn handshake_hash(&self) -> [u8; 32] {
        self.h
    }
}

/// Padded AES-256-GCM post-handshake traffic crypter (`Platform::WebAuthn::Cable::Crypter`).
#[derive(Debug, Clone)]
pub struct CableCrypter {
    read_key: [u8; 32],
    write_key: [u8; 32],
    read_seq: u32,
    write_seq: u32,
}

impl CableCrypter {
    /// Creates a new crypter with directional 32-byte traffic keys.
    #[must_use]
    pub const fn new(read_key: [u8; 32], write_key: [u8; 32]) -> Self {
        Self {
            read_key,
            write_key,
            read_seq: 0,
            write_seq: 0,
        }
    }

    /// Pads `plaintext` to a 32-byte boundary and seals it with the next write sequence counter.
    #[must_use]
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Option<Vec<u8>> {
        if self.write_seq > CABLE_MAX_SEQUENCE {
            return None;
        }
        let padded_len = (plaintext.len() + 1 + CABLE_PADDING_GRANULARITY - 1)
            & !(CABLE_PADDING_GRANULARITY - 1);
        let mut padded = vec![0u8; padded_len];
        padded[..plaintext.len()].copy_from_slice(plaintext);
        padded[padded_len - 1] = (padded_len - plaintext.len() - 1) as u8;

        let mut nonce = [0u8; 12];
        let counter = self.write_seq;
        self.write_seq += 1;
        nonce[8..12].copy_from_slice(&counter.to_be_bytes());
        aes_gcm_seal(&self.write_key, &nonce, &padded, &[])
    }

    /// Decrypts `ciphertext` with the next read sequence counter and strips trailing padding.
    #[must_use]
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Option<Vec<u8>> {
        if self.read_seq > CABLE_MAX_SEQUENCE {
            return None;
        }
        let mut nonce = [0u8; 12];
        nonce[8..12].copy_from_slice(&self.read_seq.to_be_bytes());
        let mut plaintext = aes_gcm_open(&self.read_key, &nonce, ciphertext, &[])?;
        self.read_seq += 1;
        let padding = usize::from(*plaintext.last()?);
        if padding + 1 > plaintext.len() {
            return None;
        }
        plaintext.truncate(plaintext.len() - padding - 1);
        Some(plaintext)
    }
}

/// Completed caBLEv2 Noise handshake result (`HandshakeResult`).
#[derive(Debug, Clone)]
pub struct CableHandshakeResult {
    /// Post-handshake encrypted channel.
    pub crypter: CableCrypter,
    /// Final Noise transcript hash `h`.
    pub handshake_hash: [u8; 32],
}

enum CableHandshakeInitiatorMode {
    KNpsk0 {
        local_identity: P256SecretKey,
    },
    NKpsk0 {
        peer_identity_x962: [u8; P256_X962_LEN],
    },
}

/// caBLEv2 `KNpsk0` / `NKpsk0` Noise handshake initiator (`HandshakeInitiator` in `cable_core.cpp:668`
/// and Chromium `device/fido/cable/v2_handshake.cc`).
pub struct CableHandshakeInitiator {
    psk: [u8; CABLE_PSK_LEN],
    mode: CableHandshakeInitiatorMode,
    ephemeral: Option<P256SecretKey>,
    noise: CableNoise,
}

impl CableHandshakeInitiator {
    /// Creates a `KNpsk0` initiator bound to `psk` and the QR `local_identity` P-256 key.
    #[must_use]
    pub fn new(psk: [u8; CABLE_PSK_LEN], local_identity: P256SecretKey) -> Self {
        Self {
            psk,
            mode: CableHandshakeInitiatorMode::KNpsk0 { local_identity },
            ephemeral: None,
            noise: CableNoise::new(CableHandshakeType::KNpsk0),
        }
    }

    /// Creates an `NKpsk0` initiator bound to `psk` and the paired authenticator's uncompressed
    /// 65-byte SEC1 P-256 static public key (`0x04 || X || Y`).
    #[must_use]
    pub fn new_nkpsk0(psk: [u8; CABLE_PSK_LEN], peer_identity_x962: [u8; P256_X962_LEN]) -> Self {
        Self {
            psk,
            mode: CableHandshakeInitiatorMode::NKpsk0 { peer_identity_x962 },
            ephemeral: None,
            noise: CableNoise::new(CableHandshakeType::NKpsk0),
        }
    }

    /// Returns the Noise handshake pattern configured for this initiator.
    #[must_use]
    pub const fn handshake_type(&self) -> CableHandshakeType {
        match self.mode {
            CableHandshakeInitiatorMode::KNpsk0 { .. } => CableHandshakeType::KNpsk0,
            CableHandshakeInitiatorMode::NKpsk0 { .. } => CableHandshakeType::NKpsk0,
        }
    }

    /// Builds the 81-byte initial Noise message (`ephemeral_pub[65] || encrypted_empty[16]`).
    #[must_use]
    pub fn build_initial_message(&mut self) -> Option<Vec<u8>> {
        let ephemeral = P256SecretKey::generate();
        self.build_initial_message_with_ephemeral(ephemeral)
    }

    /// Builds the initial Noise message with a caller-supplied ephemeral key (for deterministic tests).
    #[must_use]
    pub fn build_initial_message_with_ephemeral(
        &mut self,
        ephemeral: P256SecretKey,
    ) -> Option<Vec<u8>> {
        let eph_pub = ephemeral.public_key().to_sec1_bytes();
        match &self.mode {
            CableHandshakeInitiatorMode::KNpsk0 { local_identity } => {
                self.noise = CableNoise::new(CableHandshakeType::KNpsk0);
                self.noise.mix_hash(&[1u8]);
                let local_pub = local_identity.public_key().to_sec1_bytes();
                self.noise.mix_hash(&local_pub);
                self.noise.mix_key_and_hash(&self.psk);
                self.noise.mix_hash(&eph_pub);
                self.noise.mix_key(&eph_pub);
            }
            CableHandshakeInitiatorMode::NKpsk0 { peer_identity_x962 } => {
                self.noise = CableNoise::new(CableHandshakeType::NKpsk0);
                self.noise.mix_hash(&[0u8]);
                self.noise.mix_hash(peer_identity_x962);
                self.noise.mix_key_and_hash(&self.psk);
                self.noise.mix_hash(&eph_pub);
                self.noise.mix_key(&eph_pub);
                let shared_es = p256_ecdh_shared_x(&ephemeral, peer_identity_x962)?;
                self.noise.mix_key(&shared_es);
            }
        }
        self.ephemeral = Some(ephemeral);

        let ct = self.noise.encrypt_and_hash(&[])?;
        let mut msg = Vec::with_capacity(eph_pub.len() + ct.len());
        msg.extend_from_slice(&eph_pub);
        msg.extend_from_slice(&ct);
        Some(msg)
    }

    /// Processes the 81-byte responder message and yields traffic keys (`processResponse`).
    #[must_use]
    pub fn process_response(&mut self, response: &[u8]) -> Option<CableHandshakeResult> {
        if response.len() != CABLE_HANDSHAKE_RESPONSE_LEN {
            return None;
        }
        let ephemeral = self.ephemeral.as_ref()?;
        let peer_point = &response[..P256_X962_LEN];
        let ciphertext = &response[P256_X962_LEN..];

        let shared_ee = p256_ecdh_shared_x(ephemeral, peer_point)?;
        self.noise.mix_hash(peer_point);
        self.noise.mix_key(peer_point);
        self.noise.mix_key(&shared_ee);

        if let CableHandshakeInitiatorMode::KNpsk0 { local_identity } = &self.mode {
            let shared_se = p256_ecdh_shared_x(local_identity, peer_point)?;
            self.noise.mix_key(&shared_se);
        }

        let pt = self.noise.decrypt_and_hash(ciphertext)?;
        if !pt.is_empty() {
            return None;
        }
        let (write_key, read_key) = self.noise.traffic_keys();
        Some(CableHandshakeResult {
            crypter: CableCrypter::new(read_key, write_key),
            handshake_hash: self.noise.handshake_hash(),
        })
    }
}

/// Parsed caBLEv2 post-handshake authenticator message (`PostHandshakeMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CablePostHandshakeMessage {
    /// Raw CTAP2 `getInfo` response bytes (map key `1`), if present.
    pub get_info: Vec<u8>,
    /// Protocol revision (`1` for unpadded CBOR, `0` for legacy padded CBOR).
    pub protocol_revision: u8,
    /// Whether the authenticator advertises CTAP support.
    pub supports_ctap: bool,
}

fn decode_padded_cbor_map(input: &[u8]) -> Option<CborValue> {
    if input.len() >= 2 {
        let pad = usize::from(u16::from_le_bytes([
            input[input.len() - 2],
            input[input.len() - 1],
        ]));
        if pad + 2 <= input.len()
            && let Some(val) = cbor_decode(&input[..input.len() - pad - 2])
            && matches!(val, CborValue::Map(_))
        {
            return Some(val);
        }
    }
    if let Some(&last) = input.last() {
        let pad = usize::from(last);
        if pad < input.len()
            && let Some(val) = cbor_decode(&input[..input.len() - pad - 1])
            && matches!(val, CborValue::Map(_))
        {
            return Some(val);
        }
    }
    None
}

/// Parses a decrypted caBLEv2 post-handshake message (`ParsePostHandshakeMessage`).
#[must_use]
pub fn parse_post_handshake_message(plaintext: &[u8]) -> Option<CablePostHandshakeMessage> {
    let (parsed, protocol_revision) = match cbor_decode(plaintext) {
        Some(val) if matches!(val, CborValue::Map(_)) => (val, 1),
        _ => (decode_padded_cbor_map(plaintext)?, 0),
    };
    let mut supports_ctap = true;
    if let Some(features) = parsed.find_int(3) {
        let arr = features.as_array()?;
        supports_ctap = arr.iter().any(|f| f.as_text() == Some("ctap"));
    }
    let get_info = match parsed.find_int(1) {
        Some(val) => val.as_bytes()?.to_vec(),
        None => Vec::new(),
    };
    Some(CablePostHandshakeMessage {
        get_info,
        protocol_revision,
        supports_ctap,
    })
}

/// Lifecycle step of a cross-device caBLEv2 hybrid QR ceremony (`Ceremony::Step`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CableCeremonyStep {
    /// Displaying QR code and scanning BLE adverts.
    Scanning,
    /// Connecting to the tunnel WebSocket server.
    Connecting,
    /// Performing Noise KNpsk0 handshake.
    Handshake,
    /// Waiting for authenticator post-handshake capabilities.
    PostHandshake,
    /// Waiting for CTAP2 MakeCredential / GetAssertion reply.
    AwaitingReply,
    /// Ceremony finished (success or failure).
    Finished,
}

/// Stateful caBLEv2 hybrid QR session generated for a cross-device registration or login ceremony.
pub struct CableQrSession {
    identity: P256SecretKey,
    secret: [u8; CABLE_QR_SECRET_LEN],
    eid_key: [u8; CABLE_EID_KEY_LEN],
    qr_uri: String,
    is_register: bool,
    step: CableCeremonyStep,
}

impl fmt::Debug for CableQrSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CableQrSession")
            .field("qr_uri", &self.qr_uri)
            .field("is_register", &self.is_register)
            .field("step", &self.step)
            .finish()
    }
}

impl CableQrSession {
    /// Generates a fresh random caBLEv2 QR session (`MakeQRKey` + `EncodeQRContents`).
    pub fn generate(is_register: bool, now_unix_seconds: i64) -> Result<Self, WebAuthnErrorKind> {
        let identity = P256SecretKey::generate();
        let mut secret = [0u8; CABLE_QR_SECRET_LEN];
        getrandom::getrandom(&mut secret)
            .map_err(|e| WebAuthnErrorKind::Other(format!("RNG failure: {e}")))?;
        Self::from_parts(identity, secret, is_register, now_unix_seconds)
    }

    /// Builds a caBLEv2 QR session from explicit key material (for deterministic tests).
    pub fn from_parts(
        identity: P256SecretKey,
        secret: [u8; CABLE_QR_SECRET_LEN],
        is_register: bool,
        now_unix_seconds: i64,
    ) -> Result<Self, WebAuthnErrorKind> {
        let uncompressed = identity.public_key().to_sec1_bytes();
        let compressed = compress_p256_sec1(&uncompressed).ok_or_else(|| {
            WebAuthnErrorKind::Other("failed to compress P-256 public key".to_owned())
        })?;
        let eid_key =
            cable_derive::<CABLE_EID_KEY_LEN>(&secret, &[], CableDerivedValueType::EidKey);
        let qr_uri = cable_encode_qr_contents(&compressed, &secret, is_register, now_unix_seconds);
        Ok(Self {
            identity,
            secret,
            eid_key,
            qr_uri,
            is_register,
            step: CableCeremonyStep::Scanning,
        })
    }

    /// Returns the `"FIDO:/..."` URI to render as a QR code for the phone camera.
    #[must_use]
    pub fn qr_uri(&self) -> &str {
        &self.qr_uri
    }

    /// Returns whether this ceremony is a passkey registration (`true`) or login (`false`).
    #[must_use]
    pub const fn is_register(&self) -> bool {
        self.is_register
    }

    /// Returns the current ceremony step.
    #[must_use]
    pub const fn step(&self) -> CableCeremonyStep {
        self.step
    }

    /// Returns the derived 64-byte EID key used to match BLE adverts.
    #[must_use]
    pub const fn eid_key(&self) -> &[u8; CABLE_EID_KEY_LEN] {
        &self.eid_key
    }

    /// Returns the 16-byte QR secret shared with the authenticator via the QR code.
    #[must_use]
    pub const fn qr_secret(&self) -> &[u8; CABLE_QR_SECRET_LEN] {
        &self.secret
    }

    /// Returns the initiator's uncompressed 65-byte SEC1 P-256 identity public key (`0x04 || X || Y`).
    #[must_use]
    pub fn identity_public_x962(&self) -> [u8; P256_X962_LEN] {
        let sec1 = self.identity.public_key().to_sec1_bytes();
        let mut out = [0u8; P256_X962_LEN];
        out.copy_from_slice(&sec1);
        out
    }

    /// Updates the current ceremony step.
    pub fn set_step(&mut self, step: CableCeremonyStep) {
        self.step = step;
    }

    /// Attempts to match and decrypt a BLE advert, returning `(tunnel_domain, ws_path, handshake)`
    /// if the advert belongs to this QR session (`OnAdvert` in `cable_ceremony.cpp:280`).
    pub fn accept_ble_advert(
        &mut self,
        service_data: &[u8],
    ) -> Option<(String, String, CableHandshakeInitiator)> {
        if self.step != CableCeremonyStep::Scanning {
            return None;
        }
        let eid = cable_decrypt_advert(service_data, &self.eid_key)?;
        let components = cable_to_eid_components(&eid);
        let domain = cable_decode_tunnel_server_domain(components.tunnel_server_domain)?;
        let psk = cable_derive::<CABLE_PSK_LEN>(&self.secret, &eid, CableDerivedValueType::Psk);
        let tunnel_id =
            cable_derive::<CABLE_TUNNEL_ID_LEN>(&self.secret, &[], CableDerivedValueType::TunnelId);
        let path = cable_tunnel_connect_path(&components.routing_id, &tunnel_id);
        self.step = CableCeremonyStep::Connecting;
        let initiator = CableHandshakeInitiator::new(psk, self.identity.clone());
        Some((domain, path, initiator))
    }
}

/// Handle returned when initiating a cross-device caBLE / hybrid QR ceremony.
#[derive(Debug)]
pub struct CableCeremonyHandle {
    /// Active caBLE QR session with `"FIDO:/..."` payload.
    pub session: CableQrSession,
    /// Pre-serialized `clientDataJSON` bound to the ceremony challenge.
    pub client_data_json: String,
    /// `SHA-256(client_data_json)`.
    pub client_data_hash: [u8; 32],
}

fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// ============================================================================
// Windows `webauthn.dll` FFI (`#[cfg(windows)]`)
// ============================================================================

#[cfg(windows)]
mod win_ffi {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{FALSE, HMODULE, HWND, TRUE};
    use windows::Win32::System::Com::CoCreateGuid;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    use windows::core::{BOOL, GUID, HRESULT, PCSTR, PCWSTR, w};

    use super::{
        AuthenticatorCapabilities, LoginData, LoginResult, RegisterData, RegisterResult,
        UserVerification, WebAuthnErrorKind, origin_for_rp_id,
        serialize_client_data_create_with_origin, serialize_client_data_get_with_origin,
    };

    const WEBAUTHN_RP_ENTITY_INFORMATION_VERSION_1: u32 = 1;
    const WEBAUTHN_USER_ENTITY_INFORMATION_VERSION_1: u32 = 1;
    const WEBAUTHN_CLIENT_DATA_CURRENT_VERSION: u32 = 1;
    const WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION: u32 = 1;
    const WEBAUTHN_CREDENTIAL_CURRENT_VERSION: u32 = 1;

    const WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_3: u32 = 3;
    const WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_4: u32 = 4;
    const WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_4: u32 = 4;

    const WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY: u32 = 0;
    const WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED: u32 = 1;
    const WEBAUTHN_USER_VERIFICATION_REQUIREMENT_PREFERRED: u32 = 2;
    const WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED: u32 = 3;
    const WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE: u32 = 1;

    // Standard HRESULT error codes returned by webauthn.dll (`webauthn.h:1381-1395`).
    const NTE_USER_CANCELLED: i32 = 0x8009_0036_u32 as i32;
    const HRESULT_ERROR_CANCELLED: i32 = 0x8007_04C7_u32 as i32;
    const NTE_NOT_FOUND: i32 = 0x8009_0011_u32 as i32;
    const NTE_DEVICE_NOT_FOUND: i32 = 0x8009_0035_u32 as i32;
    const NTE_NOT_SUPPORTED: i32 = 0x8009_0029_u32 as i32;
    const HRESULT_ERROR_NOT_SUPPORTED: i32 = 0x8007_0032_u32 as i32;

    #[repr(C)]
    struct WebAuthnRpEntityInformation {
        dw_version: u32,
        pwsz_id: PCWSTR,
        pwsz_name: PCWSTR,
        pwsz_icon: PCWSTR,
    }

    #[repr(C)]
    struct WebAuthnUserEntityInformation {
        dw_version: u32,
        cb_id: u32,
        pb_id: *mut u8,
        pwsz_name: PCWSTR,
        pwsz_icon: PCWSTR,
        pwsz_display_name: PCWSTR,
    }

    #[repr(C)]
    struct WebAuthnClientData {
        dw_version: u32,
        cb_client_data_json: u32,
        pb_client_data_json: *mut u8,
        pwsz_hash_alg_id: PCWSTR,
    }

    #[repr(C)]
    struct WebAuthnCoseCredentialParameter {
        dw_version: u32,
        pwsz_credential_type: PCWSTR,
        l_alg: i32,
    }

    #[repr(C)]
    struct WebAuthnCoseCredentialParameters {
        c_credential_parameters: u32,
        p_credential_parameters: *mut WebAuthnCoseCredentialParameter,
    }

    #[repr(C)]
    struct WebAuthnCredential {
        dw_version: u32,
        cb_id: u32,
        pb_id: *mut u8,
        pwsz_credential_type: PCWSTR,
    }

    #[repr(C)]
    struct WebAuthnCredentials {
        c_credentials: u32,
        p_credentials: *mut WebAuthnCredential,
    }

    #[repr(C)]
    struct WebAuthnExtensions {
        c_extensions: u32,
        p_extensions: *mut c_void,
    }

    /// Matches `WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS` up to version 4 (`webauthn.h:749-808`).
    #[repr(C)]
    struct WebAuthnMakeCredentialOptions {
        dw_version: u32,
        dw_timeout_milliseconds: u32,
        credential_list: WebAuthnCredentials,
        extensions: WebAuthnExtensions,
        dw_authenticator_attachment: u32,
        b_require_resident_key: BOOL,
        dw_user_verification_requirement: u32,
        dw_attestation_conveyance_preference: u32,
        dw_flags: u32,
        p_cancellation_id: *mut GUID,
        p_exclude_credential_list: *mut c_void,
        // Version 4 fields (API version >= 3):
        dw_enterprise_attestation: u32,
        dw_large_blob_support: u32,
        b_prefer_resident_key: BOOL,
    }

    /// Matches `WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS` up to version 4 (`webauthn.h:896-943`).
    #[repr(C)]
    struct WebAuthnGetAssertionOptions {
        dw_version: u32,
        dw_timeout_milliseconds: u32,
        credential_list: WebAuthnCredentials,
        extensions: WebAuthnExtensions,
        dw_authenticator_attachment: u32,
        dw_user_verification_requirement: u32,
        dw_flags: u32,
        pwsz_u2f_app_id: PCWSTR,
        pb_u2f_app_id: *mut BOOL,
        p_cancellation_id: *mut GUID,
        p_allow_credential_list: *mut c_void,
    }

    /// Matches the Version 1 prefix of `WEBAUTHN_CREDENTIAL_ATTESTATION` (`webauthn.h:1090-1127`).
    #[repr(C)]
    struct WebAuthnCredentialAttestation {
        dw_version: u32,
        pwsz_format_type: PCWSTR,
        cb_authenticator_data: u32,
        pb_authenticator_data: *mut u8,
        cb_attestation: u32,
        pb_attestation: *mut u8,
        dw_attestation_decode_type: u32,
        pv_attestation_decode: *mut c_void,
        cb_attestation_object: u32,
        pb_attestation_object: *mut u8,
        cb_credential_id: u32,
        pb_credential_id: *mut u8,
    }

    /// Matches the Version 1 prefix of `WEBAUTHN_ASSERTION` (`webauthn.h:1218-1239`).
    #[repr(C)]
    struct WebAuthnAssertion {
        dw_version: u32,
        cb_authenticator_data: u32,
        pb_authenticator_data: *mut u8,
        cb_signature: u32,
        pb_signature: *mut u8,
        credential: WebAuthnCredential,
        cb_user_id: u32,
        pb_user_id: *mut u8,
    }

    type FnGetApiVersionNumber = unsafe extern "system" fn() -> u32;
    type FnIsUvPlatformAvail = unsafe extern "system" fn(*mut BOOL) -> HRESULT;
    type FnMakeCredential = unsafe extern "system" fn(
        HWND,
        *const WebAuthnRpEntityInformation,
        *const WebAuthnUserEntityInformation,
        *const WebAuthnCoseCredentialParameters,
        *const WebAuthnClientData,
        *const WebAuthnMakeCredentialOptions,
        *mut *mut WebAuthnCredentialAttestation,
    ) -> HRESULT;
    type FnFreeCredentialAttestation =
        unsafe extern "system" fn(*mut WebAuthnCredentialAttestation);
    type FnGetAssertion = unsafe extern "system" fn(
        HWND,
        PCWSTR,
        *const WebAuthnClientData,
        *const WebAuthnGetAssertionOptions,
        *mut *mut WebAuthnAssertion,
    ) -> HRESULT;
    type FnFreeAssertion = unsafe extern "system" fn(*mut WebAuthnAssertion);

    struct WebAuthnDll {
        _module: HMODULE,
        get_api_version_number: Option<FnGetApiVersionNumber>,
        is_uv_platform_available: FnIsUvPlatformAvail,
        make_credential: FnMakeCredential,
        free_credential_attestation: FnFreeCredentialAttestation,
        get_assertion: FnGetAssertion,
        free_assertion: FnFreeAssertion,
    }

    // SAFETY: Function pointers resolved from a process-global system DLL (`webauthn.dll`) that
    // remains loaded for the lifetime of the process are immutable and safe to invoke from any
    // thread.
    unsafe impl Send for WebAuthnDll {}
    // SAFETY: Same invariant as `Send`: `webauthn.dll` exports are thread-safe Win32 entry points.
    unsafe impl Sync for WebAuthnDll {}

    static WEBAUTHN_DLL: OnceLock<Option<WebAuthnDll>> = OnceLock::new();

    fn load_dll() -> Option<&'static WebAuthnDll> {
        WEBAUTHN_DLL
            .get_or_init(|| {
                // SAFETY: `LoadLibraryW` is called with a static null-terminated UTF-16 literal
                // `"webauthn.dll"`. On older Windows (< Win10 1903) or environments without the
                // DLL, `LoadLibraryW` returns `Err` and we gracefully return `None`.
                let module = unsafe { LoadLibraryW(w!("webauthn.dll")) }.ok()?;

                // SAFETY: Each `GetProcAddress` call passes a valid loaded `HMODULE` and a static
                // null-terminated ASCII export name. The returned function pointers are transmuted
                // to the exact `extern "system"` signatures declared in Windows SDK `webauthn.h`.
                unsafe {
                    let get_api_version_number = GetProcAddress(
                        module,
                        PCSTR(c"WebAuthNGetApiVersionNumber".as_ptr().cast()),
                    )
                    .map(|f| std::mem::transmute::<_, FnGetApiVersionNumber>(f));

                    let is_uv_platform_available = GetProcAddress(
                        module,
                        PCSTR(
                            c"WebAuthNIsUserVerifyingPlatformAuthenticatorAvailable"
                                .as_ptr()
                                .cast(),
                        ),
                    )
                    .map(|f| std::mem::transmute::<_, FnIsUvPlatformAvail>(f))?;

                    let make_credential = GetProcAddress(
                        module,
                        PCSTR(c"WebAuthNAuthenticatorMakeCredential".as_ptr().cast()),
                    )
                    .map(|f| std::mem::transmute::<_, FnMakeCredential>(f))?;

                    let free_credential_attestation = GetProcAddress(
                        module,
                        PCSTR(c"WebAuthNFreeCredentialAttestation".as_ptr().cast()),
                    )
                    .map(|f| std::mem::transmute::<_, FnFreeCredentialAttestation>(f))?;

                    let get_assertion = GetProcAddress(
                        module,
                        PCSTR(c"WebAuthNAuthenticatorGetAssertion".as_ptr().cast()),
                    )
                    .map(|f| std::mem::transmute::<_, FnGetAssertion>(f))?;

                    let free_assertion =
                        GetProcAddress(module, PCSTR(c"WebAuthNFreeAssertion".as_ptr().cast()))
                            .map(|f| std::mem::transmute::<_, FnFreeAssertion>(f))?;

                    Some(WebAuthnDll {
                        _module: module,
                        get_api_version_number,
                        is_uv_platform_available,
                        make_credential,
                        free_credential_attestation,
                        get_assertion,
                        free_assertion,
                    })
                }
            })
            .as_ref()
    }

    fn to_wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn map_hresult(hr: HRESULT, context: &str) -> WebAuthnErrorKind {
        match hr.0 {
            NTE_USER_CANCELLED | HRESULT_ERROR_CANCELLED => WebAuthnErrorKind::Cancelled,
            NTE_NOT_FOUND => WebAuthnErrorKind::NoCredentials,
            NTE_DEVICE_NOT_FOUND => WebAuthnErrorKind::NoDevice,
            NTE_NOT_SUPPORTED | HRESULT_ERROR_NOT_SUPPORTED => WebAuthnErrorKind::NotSupported,
            other => WebAuthnErrorKind::Other(format!(
                "{context} failed: HRESULT 0x{:08x}",
                other as u32
            )),
        }
    }

    fn focus_parent(hwnd: HWND) {
        if !hwnd.0.is_null() {
            // SAFETY: `SetForegroundWindow` and `SetFocus` accept any valid or null window handle
            // (already guarded against null above) and fail harmlessly if focus cannot be changed.
            unsafe {
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
            }
        }
    }

    pub fn capabilities() -> AuthenticatorCapabilities {
        let Some(dll) = load_dll() else {
            return AuthenticatorCapabilities::default();
        };
        let api_version = match dll.get_api_version_number {
            Some(f) => {
                // SAFETY: `WebAuthNGetApiVersionNumber` takes no arguments and returns a `DWORD`.
                unsafe { f() }
            }
            None => 1,
        };
        let mut available = FALSE;
        // SAFETY: `available` is a valid aligned stack `BOOL` passed as an out-pointer to
        // `WebAuthNIsUserVerifyingPlatformAuthenticatorAvailable`.
        let hr = unsafe { (dll.is_uv_platform_available)(&mut available) };
        let platform_authenticator_available = hr.is_ok() && available.as_bool();
        AuthenticatorCapabilities {
            dll_available: true,
            api_version,
            platform_authenticator_available,
            // WEBAUTHN_CTAP_TRANSPORT_HYBRID was introduced in WEBAUTHN_API_VERSION_6.
            hybrid_transport_supported: api_version >= 6,
        }
    }

    pub fn make_credential(
        hwnd_raw: usize,
        data: &RegisterData,
    ) -> Result<RegisterResult, WebAuthnErrorKind> {
        let Some(dll) = load_dll() else {
            return Err(WebAuthnErrorKind::NotSupported);
        };
        let caps = capabilities();
        let origin = origin_for_rp_id(&data.rp.id)?;
        let mut client_data_json =
            serialize_client_data_create_with_origin(&data.challenge, &origin).into_bytes();

        let rp_id_w = to_wide_null(&data.rp.id);
        let rp_name_w = to_wide_null(&data.rp.name);
        let user_name_w = to_wide_null(&data.user.name);
        let user_display_w = to_wide_null(&data.user.display_name);
        let mut user_id = data.user.id.clone();

        let rp_info = WebAuthnRpEntityInformation {
            dw_version: WEBAUTHN_RP_ENTITY_INFORMATION_VERSION_1,
            pwsz_id: PCWSTR(rp_id_w.as_ptr()),
            pwsz_name: PCWSTR(rp_name_w.as_ptr()),
            pwsz_icon: PCWSTR::null(),
        };

        let user_info = WebAuthnUserEntityInformation {
            dw_version: WEBAUTHN_USER_ENTITY_INFORMATION_VERSION_1,
            cb_id: u32::try_from(user_id.len()).unwrap_or(u32::MAX),
            pb_id: user_id.as_mut_ptr(),
            pwsz_name: PCWSTR(user_name_w.as_ptr()),
            pwsz_icon: PCWSTR::null(),
            pwsz_display_name: PCWSTR(user_display_w.as_ptr()),
        };

        let mut cred_params: Vec<WebAuthnCoseCredentialParameter> = data
            .pub_key_cred_params
            .iter()
            .map(|p| WebAuthnCoseCredentialParameter {
                dw_version: WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
                pwsz_credential_type: if p.cred_type == "public-key" {
                    w!("public-key")
                } else {
                    w!("")
                },
                l_alg: p.alg,
            })
            .collect();

        let cred_params_list = WebAuthnCoseCredentialParameters {
            c_credential_parameters: u32::try_from(cred_params.len()).unwrap_or(0),
            p_credential_parameters: cred_params.as_mut_ptr(),
        };

        let client_data = WebAuthnClientData {
            dw_version: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cb_client_data_json: u32::try_from(client_data_json.len()).unwrap_or(u32::MAX),
            pb_client_data_json: client_data_json.as_mut_ptr(),
            pwsz_hash_alg_id: w!("SHA-256"),
        };

        // SAFETY: `CoCreateGuid` generates a fresh random GUID with no preconditions.
        let mut cancellation_id = unsafe { CoCreateGuid() }.unwrap_or_default();

        let options_version = if caps.api_version >= 3 {
            WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_4
        } else {
            WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_3
        };
        let options = WebAuthnMakeCredentialOptions {
            dw_version: options_version,
            dw_timeout_milliseconds: data.timeout_ms,
            credential_list: WebAuthnCredentials {
                c_credentials: 0,
                p_credentials: std::ptr::null_mut(),
            },
            extensions: WebAuthnExtensions {
                c_extensions: 0,
                p_extensions: std::ptr::null_mut(),
            },
            dw_authenticator_attachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
            b_require_resident_key: FALSE,
            dw_user_verification_requirement: WEBAUTHN_USER_VERIFICATION_REQUIREMENT_PREFERRED,
            dw_attestation_conveyance_preference: WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE,
            dw_flags: 0,
            p_cancellation_id: &mut cancellation_id,
            p_exclude_credential_list: std::ptr::null_mut(),
            dw_enterprise_attestation: 0,
            dw_large_blob_support: 0,
            b_prefer_resident_key: TRUE,
        };

        let hwnd = HWND(hwnd_raw as *mut c_void);
        focus_parent(hwnd);

        let mut attestation_ptr: *mut WebAuthnCredentialAttestation = std::ptr::null_mut();
        // SAFETY: All input structs, wide-string buffers (`rp_id_w`, `rp_name_w`, `user_name_w`,
        // `user_display_w`), byte buffers (`user_id`, `client_data_json`), `cred_params`, and
        // `cancellation_id` remain alive and unmodified for the entire synchronous call to
        // `WebAuthNAuthenticatorMakeCredential`. The returned `attestation_ptr` is freed below via
        // `WebAuthNFreeCredentialAttestation` if non-null.
        let hr = unsafe {
            (dll.make_credential)(
                hwnd,
                &rp_info,
                &user_info,
                &cred_params_list,
                &client_data,
                &options,
                &mut attestation_ptr,
            )
        };

        if hr.is_err() || attestation_ptr.is_null() {
            if !attestation_ptr.is_null() {
                // SAFETY: `attestation_ptr` was allocated by `webauthn.dll` above.
                unsafe { (dll.free_credential_attestation)(attestation_ptr) };
            }
            return Err(map_hresult(hr, "WebAuthNAuthenticatorMakeCredential"));
        }

        // SAFETY: `attestation_ptr` is non-null and `hr.is_ok()`. We copy out only Version 1 fields
        // (`pb_credential_id`/`cb_credential_id` and `pb_attestation_object`/`cb_attestation_object`)
        // after checking each pointer is non-null, and immediately free the struct afterwards.
        let result = unsafe {
            let att = &*attestation_ptr;
            let credential_id = if !att.pb_credential_id.is_null() && att.cb_credential_id > 0 {
                std::slice::from_raw_parts(att.pb_credential_id, att.cb_credential_id as usize)
                    .to_vec()
            } else {
                Vec::new()
            };
            let attestation_object =
                if !att.pb_attestation_object.is_null() && att.cb_attestation_object > 0 {
                    std::slice::from_raw_parts(
                        att.pb_attestation_object,
                        att.cb_attestation_object as usize,
                    )
                    .to_vec()
                } else {
                    Vec::new()
                };
            (dll.free_credential_attestation)(attestation_ptr);
            RegisterResult {
                credential_id,
                attestation_object,
                client_data_json,
            }
        };

        Ok(result)
    }

    pub fn get_assertion(
        hwnd_raw: usize,
        data: &LoginData,
    ) -> Result<LoginResult, WebAuthnErrorKind> {
        let Some(dll) = load_dll() else {
            return Err(WebAuthnErrorKind::NotSupported);
        };
        let origin = origin_for_rp_id(&data.rp_id)?;
        let rp_id_w = to_wide_null(&data.rp_id);
        let mut client_data_json =
            serialize_client_data_get_with_origin(&data.challenge, &origin).into_bytes();

        let client_data = WebAuthnClientData {
            dw_version: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cb_client_data_json: u32::try_from(client_data_json.len()).unwrap_or(u32::MAX),
            pb_client_data_json: client_data_json.as_mut_ptr(),
            pwsz_hash_alg_id: w!("SHA-256"),
        };

        let mut cred_ids: Vec<Vec<u8>> = data
            .allow_credentials
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let mut allow_credentials: Vec<WebAuthnCredential> = cred_ids
            .iter_mut()
            .map(|id| WebAuthnCredential {
                dw_version: WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
                cb_id: u32::try_from(id.len()).unwrap_or(u32::MAX),
                pb_id: id.as_mut_ptr(),
                pwsz_credential_type: w!("public-key"),
            })
            .collect();

        // SAFETY: `CoCreateGuid` generates a fresh random GUID with no preconditions.
        let mut cancellation_id = unsafe { CoCreateGuid() }.unwrap_or_default();

        let uv_req = match data.user_verification {
            UserVerification::Required => WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
            UserVerification::Preferred => WEBAUTHN_USER_VERIFICATION_REQUIREMENT_PREFERRED,
            UserVerification::Discouraged => WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED,
        };

        let options = WebAuthnGetAssertionOptions {
            dw_version: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_4,
            dw_timeout_milliseconds: data.timeout_ms,
            credential_list: WebAuthnCredentials {
                c_credentials: u32::try_from(allow_credentials.len()).unwrap_or(0),
                p_credentials: if allow_credentials.is_empty() {
                    std::ptr::null_mut()
                } else {
                    allow_credentials.as_mut_ptr()
                },
            },
            extensions: WebAuthnExtensions {
                c_extensions: 0,
                p_extensions: std::ptr::null_mut(),
            },
            dw_authenticator_attachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
            dw_user_verification_requirement: uv_req,
            dw_flags: 0,
            pwsz_u2f_app_id: PCWSTR::null(),
            pb_u2f_app_id: std::ptr::null_mut(),
            p_cancellation_id: &mut cancellation_id,
            p_allow_credential_list: std::ptr::null_mut(),
        };

        let hwnd = HWND(hwnd_raw as *mut c_void);
        focus_parent(hwnd);

        let mut assertion_ptr: *mut WebAuthnAssertion = std::ptr::null_mut();
        // SAFETY: `rp_id_w`, `client_data_json`, `cred_ids`, `allow_credentials`, and
        // `cancellation_id` remain pinned and alive across the synchronous FFI call. The returned
        // `assertion_ptr` is freed via `WebAuthNFreeAssertion` before returning.
        let hr = unsafe {
            (dll.get_assertion)(
                hwnd,
                PCWSTR(rp_id_w.as_ptr()),
                &client_data,
                &options,
                &mut assertion_ptr,
            )
        };

        if hr.is_err() || assertion_ptr.is_null() {
            if !assertion_ptr.is_null() {
                // SAFETY: `assertion_ptr` was allocated by `webauthn.dll` above.
                unsafe { (dll.free_assertion)(assertion_ptr) };
            }
            return Err(map_hresult(hr, "WebAuthNAuthenticatorGetAssertion"));
        }

        // SAFETY: `assertion_ptr` is non-null and `hr.is_ok()`. All fields accessed are part of
        // `WEBAUTHN_ASSERTION_VERSION_1`. Each buffer pointer is null-checked before reading and
        // `assertion_ptr` is freed via `WebAuthNFreeAssertion`.
        let result = unsafe {
            let a = &*assertion_ptr;
            let credential_id = if !a.credential.pb_id.is_null() && a.credential.cb_id > 0 {
                std::slice::from_raw_parts(a.credential.pb_id, a.credential.cb_id as usize).to_vec()
            } else {
                Vec::new()
            };
            let authenticator_data =
                if !a.pb_authenticator_data.is_null() && a.cb_authenticator_data > 0 {
                    std::slice::from_raw_parts(
                        a.pb_authenticator_data,
                        a.cb_authenticator_data as usize,
                    )
                    .to_vec()
                } else {
                    Vec::new()
                };
            let signature = if !a.pb_signature.is_null() && a.cb_signature > 0 {
                std::slice::from_raw_parts(a.pb_signature, a.cb_signature as usize).to_vec()
            } else {
                Vec::new()
            };
            let user_handle = if !a.pb_user_id.is_null() && a.cb_user_id > 0 {
                std::slice::from_raw_parts(a.pb_user_id, a.cb_user_id as usize).to_vec()
            } else {
                Vec::new()
            };
            (dll.free_assertion)(assertion_ptr);
            LoginResult {
                client_data_json,
                credential_id,
                authenticator_data,
                signature,
                user_handle,
            }
        };

        Ok(result)
    }
}

#[cfg(not(windows))]
mod win_ffi {
    use super::{
        AuthenticatorCapabilities, LoginData, LoginResult, RegisterData, RegisterResult,
        WebAuthnErrorKind,
    };

    pub fn capabilities() -> AuthenticatorCapabilities {
        AuthenticatorCapabilities::default()
    }

    pub fn make_credential(
        _hwnd_raw: usize,
        _data: &RegisterData,
    ) -> Result<RegisterResult, WebAuthnErrorKind> {
        Err(WebAuthnErrorKind::NotSupported)
    }

    pub fn get_assertion(
        _hwnd_raw: usize,
        _data: &LoginData,
    ) -> Result<LoginResult, WebAuthnErrorKind> {
        Err(WebAuthnErrorKind::NotSupported)
    }
}

// ============================================================================
// Provider Trait, Real Windows Provider, and Test Fake Provider
// ============================================================================

/// Abstraction over platform WebAuthn / FIDO2 operations so tests never invoke real OS dialogs
/// or hardware authenticators.
pub trait WebAuthnProvider: Send + Sync {
    /// Probes OS WebAuthn capabilities (API version, platform authenticator, hybrid transport).
    fn capabilities(&self) -> AuthenticatorCapabilities;

    /// Returns `true` if passkey registration / login is available via this provider.
    fn is_supported(&self) -> bool {
        let caps = self.capabilities();
        caps.is_native_supported() || caps.hybrid_transport_supported
    }

    /// Runs a passkey creation ceremony (`WebAuthNAuthenticatorMakeCredential`).
    fn make_credential(
        &self,
        hwnd: usize,
        data: &RegisterData,
    ) -> Result<RegisterResult, WebAuthnErrorKind>;

    /// Runs a passkey authentication ceremony (`WebAuthNAuthenticatorGetAssertion`).
    fn get_assertion(
        &self,
        hwnd: usize,
        data: &LoginData,
    ) -> Result<LoginResult, WebAuthnErrorKind>;

    /// Starts a cross-device caBLEv2 / hybrid QR registration ceremony, returning a `"FIDO:/..."`
    /// QR session handle (`RegisterViaCable`).
    fn start_cable_register(
        &self,
        data: &RegisterData,
    ) -> Result<CableCeremonyHandle, WebAuthnErrorKind> {
        let origin = origin_for_rp_id(&data.rp.id)?;
        let client_data_json = serialize_client_data_create_with_origin(&data.challenge, &origin);
        let client_data_hash = client_data_hash(client_data_json.as_bytes());
        let session = CableQrSession::generate(true, current_unix_seconds())?;
        Ok(CableCeremonyHandle {
            session,
            client_data_json,
            client_data_hash,
        })
    }

    /// Starts a cross-device caBLEv2 / hybrid QR authentication ceremony (`LoginViaCable`).
    fn start_cable_login(
        &self,
        data: &LoginData,
    ) -> Result<CableCeremonyHandle, WebAuthnErrorKind> {
        let origin = origin_for_rp_id(&data.rp_id)?;
        let client_data_json = serialize_client_data_get_with_origin(&data.challenge, &origin);
        let client_data_hash = client_data_hash(client_data_json.as_bytes());
        let session = CableQrSession::generate(false, current_unix_seconds())?;
        Ok(CableCeremonyHandle {
            session,
            client_data_json,
            client_data_hash,
        })
    }
}

/// Real Windows `webauthn.dll` FIDO2 provider (`Platform::WebAuthn` in `webauthn_win.cpp`).
///
/// Gracefully reports `AuthenticatorCapabilities::default()` and [`WebAuthnErrorKind::NotSupported`]
/// when `webauthn.dll` is absent (older Windows versions or non-Windows platforms).
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsWebAuthnProvider;

impl WindowsWebAuthnProvider {
    /// Creates a new [`WindowsWebAuthnProvider`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl WebAuthnProvider for WindowsWebAuthnProvider {
    fn capabilities(&self) -> AuthenticatorCapabilities {
        win_ffi::capabilities()
    }

    fn make_credential(
        &self,
        hwnd: usize,
        data: &RegisterData,
    ) -> Result<RegisterResult, WebAuthnErrorKind> {
        win_ffi::make_credential(hwnd, data)
    }

    fn get_assertion(
        &self,
        hwnd: usize,
        data: &LoginData,
    ) -> Result<LoginResult, WebAuthnErrorKind> {
        win_ffi::get_assertion(hwnd, data)
    }
}

/// Deterministic in-memory [`WebAuthnProvider`] fake for unit and integration tests.
///
/// Never loads `webauthn.dll`, never opens OS UI, and never touches hardware authenticators.
pub struct FakeWebAuthnProvider {
    inner: Mutex<FakeWebAuthnInner>,
}

struct FakeWebAuthnInner {
    capabilities: AuthenticatorCapabilities,
    make_credential_queue: VecDeque<Result<RegisterResult, WebAuthnErrorKind>>,
    get_assertion_queue: VecDeque<Result<LoginResult, WebAuthnErrorKind>>,
    recorded_registrations: Vec<RegisterData>,
    recorded_registration_hwnds: Vec<usize>,
    recorded_logins: Vec<LoginData>,
    recorded_login_hwnds: Vec<usize>,
    default_user_handle: Vec<u8>,
    default_credential_id: Vec<u8>,
}

impl Default for FakeWebAuthnProvider {
    fn default() -> Self {
        Self::new_supported()
    }
}

impl FakeWebAuthnProvider {
    /// Creates a fake provider simulating Windows 11 `webauthn.dll` v6 with hybrid QR support.
    #[must_use]
    pub fn new_supported() -> Self {
        Self::with_capabilities(AuthenticatorCapabilities {
            dll_available: true,
            api_version: 6,
            platform_authenticator_available: true,
            hybrid_transport_supported: true,
        })
    }

    /// Creates a fake provider simulating an older OS where `webauthn.dll` is missing.
    #[must_use]
    pub fn new_unsupported() -> Self {
        Self::with_capabilities(AuthenticatorCapabilities::default())
    }

    /// Creates a fake provider with custom [`AuthenticatorCapabilities`].
    #[must_use]
    pub fn with_capabilities(capabilities: AuthenticatorCapabilities) -> Self {
        Self {
            inner: Mutex::new(FakeWebAuthnInner {
                capabilities,
                make_credential_queue: VecDeque::new(),
                get_assertion_queue: VecDeque::new(),
                recorded_registrations: Vec::new(),
                recorded_registration_hwnds: Vec::new(),
                recorded_logins: Vec::new(),
                recorded_login_hwnds: Vec::new(),
                default_user_handle: b"2:1000".to_vec(),
                default_credential_id: b"fake-passkey-cred-id-0001".to_vec(),
            }),
        }
    }

    /// Overrides the default user handle (`"<dc_id>:<user_id>"`) emitted when the assertion queue is empty.
    pub fn set_default_user_handle(&self, handle: impl Into<Vec<u8>>) {
        if let Ok(mut g) = self.inner.lock() {
            g.default_user_handle = handle.into();
        }
    }

    /// Overrides the default credential ID emitted when queues are empty.
    pub fn set_default_credential_id(&self, id: impl Into<Vec<u8>>) {
        if let Ok(mut g) = self.inner.lock() {
            g.default_credential_id = id.into();
        }
    }

    /// Queues an explicit `make_credential` response or error.
    pub fn push_make_credential_result(&self, res: Result<RegisterResult, WebAuthnErrorKind>) {
        if let Ok(mut g) = self.inner.lock() {
            g.make_credential_queue.push_back(res);
        }
    }

    /// Queues an explicit `get_assertion` response or error.
    pub fn push_get_assertion_result(&self, res: Result<LoginResult, WebAuthnErrorKind>) {
        if let Ok(mut g) = self.inner.lock() {
            g.get_assertion_queue.push_back(res);
        }
    }

    /// Returns all [`RegisterData`] requests received by this fake.
    #[must_use]
    pub fn recorded_registrations(&self) -> Vec<RegisterData> {
        self.inner
            .lock()
            .map(|g| g.recorded_registrations.clone())
            .unwrap_or_default()
    }

    /// Returns all parent window `HWND` values passed to `make_credential`.
    #[must_use]
    pub fn recorded_registration_hwnds(&self) -> Vec<usize> {
        self.inner
            .lock()
            .map(|g| g.recorded_registration_hwnds.clone())
            .unwrap_or_default()
    }

    /// Returns all [`LoginData`] requests received by this fake.
    #[must_use]
    pub fn recorded_logins(&self) -> Vec<LoginData> {
        self.inner
            .lock()
            .map(|g| g.recorded_logins.clone())
            .unwrap_or_default()
    }

    /// Returns all parent window `HWND` values passed to `get_assertion`.
    #[must_use]
    pub fn recorded_login_hwnds(&self) -> Vec<usize> {
        self.inner
            .lock()
            .map(|g| g.recorded_login_hwnds.clone())
            .unwrap_or_default()
    }
}

/// Builds synthetic attested `authenticatorData` containing `credential_id` for tests/fakes.
#[must_use]
pub fn build_synthetic_auth_data(rp_id: &str, credential_id: &[u8], flags: u8) -> Vec<u8> {
    let rp_hash = sha256_digest(rp_id.as_bytes());
    let mut auth_data = Vec::with_capacity(37 + 18 + credential_id.len() + 8);
    auth_data.extend_from_slice(&rp_hash);
    auth_data.push(flags);
    auth_data.extend_from_slice(&1u32.to_be_bytes());
    if (flags & 0x40) != 0 {
        auth_data.extend_from_slice(&[0x11u8; 16]); // synthetic AAGUID
        let len_u16 = u16::try_from(credential_id.len()).unwrap_or(u16::MAX);
        auth_data.extend_from_slice(&len_u16.to_be_bytes());
        auth_data.extend_from_slice(credential_id);
        // Minimal COSE EC2 key map stub `{1: 2, 3: -7}`
        let cose_key = cbor_encode(&CborValue::Map(vec![
            (CborValue::Int(1), CborValue::Int(2)),
            (CborValue::Int(3), CborValue::Int(i64::from(COSE_ES256))),
        ]));
        auth_data.extend_from_slice(&cose_key);
    }
    auth_data
}

impl WebAuthnProvider for FakeWebAuthnProvider {
    fn capabilities(&self) -> AuthenticatorCapabilities {
        self.inner
            .lock()
            .map(|g| g.capabilities)
            .unwrap_or_default()
    }

    fn make_credential(
        &self,
        hwnd: usize,
        data: &RegisterData,
    ) -> Result<RegisterResult, WebAuthnErrorKind> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| WebAuthnErrorKind::Other("fake provider lock poisoned".to_owned()))?;
        g.recorded_registrations.push(data.clone());
        g.recorded_registration_hwnds.push(hwnd);
        if let Some(queued) = g.make_credential_queue.pop_front() {
            return queued;
        }
        if !g.capabilities.is_native_supported() {
            return Err(WebAuthnErrorKind::NotSupported);
        }
        let origin = origin_for_rp_id(&data.rp.id)?;
        let client_data_json =
            serialize_client_data_create_with_origin(&data.challenge, &origin).into_bytes();
        let cred_id = g.default_credential_id.clone();
        let auth_data = build_synthetic_auth_data(&data.rp.id, &cred_id, 0x45);
        let attestation_object = build_none_attestation_object(&auth_data);
        Ok(RegisterResult {
            credential_id: cred_id,
            attestation_object,
            client_data_json,
        })
    }

    fn get_assertion(
        &self,
        hwnd: usize,
        data: &LoginData,
    ) -> Result<LoginResult, WebAuthnErrorKind> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| WebAuthnErrorKind::Other("fake provider lock poisoned".to_owned()))?;
        g.recorded_logins.push(data.clone());
        g.recorded_login_hwnds.push(hwnd);
        if let Some(queued) = g.get_assertion_queue.pop_front() {
            return queued;
        }
        if !g.capabilities.is_native_supported() {
            return Err(WebAuthnErrorKind::NotSupported);
        }
        let origin = origin_for_rp_id(&data.rp_id)?;
        let client_data_json =
            serialize_client_data_get_with_origin(&data.challenge, &origin).into_bytes();
        let cred_id = data
            .allow_credentials
            .first()
            .map_or_else(|| g.default_credential_id.clone(), |c| c.id.clone());
        let authenticator_data = build_synthetic_auth_data(&data.rp_id, &[], 0x05);
        let mut sig_input = authenticator_data.clone();
        sig_input.extend_from_slice(&client_data_hash(&client_data_json));
        let signature = sha256_digest(&sig_input).to_vec();
        Ok(LoginResult {
            client_data_json,
            credential_id: cred_id,
            authenticator_data,
            signature,
            user_handle: g.default_user_handle.clone(),
        })
    }
}

// ============================================================================
// Unit Tests (pure protocol, codecs, caBLE QR, and FakeWebAuthnProvider)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_data_json_create_and_get_roundtrip() {
        let challenge = b"\x01\x02\x03\xfe\xffchallenge-bytes";
        let create_json = serialize_client_data_create(challenge);
        let parsed_create = parse_client_data(create_json.as_bytes()).expect("parse create");
        assert_eq!(parsed_create.kind, "webauthn.create");
        assert_eq!(parsed_create.challenge, challenge);
        assert_eq!(parsed_create.origin, DEFAULT_ORIGIN);
        assert!(!parsed_create.cross_origin);

        let get_json = serialize_client_data_get(challenge);
        let parsed_get = parse_client_data(get_json.as_bytes()).expect("parse get");
        assert_eq!(parsed_get.kind, "webauthn.get");
        assert_eq!(parsed_get.challenge, challenge);
        assert_eq!(parsed_get.origin, DEFAULT_ORIGIN);
        assert!(!parsed_get.cross_origin);

        // Verify challenge is base64url without padding.
        let v: JsonValue = serde_json::from_str(&create_json).expect("valid json");
        let ch_str = v["challenge"].as_str().expect("string");
        assert!(!ch_str.contains('='));
        assert!(!ch_str.contains('+'));
        assert!(!ch_str.contains('/'));
    }

    #[test]
    fn origin_and_rp_id_validation_and_matching() {
        assert_eq!(validate_rp_id("Telegram.ORG").unwrap(), "telegram.org");
        assert_eq!(
            origin_for_rp_id("telegram.org").unwrap(),
            "https://telegram.org"
        );
        assert!(validate_rp_id("").is_err());
        assert!(validate_rp_id("https://telegram.org").is_err());
        assert!(validate_rp_id(".telegram.org").is_err());
        assert!(validate_rp_id("telegram.org/").is_err());
        assert!(validate_rp_id("-bad.telegram.org").is_err());

        assert!(origin_matches_rp_id("https://telegram.org", "telegram.org"));
        assert!(origin_matches_rp_id(
            "https://web.telegram.org",
            "telegram.org"
        ));
        assert!(origin_matches_rp_id(
            "https://a.b.telegram.org:8443",
            "telegram.org"
        ));
        assert!(!origin_matches_rp_id(
            "https://eviltelegram.org",
            "telegram.org"
        ));
        assert!(!origin_matches_rp_id("http://telegram.org", "telegram.org"));
        assert!(!origin_matches_rp_id(
            "https://telegram.org/path",
            "telegram.org"
        ));
        assert!(origin_matches_rp_id("http://localhost:8080", "localhost"));
    }

    #[test]
    fn deserialize_register_and_login_options() {
        let reg_json = json!({
            "publicKey": {
                "rp": { "id": "telegram.org", "name": "Telegram" },
                "user": {
                    "id": encode_base64url_padded(b"2:123456"),
                    "name": "demo_user",
                    "displayName": "Demo User"
                },
                "challenge": encode_base64url(b"server-reg-challenge"),
                "pubKeyCredParams": [
                    { "type": "public-key", "alg": -7 },
                    { "type": "public-key", "alg": -257 }
                ],
                "timeout": 45000
            }
        })
        .to_string();

        let reg = deserialize_register_data(reg_json.as_bytes()).expect("register data");
        assert_eq!(reg.rp.id, "telegram.org");
        assert_eq!(reg.rp.name, "Telegram");
        assert_eq!(reg.user.id, b"2:123456");
        assert_eq!(reg.user.name, "demo_user");
        assert_eq!(reg.user.display_name, "Demo User");
        assert_eq!(reg.challenge, b"server-reg-challenge");
        assert_eq!(reg.pub_key_cred_params.len(), 2);
        assert_eq!(
            preferred_cose_algorithm(&reg.pub_key_cred_params),
            COSE_ES256
        );
        assert_eq!(reg.timeout_ms, 45_000);

        let login_json = json!({
            "publicKey": {
                "challenge": encode_base64url(b"server-login-challenge"),
                "rpId": "telegram.org",
                "timeout": 30000,
                "userVerification": "required",
                "allowCredentials": [
                    { "type": "public-key", "id": encode_base64url(b"cred-abc") }
                ]
            }
        })
        .to_string();

        let login = deserialize_login_data(login_json.as_bytes()).expect("login data");
        assert_eq!(login.challenge, b"server-login-challenge");
        assert_eq!(login.rp_id, "telegram.org");
        assert_eq!(login.timeout_ms, 30_000);
        assert_eq!(login.user_verification, UserVerification::Required);
        assert_eq!(login.allow_credentials.len(), 1);
        assert_eq!(login.allow_credentials[0].id, b"cred-abc");
    }

    #[test]
    fn parse_user_handle_validation() {
        assert_eq!(
            parse_user_handle(b"2:987654321").unwrap(),
            ParsedUserHandle {
                dc_id: 2,
                user_id: 987_654_321
            }
        );
        assert!(parse_user_handle(b"").is_err());
        assert!(parse_user_handle(b"0:123").is_err());
        assert!(parse_user_handle(b"1000:123").is_err());
        assert!(parse_user_handle(b"2:0").is_err());
        assert!(parse_user_handle(b"2:123:extra").is_err());
        assert!(parse_user_handle(b"not-valid").is_err());
    }

    #[test]
    fn cbor_encode_decode_and_attestation_parsing() {
        let cred_id = b"credential-identifier-0123456789";
        let auth_data = build_synthetic_auth_data("telegram.org", cred_id, 0x45);
        let att_obj = build_none_attestation_object(&auth_data);

        let parsed = parse_attestation_object(&att_obj).expect("parse attestation");
        assert_eq!(parsed.fmt, "none");
        assert_eq!(parsed.auth_data, auth_data);
        assert_eq!(parsed.att_stmt, CborValue::Map(Vec::new()));
        assert_eq!(
            parsed.parsed_auth_data.rp_id_hash,
            sha256_digest(b"telegram.org")
        );
        assert!(parsed.parsed_auth_data.user_present());
        assert!(parsed.parsed_auth_data.user_verified());
        assert!(parsed.parsed_auth_data.has_attested_credential_data());
        assert_eq!(
            parsed.parsed_auth_data.credential_id.as_deref(),
            Some(&cred_id[..])
        );
        assert_eq!(
            credential_id_from_auth_data(&auth_data).as_deref(),
            Some(&cred_id[..])
        );

        // Reject truncated CBOR and trailing bytes.
        assert!(cbor_decode(&att_obj[..att_obj.len() - 1]).is_none());
        let mut trailing = att_obj.clone();
        trailing.push(0x00);
        assert!(cbor_decode(&trailing).is_none());
    }

    #[test]
    fn ctap2_make_credential_and_get_assertion_parsing() {
        let client_hash = client_data_hash(serialize_client_data_create(b"ch1").as_bytes());
        let mc_req = build_ctap_make_credential_request(
            &client_hash,
            "telegram.org",
            "Telegram",
            b"2:42",
            "demo",
            "Demo",
            &[COSE_ES256, COSE_RS256],
        );
        assert_eq!(mc_req[0], 0x01);
        let mc_cbor = cbor_decode(&mc_req[1..]).expect("valid CTAP MC CBOR");
        assert_eq!(
            mc_cbor.find_int(1).and_then(CborValue::as_bytes),
            Some(&client_hash[..])
        );

        let cred_id = b"ctap2-cred-id";
        let auth_data = build_synthetic_auth_data("telegram.org", cred_id, 0x45);
        let mut mc_resp = vec![CTAP_SUCCESS];
        mc_resp.extend_from_slice(&cbor_encode(&CborValue::Map(vec![
            (CborValue::Int(1), CborValue::Text("none".to_owned())),
            (CborValue::Int(2), CborValue::Bytes(auth_data.clone())),
            (CborValue::Int(3), CborValue::Map(Vec::new())),
        ])));
        let parsed_mc = parse_ctap_make_credential_response(&mc_resp).expect("parse MC");
        assert_eq!(parsed_mc.auth_data, auth_data);
        assert_eq!(parsed_mc.credential_id, cred_id);

        // Non-zero status fails parsing.
        let mut err_mc = mc_resp.clone();
        err_mc[0] = CTAP_ERR_OPERATION_DENIED;
        assert!(parse_ctap_make_credential_response(&err_mc).is_none());

        // CTAP2 GetAssertion request + response parsing.
        let ga_req =
            build_ctap_get_assertion_request("telegram.org", &client_hash, &[cred_id.to_vec()]);
        assert_eq!(ga_req[0], 0x02);
        let ga_cbor = cbor_decode(&ga_req[1..]).expect("valid CTAP GA CBOR");
        assert_eq!(
            ga_cbor.find_int(1).and_then(CborValue::as_text),
            Some("telegram.org")
        );

        let ga_auth_data = build_synthetic_auth_data("telegram.org", &[], 0x05);
        let sig = vec![0x30, 0x44, 0x02, 0x20];
        let mut ga_resp = vec![CTAP_SUCCESS];
        ga_resp.extend_from_slice(&cbor_encode(&CborValue::Map(vec![
            (
                CborValue::Int(1),
                CborValue::Map(vec![
                    (
                        CborValue::Text("id".to_owned()),
                        CborValue::Bytes(cred_id.to_vec()),
                    ),
                    (
                        CborValue::Text("type".to_owned()),
                        CborValue::Text("public-key".to_owned()),
                    ),
                ]),
            ),
            (CborValue::Int(2), CborValue::Bytes(ga_auth_data.clone())),
            (CborValue::Int(3), CborValue::Bytes(sig.clone())),
            (
                CborValue::Int(4),
                CborValue::Map(vec![(
                    CborValue::Text("id".to_owned()),
                    CborValue::Bytes(b"2:1000".to_vec()),
                )]),
            ),
        ])));
        let parsed_ga = parse_ctap_get_assertion_response(&ga_resp).expect("parse GA");
        assert_eq!(parsed_ga.credential_id, cred_id);
        assert_eq!(parsed_ga.auth_data, ga_auth_data);
        assert_eq!(parsed_ga.signature, sig);
        assert_eq!(parsed_ga.user_handle, b"2:1000");
    }

    #[test]
    fn cable_qr_uri_advert_domain_and_noise_handshake() {
        // Known-answer digit encoding test: 1 byte (0x01 -> "001"), 7 bytes.
        assert_eq!(cable_bytes_to_digits(&[1]), "001");
        assert_eq!(cable_bytes_to_digits(&[0, 1]), "00256");

        // Tunnel server domain decoding (assigned + hashed).
        assert_eq!(
            cable_decode_tunnel_server_domain(0).as_deref(),
            Some("cable.ua5v.com")
        );
        assert_eq!(
            cable_decode_tunnel_server_domain(1).as_deref(),
            Some("cable.auth.com")
        );
        assert_eq!(cable_decode_tunnel_server_domain(2), None);
        let hashed = cable_decode_tunnel_server_domain(256).expect("hashed domain");
        assert!(hashed.starts_with("cable."));
        assert!(
            hashed.ends_with(".com")
                || hashed.ends_with(".org")
                || hashed.ends_with(".net")
                || hashed.ends_with(".info")
        );

        // Full QR session + BLE advert round-trip.
        let mut session = CableQrSession::generate(true, 1_700_000_000).expect("qr session");
        assert!(session.qr_uri().starts_with("FIDO:/"));
        assert!(
            session.qr_uri()["FIDO:/".len()..]
                .bytes()
                .all(|b| b.is_ascii_digit())
        );

        let mut eid = [0u8; CABLE_EID_LEN];
        eid[0] = 0; // reserved zero byte
        eid[1..11].copy_from_slice(&[0xAA; 10]);
        eid[11..14].copy_from_slice(&[0x12, 0x34, 0x56]);
        eid[14..16].copy_from_slice(&0u16.to_le_bytes()); // cable.ua5v.com
        let advert = cable_encrypt_advert(&eid, session.eid_key());

        let (domain, path, mut initiator) = session
            .accept_ble_advert(&advert)
            .expect("advert should decrypt and match");
        assert_eq!(domain, "cable.ua5v.com");
        assert!(path.starts_with("/cable/connect/123456/"));
        assert_eq!(session.step(), CableCeremonyStep::Connecting);

        // Full Noise KNpsk0 initiator <-> responder handshake + Crypter round-trip.
        let psk = [0x42u8; CABLE_PSK_LEN];
        let local_id = P256SecretKey::generate();
        let peer_eph = P256SecretKey::generate();
        let mut init2 = CableHandshakeInitiator::new(psk, local_id.clone());
        let init_msg = init2.build_initial_message().expect("initial message");
        assert_eq!(init_msg.len(), CABLE_HANDSHAKE_RESPONSE_LEN);

        // Simulate responder side of KNpsk0:
        let mut resp_noise = CableNoise::new(CableHandshakeType::KNpsk0);
        resp_noise.mix_hash(&[1u8]);
        resp_noise.mix_hash(&local_id.public_key().to_sec1_bytes());
        resp_noise.mix_key_and_hash(&psk);
        let init_eph_pub = &init_msg[..P256_X962_LEN];
        resp_noise.mix_hash(init_eph_pub);
        resp_noise.mix_key(init_eph_pub);
        let pt0 = resp_noise
            .decrypt_and_hash(&init_msg[P256_X962_LEN..])
            .expect("responder decrypt init");
        assert!(pt0.is_empty());

        let peer_eph_pub = peer_eph.public_key().to_sec1_bytes();
        let shared_ee = p256_ecdh_shared_x(&peer_eph, init_eph_pub).expect("ee");
        let shared_se =
            p256_ecdh_shared_x(&peer_eph, &local_id.public_key().to_sec1_bytes()).expect("se");
        resp_noise.mix_hash(&peer_eph_pub);
        resp_noise.mix_key(&peer_eph_pub);
        resp_noise.mix_key(&shared_ee);
        resp_noise.mix_key(&shared_se);
        let resp_ct = resp_noise.encrypt_and_hash(&[]).expect("resp encrypt");
        let mut resp_msg = peer_eph_pub.to_vec();
        resp_msg.extend_from_slice(&resp_ct);

        let mut init_res = init2
            .process_response(&resp_msg)
            .expect("initiator process response");
        assert_eq!(init_res.handshake_hash, resp_noise.handshake_hash());
        let (resp_read_key, resp_write_key) = resp_noise.traffic_keys();
        let mut resp_crypter = CableCrypter::new(resp_read_key, resp_write_key);

        let sealed = init_res.crypter.encrypt(b"hello-ctap").expect("encrypt");
        let opened = resp_crypter.decrypt(&sealed).expect("decrypt");
        assert_eq!(opened, b"hello-ctap");

        // Post-handshake CBOR parsing (unpadded rev 1 + padded rev 0).
        let ph_bytes = cbor_encode(&CborValue::Map(vec![
            (CborValue::Int(1), CborValue::Bytes(vec![0xA1, 0x01, 0x80])),
            (
                CborValue::Int(3),
                CborValue::Array(vec![CborValue::Text("ctap".to_owned())]),
            ),
        ]));
        let ph = parse_post_handshake_message(&ph_bytes).expect("parse post-handshake");
        assert_eq!(ph.protocol_revision, 1);
        assert!(ph.supports_ctap);
        assert_eq!(ph.get_info, vec![0xA1, 0x01, 0x80]);

        // Suppress unused warning on initiator from advert test.
        let _ = initiator.build_initial_message();
    }

    #[test]
    fn fake_provider_handles_supported_and_unsupported_gracefully() {
        let unsupported = FakeWebAuthnProvider::new_unsupported();
        assert!(!unsupported.is_supported());
        assert_eq!(unsupported.capabilities().api_version, 0);

        let reg = RegisterData {
            rp: RelyingParty {
                id: "telegram.org".to_owned(),
                name: "Telegram".to_owned(),
            },
            user: PasskeyUser {
                id: b"2:1000".to_vec(),
                name: "demo".to_owned(),
                display_name: "Demo".to_owned(),
            },
            challenge: b"ch".to_vec(),
            pub_key_cred_params: vec![CredentialParameter {
                cred_type: "public-key".to_owned(),
                alg: COSE_ES256,
            }],
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };
        assert_eq!(
            unsupported.make_credential(0, &reg),
            Err(WebAuthnErrorKind::NotSupported)
        );

        let supported = FakeWebAuthnProvider::new_supported();
        assert!(supported.is_supported());
        let reg_out = supported.make_credential(0, &reg).expect("fake register");
        let parsed_att =
            parse_attestation_object(&reg_out.attestation_object).expect("valid attestation");
        assert_eq!(
            parsed_att.parsed_auth_data.credential_id.as_deref(),
            Some(reg_out.credential_id.as_slice())
        );

        let cable = supported.start_cable_register(&reg).expect("cable qr");
        assert!(cable.session.qr_uri().starts_with("FIDO:/"));
    }
}
