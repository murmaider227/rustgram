//! End-to-End Encrypted (E2E) Conference Calls (`tde2e`) protocol core.
//!
//! Port of:
//! - `Telegram/SourceFiles/tde2e/tde2e_api.h` and `tde2e/tde2e_api.cpp`
//! - `Telegram/SourceFiles/tde2e/tde2e_integration.h` and `tde2e/tde2e_integration.cpp`
//! - Conference E2E integration in `Telegram/SourceFiles/calls/group/calls_group_call.h/.cpp`
//!   (`initConferenceE2E`, `setupConferenceCall`, `trackParticipantsWithAccess`,
//!   `removeConferenceParticipants`, `refreshLastBlockAndJoin`, `startConference`,
//!   `requestSubchainBlocks`, `sendOutboundBlock`, `handleUpdate(MTPDupdateGroupCallChainBlocks)`,
//!   `inviteToConference`)
//! - Public Telegram E2E Group Call TL schema (`e2e.chain.*` and `e2e.callPacket*`) from
//!   `core.telegram.org` and Layer 233 `phone.*` conference RPCs in `schema/api.tl`.
//!
//! # Protocol Fidelity & Notes on External `tde2e` Internals
//!
//! In upstream Telegram Desktop, `tde2e/tde2e_api.cpp` is a thin wrapper around TDLib's
//! standalone C++ static library (`<td/e2e/e2e_api.h>`), whose source does not live inside
//! a Telegram Desktop checkout. This module reproduces the public protocol and tdesktop state
//! machine cleanly in pure Rust. Specifically:
//!
//! **Reproduced faithfully from tdesktop + `core.telegram.org` + `schema/api.tl`:**
//! - Exact Layer 233 `phone.*` conference RPC constructors (`phone.createConferenceCall`,
//!   `phone.joinGroupCall` with `public_key` + `block`, `phone.deleteConferenceCallParticipants`,
//!   `phone.sendConferenceCallBroadcast`, `phone.inviteConferenceCallParticipant`,
//!   `phone.declineConferenceCallInvite`, `phone.getGroupCallChainBlocks`).
//! - Exact `TdE2E::Call` two-subchain sync state machine (subchain `0` = participant/key
//!   blockchain, subchain `1` = verification broadcasts), out-of-order block buffering
//!   (`waiting`), short-poll retry/wait timers (`5_000 ms` / `1_000 ms`, `50` blocks per request),
//!   `refreshLastBlock0`, `makeJoinBlock`, `makeRemoveBlock`, `joined()`, and failure signaling.
//! - Exact `TdE2E::TemporaryKeyPair` (`generate`, `public_key`, `encrypt_for_one`, `decrypt_for_one`)
//!   and `TdE2E::EncryptDecrypt` thread-safe callback surface.
//! - Exact published TL constructor IDs and wire layouts for `e2e.chain.block#639a3db6`,
//!   `e2e.chain.groupState#1ddc7584`, `e2e.chain.groupParticipant#28852f20`,
//!   `e2e.chain.sharedKey#8a847e7f`, `e2e.chain.stateProof#d6b679e6`,
//!   `e2e.chain.changeNoop#deb4a41b`, `e2e.chain.changeSetGroupState#2cf17146`,
//!   `e2e.chain.changeSetSharedKey#987a2158`,
//!   `e2e.chain.groupBroadcastNonceCommit#d1512ae7`, and
//!   `e2e.chain.groupBroadcastNonceReveal#83f4f9d8`.
//! - Exact Ed25519 block signing over the TL serialization with the 64-byte `signature` field
//!   zeroed out, SHA-256 `prev_block_hash` chaining, permission checks (`add_users = 1`,
//!   `remove_users = 2`, `set_value = 4`, monotonic `external_permissions`), and
//!   commit-reveal emoji verification (`HMAC-SHA512(sorted_nonces, block_hash)[..32]` mapped
//!   via Telegram's exact 333-emoji table in [`crate::emoji_fingerprint`]).
//!
//! **Approximations where TDLib's internal C++ `tde2e` implementation is not in tdesktop:**
//! - *Key-wrapping envelope inside `e2e.chain.sharedKey` (`dest_header`, `encrypted_shared_key`)
//!   and `TemporaryKeyPair::{encrypt,decrypt}_for_one`*: uses X25519 ECDH (converted from the
//!   Ed25519 key pair) + SHA-256 domain-separated key derivation + AES-256-CTR + 16-byte
//!   HMAC-SHA256 authentication tag. Wire-compatible between Rustgram peers; exact byte-level
//!   interop with upstream TDLib `tde2e` requires matching TDLib's internal `MessageEncryption`
//!   CBC/padding format once verified against TDLib test vectors.
//! - *Frame encryption packet format (`call_encrypt` / `call_decrypt`)*: preserves
//!   `unencrypted_prefix_size` bytes at the front, appends `header_a` (epoch, channel_id,
//!   msg_id), AES-256-CTR ciphertext, 16-byte HMAC-SHA256 tag (bound to `e2e.callPacket#40a6bee9`,
//!   `header_a`, unencrypted prefix, and ciphertext), 64-byte Ed25519 signature over
//!   `e2e.callPacketLargeMsgId#1ce56c2d || large_msg_id`, and trailing 2-byte LE
//!   `unencrypted_prefix_size`. Wire-compatible between Rustgram peers; upstream TDLib `tde2e`
//!   uses a multi-epoch `header_b` key-slot encoding whose exact bit-packing is internal to
//!   `td/e2e/Call.cpp`.
//! - *Key-value trie root (`stateProof.kv_hash`)*: tdesktop conference calls never write KV
//!   entries (`set_value` is unused by `calls_group_call.cpp`), so `kv_hash` is maintained as
//!   the canonical empty root (`[0u8; 32]`).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, RwLock};

pub use tg_crypto::e2e::E2eKeyPair;

use tg_crypto::aes::{CtrState, aes_ctr_encrypt};
use tg_crypto::e2e::{
    E2E_ENVELOPE_NONCE_SIZE, E2E_KEY_SIZE, E2E_SIGNATURE_SIZE, open_with_shared_secret,
    seal_with_shared_secret, validate_ed25519_public_key, verify_ed25519,
};
use tg_crypto::hash::{const_time_eq, hmac_sha256_parts, hmac_sha512, sha256};
use tg_crypto::random::random_array;
use tg_tl::{
    Cursor, Deserializable, Serializable, VECTOR_ID, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use thiserror::Error;
use zeroize::Zeroize;

use crate::emoji_fingerprint::{EMOJI_IN_FINGERPRINT, compute_emoji_fingerprint};
use crate::tl_conv::TlConversionError;
use crate::types::{InputGroupCall, MsgId, UserId};

// ---------------------------------------------------------------------------
// Constants (`tde2e_api.h`, `tde2e_api.cpp`, `calls_group_call.cpp`, core.telegram.org)
// ---------------------------------------------------------------------------

/// Number of conference E2E subchains (`TdE2E::Call::kSubChainsCount = 2`).
/// - Subchain `0`: participant & shared key state blockchain.
/// - Subchain `1`: emoji verification commit-reveal broadcasts.
pub const SUBCHAINS_COUNT: usize = 2;

/// Main state blockchain subchain ID.
pub const SUBCHAIN_STATE: i32 = 0;

/// Verification broadcast subchain ID.
pub const SUBCHAIN_BROADCAST: i32 = 1;

/// Participant permission bit: may add users (`kPermissionAdd = 1`, `tde2e_api.cpp:29`).
pub const PERMISSION_ADD_USERS: u32 = 1;

/// Participant permission bit: may remove users (`kPermissionRemove = 2`, `tde2e_api.cpp:30`).
pub const PERMISSION_REMOVE_USERS: u32 = 2;

/// Participant permission bit: may modify KV trie (`set_value = 4`).
pub const PERMISSION_SET_VALUE: u32 = 4;

/// Default permissions assigned to conference participants (`kPermissionAdd | kPermissionRemove`).
pub const DEFAULT_PARTICIPANT_PERMISSIONS: u32 = PERMISSION_ADD_USERS | PERMISSION_REMOVE_USERS;

/// Default E2E participant protocol version.
pub const DEFAULT_PARTICIPANT_VERSION: i32 = 1;

/// Default media channel ID used by `TdE2E::EncryptDecrypt::callback()` (`tde2e_api.cpp:89`).
pub const DEFAULT_CALL_CHANNEL_ID: i32 = 0;

/// Periodic short-poll timeout for subchain blocks (`kShortPollChainBlocksTimeout = 5000 ms`).
pub const SHORT_POLL_CHAIN_BLOCKS_TIMEOUT_MS: u64 = 5_000;

/// Wait duration for missing out-of-order subchain blocks before short-polling
/// (`kShortPollChainBlocksWaitFor = 1000 ms`).
pub const SHORT_POLL_CHAIN_BLOCKS_WAIT_FOR_MS: u64 = 1_000;

/// Maximum blocks requested per `phone.getGroupCallChainBlocks` call
/// (`kShortPollChainBlocksPerRequest = 50`, `calls_group_call.cpp:62`).
pub const SHORT_POLL_CHAIN_BLOCKS_PER_REQUEST: i32 = 50;

/// Maximum number of recent epoch shared keys retained for decrypting in-flight frames.
const MAX_RETAINED_EPOCHS: usize = 8;

/// Replay window size (in packet message IDs) per `(sender, channel)` stream.
const FRAME_REPLAY_WINDOW: u64 = 256;

/// Published TL constructor IDs from `core.telegram.org` E2E group calls schema.
pub mod tl_ids {
    /// `e2e.chain.block#639a3db6`
    pub const CHAIN_BLOCK: u32 = 0x639a_3db6;
    /// `e2e.chain.groupState#1ddc7584`
    pub const CHAIN_GROUP_STATE: u32 = 0x1ddc_7584;
    /// `e2e.chain.groupParticipant#28852f20`
    pub const CHAIN_GROUP_PARTICIPANT: u32 = 0x2885_2f20;
    /// `e2e.chain.sharedKey#8a847e7f`
    pub const CHAIN_SHARED_KEY: u32 = 0x8a84_7e7f;
    /// `e2e.chain.stateProof#d6b679e6`
    pub const CHAIN_STATE_PROOF: u32 = 0xd6b6_79e6;
    /// `e2e.chain.changeNoop#deb4a41b`
    pub const CHAIN_CHANGE_NOOP: u32 = 0xdeb4_a41b;
    /// `e2e.chain.changeSetGroupState#2cf17146`
    pub const CHAIN_CHANGE_SET_GROUP_STATE: u32 = 0x2cf1_7146;
    /// `e2e.chain.changeSetSharedKey#987a2158`
    pub const CHAIN_CHANGE_SET_SHARED_KEY: u32 = 0x987a_2158;
    /// `e2e.chain.groupBroadcastNonceCommit#d1512ae7`
    pub const CHAIN_BROADCAST_NONCE_COMMIT: u32 = 0xd151_2ae7;
    /// `e2e.chain.groupBroadcastNonceReveal#83f4f9d8`
    pub const CHAIN_BROADCAST_NONCE_REVEAL: u32 = 0x83f4_f9d8;
    /// `e2e.callPacket#40a6bee9`
    pub const CALL_PACKET: u32 = 0x40a6_bee9;
    /// `e2e.callPacketLargeMsgId#1ce56c2d`
    pub const CALL_PACKET_LARGE_MSG_ID: u32 = 0x1ce5_6c2d;
}

const SHARED_KEY_WRAP_DOMAIN: &[u8] = b"tde2e_shared_key_wrap_v1";
const FRAME_HEADER_LEN: usize = 4 + 4 + 8 + 1; // epoch(i32) + channel_id(i32) + msg_id(u64) + flags(u8)
const FRAME_MAC_LEN: usize = 16;
const FRAME_TRAILER_LEN: usize = 2; // u16 LE unencrypted_prefix_size

// ---------------------------------------------------------------------------
// Error & Basic Value Types
// ---------------------------------------------------------------------------

/// Errors produced by E2E conference blockchain, key exchange, verification, and frame crypto.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum E2eError {
    /// Wire block or broadcast failed TL deserialization.
    #[error("invalid E2E wire encoding: {0}")]
    InvalidEncoding(String),
    /// Block or broadcast Ed25519 signature failed verification.
    #[error("invalid Ed25519 signature: {0}")]
    InvalidSignature(&'static str),
    /// Block `prev_block_hash` did not match the previous block's SHA-256 hash.
    #[error("prev_block_hash mismatch at height {height}")]
    InvalidPrevHash {
        /// Height of the offending block.
        height: i32,
    },
    /// Block height did not advance sequentially.
    #[error("unexpected block height: expected {expected}, got {actual}")]
    InvalidHeight {
        /// Expected height.
        expected: i32,
        /// Observed height.
        actual: i32,
    },
    /// Block signer lacked permissions for the proposed participant state change.
    #[error("permission denied for block change: {0}")]
    PermissionDenied(&'static str),
    /// Block `state_proof` did not match the computed state after applying `changes`.
    #[error("tampered or inconsistent state_proof: {0}")]
    TamperedStateProof(&'static str),
    /// Referenced participant is not present in the active group state.
    #[error("unknown conference participant: {0}")]
    UnknownParticipant(i64),
    /// Shared key or ECDH derivation failed.
    #[error("shared key derivation failed: {0}")]
    KeyDerivationFailed(&'static str),
    /// Frame or envelope decryption/authentication failed.
    #[error("E2E decryption failed: {0}")]
    DecryptionFailed(&'static str),
    /// Frame replay detected (duplicate or stale message ID).
    #[error("replayed frame msg_id {0}")]
    ReplayDetected(u64),
    /// Broadcast commit/reveal validation failed.
    #[error("invalid verification broadcast: {0}")]
    InvalidBroadcast(&'static str),
    /// E2E call is not yet initialized with a blockchain block.
    #[error("E2E conference call not initialized")]
    NotInitialized,
    /// Invalid subchain index (must be 0 or 1).
    #[error("invalid subchain id: {0}")]
    InvalidSubchain(i32),
}

/// Terminal failure reason reported by [`E2eCall`] (`TdE2E::CallFailure` in `tde2e_api.h:96-98`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallFailure {
    /// Generic protocol/verification failure requiring a conference rejoin.
    Unknown,
}

/// 32-byte Ed25519 conference public key (`TdE2E::PublicKey` in `tde2e_api.h:42-47`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PublicKey(pub [u8; E2E_KEY_SIZE]);

impl PublicKey {
    /// Wrap raw 32-byte compressed Ed25519 public key bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; E2E_KEY_SIZE]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 32-byte key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; E2E_KEY_SIZE] {
        &self.0
    }

    /// Convert to the `int256` (`[u8; 32]`) representation used by `tg-tl`
    /// (`TdE2E::PublicKeyToMTP` in `tde2e_integration.cpp:23-25`).
    #[must_use]
    pub const fn to_tl_int256(self) -> [u8; 32] {
        self.0
    }

    /// Construct from a `tg-tl` `int256` (`[u8; 32]`).
    #[must_use]
    pub const fn from_tl_int256(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Validate that this key is a valid compressed Edwards25519 point.
    pub fn validate(&self) -> Result<(), E2eError> {
        validate_ed25519_public_key(&self.0)
            .map_err(|_| E2eError::InvalidEncoding("invalid Ed25519 public key".into()))
    }
}

/// Temporary one-shot Curve25519 key pair for peer-to-peer ECDH envelopes.
///
/// Port of `TdE2E::TemporaryKeyPair` (`tde2e_api.h:53-77`, `tde2e_api.cpp:120-229`).
#[derive(Debug, Clone)]
pub struct TemporaryKeyPair {
    inner: E2eKeyPair,
}

impl TemporaryKeyPair {
    /// Generate a fresh random temporary key pair.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            inner: E2eKeyPair::generate(),
        }
    }

    /// Create a deterministic temporary key pair from a 32-byte seed (useful in tests).
    #[must_use]
    pub const fn from_seed(seed: [u8; E2E_KEY_SIZE]) -> Self {
        Self {
            inner: E2eKeyPair::from_seed(seed),
        }
    }

    /// Export the 32-byte public key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.inner.public_key())
    }

    /// Encrypt `plaintext` to a peer's 32-byte public key using one-shot ECDH.
    pub fn encrypt_for_one(
        &self,
        peer_public_key: &PublicKey,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, E2eError> {
        self.inner
            .encrypt_for_one(&peer_public_key.0, plaintext)
            .map_err(|_| E2eError::KeyDerivationFailed("encrypt_for_one failed"))
    }

    /// Decrypt an envelope created by `peer_public_key` using one-shot ECDH.
    pub fn decrypt_for_one(
        &self,
        peer_public_key: &PublicKey,
        encrypted: &[u8],
    ) -> Result<Vec<u8>, E2eError> {
        self.inner
            .decrypt_for_one(&peer_public_key.0, encrypted)
            .map_err(|_| E2eError::DecryptionFailed("decrypt_for_one failed"))
    }
}

// ---------------------------------------------------------------------------
// TL Wire Serialization Helpers
// ---------------------------------------------------------------------------

fn write_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn read_u32(cur: &mut Cursor<'_>) -> Result<u32, E2eError> {
    cur.read_u32()
        .map_err(|e| E2eError::InvalidEncoding(e.to_string()))
}

fn read_i32(cur: &mut Cursor<'_>) -> Result<i32, E2eError> {
    i32::deserialize(cur).map_err(|e| E2eError::InvalidEncoding(e.to_string()))
}

fn read_i64(cur: &mut Cursor<'_>) -> Result<i64, E2eError> {
    i64::deserialize(cur).map_err(|e| E2eError::InvalidEncoding(e.to_string()))
}

fn read_int256(cur: &mut Cursor<'_>) -> Result<[u8; 32], E2eError> {
    <[u8; 32]>::deserialize(cur).map_err(|e| E2eError::InvalidEncoding(e.to_string()))
}

fn read_int512(cur: &mut Cursor<'_>) -> Result<[u8; 64], E2eError> {
    let lo = read_int256(cur)?;
    let hi = read_int256(cur)?;
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&lo);
    out[32..].copy_from_slice(&hi);
    Ok(out)
}

fn read_bytes(cur: &mut Cursor<'_>) -> Result<Vec<u8>, E2eError> {
    Vec::<u8>::deserialize(cur).map_err(|e| E2eError::InvalidEncoding(e.to_string()))
}

fn write_vec_header(buf: &mut Vec<u8>, len: usize) {
    write_u32(buf, VECTOR_ID);
    (len as i32).serialize(buf);
}

fn read_vec_header(cur: &mut Cursor<'_>) -> Result<usize, E2eError> {
    let id = read_u32(cur)?;
    if id != VECTOR_ID {
        return Err(E2eError::InvalidEncoding(format!(
            "expected vector#1cb5c415, got 0x{id:08x}"
        )));
    }
    let count = read_i32(cur)?;
    if !(0..=4096).contains(&count) {
        return Err(E2eError::InvalidEncoding(format!(
            "invalid vector length {count}"
        )));
    }
    Ok(count as usize)
}

// ---------------------------------------------------------------------------
// Blockchain TL Structures (`e2e.chain.*`)
// ---------------------------------------------------------------------------

/// `e2e.chain.groupParticipant#28852f20`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainGroupParticipant {
    /// Telegram user ID.
    pub user_id: UserId,
    /// Participant's 32-byte Ed25519 public key.
    pub public_key: PublicKey,
    /// Permission bitmask (`PERMISSION_ADD_USERS | PERMISSION_REMOVE_USERS | PERMISSION_SET_VALUE`).
    pub permissions: u32,
    /// Supported E2E protocol version.
    pub version: i32,
}

impl ChainGroupParticipant {
    /// Whether this participant may add new users to the group state.
    #[must_use]
    pub const fn can_add_users(&self) -> bool {
        (self.permissions & PERMISSION_ADD_USERS) != 0
    }

    /// Whether this participant may remove users from the group state.
    #[must_use]
    pub const fn can_remove_users(&self) -> bool {
        (self.permissions & PERMISSION_REMOVE_USERS) != 0
    }

    /// Serialize boxed `e2e.chain.groupParticipant#28852f20`.
    pub fn serialize_tl(&self, buf: &mut Vec<u8>) {
        write_u32(buf, tl_ids::CHAIN_GROUP_PARTICIPANT);
        write_u32(buf, self.permissions);
        self.user_id.to_tl().serialize(buf);
        self.public_key.0.serialize(buf);
        self.version.serialize(buf);
    }

    /// Deserialize boxed `e2e.chain.groupParticipant#28852f20`.
    pub fn deserialize_tl(cur: &mut Cursor<'_>) -> Result<Self, E2eError> {
        let id = read_u32(cur)?;
        if id != tl_ids::CHAIN_GROUP_PARTICIPANT {
            return Err(E2eError::InvalidEncoding(format!(
                "expected e2e.chain.groupParticipant, got 0x{id:08x}"
            )));
        }
        let permissions = read_u32(cur)?;
        let user_id = UserId::from_tl(read_i64(cur)?);
        let public_key = PublicKey(read_int256(cur)?);
        let version = read_i32(cur)?;
        Ok(Self {
            user_id,
            public_key,
            permissions,
            version,
        })
    }
}

/// `e2e.chain.groupState#1ddc7584`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainGroupState {
    /// Active call participants with their public keys and permissions.
    pub participants: Vec<ChainGroupParticipant>,
    /// External permission bitmask applied when a non-member signs a self-add block.
    pub external_permissions: u32,
}

impl ChainGroupState {
    /// Look up a participant by user ID.
    #[must_use]
    pub fn participant(&self, user_id: UserId) -> Option<&ChainGroupParticipant> {
        self.participants.iter().find(|p| p.user_id == user_id)
    }

    /// Look up a participant by Ed25519 public key.
    #[must_use]
    pub fn participant_by_key(&self, pk: &PublicKey) -> Option<&ChainGroupParticipant> {
        self.participants.iter().find(|p| p.public_key == *pk)
    }

    /// Collect the set of active participant user IDs.
    #[must_use]
    pub fn user_ids(&self) -> BTreeSet<UserId> {
        self.participants.iter().map(|p| p.user_id).collect()
    }

    /// Validate uniqueness of user IDs and validity of all Ed25519 public keys.
    pub fn validate(&self) -> Result<(), E2eError> {
        let mut seen = BTreeSet::new();
        for p in &self.participants {
            if !seen.insert(p.user_id) {
                return Err(E2eError::InvalidEncoding(
                    "duplicate user_id in groupState".into(),
                ));
            }
            p.public_key.validate()?;
        }
        Ok(())
    }

    /// Serialize boxed `e2e.chain.groupState#1ddc7584`.
    pub fn serialize_tl(&self, buf: &mut Vec<u8>) {
        write_u32(buf, tl_ids::CHAIN_GROUP_STATE);
        write_vec_header(buf, self.participants.len());
        for p in &self.participants {
            p.serialize_tl(buf);
        }
        (self.external_permissions as i32).serialize(buf);
    }

    /// Deserialize boxed `e2e.chain.groupState#1ddc7584`.
    pub fn deserialize_tl(cur: &mut Cursor<'_>) -> Result<Self, E2eError> {
        let id = read_u32(cur)?;
        if id != tl_ids::CHAIN_GROUP_STATE {
            return Err(E2eError::InvalidEncoding(format!(
                "expected e2e.chain.groupState, got 0x{id:08x}"
            )));
        }
        let count = read_vec_header(cur)?;
        let mut participants = Vec::with_capacity(count);
        for _ in 0..count {
            participants.push(ChainGroupParticipant::deserialize_tl(cur)?);
        }
        let external_permissions = read_i32(cur)? as u32;
        Ok(Self {
            participants,
            external_permissions,
        })
    }
}

/// `e2e.chain.sharedKey#8a847e7f`.
///
/// Distributes a freshly generated 32-byte epoch shared key to every participant in the
/// active `ChainGroupState` using an ephemeral Curve25519 key pair `ek`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainSharedKey {
    /// Ephemeral Ed25519/X25519 public key (`int256`) used to derive per-recipient wrap keys.
    pub ek: PublicKey,
    /// Epoch 32-byte shared key encrypted under a one-time 32-byte content key.
    pub encrypted_shared_key: Vec<u8>,
    /// Recipient Telegram user IDs, 1:1 with `dest_headers`.
    pub dest_user_ids: Vec<UserId>,
    /// Per-recipient encrypted content-key headers.
    pub dest_headers: Vec<Vec<u8>>,
}

impl ChainSharedKey {
    /// Encrypt `raw_shared_key` for every participant in `group_state`, using a fresh ephemeral
    /// key pair (or deterministic `ephemeral_seed` / `content_key` in tests).
    pub fn seal_for_group(
        raw_shared_key: &[u8; 32],
        group_state: &ChainGroupState,
    ) -> Result<Self, E2eError> {
        let ephem = E2eKeyPair::generate();
        let content_key = random_array::<32>();
        let content_nonce = random_array::<E2E_ENVELOPE_NONCE_SIZE>();
        Self::seal_with_params(
            raw_shared_key,
            group_state,
            &ephem,
            &content_key,
            &content_nonce,
        )
    }

    /// Deterministic shared-key distribution helper (used for known-answer / unit tests).
    pub fn seal_with_params(
        raw_shared_key: &[u8; 32],
        group_state: &ChainGroupState,
        ephemeral_key: &E2eKeyPair,
        content_key: &[u8; 32],
        content_nonce: &[u8; E2E_ENVELOPE_NONCE_SIZE],
    ) -> Result<Self, E2eError> {
        if group_state.participants.is_empty() {
            return Err(E2eError::KeyDerivationFailed(
                "cannot distribute shared key to empty group",
            ));
        }
        let ek = PublicKey(ephemeral_key.public_key());
        let encrypted_shared_key =
            seal_with_shared_secret(content_key, content_nonce, raw_shared_key);

        let mut dest_user_ids = Vec::with_capacity(group_state.participants.len());
        let mut dest_headers = Vec::with_capacity(group_state.participants.len());

        for p in &group_state.participants {
            let mut ecdh = ephemeral_key
                .ecdh(&p.public_key.0)
                .map_err(|_| E2eError::KeyDerivationFailed("recipient ECDH failed"))?;
            let mut wrap_key = derive_wrap_key(&ecdh, &ek, &p.public_key, p.user_id);
            ecdh.zeroize();

            let header_nonce = derive_header_nonce(content_nonce, p.user_id, &p.public_key);
            let header = seal_with_shared_secret(&wrap_key, &header_nonce, content_key);
            wrap_key.zeroize();

            dest_user_ids.push(p.user_id);
            dest_headers.push(header);
        }

        Ok(Self {
            ek,
            encrypted_shared_key,
            dest_user_ids,
            dest_headers,
        })
    }

    /// Decrypt the 32-byte epoch shared key for `my_user_id` using `my_key_pair`.
    pub fn open_for_participant(
        &self,
        my_user_id: UserId,
        my_key_pair: &E2eKeyPair,
    ) -> Result<[u8; 32], E2eError> {
        if self.dest_user_ids.len() != self.dest_headers.len() {
            return Err(E2eError::KeyDerivationFailed(
                "dest_user_ids and dest_headers length mismatch",
            ));
        }
        let idx = self
            .dest_user_ids
            .iter()
            .position(|&u| u == my_user_id)
            .ok_or(E2eError::UnknownParticipant(my_user_id.to_tl()))?;

        let my_pk = PublicKey(my_key_pair.public_key());
        let mut ecdh = my_key_pair
            .ecdh(&self.ek.0)
            .map_err(|_| E2eError::KeyDerivationFailed("ephemeral ECDH failed"))?;
        let mut wrap_key = derive_wrap_key(&ecdh, &self.ek, &my_pk, my_user_id);
        ecdh.zeroize();

        let mut content_key_vec = open_with_shared_secret(&wrap_key, &self.dest_headers[idx])
            .map_err(|_| E2eError::DecryptionFailed("dest_header authentication failed"))?;
        wrap_key.zeroize();

        if content_key_vec.len() != 32 {
            content_key_vec.zeroize();
            return Err(E2eError::KeyDerivationFailed("invalid content key length"));
        }
        let mut content_key = [0u8; 32];
        content_key.copy_from_slice(&content_key_vec);
        content_key_vec.zeroize();

        let mut raw_vec = open_with_shared_secret(&content_key, &self.encrypted_shared_key)
            .map_err(|_| E2eError::DecryptionFailed("encrypted_shared_key auth failed"))?;
        content_key.zeroize();

        if raw_vec.len() != 32 {
            raw_vec.zeroize();
            return Err(E2eError::KeyDerivationFailed("invalid shared key length"));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&raw_vec);
        raw_vec.zeroize();
        Ok(out)
    }

    /// Verify that `dest_user_ids` contains exactly the unique participants in `group_state`.
    pub fn validate_recipients(&self, group_state: &ChainGroupState) -> Result<(), E2eError> {
        if self.dest_user_ids.len() != self.dest_headers.len() {
            return Err(E2eError::TamperedStateProof(
                "sharedKey dest_user_ids/dest_headers length mismatch",
            ));
        }
        self.ek.validate()?;
        let expected = group_state.user_ids();
        let mut actual = BTreeSet::new();
        for &uid in &self.dest_user_ids {
            if !actual.insert(uid) {
                return Err(E2eError::TamperedStateProof(
                    "duplicate user_id in sharedKey.dest_user_ids",
                ));
            }
        }
        if actual != expected {
            return Err(E2eError::TamperedStateProof(
                "sharedKey recipients do not match group_state participants",
            ));
        }
        Ok(())
    }

    /// Serialize boxed `e2e.chain.sharedKey#8a847e7f`.
    pub fn serialize_tl(&self, buf: &mut Vec<u8>) {
        write_u32(buf, tl_ids::CHAIN_SHARED_KEY);
        self.ek.0.serialize(buf);
        self.encrypted_shared_key.serialize(buf);
        write_vec_header(buf, self.dest_user_ids.len());
        for uid in &self.dest_user_ids {
            uid.to_tl().serialize(buf);
        }
        write_vec_header(buf, self.dest_headers.len());
        for header in &self.dest_headers {
            header.serialize(buf);
        }
    }

    /// Deserialize boxed `e2e.chain.sharedKey#8a847e7f`.
    pub fn deserialize_tl(cur: &mut Cursor<'_>) -> Result<Self, E2eError> {
        let id = read_u32(cur)?;
        if id != tl_ids::CHAIN_SHARED_KEY {
            return Err(E2eError::InvalidEncoding(format!(
                "expected e2e.chain.sharedKey, got 0x{id:08x}"
            )));
        }
        let ek = PublicKey(read_int256(cur)?);
        let encrypted_shared_key = read_bytes(cur)?;
        let uid_count = read_vec_header(cur)?;
        let mut dest_user_ids = Vec::with_capacity(uid_count);
        for _ in 0..uid_count {
            dest_user_ids.push(UserId::from_tl(read_i64(cur)?));
        }
        let hdr_count = read_vec_header(cur)?;
        let mut dest_headers = Vec::with_capacity(hdr_count);
        for _ in 0..hdr_count {
            dest_headers.push(read_bytes(cur)?);
        }
        Ok(Self {
            ek,
            encrypted_shared_key,
            dest_user_ids,
            dest_headers,
        })
    }
}

fn derive_wrap_key(
    ecdh_secret: &[u8; 32],
    ek: &PublicKey,
    recipient_pk: &PublicKey,
    user_id: UserId,
) -> [u8; 32] {
    sha256(&[
        SHARED_KEY_WRAP_DOMAIN,
        ecdh_secret,
        &ek.0,
        &recipient_pk.0,
        &user_id.to_tl().to_le_bytes(),
    ])
}

fn derive_header_nonce(
    content_nonce: &[u8; E2E_ENVELOPE_NONCE_SIZE],
    user_id: UserId,
    recipient_pk: &PublicKey,
) -> [u8; E2E_ENVELOPE_NONCE_SIZE] {
    let h = sha256(&[
        b"tde2e_hdr_nonce",
        content_nonce,
        &user_id.to_tl().to_le_bytes(),
        &recipient_pk.0,
    ]);
    let mut out = [0u8; E2E_ENVELOPE_NONCE_SIZE];
    out.copy_from_slice(&h[..E2E_ENVELOPE_NONCE_SIZE]);
    out
}

/// `e2e.chain.Change` boxed union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainChange {
    /// `e2e.chain.changeNoop#deb4a41b nonce:int256 = e2e.chain.Change`.
    Noop {
        /// Random 256-bit nonce used to randomize the block hash.
        nonce: [u8; 32],
    },
    /// `e2e.chain.changeSetGroupState#2cf17146 group_state:e2e.chain.GroupState = e2e.chain.Change`.
    SetGroupState(ChainGroupState),
    /// `e2e.chain.changeSetSharedKey#987a2158 shared_key:e2e.chain.SharedKey = e2e.chain.Change`.
    SetSharedKey(ChainSharedKey),
}

impl ChainChange {
    /// Serialize boxed `e2e.chain.Change`.
    pub fn serialize_tl(&self, buf: &mut Vec<u8>) {
        match self {
            Self::Noop { nonce } => {
                write_u32(buf, tl_ids::CHAIN_CHANGE_NOOP);
                nonce.serialize(buf);
            }
            Self::SetGroupState(gs) => {
                write_u32(buf, tl_ids::CHAIN_CHANGE_SET_GROUP_STATE);
                gs.serialize_tl(buf);
            }
            Self::SetSharedKey(sk) => {
                write_u32(buf, tl_ids::CHAIN_CHANGE_SET_SHARED_KEY);
                sk.serialize_tl(buf);
            }
        }
    }

    /// Deserialize boxed `e2e.chain.Change`.
    pub fn deserialize_tl(cur: &mut Cursor<'_>) -> Result<Self, E2eError> {
        let id = read_u32(cur)?;
        match id {
            tl_ids::CHAIN_CHANGE_NOOP => Ok(Self::Noop {
                nonce: read_int256(cur)?,
            }),
            tl_ids::CHAIN_CHANGE_SET_GROUP_STATE => {
                Ok(Self::SetGroupState(ChainGroupState::deserialize_tl(cur)?))
            }
            tl_ids::CHAIN_CHANGE_SET_SHARED_KEY => {
                Ok(Self::SetSharedKey(ChainSharedKey::deserialize_tl(cur)?))
            }
            other => Err(E2eError::InvalidEncoding(format!(
                "unknown e2e.chain.Change constructor 0x{other:08x}"
            ))),
        }
    }
}

/// `e2e.chain.stateProof#d6b679e6`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainStateProof {
    /// Root hash of the key-value trie (`[0u8; 32]` when empty).
    pub kv_hash: [u8; 32],
    /// Resulting group state after applying this block's changes (`flags.0`).
    pub group_state: Option<ChainGroupState>,
    /// Resulting shared key after applying this block's changes (`flags.1`).
    pub shared_key: Option<ChainSharedKey>,
}

impl ChainStateProof {
    /// Serialize boxed `e2e.chain.stateProof#d6b679e6`.
    pub fn serialize_tl(&self, buf: &mut Vec<u8>) {
        write_u32(buf, tl_ids::CHAIN_STATE_PROOF);
        let mut flags = 0u32;
        if self.group_state.is_some() {
            flags |= 1 << 0;
        }
        if self.shared_key.is_some() {
            flags |= 1 << 1;
        }
        write_u32(buf, flags);
        self.kv_hash.serialize(buf);
        if let Some(ref gs) = self.group_state {
            gs.serialize_tl(buf);
        }
        if let Some(ref sk) = self.shared_key {
            sk.serialize_tl(buf);
        }
    }

    /// Deserialize boxed `e2e.chain.stateProof#d6b679e6`.
    pub fn deserialize_tl(cur: &mut Cursor<'_>) -> Result<Self, E2eError> {
        let id = read_u32(cur)?;
        if id != tl_ids::CHAIN_STATE_PROOF {
            return Err(E2eError::InvalidEncoding(format!(
                "expected e2e.chain.stateProof, got 0x{id:08x}"
            )));
        }
        let flags = read_u32(cur)?;
        let kv_hash = read_int256(cur)?;
        let group_state = if (flags & (1 << 0)) != 0 {
            Some(ChainGroupState::deserialize_tl(cur)?)
        } else {
            None
        };
        let shared_key = if (flags & (1 << 1)) != 0 {
            Some(ChainSharedKey::deserialize_tl(cur)?)
        } else {
            None
        };
        Ok(Self {
            kv_hash,
            group_state,
            shared_key,
        })
    }
}

/// `e2e.chain.block#639a3db6`.
///
/// ```tl
/// e2e.chain.block#639a3db6
///   signature:int512
///   flags:#
///   prev_block_hash:int256
///   changes:Vector<e2e.chain.Change>
///   height:int
///   state_proof:e2e.chain.StateProof
///   signature_public_key:flags.0?int256
///   = e2e.chain.Block;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBlock {
    /// 64-byte Ed25519 signature over the block's TL encoding with `signature` zeroed out.
    pub signature: [u8; E2E_SIGNATURE_SIZE],
    /// SHA-256 hash of the complete signed previous block (`[0u8; 32]` at height 0).
    pub prev_block_hash: [u8; 32],
    /// Ordered list of state changes applied by this block.
    pub changes: Vec<ChainChange>,
    /// Sequential block height (`0, 1, 2, ...`).
    pub height: i32,
    /// Cryptographic proof of the state immediately after applying `changes`.
    pub state_proof: ChainStateProof,
    /// Public key of the block signer (`flags.0`).
    pub signature_public_key: Option<PublicKey>,
}

impl ChainBlock {
    /// Serialize boxed `e2e.chain.block#639a3db6` to TL wire bytes.
    #[must_use]
    pub fn to_tl_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);
        self.serialize_tl(&mut buf);
        buf
    }

    /// Serialize into `buf`.
    pub fn serialize_tl(&self, buf: &mut Vec<u8>) {
        write_u32(buf, tl_ids::CHAIN_BLOCK);
        buf.extend_from_slice(&self.signature);
        let flags: u32 = u32::from(self.signature_public_key.is_some());
        write_u32(buf, flags);
        self.prev_block_hash.serialize(buf);
        write_vec_header(buf, self.changes.len());
        for change in &self.changes {
            change.serialize_tl(buf);
        }
        self.height.serialize(buf);
        self.state_proof.serialize_tl(buf);
        if let Some(pk) = self.signature_public_key {
            pk.0.serialize(buf);
        }
    }

    /// Deserialize boxed `e2e.chain.block#639a3db6` from TL wire bytes (rejecting trailing garbage).
    pub fn from_tl_bytes(bytes: &[u8]) -> Result<Self, E2eError> {
        let mut cur = Cursor::new(bytes);
        let id = read_u32(&mut cur)?;
        if id != tl_ids::CHAIN_BLOCK {
            return Err(E2eError::InvalidEncoding(format!(
                "expected e2e.chain.block, got 0x{id:08x}"
            )));
        }
        let signature = read_int512(&mut cur)?;
        let flags = read_u32(&mut cur)?;
        let prev_block_hash = read_int256(&mut cur)?;
        let change_count = read_vec_header(&mut cur)?;
        let mut changes = Vec::with_capacity(change_count);
        for _ in 0..change_count {
            changes.push(ChainChange::deserialize_tl(&mut cur)?);
        }
        let height = read_i32(&mut cur)?;
        let state_proof = ChainStateProof::deserialize_tl(&mut cur)?;
        let signature_public_key = if (flags & (1 << 0)) != 0 {
            Some(PublicKey(read_int256(&mut cur)?))
        } else {
            None
        };
        if !cur.is_empty() {
            return Err(E2eError::InvalidEncoding(format!(
                "trailing {} bytes after e2e.chain.block",
                cur.remaining()
            )));
        }
        Ok(Self {
            signature,
            prev_block_hash,
            changes,
            height,
            state_proof,
            signature_public_key,
        })
    }

    /// Compute the canonical signing payload (TL serialization with `signature = [0u8; 64]`).
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature = [0u8; E2E_SIGNATURE_SIZE];
        unsigned.to_tl_bytes()
    }

    /// Compute `SHA256(block.to_tl_bytes())` — used as `prev_block_hash` by the next block
    /// and as `chain_hash` in verification broadcasts.
    #[must_use]
    pub fn block_hash(&self) -> [u8; 32] {
        let bytes = self.to_tl_bytes();
        sha256(&[&bytes])
    }

    /// Sign this block with `key_pair` (setting `signature_public_key` and `signature`).
    pub fn sign(&mut self, key_pair: &E2eKeyPair) {
        self.signature_public_key = Some(PublicKey(key_pair.public_key()));
        let payload = self.signing_payload();
        self.signature = key_pair.sign(&payload);
    }

    /// Verify the block's Ed25519 signature against `signer_pk`.
    pub fn verify_signature(&self, signer_pk: &PublicKey) -> Result<(), E2eError> {
        let payload = self.signing_payload();
        verify_ed25519(&signer_pk.0, &payload, &self.signature)
            .map_err(|_| E2eError::InvalidSignature("chain block Ed25519 signature mismatch"))
    }
}

// ---------------------------------------------------------------------------
// Verification Subchain Broadcasts (`e2e.chain.GroupBroadcast`)
// ---------------------------------------------------------------------------

/// Verification broadcast on subchain `1` (`e2e.chain.GroupBroadcast`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupBroadcast {
    /// `e2e.chain.groupBroadcastNonceCommit#d1512ae7`.
    NonceCommit {
        /// 64-byte Ed25519 signature over the TL serialization with `signature` zeroed out.
        signature: [u8; E2E_SIGNATURE_SIZE],
        /// Sender user ID.
        user_id: UserId,
        /// Height of the target main-chain block.
        chain_height: i32,
        /// SHA-256 hash of the target main-chain block.
        chain_hash: [u8; 32],
        /// `SHA256(nonce)` commitment.
        nonce_hash: [u8; 32],
    },
    /// `e2e.chain.groupBroadcastNonceReveal#83f4f9d8`.
    NonceReveal {
        /// 64-byte Ed25519 signature over the TL serialization with `signature` zeroed out.
        signature: [u8; E2E_SIGNATURE_SIZE],
        /// Sender user ID.
        user_id: UserId,
        /// Height of the target main-chain block.
        chain_height: i32,
        /// SHA-256 hash of the target main-chain block.
        chain_hash: [u8; 32],
        /// Original 32-byte random nonce matching `nonce_hash = SHA256(nonce)`.
        nonce: [u8; 32],
    },
}

impl GroupBroadcast {
    /// Create a signed `e2e.chain.groupBroadcastNonceCommit#d1512ae7`.
    #[must_use]
    pub fn new_commit(
        key_pair: &E2eKeyPair,
        user_id: UserId,
        chain_height: i32,
        chain_hash: [u8; 32],
        nonce: &[u8; 32],
    ) -> Self {
        let nonce_hash = sha256(&[nonce]);
        let mut msg = Self::NonceCommit {
            signature: [0u8; E2E_SIGNATURE_SIZE],
            user_id,
            chain_height,
            chain_hash,
            nonce_hash,
        };
        let sig = key_pair.sign(&msg.signing_payload());
        if let Self::NonceCommit { signature, .. } = &mut msg {
            *signature = sig;
        }
        msg
    }

    /// Create a signed `e2e.chain.groupBroadcastNonceReveal#83f4f9d8`.
    #[must_use]
    pub fn new_reveal(
        key_pair: &E2eKeyPair,
        user_id: UserId,
        chain_height: i32,
        chain_hash: [u8; 32],
        nonce: [u8; 32],
    ) -> Self {
        let mut msg = Self::NonceReveal {
            signature: [0u8; E2E_SIGNATURE_SIZE],
            user_id,
            chain_height,
            chain_hash,
            nonce,
        };
        let sig = key_pair.sign(&msg.signing_payload());
        if let Self::NonceReveal { signature, .. } = &mut msg {
            *signature = sig;
        }
        msg
    }

    /// Sender user ID of this broadcast.
    #[must_use]
    pub const fn user_id(&self) -> UserId {
        match self {
            Self::NonceCommit { user_id, .. } | Self::NonceReveal { user_id, .. } => *user_id,
        }
    }

    /// Target main-chain height.
    #[must_use]
    pub const fn chain_height(&self) -> i32 {
        match self {
            Self::NonceCommit { chain_height, .. } | Self::NonceReveal { chain_height, .. } => {
                *chain_height
            }
        }
    }

    /// Target main-chain block hash.
    #[must_use]
    pub const fn chain_hash(&self) -> &[u8; 32] {
        match self {
            Self::NonceCommit { chain_hash, .. } | Self::NonceReveal { chain_hash, .. } => {
                chain_hash
            }
        }
    }

    /// Serialize boxed `e2e.chain.GroupBroadcast` to TL wire bytes.
    #[must_use]
    pub fn to_tl_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(144);
        match self {
            Self::NonceCommit {
                signature,
                user_id,
                chain_height,
                chain_hash,
                nonce_hash,
            } => {
                write_u32(&mut buf, tl_ids::CHAIN_BROADCAST_NONCE_COMMIT);
                buf.extend_from_slice(signature);
                user_id.to_tl().serialize(&mut buf);
                chain_height.serialize(&mut buf);
                chain_hash.serialize(&mut buf);
                nonce_hash.serialize(&mut buf);
            }
            Self::NonceReveal {
                signature,
                user_id,
                chain_height,
                chain_hash,
                nonce,
            } => {
                write_u32(&mut buf, tl_ids::CHAIN_BROADCAST_NONCE_REVEAL);
                buf.extend_from_slice(signature);
                user_id.to_tl().serialize(&mut buf);
                chain_height.serialize(&mut buf);
                chain_hash.serialize(&mut buf);
                nonce.serialize(&mut buf);
            }
        }
        buf
    }

    /// Deserialize boxed `e2e.chain.GroupBroadcast` from TL wire bytes.
    pub fn from_tl_bytes(bytes: &[u8]) -> Result<Self, E2eError> {
        let mut cur = Cursor::new(bytes);
        let id = read_u32(&mut cur)?;
        let signature = read_int512(&mut cur)?;
        let user_id = UserId::from_tl(read_i64(&mut cur)?);
        let chain_height = read_i32(&mut cur)?;
        let chain_hash = read_int256(&mut cur)?;
        let payload = read_int256(&mut cur)?;
        if !cur.is_empty() {
            return Err(E2eError::InvalidEncoding(format!(
                "trailing {} bytes after e2e.chain.GroupBroadcast",
                cur.remaining()
            )));
        }
        match id {
            tl_ids::CHAIN_BROADCAST_NONCE_COMMIT => Ok(Self::NonceCommit {
                signature,
                user_id,
                chain_height,
                chain_hash,
                nonce_hash: payload,
            }),
            tl_ids::CHAIN_BROADCAST_NONCE_REVEAL => Ok(Self::NonceReveal {
                signature,
                user_id,
                chain_height,
                chain_hash,
                nonce: payload,
            }),
            other => Err(E2eError::InvalidEncoding(format!(
                "unknown GroupBroadcast constructor 0x{other:08x}"
            ))),
        }
    }

    /// Signing payload with `signature` zeroed out.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut copy = self.clone();
        match &mut copy {
            Self::NonceCommit { signature, .. } | Self::NonceReveal { signature, .. } => {
                *signature = [0u8; E2E_SIGNATURE_SIZE];
            }
        }
        copy.to_tl_bytes()
    }

    /// Verify Ed25519 signature against `sender_pk`.
    pub fn verify_signature(&self, sender_pk: &PublicKey) -> Result<(), E2eError> {
        let sig = match self {
            Self::NonceCommit { signature, .. } | Self::NonceReveal { signature, .. } => signature,
        };
        verify_ed25519(&sender_pk.0, &self.signing_payload(), sig)
            .map_err(|_| E2eError::InvalidSignature("broadcast Ed25519 signature mismatch"))
    }
}

/// Compute the 32-byte conference `emoji_hash` from revealed participant nonces and `chain_hash`.
///
/// Exact formula from Telegram's E2E group call verification specification:
/// 1. Sort all 32-byte revealed nonces in lexicographical byte order.
/// 2. Concatenate them into `key`.
/// 3. `emoji_hash = HMAC-SHA512(key = concatenated_sorted_nonces, message = chain_hash)[..32]`.
#[must_use]
pub fn compute_conference_emoji_hash(nonces: &[[u8; 32]], chain_hash: &[u8; 32]) -> [u8; 32] {
    let mut sorted = nonces.to_vec();
    sorted.sort_unstable();
    let mut key = Vec::with_capacity(sorted.len() * 32);
    for n in &sorted {
        key.extend_from_slice(n);
    }
    let full = hmac_sha512(&key, chain_hash);
    let mut out = [0u8; 32];
    out.copy_from_slice(&full[..32]);
    out
}

/// Convert a 32-byte conference `emoji_hash` into the 4 verification emoji strings using
/// Telegram's exact 333-emoji table (`calls/calls_emoji_fingerprint.cpp:530-533`).
#[must_use]
pub fn conference_emoji_fingerprint(emoji_hash: &[u8; 32]) -> [String; EMOJI_IN_FINGERPRINT] {
    compute_emoji_fingerprint(emoji_hash)
}

// ---------------------------------------------------------------------------
// Blockchain & Verification Core (`ConferenceBlockchain`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct VerificationRound {
    chain_height: i32,
    chain_hash: [u8; 32],
    my_nonce: [u8; 32],
    reveal_sent: bool,
    commits: BTreeMap<UserId, [u8; 32]>,
    reveals: BTreeMap<UserId, [u8; 32]>,
    emoji_hash: Option<[u8; 32]>,
}

/// Stateful participant/key blockchain and emoji verification engine for a single conference call.
#[derive(Debug, Clone)]
pub struct ConferenceBlockchain {
    my_user_id: UserId,
    my_key_pair: E2eKeyPair,
    height: Option<i32>,
    last_block_hash: [u8; 32],
    group_state: ChainGroupState,
    shared_key: Option<ChainSharedKey>,
    epoch_keys: BTreeMap<i32, [u8; 32]>,
    epoch_participants: BTreeMap<i32, BTreeMap<UserId, PublicKey>>,
    verification: Option<VerificationRound>,
    outbound_broadcasts: VecDeque<Vec<u8>>,
}

impl ConferenceBlockchain {
    /// Create a new uninitialized blockchain context for `my_user_id` with `my_key_pair`.
    #[must_use]
    pub fn new(my_user_id: UserId, my_key_pair: E2eKeyPair) -> Self {
        Self {
            my_user_id,
            my_key_pair,
            height: None,
            last_block_hash: [0u8; 32],
            group_state: ChainGroupState::default(),
            shared_key: None,
            epoch_keys: BTreeMap::new(),
            epoch_participants: BTreeMap::new(),
            verification: None,
            outbound_broadcasts: VecDeque::new(),
        }
    }

    /// Local user ID.
    #[must_use]
    pub const fn my_user_id(&self) -> UserId {
        self.my_user_id
    }

    /// Local 32-byte Ed25519 public key.
    #[must_use]
    pub fn my_public_key(&self) -> PublicKey {
        PublicKey(self.my_key_pair.public_key())
    }

    /// Current applied main-chain height, or `None` before block initialization.
    #[must_use]
    pub const fn height(&self) -> Option<i32> {
        self.height
    }

    /// SHA-256 hash of the last applied block (`[0u8; 32]` if uninitialized).
    #[must_use]
    pub const fn last_block_hash(&self) -> &[u8; 32] {
        &self.last_block_hash
    }

    /// Current validated `ChainGroupState`.
    #[must_use]
    pub fn group_state(&self) -> &ChainGroupState {
        &self.group_state
    }

    /// Current active epoch shared key (if this participant has access).
    #[must_use]
    pub fn current_shared_key(&self) -> Option<(i32, [u8; 32])> {
        let epoch = self.height?;
        let key = self.epoch_keys.get(&epoch).copied()?;
        Some((epoch, key))
    }

    /// Current 32-byte emoji verification hash, once commit-reveal completes for the current block.
    #[must_use]
    pub fn emoji_hash(&self) -> Option<[u8; 32]> {
        self.verification.as_ref().and_then(|v| v.emoji_hash)
    }

    /// Current 4-emoji verification fingerprint, if ready.
    #[must_use]
    pub fn emoji_fingerprint(&self) -> Option<[String; EMOJI_IN_FINGERPRINT]> {
        self.emoji_hash().map(|h| conference_emoji_fingerprint(&h))
    }

    /// Pull all pending outbound subchain-1 verification broadcasts (`call_pull_outbound_messages`).
    pub fn pull_outbound_broadcasts(&mut self) -> Vec<Vec<u8>> {
        self.outbound_broadcasts.drain(..).collect()
    }

    /// Create block 0 (zero block) for starting a brand-new conference call
    /// (`tde2e_api::call_create_zero_block`, `tde2e_api.cpp:294-297`).
    pub fn create_zero_block(
        my_key_pair: &E2eKeyPair,
        participants: Vec<ChainGroupParticipant>,
        external_permissions: u32,
    ) -> Result<ChainBlock, E2eError> {
        let group_state = ChainGroupState {
            participants,
            external_permissions,
        };
        group_state.validate()?;
        let my_pk = PublicKey(my_key_pair.public_key());
        if group_state.participant_by_key(&my_pk).is_none() {
            return Err(E2eError::PermissionDenied(
                "zero block creator must be in initial participants",
            ));
        }

        let raw_shared_key = random_array::<32>();
        let shared_key = ChainSharedKey::seal_for_group(&raw_shared_key, &group_state)?;
        let noop_nonce = random_array::<32>();

        let mut block = ChainBlock {
            signature: [0u8; E2E_SIGNATURE_SIZE],
            prev_block_hash: [0u8; 32],
            changes: vec![
                ChainChange::Noop { nonce: noop_nonce },
                ChainChange::SetGroupState(group_state.clone()),
                ChainChange::SetSharedKey(shared_key.clone()),
            ],
            height: 0,
            state_proof: ChainStateProof {
                kv_hash: [0u8; 32],
                group_state: Some(group_state),
                shared_key: Some(shared_key),
            },
            signature_public_key: Some(my_pk),
        };
        block.sign(my_key_pair);
        Ok(block)
    }

    /// Create a self-add join block extending `last_block_bytes`
    /// (`tde2e_api::call_create_self_add_block`, `tde2e_api.cpp:290-293`).
    pub fn create_self_add_block(
        my_key_pair: &E2eKeyPair,
        last_block_bytes: &[u8],
        my_participant: ChainGroupParticipant,
    ) -> Result<ChainBlock, E2eError> {
        let prev = ChainBlock::from_tl_bytes(last_block_bytes)?;
        let prev_signer = prev.signature_public_key.ok_or(E2eError::InvalidSignature(
            "previous block missing signature_public_key",
        ))?;
        prev.verify_signature(&prev_signer)?;

        let mut group_state =
            prev.state_proof
                .group_state
                .clone()
                .ok_or(E2eError::TamperedStateProof(
                    "previous block missing group_state in state_proof",
                ))?;
        group_state.validate()?;

        // Check permission: either already in group with ADD_USERS or external_permissions allows ADD_USERS.
        let existing = group_state.participant(my_participant.user_id).cloned();
        let allowed = match existing {
            Some(ref p) => {
                p.can_add_users() || (group_state.external_permissions & PERMISSION_ADD_USERS) != 0
            }
            None => (group_state.external_permissions & PERMISSION_ADD_USERS) != 0,
        };
        if !allowed {
            return Err(E2eError::PermissionDenied(
                "self-add not permitted by group external_permissions",
            ));
        }

        // Replace existing entry for same user_id (e.g. rejoin with new key) or append.
        if let Some(slot) = group_state
            .participants
            .iter_mut()
            .find(|p| p.user_id == my_participant.user_id)
        {
            *slot = my_participant;
        } else {
            group_state.participants.push(my_participant);
        }

        let raw_shared_key = random_array::<32>();
        let shared_key = ChainSharedKey::seal_for_group(&raw_shared_key, &group_state)?;
        let next_height = prev.height.checked_add(1).ok_or(E2eError::InvalidHeight {
            expected: i32::MAX,
            actual: prev.height,
        })?;

        let mut block = ChainBlock {
            signature: [0u8; E2E_SIGNATURE_SIZE],
            prev_block_hash: prev.block_hash(),
            changes: vec![
                ChainChange::SetGroupState(group_state.clone()),
                ChainChange::SetSharedKey(shared_key.clone()),
            ],
            height: next_height,
            state_proof: ChainStateProof {
                kv_hash: prev.state_proof.kv_hash,
                group_state: Some(group_state),
                shared_key: Some(shared_key),
            },
            signature_public_key: Some(PublicKey(my_key_pair.public_key())),
        };
        block.sign(my_key_pair);
        Ok(block)
    }

    /// Create a block changing the group state (e.g. removing departed/kicked participants)
    /// and rotating the epoch shared key (`tde2e_api::call_create_change_state_block`).
    pub fn create_change_state_block(
        &self,
        updated_participants: Vec<ChainGroupParticipant>,
    ) -> Result<ChainBlock, E2eError> {
        let current_height = self.height.ok_or(E2eError::NotInitialized)?;
        let me = self
            .group_state
            .participant(self.my_user_id)
            .ok_or(E2eError::PermissionDenied("local user not in group_state"))?;

        let new_state = ChainGroupState {
            participants: updated_participants,
            external_permissions: self.group_state.external_permissions,
        };
        new_state.validate()?;
        validate_state_transition(&self.group_state, &new_state, Some(me))?;

        let raw_shared_key = random_array::<32>();
        let shared_key = ChainSharedKey::seal_for_group(&raw_shared_key, &new_state)?;
        let next_height = current_height
            .checked_add(1)
            .ok_or(E2eError::InvalidHeight {
                expected: i32::MAX,
                actual: current_height,
            })?;

        let mut block = ChainBlock {
            signature: [0u8; E2E_SIGNATURE_SIZE],
            prev_block_hash: self.last_block_hash,
            changes: vec![
                ChainChange::SetGroupState(new_state.clone()),
                ChainChange::SetSharedKey(shared_key.clone()),
            ],
            height: next_height,
            state_proof: ChainStateProof {
                kv_hash: [0u8; 32],
                group_state: Some(new_state),
                shared_key: Some(shared_key),
            },
            signature_public_key: Some(PublicKey(self.my_key_pair.public_key())),
        };
        block.sign(&self.my_key_pair);
        Ok(block)
    }

    /// Verify and apply a main-chain block (subchain `0`).
    ///
    /// - If this blockchain is uninitialized (`height == None`), initializes state from `block`
    ///   (matching `tde2e_api::call_create` in `tde2e_api.cpp:405-413`) after checking its
    ///   signature, internal changes consistency, and `state_proof`.
    /// - Otherwise enforces sequential `height == prev + 1`, exact `prev_block_hash`,
    ///   valid Ed25519 signature, transition permissions, and exact `state_proof` match.
    pub fn apply_block(&mut self, raw_bytes: &[u8]) -> Result<&ChainGroupState, E2eError> {
        let block = ChainBlock::from_tl_bytes(raw_bytes)?;
        if block.height < 0 {
            return Err(E2eError::InvalidHeight {
                expected: 0,
                actual: block.height,
            });
        }

        let signer_pk = block
            .signature_public_key
            .ok_or(E2eError::InvalidSignature(
                "block missing signature_public_key",
            ))?;
        signer_pk.validate()?;
        block.verify_signature(&signer_pk)?;

        let (candidate_group, candidate_shared) =
            match self.height {
                None => {
                    // Initializing from block 0 or from a mid-call snapshot block on join.
                    if block.height == 0 && block.prev_block_hash != [0u8; 32] {
                        return Err(E2eError::InvalidPrevHash { height: 0 });
                    }
                    let (mut gs, mut sk) = (ChainGroupState::default(), None);
                    apply_changes(&mut gs, &mut sk, &block.changes)?;
                    gs.validate()?;

                    // If block 0, signer must be in initial participants.
                    if block.height == 0 && gs.participant_by_key(&signer_pk).is_none() {
                        return Err(E2eError::PermissionDenied(
                            "block 0 signer not present in initial participants",
                        ));
                    }

                    // Verify state_proof matches resulting state (or for a mid-call snapshot
                    // whose changes only touch part of state, ensure state_proof has group_state).
                    let proof_gs = block.state_proof.group_state.as_ref().ok_or(
                        E2eError::TamperedStateProof("state_proof missing group_state"),
                    )?;
                    proof_gs.validate()?;
                    if !gs.participants.is_empty() && *proof_gs != gs {
                        return Err(E2eError::TamperedStateProof(
                            "state_proof.group_state disagrees with block changes",
                        ));
                    }
                    let final_gs = proof_gs.clone();

                    let final_sk = match (&sk, &block.state_proof.shared_key) {
                        (Some(c), Some(p)) => {
                            if c != p {
                                return Err(E2eError::TamperedStateProof(
                                    "state_proof.shared_key disagrees with block changes",
                                ));
                            }
                            Some(p.clone())
                        }
                        (None, Some(p)) => Some(p.clone()),
                        (Some(_), None) => {
                            return Err(E2eError::TamperedStateProof(
                                "state_proof missing shared_key set by changes",
                            ));
                        }
                        (None, None) => None,
                    };
                    if let Some(ref key) = final_sk {
                        key.validate_recipients(&final_gs)?;
                    }
                    (final_gs, final_sk)
                }
                Some(prev_height) => {
                    let expected_height = prev_height.saturating_add(1);
                    if block.height != expected_height {
                        return Err(E2eError::InvalidHeight {
                            expected: expected_height,
                            actual: block.height,
                        });
                    }
                    if block.prev_block_hash != self.last_block_hash {
                        return Err(E2eError::InvalidPrevHash {
                            height: block.height,
                        });
                    }

                    let mut gs = self.group_state.clone();
                    let mut sk = self.shared_key.clone();
                    apply_changes(&mut gs, &mut sk, &block.changes)?;
                    gs.validate()?;

                    // Validate signer permissions for transition from self.group_state -> gs.
                    let signer_in_prev = self.group_state.participant_by_key(&signer_pk);
                    if let Some(existing_signer) = signer_in_prev {
                        validate_state_transition(&self.group_state, &gs, Some(existing_signer))?;
                    } else {
                        // Self-add by an external user joining the conference.
                        let signer_in_new =
                            gs.participant_by_key(&signer_pk)
                                .ok_or(E2eError::PermissionDenied(
                                    "block signer not in previous or new group_state",
                                ))?;
                        validate_self_add_transition(&self.group_state, &gs, signer_in_new)?;
                    }

                    // Verify state_proof matches computed post-block state exactly.
                    let proof_gs = block.state_proof.group_state.as_ref().ok_or(
                        E2eError::TamperedStateProof("state_proof missing group_state"),
                    )?;
                    if *proof_gs != gs {
                        return Err(E2eError::TamperedStateProof(
                            "state_proof.group_state mismatch",
                        ));
                    }
                    if block.state_proof.shared_key != sk {
                        return Err(E2eError::TamperedStateProof(
                            "state_proof.shared_key mismatch",
                        ));
                    }
                    if let Some(ref key) = sk {
                        key.validate_recipients(&gs)?;
                    }
                    (gs, sk)
                }
            };

        // Commit validated block state.
        let block_hash = block.block_hash();
        self.height = Some(block.height);
        self.last_block_hash = block_hash;
        self.group_state = candidate_group;
        self.shared_key = candidate_shared;

        // Snapshot participant keys for this epoch (used for verifying in-flight frames).
        let pmap: BTreeMap<UserId, PublicKey> = self
            .group_state
            .participants
            .iter()
            .map(|p| (p.user_id, p.public_key))
            .collect();
        self.epoch_participants.insert(block.height, pmap);
        while self.epoch_participants.len() > MAX_RETAINED_EPOCHS {
            if let Some((&oldest, _)) = self.epoch_participants.first_key_value() {
                self.epoch_participants.remove(&oldest);
            } else {
                break;
            }
        }

        // Derive/decrypt epoch shared key if we are in the recipient set.
        if let Some(ref sk) = self.shared_key
            && self.group_state.participant(self.my_user_id).is_some()
        {
            let decrypted = sk.open_for_participant(self.my_user_id, &self.my_key_pair)?;
            self.epoch_keys.insert(block.height, decrypted);
            while self.epoch_keys.len() > MAX_RETAINED_EPOCHS {
                if let Some((&oldest, _)) = self.epoch_keys.first_key_value() {
                    if let Some(mut old_key) = self.epoch_keys.remove(&oldest) {
                        old_key.zeroize();
                    }
                } else {
                    break;
                }
            }
        }

        // Start a fresh emoji verification commit-reveal round for this block height/hash.
        self.start_verification_round(block.height, block_hash);

        Ok(&self.group_state)
    }

    /// Process an incoming subchain `1` verification broadcast (`call_receive_inbound_message`).
    ///
    /// Returns the updated `emoji_hash` (if commit-reveal is complete for the current block).
    pub fn apply_broadcast(&mut self, raw_bytes: &[u8]) -> Result<Option<[u8; 32]>, E2eError> {
        let broadcast = GroupBroadcast::from_tl_bytes(raw_bytes)?;
        let Some(ref mut round) = self.verification else {
            return Err(E2eError::NotInitialized);
        };

        // Ignore broadcasts targeting older or mismatched chain heights (stale subchain 1 traffic).
        if broadcast.chain_height() != round.chain_height
            || *broadcast.chain_hash() != round.chain_hash
        {
            return Ok(round.emoji_hash);
        }

        // Look up sender in current group state and verify Ed25519 signature.
        let sender_id = broadcast.user_id();
        let sender = self
            .group_state
            .participant(sender_id)
            .ok_or(E2eError::UnknownParticipant(sender_id.to_tl()))?;
        broadcast.verify_signature(&sender.public_key)?;

        match broadcast {
            GroupBroadcast::NonceCommit { nonce_hash, .. } => {
                // First valid commit per participant for this round wins (prevents equivocation).
                if let Some(existing) = round.commits.get(&sender_id) {
                    if !const_time_eq(existing, &nonce_hash) {
                        return Err(E2eError::InvalidBroadcast(
                            "conflicting nonce commitment from participant",
                        ));
                    }
                } else {
                    round.commits.insert(sender_id, nonce_hash);
                }

                // Once every active participant has committed, broadcast our reveal.
                let all_committed = self
                    .group_state
                    .participants
                    .iter()
                    .all(|p| round.commits.contains_key(&p.user_id));
                if all_committed && !round.reveal_sent {
                    round.reveal_sent = true;
                    let reveal = GroupBroadcast::new_reveal(
                        &self.my_key_pair,
                        self.my_user_id,
                        round.chain_height,
                        round.chain_hash,
                        round.my_nonce,
                    );
                    self.outbound_broadcasts.push_back(reveal.to_tl_bytes());
                }
            }
            GroupBroadcast::NonceReveal { nonce, .. } => {
                let committed = round
                    .commits
                    .get(&sender_id)
                    .ok_or(E2eError::InvalidBroadcast(
                        "nonce reveal received before commit",
                    ))?;
                let actual_hash = sha256(&[&nonce]);
                if !const_time_eq(committed, &actual_hash) {
                    return Err(E2eError::InvalidBroadcast(
                        "revealed nonce does not match committed hash",
                    ));
                }
                round.reveals.insert(sender_id, nonce);

                let all_revealed = self
                    .group_state
                    .participants
                    .iter()
                    .all(|p| round.reveals.contains_key(&p.user_id));
                if all_revealed {
                    let nonces: Vec<[u8; 32]> = round.reveals.values().copied().collect();
                    round.emoji_hash =
                        Some(compute_conference_emoji_hash(&nonces, &round.chain_hash));
                }
            }
        }

        Ok(round.emoji_hash)
    }

    fn start_verification_round(&mut self, chain_height: i32, chain_hash: [u8; 32]) {
        if self.group_state.participant(self.my_user_id).is_none() {
            self.verification = None;
            return;
        }
        let my_nonce = random_array::<32>();
        let commit = GroupBroadcast::new_commit(
            &self.my_key_pair,
            self.my_user_id,
            chain_height,
            chain_hash,
            &my_nonce,
        );
        self.outbound_broadcasts.push_back(commit.to_tl_bytes());
        self.verification = Some(VerificationRound {
            chain_height,
            chain_hash,
            my_nonce,
            reveal_sent: false,
            commits: BTreeMap::new(),
            reveals: BTreeMap::new(),
            emoji_hash: None,
        });
    }
}

fn apply_changes(
    group_state: &mut ChainGroupState,
    shared_key: &mut Option<ChainSharedKey>,
    changes: &[ChainChange],
) -> Result<(), E2eError> {
    if changes.is_empty() {
        return Err(E2eError::InvalidEncoding(
            "e2e.chain.block must contain at least one change".into(),
        ));
    }
    for change in changes {
        match change {
            ChainChange::Noop { .. } => {}
            ChainChange::SetGroupState(new_gs) => {
                *group_state = new_gs.clone();
                // Per Telegram spec: changing group state invalidates the previous shared key.
                *shared_key = None;
            }
            ChainChange::SetSharedKey(new_sk) => {
                *shared_key = Some(new_sk.clone());
            }
        }
    }
    Ok(())
}

fn validate_state_transition(
    prev: &ChainGroupState,
    next: &ChainGroupState,
    signer: Option<&ChainGroupParticipant>,
) -> Result<(), E2eError> {
    // External permissions bitmask may never increase.
    if (next.external_permissions & !prev.external_permissions) != 0 {
        return Err(E2eError::PermissionDenied(
            "external_permissions cannot be increased",
        ));
    }
    let Some(signer) = signer else {
        return Err(E2eError::PermissionDenied("missing authorized signer"));
    };

    let prev_ids = prev.user_ids();
    let next_ids = next.user_ids();

    let added = next_ids.difference(&prev_ids).count();
    let removed = prev_ids.difference(&next_ids).count();

    if added > 0 && !signer.can_add_users() {
        return Err(E2eError::PermissionDenied(
            "signer lacks add_users permission",
        ));
    }
    if removed > 0 && !signer.can_remove_users() {
        return Err(E2eError::PermissionDenied(
            "signer lacks remove_users permission",
        ));
    }
    Ok(())
}

fn validate_self_add_transition(
    prev: &ChainGroupState,
    next: &ChainGroupState,
    signer: &ChainGroupParticipant,
) -> Result<(), E2eError> {
    if (next.external_permissions & !prev.external_permissions) != 0 {
        return Err(E2eError::PermissionDenied(
            "external_permissions cannot be increased",
        ));
    }
    if (prev.external_permissions & PERMISSION_ADD_USERS) == 0 {
        return Err(E2eError::PermissionDenied(
            "external self-add disallowed by external_permissions",
        ));
    }
    // External signer may only add themselves (or replace their own entry) without removing anyone else.
    for old_p in &prev.participants {
        if old_p.user_id == signer.user_id {
            continue;
        }
        let Some(kept) = next.participant(old_p.user_id) else {
            return Err(E2eError::PermissionDenied(
                "external self-add block cannot remove existing participants",
            ));
        };
        if kept != old_p {
            return Err(E2eError::PermissionDenied(
                "external self-add block cannot alter existing participants",
            ));
        }
    }
    for new_p in &next.participants {
        if new_p.user_id != signer.user_id && prev.participant(new_p.user_id).is_none() {
            return Err(E2eError::PermissionDenied(
                "external self-add block cannot add third-party users",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Frame Encryption / Decryption (`ConferenceEncryptDecrypt`)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ReplayTracker {
    highest_msg_id: u64,
    seen_mask: [u64; 4], // 256-bit sliding window
    initialized: bool,
}

impl ReplayTracker {
    fn check_and_record(&mut self, msg_id: u64) -> Result<(), E2eError> {
        if !self.initialized {
            self.initialized = true;
            self.highest_msg_id = msg_id;
            self.seen_mask = [0; 4];
            self.set_bit(0);
            return Ok(());
        }
        if msg_id > self.highest_msg_id {
            let shift = msg_id - self.highest_msg_id;
            self.shift_left(shift);
            self.highest_msg_id = msg_id;
            self.set_bit(0);
            Ok(())
        } else {
            let delta = self.highest_msg_id - msg_id;
            if delta >= FRAME_REPLAY_WINDOW {
                return Err(E2eError::ReplayDetected(msg_id));
            }
            if self.test_bit(delta) {
                return Err(E2eError::ReplayDetected(msg_id));
            }
            self.set_bit(delta);
            Ok(())
        }
    }

    fn test_bit(&self, bit: u64) -> bool {
        let word = (bit / 64) as usize;
        let offset = bit % 64;
        (self.seen_mask[word] & (1u64 << offset)) != 0
    }

    fn set_bit(&mut self, bit: u64) {
        let word = (bit / 64) as usize;
        let offset = bit % 64;
        self.seen_mask[word] |= 1u64 << offset;
    }

    fn shift_left(&mut self, shift: u64) {
        if shift >= FRAME_REPLAY_WINDOW {
            self.seen_mask = [0; 4];
            return;
        }
        let word_shift = (shift / 64) as usize;
        let bit_shift = shift % 64;
        let mut next = [0u64; 4];
        for i in 0..4 {
            if i + word_shift < 4 {
                next[i + word_shift] |= self.seen_mask[i] << bit_shift;
            }
            if bit_shift > 0 && i + word_shift + 1 < 4 {
                next[i + word_shift + 1] |= self.seen_mask[i] >> (64 - bit_shift);
            }
        }
        self.seen_mask = next;
    }
}

#[derive(Debug)]
struct FrameCryptoState {
    active: bool,
    my_user_id: UserId,
    my_key_pair: E2eKeyPair,
    current_epoch: Option<i32>,
    epoch_keys: BTreeMap<i32, [u8; 32]>,
    epoch_participants: BTreeMap<i32, BTreeMap<UserId, PublicKey>>,
    next_msg_ids: HashMap<i32, u64>,
    replay_windows: HashMap<(UserId, i32), ReplayTracker>,
}

impl Drop for FrameCryptoState {
    fn drop(&mut self) {
        for key in self.epoch_keys.values_mut() {
            key.zeroize();
        }
    }
}

/// Thread-safe conference media & message frame encryptor/decryptor.
///
/// Port of `TdE2E::EncryptDecrypt` (`tde2e_api.h:102-114`, `tde2e_api.cpp:78-118`).
/// Shared via `Arc<ConferenceEncryptDecrypt>` between [`E2eCall`], `tg-voip::GroupMediaEngine`,
/// and [`crate::group::messages::GroupCallMessages`].
#[derive(Debug)]
pub struct ConferenceEncryptDecrypt {
    state: RwLock<FrameCryptoState>,
}

impl ConferenceEncryptDecrypt {
    /// Create a detached (inactive) encryptor/decryptor handle before a conference call starts.
    #[must_use]
    pub fn new_inactive() -> Arc<Self> {
        Arc::new(Self {
            state: RwLock::new(FrameCryptoState {
                active: false,
                my_user_id: UserId(0),
                my_key_pair: E2eKeyPair::from_seed([0u8; 32]),
                current_epoch: None,
                epoch_keys: BTreeMap::new(),
                epoch_participants: BTreeMap::new(),
                next_msg_ids: HashMap::new(),
                replay_windows: HashMap::new(),
            }),
        })
    }

    /// Synchronize active keys & participant signing keys from a [`ConferenceBlockchain`].
    pub fn sync_from_blockchain(&self, chain: &ConferenceBlockchain) {
        if let Ok(mut guard) = self.state.write() {
            guard.active = chain.height.is_some();
            guard.my_user_id = chain.my_user_id;
            guard.my_key_pair = chain.my_key_pair.clone();
            guard.current_epoch = chain.height;
            guard.epoch_keys = chain.epoch_keys.clone();
            guard.epoch_participants = chain.epoch_participants.clone();
        }
    }

    /// Deactivate frame encryption/decryption (e.g. on call teardown or rejoin).
    pub fn clear(&self) {
        if let Ok(mut guard) = self.state.write() {
            guard.active = false;
            guard.current_epoch = None;
            for key in guard.epoch_keys.values_mut() {
                key.zeroize();
            }
            guard.epoch_keys.clear();
            guard.epoch_participants.clear();
            guard.next_msg_ids.clear();
            guard.replay_windows.clear();
        }
    }

    /// Whether an active epoch shared key is currently installed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state.read().is_ok_and(|g| {
            g.active
                && g.current_epoch
                    .is_some_and(|e| g.epoch_keys.contains_key(&e))
        })
    }

    /// Current active encryption epoch (main-chain height), if ready.
    #[must_use]
    pub fn current_epoch(&self) -> Option<i32> {
        self.state
            .read()
            .ok()
            .and_then(|g| if g.active { g.current_epoch } else { None })
    }

    /// Encrypt an audio/video media frame or in-call encrypted message on `channel_id`.
    ///
    /// Preserves the leading `unencrypted_prefix_size` bytes in plaintext (needed by SFU
    /// packetizers for codec headers) while authenticating them under both the HMAC tag and
    /// Ed25519 signature.
    pub fn encrypt_frame(
        &self,
        channel_id: i32,
        plaintext: &[u8],
        unencrypted_prefix_size: usize,
    ) -> Result<Vec<u8>, E2eError> {
        if unencrypted_prefix_size > plaintext.len()
            || unencrypted_prefix_size > usize::from(u16::MAX)
        {
            return Err(E2eError::InvalidEncoding(
                "unencrypted_prefix_size exceeds frame bounds".into(),
            ));
        }

        let (epoch, shared_key, my_user_id, my_key_pair, msg_id) = {
            let mut guard = self.state.write().map_err(|_| E2eError::NotInitialized)?;
            if !guard.active {
                return Err(E2eError::NotInitialized);
            }
            let epoch = guard.current_epoch.ok_or(E2eError::NotInitialized)?;
            let shared_key = *guard
                .epoch_keys
                .get(&epoch)
                .ok_or(E2eError::KeyDerivationFailed(
                    "no shared key for current epoch",
                ))?;
            let counter = guard.next_msg_ids.entry(channel_id).or_insert(0);
            *counter = counter.wrapping_add(1);
            let msg_id = *counter;
            (
                epoch,
                shared_key,
                guard.my_user_id,
                guard.my_key_pair.clone(),
                msg_id,
            )
        };

        let extra_data = &plaintext[..unencrypted_prefix_size];
        let body = &plaintext[unencrypted_prefix_size..];

        let mut header_a = [0u8; FRAME_HEADER_LEN];
        header_a[0..4].copy_from_slice(&epoch.to_le_bytes());
        header_a[4..8].copy_from_slice(&channel_id.to_le_bytes());
        header_a[8..16].copy_from_slice(&msg_id.to_le_bytes());
        header_a[16] = 0; // reserved flags

        let (mut enc_key, iv, mut mac_key) =
            derive_frame_keys(&shared_key, my_user_id, channel_id, msg_id);

        let mut encrypted_payload = body.to_vec();
        let mut ctr = CtrState::new(enc_key, iv);
        aes_ctr_encrypt(&mut encrypted_payload, &mut ctr);
        enc_key.zeroize();

        let magic1 = tl_ids::CALL_PACKET.to_le_bytes();
        let full_mac = hmac_sha256_parts(
            &mac_key,
            &[&magic1, &header_a, extra_data, &encrypted_payload],
        );
        mac_key.zeroize();
        let mac_tag = &full_mac[..FRAME_MAC_LEN];

        let large_msg_id = sha256(&[
            &magic1,
            &epoch.to_le_bytes(),
            &my_user_id.to_tl().to_le_bytes(),
            &channel_id.to_le_bytes(),
            &msg_id.to_le_bytes(),
            mac_tag,
        ]);
        let magic2 = tl_ids::CALL_PACKET_LARGE_MSG_ID.to_le_bytes();
        let mut sig_input = [0u8; 4 + 32];
        sig_input[..4].copy_from_slice(&magic2);
        sig_input[4..].copy_from_slice(&large_msg_id);
        let signature = my_key_pair.sign(&sig_input);

        let total_len = extra_data.len()
            + FRAME_HEADER_LEN
            + encrypted_payload.len()
            + FRAME_MAC_LEN
            + E2E_SIGNATURE_SIZE
            + FRAME_TRAILER_LEN;
        let mut out = Vec::with_capacity(total_len);
        out.extend_from_slice(extra_data);
        out.extend_from_slice(&header_a);
        out.extend_from_slice(&encrypted_payload);
        out.extend_from_slice(mac_tag);
        out.extend_from_slice(&signature);
        out.extend_from_slice(&(unencrypted_prefix_size as u16).to_le_bytes());
        Ok(out)
    }

    /// Authenticate and decrypt an incoming frame from `sender_user_id` on `channel_id`.
    pub fn decrypt_frame(
        &self,
        sender_user_id: UserId,
        channel_id: i32,
        packet: &[u8],
    ) -> Result<Vec<u8>, E2eError> {
        let min_len = FRAME_HEADER_LEN + FRAME_MAC_LEN + E2E_SIGNATURE_SIZE + FRAME_TRAILER_LEN;
        if packet.len() < min_len {
            return Err(E2eError::DecryptionFailed("encrypted frame too short"));
        }

        let trailer_start = packet.len() - FRAME_TRAILER_LEN;
        let prefix_len =
            u16::from_le_bytes([packet[trailer_start], packet[trailer_start + 1]]) as usize;
        if prefix_len + min_len > packet.len() {
            return Err(E2eError::DecryptionFailed(
                "invalid unencrypted_prefix_size trailer",
            ));
        }

        let extra_data = &packet[..prefix_len];
        let header_start = prefix_len;
        let header_end = header_start + FRAME_HEADER_LEN;
        let header_a = &packet[header_start..header_end];

        let sig_start = trailer_start - E2E_SIGNATURE_SIZE;
        let mac_start = sig_start - FRAME_MAC_LEN;
        let encrypted_payload = &packet[header_end..mac_start];
        let mac_tag = &packet[mac_start..sig_start];
        let signature: [u8; E2E_SIGNATURE_SIZE] = packet[sig_start..trailer_start]
            .try_into()
            .map_err(|_| E2eError::DecryptionFailed("invalid signature slice"))?;

        let epoch = i32::from_le_bytes([header_a[0], header_a[1], header_a[2], header_a[3]]);
        let pkt_channel = i32::from_le_bytes([header_a[4], header_a[5], header_a[6], header_a[7]]);
        let msg_id = u64::from_le_bytes([
            header_a[8],
            header_a[9],
            header_a[10],
            header_a[11],
            header_a[12],
            header_a[13],
            header_a[14],
            header_a[15],
        ]);
        if pkt_channel != channel_id {
            return Err(E2eError::DecryptionFailed("channel_id mismatch"));
        }

        let (shared_key, sender_pk) = {
            let guard = self.state.read().map_err(|_| E2eError::NotInitialized)?;
            if !guard.active {
                return Err(E2eError::NotInitialized);
            }
            let shared_key = *guard
                .epoch_keys
                .get(&epoch)
                .ok_or(E2eError::DecryptionFailed("unknown epoch shared key"))?;
            let sender_pk = guard
                .epoch_participants
                .get(&epoch)
                .and_then(|m| m.get(&sender_user_id))
                .copied()
                .ok_or(E2eError::UnknownParticipant(sender_user_id.to_tl()))?;
            (shared_key, sender_pk)
        };

        // Verify MAC tag first (constant-time).
        let (mut enc_key, iv, mut mac_key) =
            derive_frame_keys(&shared_key, sender_user_id, channel_id, msg_id);
        let magic1 = tl_ids::CALL_PACKET.to_le_bytes();
        let expected_mac = hmac_sha256_parts(
            &mac_key,
            &[&magic1, header_a, extra_data, encrypted_payload],
        );
        mac_key.zeroize();
        if !const_time_eq(mac_tag, &expected_mac[..FRAME_MAC_LEN]) {
            enc_key.zeroize();
            return Err(E2eError::DecryptionFailed("frame HMAC tag mismatch"));
        }

        // Verify Ed25519 signature over `e2e.callPacketLargeMsgId || large_msg_id`.
        let large_msg_id = sha256(&[
            &magic1,
            &epoch.to_le_bytes(),
            &sender_user_id.to_tl().to_le_bytes(),
            &channel_id.to_le_bytes(),
            &msg_id.to_le_bytes(),
            mac_tag,
        ]);
        let magic2 = tl_ids::CALL_PACKET_LARGE_MSG_ID.to_le_bytes();
        let mut sig_input = [0u8; 4 + 32];
        sig_input[..4].copy_from_slice(&magic2);
        sig_input[4..].copy_from_slice(&large_msg_id);
        if verify_ed25519(&sender_pk.0, &sig_input, &signature).is_err() {
            enc_key.zeroize();
            return Err(E2eError::InvalidSignature(
                "frame Ed25519 signature mismatch",
            ));
        }

        // Record msg_id in replay window (after authenticating sender & payload).
        {
            let mut guard = self.state.write().map_err(|_| E2eError::NotInitialized)?;
            let tracker = guard
                .replay_windows
                .entry((sender_user_id, channel_id))
                .or_default();
            if let Err(e) = tracker.check_and_record(msg_id) {
                enc_key.zeroize();
                return Err(e);
            }
        }

        let mut plaintext = Vec::with_capacity(extra_data.len() + encrypted_payload.len());
        plaintext.extend_from_slice(extra_data);
        let mut body = encrypted_payload.to_vec();
        let mut ctr = CtrState::new(enc_key, iv);
        aes_ctr_encrypt(&mut body, &mut ctr);
        enc_key.zeroize();
        plaintext.extend_from_slice(&body);
        Ok(plaintext)
    }

    /// Callback-shaped helper matching `TdE2E::EncryptDecrypt::callback()`
    /// (`tde2e_api.cpp:78-106`): returns empty `Vec` on failure.
    #[must_use]
    pub fn transform(
        &self,
        data: &[u8],
        user_id: i64,
        encrypt: bool,
        unencrypted_prefix_size: i32,
    ) -> Vec<u8> {
        if encrypt {
            let prefix = usize::try_from(unencrypted_prefix_size).unwrap_or(0);
            self.encrypt_frame(DEFAULT_CALL_CHANNEL_ID, data, prefix)
                .unwrap_or_default()
        } else {
            self.decrypt_frame(UserId::from_tl(user_id), DEFAULT_CALL_CHANNEL_ID, data)
                .unwrap_or_default()
        }
    }
}

fn derive_frame_keys(
    shared_key: &[u8; 32],
    sender_user_id: UserId,
    channel_id: i32,
    msg_id: u64,
) -> ([u8; 32], [u8; 16], [u8; 32]) {
    let magic = tl_ids::CALL_PACKET.to_le_bytes();
    let uid = sender_user_id.to_tl().to_le_bytes();
    let ch = channel_id.to_le_bytes();
    let mid = msg_id.to_le_bytes();

    let enc_key = sha256(&[&magic, shared_key, &uid, &ch, &mid, b"key"]);
    let iv_full = sha256(&[&magic, shared_key, &uid, &ch, &mid, b"iv"]);
    let mac_key = sha256(&[&magic, shared_key, &uid, &ch, &mid, b"mac"]);
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&iv_full[..16]);
    (enc_key, iv, mac_key)
}

// ---------------------------------------------------------------------------
// `E2eCall` Sans-IO Coordinator (port of `TdE2E::Call` in `tde2e_api.h/.cpp`)
// ---------------------------------------------------------------------------

/// Timers managed per subchain by [`E2eCall`] (`SubChainState` in `tde2e_api.h:159-166`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum E2eTimerKind {
    /// Periodic short-poll timer (`kShortPollChainBlocksTimeout = 5000 ms`).
    ShortPoll,
    /// Out-of-order block gap wait timer (`kShortPollChainBlocksWaitFor = 1000 ms`).
    WaitingBlocks,
}

/// Sans-IO actions emitted by [`E2eCall`] for `calls_group_call` / `tg-app`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum E2eAction {
    /// Fetch subchain blocks via `phone.getGroupCallChainBlocks(sub_chain_id, offset, limit)`.
    RequestSubchainBlocks {
        /// Subchain index (`0` = state chain, `1` = verification broadcasts).
        subchain: i32,
        /// Starting block height (`offset`).
        offset: i32,
        /// Maximum blocks to request (`SHORT_POLL_CHAIN_BLOCKS_PER_REQUEST`).
        limit: i32,
    },
    /// Broadcast a verification subchain-1 block via `phone.sendConferenceCallBroadcast`.
    SendOutboundBlock(Vec<u8>),
    /// Active participant set with E2E access changed (`participantsSetValue`).
    ParticipantsSetChanged(BTreeSet<UserId>),
    /// 32-byte emoji verification hash updated (`emojiHashValue`; `None` while negotiating).
    EmojiHashChanged(Option<[u8; 32]>),
    /// Arm a per-subchain timer.
    SetTimer {
        /// Subchain index (`0` or `1`).
        subchain: i32,
        /// Timer kind.
        kind: E2eTimerKind,
        /// Timeout in milliseconds.
        duration_ms: u64,
    },
    /// Cancel a per-subchain timer.
    CancelTimer {
        /// Subchain index (`0` or `1`).
        subchain: i32,
        /// Timer kind.
        kind: E2eTimerKind,
    },
    /// E2E protocol encountered an unrecoverable error (caller should trigger `startRejoin`).
    Failed(CallFailure),
}

#[derive(Debug, Clone)]
struct SubchainSync {
    waiting: BTreeMap<i32, Vec<u8>>,
    short_polling: bool,
    height: i32,
    waiting_timer_active: bool,
}

impl Default for SubchainSync {
    fn default() -> Self {
        Self {
            waiting: BTreeMap::new(),
            short_polling: true,
            height: 0,
            waiting_timer_active: false,
        }
    }
}

/// Sans-IO conference E2E controller managing both subchains, join/remove block generation,
/// emoji verification, and frame crypto synchronization.
///
/// Port of `TdE2E::Call` (`tde2e/tde2e_api.h:116-198`, `tde2e/tde2e_api.cpp:231-634`).
#[derive(Debug)]
pub struct E2eCall {
    blockchain: ConferenceBlockchain,
    encrypt_decrypt: Arc<ConferenceEncryptDecrypt>,
    failure: Option<CallFailure>,
    subchains: [SubchainSync; SUBCHAINS_COUNT],
    last_block0: Option<Vec<u8>>,
    last_block0_height: i32,
    participants_set: BTreeSet<UserId>,
    emoji_hash: Option<[u8; 32]>,
    actions: Vec<E2eAction>,
}

impl E2eCall {
    /// Create a new conference E2E session with a fresh random Curve25519 key pair.
    #[must_use]
    pub fn new(my_user_id: UserId) -> Self {
        Self::with_key_pair(my_user_id, E2eKeyPair::generate())
    }

    /// Create a new conference E2E session with a caller-provided (e.g. deterministic) key pair.
    #[must_use]
    pub fn with_key_pair(my_user_id: UserId, key_pair: E2eKeyPair) -> Self {
        Self {
            blockchain: ConferenceBlockchain::new(my_user_id, key_pair),
            encrypt_decrypt: ConferenceEncryptDecrypt::new_inactive(),
            failure: None,
            subchains: [SubchainSync::default(), SubchainSync::default()],
            last_block0: None,
            last_block0_height: 0,
            participants_set: BTreeSet::new(),
            emoji_hash: None,
            actions: Vec::new(),
        }
    }

    /// Local 32-byte Ed25519 public key (`Call::myKey`).
    #[must_use]
    pub fn my_key(&self) -> PublicKey {
        self.blockchain.my_public_key()
    }

    /// Shared thread-safe frame encryptor/decryptor handle for `tg-voip` and encrypted messages.
    #[must_use]
    pub fn encrypt_decrypt(&self) -> Arc<ConferenceEncryptDecrypt> {
        Arc::clone(&self.encrypt_decrypt)
    }

    /// Access the underlying validated [`ConferenceBlockchain`].
    #[must_use]
    pub fn blockchain(&self) -> &ConferenceBlockchain {
        &self.blockchain
    }

    /// Whether the conference E2E state has been initialized from at least one subchain-0 block.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.blockchain.height().is_some()
    }

    /// Terminal failure state, if any (`Call::failed`).
    #[must_use]
    pub const fn failed(&self) -> Option<CallFailure> {
        self.failure
    }

    /// Current set of user IDs with E2E access (`participantsSetValue`).
    #[must_use]
    pub fn participants_set(&self) -> &BTreeSet<UserId> {
        &self.participants_set
    }

    /// Current 32-byte emoji verification hash, if ready.
    #[must_use]
    pub const fn emoji_hash(&self) -> Option<[u8; 32]> {
        self.emoji_hash
    }

    /// Current 4-emoji verification fingerprint, if ready.
    #[must_use]
    pub fn emoji_fingerprint(&self) -> Option<[String; EMOJI_IN_FINGERPRINT]> {
        self.emoji_hash.map(|h| conference_emoji_fingerprint(&h))
    }

    /// Drain all emitted [`E2eAction`]s.
    pub fn take_actions(&mut self) -> Vec<E2eAction> {
        std::mem::take(&mut self.actions)
    }

    /// Whether a cached latest subchain-0 block is available for building a self-add join block.
    #[must_use]
    pub fn has_last_block0(&self) -> bool {
        self.last_block0.is_some()
    }

    /// Update the cached latest subchain-0 block fetched via `getGroupCallChainBlocks(0, -1, 1)`
    /// (`Call::refreshLastBlock0`, `tde2e_api.cpp:266-268`).
    pub fn refresh_last_block0(&mut self, block: Option<Vec<u8>>) {
        self.last_block0 = block;
    }

    /// Construct a signed join block:
    /// - If `last_block0` is `None`, builds block `0` (for `phone.createConferenceCall` or empty chain).
    /// - Otherwise builds a self-add block chaining onto `last_block0` (for `phone.joinGroupCall`).
    ///
    /// Port of `TdE2E::Call::makeJoinBlock` (`tde2e_api.cpp:270-306`).
    pub fn make_join_block(&mut self) -> Result<Vec<u8>, E2eError> {
        if self.failure.is_some() {
            return Err(E2eError::NotInitialized);
        }
        let my_participant = ChainGroupParticipant {
            user_id: self.blockchain.my_user_id(),
            public_key: self.my_key(),
            permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
            version: DEFAULT_PARTICIPANT_VERSION,
        };
        let block = match &self.last_block0 {
            Some(prev) => ConferenceBlockchain::create_self_add_block(
                &self.blockchain.my_key_pair,
                prev,
                my_participant,
            ),
            None => ConferenceBlockchain::create_zero_block(
                &self.blockchain.my_key_pair,
                vec![my_participant],
                DEFAULT_PARTICIPANT_PERMISSIONS,
            ),
        };
        match block {
            Ok(b) => Ok(b.to_tl_bytes()),
            Err(e) => {
                self.fail(CallFailure::Unknown);
                Err(e)
            }
        }
    }

    /// Construct a signed state-change block removing `ids` (never removing self) and rotating
    /// the epoch shared key (`TdE2E::Call::makeRemoveBlock`, `tde2e_api.cpp:308-343`).
    ///
    /// Returns `Ok(None)` if none of `ids` are currently present in the E2E participant set.
    pub fn make_remove_block(
        &mut self,
        ids: &BTreeSet<UserId>,
    ) -> Result<Option<Vec<u8>>, E2eError> {
        if self.failure.is_some() || !self.is_initialized() {
            return Ok(None);
        }
        let my_uid = self.blockchain.my_user_id();
        let mut updated = Vec::with_capacity(self.blockchain.group_state().participants.len());
        let mut found = false;
        for p in &self.blockchain.group_state().participants {
            if p.user_id != my_uid && ids.contains(&p.user_id) {
                found = true;
            } else {
                updated.push(p.clone());
            }
        }
        if !found {
            return Ok(None);
        }
        match self.blockchain.create_change_state_block(updated) {
            Ok(block) => Ok(Some(block.to_tl_bytes())),
            Err(e) => {
                self.fail(CallFailure::Unknown);
                Err(e)
            }
        }
    }

    /// Notify E2E engine that `phone.createConferenceCall` / `phone.joinGroupCall` succeeded,
    /// kicking off initial short-polls on subchains `0` and `1` (`Call::joined`, `tde2e_api.cpp:349-357`).
    pub fn joined(&mut self) {
        if !self.is_initialized() {
            self.fail(CallFailure::Unknown);
            return;
        }
        self.short_poll(SUBCHAIN_STATE);
        self.short_poll(SUBCHAIN_BROADCAST);
    }

    /// Apply a batch of blocks received either from an `updateGroupCallChainBlocks` push
    /// (`from_short_poll = false`) or from a `phone.getGroupCallChainBlocks` response
    /// (`from_short_poll = true`).
    ///
    /// Exact port of `TdE2E::Call::apply` (`tde2e_api.cpp:458-505`).
    pub fn apply_blocks(
        &mut self,
        subchain: i32,
        index_after_last: i32,
        blocks: &[Vec<u8>],
        from_short_poll: bool,
    ) -> Result<(), E2eError> {
        let sc = validate_subchain(subchain)?;
        if subchain == SUBCHAIN_STATE
            && !blocks.is_empty()
            && index_after_last > self.last_block0_height
        {
            self.last_block0 = blocks.last().cloned();
            self.last_block0_height = index_after_last;
        }

        if from_short_poll {
            self.subchains[sc]
                .waiting
                .retain(|&idx, _| idx >= index_after_last);
            if subchain == SUBCHAIN_BROADCAST && !self.is_initialized() && !blocks.is_empty() {
                self.fail(CallFailure::Unknown);
                return Err(E2eError::NotInitialized);
            }
        }
        if self.failure.is_some() {
            return Ok(());
        }

        let mut index = index_after_last.saturating_sub(blocks.len() as i32);
        let need_buffer = !from_short_poll
            && (index > self.subchains[sc].height
                || (!self.is_initialized() && subchain == SUBCHAIN_BROADCAST));

        if need_buffer {
            for block in blocks {
                self.subchains[sc].waiting.insert(index, block.clone());
                index = index.saturating_add(1);
            }
        } else {
            for block in blocks {
                if !self.is_initialized() || self.subchains[sc].height == index {
                    self.apply_single_block(subchain, block)?;
                }
                index = index.saturating_add(1);
                self.subchains[sc].height = self.subchains[sc].height.max(index);
            }
            self.subchains[sc].height = self.subchains[sc].height.max(index_after_last);
        }

        self.check_waiting_blocks(subchain, false)?;
        Ok(())
    }

    /// Notify [`E2eCall`] that the in-flight `phone.getGroupCallChainBlocks` request for
    /// `subchain` has finished (`Call::subchainBlocksRequestFinished`, `tde2e_api.cpp:589-599`).
    pub fn subchain_blocks_request_finished(&mut self, subchain: i32) -> Result<(), E2eError> {
        let sc = validate_subchain(subchain)?;
        if self.failure.is_some() {
            return Ok(());
        }
        self.subchains[sc].short_polling = false;
        self.check_waiting_blocks(subchain, false)
    }

    /// Dispatch a fired per-subchain timer (`ShortPoll` or `WaitingBlocks`).
    pub fn on_timer(&mut self, subchain: i32, kind: E2eTimerKind) -> Result<(), E2eError> {
        let sc = validate_subchain(subchain)?;
        if self.failure.is_some() {
            return Ok(());
        }
        match kind {
            E2eTimerKind::ShortPoll => {
                self.short_poll(subchain);
                Ok(())
            }
            E2eTimerKind::WaitingBlocks => {
                self.subchains[sc].waiting_timer_active = false;
                self.check_waiting_blocks(subchain, true)
            }
        }
    }

    fn apply_single_block(&mut self, subchain: i32, raw_block: &[u8]) -> Result<(), E2eError> {
        let was_initialized = self.is_initialized();
        if subchain == SUBCHAIN_BROADCAST {
            if !was_initialized {
                self.fail(CallFailure::Unknown);
                return Err(E2eError::NotInitialized);
            }
            match self.blockchain.apply_broadcast(raw_block) {
                Ok(new_hash) => {
                    self.update_emoji_hash(new_hash);
                    self.flush_outbound_broadcasts();
                    Ok(())
                }
                Err(e) => {
                    self.fail(CallFailure::Unknown);
                    Err(e)
                }
            }
        } else {
            match self.blockchain.apply_block(raw_block) {
                Ok(state) => {
                    let next_set = state.user_ids();
                    self.encrypt_decrypt.sync_from_blockchain(&self.blockchain);
                    if !was_initialized {
                        for sc in 0..SUBCHAINS_COUNT {
                            if !self.subchains[sc].waiting_timer_active {
                                self.actions.push(E2eAction::SetTimer {
                                    subchain: sc as i32,
                                    kind: E2eTimerKind::ShortPoll,
                                    duration_ms: SHORT_POLL_CHAIN_BLOCKS_TIMEOUT_MS,
                                });
                            }
                        }
                    }
                    if next_set != self.participants_set {
                        self.participants_set = next_set.clone();
                        self.actions
                            .push(E2eAction::ParticipantsSetChanged(next_set));
                    }
                    self.update_emoji_hash(self.blockchain.emoji_hash());
                    self.flush_outbound_broadcasts();
                    Ok(())
                }
                Err(e) => {
                    self.fail(CallFailure::Unknown);
                    Err(e)
                }
            }
        }
    }

    fn check_waiting_blocks(&mut self, subchain: i32, waited: bool) -> Result<(), E2eError> {
        let sc = validate_subchain(subchain)?;
        if self.failure.is_some() {
            return Ok(());
        }
        if !self.is_initialized() {
            self.subchains[sc].waiting_timer_active = true;
            self.actions.push(E2eAction::SetTimer {
                subchain,
                kind: E2eTimerKind::WaitingBlocks,
                duration_ms: SHORT_POLL_CHAIN_BLOCKS_WAIT_FOR_MS,
            });
            return Ok(());
        }
        if self.subchains[sc].short_polling {
            return Ok(());
        }

        self.actions.push(E2eAction::CancelTimer {
            subchain,
            kind: E2eTimerKind::ShortPoll,
        });

        while let Some((&idx, _)) = self.subchains[sc].waiting.first_key_value() {
            if idx > self.subchains[sc].height {
                if waited {
                    self.short_poll(subchain);
                } else {
                    self.subchains[sc].waiting_timer_active = true;
                    self.actions.push(E2eAction::SetTimer {
                        subchain,
                        kind: E2eTimerKind::WaitingBlocks,
                        duration_ms: SHORT_POLL_CHAIN_BLOCKS_WAIT_FOR_MS,
                    });
                }
                return Ok(());
            }
            if let Some((idx, block)) = self.subchains[sc].waiting.pop_first()
                && idx == self.subchains[sc].height
            {
                self.apply_single_block(subchain, &block)?;
                self.subchains[sc].height = self.subchains[sc].height.max(idx.saturating_add(1));
            }
        }

        self.subchains[sc].waiting_timer_active = false;
        self.actions.push(E2eAction::CancelTimer {
            subchain,
            kind: E2eTimerKind::WaitingBlocks,
        });
        self.actions.push(E2eAction::SetTimer {
            subchain,
            kind: E2eTimerKind::ShortPoll,
            duration_ms: SHORT_POLL_CHAIN_BLOCKS_TIMEOUT_MS,
        });
        Ok(())
    }

    fn short_poll(&mut self, subchain: i32) {
        let Ok(sc) = validate_subchain(subchain) else {
            return;
        };
        self.subchains[sc].waiting_timer_active = false;
        self.actions.push(E2eAction::CancelTimer {
            subchain,
            kind: E2eTimerKind::WaitingBlocks,
        });
        self.actions.push(E2eAction::CancelTimer {
            subchain,
            kind: E2eTimerKind::ShortPoll,
        });
        if subchain == SUBCHAIN_BROADCAST && !self.is_initialized() {
            self.subchains[sc].waiting_timer_active = true;
            self.actions.push(E2eAction::SetTimer {
                subchain,
                kind: E2eTimerKind::WaitingBlocks,
                duration_ms: SHORT_POLL_CHAIN_BLOCKS_WAIT_FOR_MS,
            });
            return;
        }
        self.subchains[sc].short_polling = true;
        self.actions.push(E2eAction::RequestSubchainBlocks {
            subchain,
            offset: self.subchains[sc].height,
            limit: SHORT_POLL_CHAIN_BLOCKS_PER_REQUEST,
        });
    }

    fn update_emoji_hash(&mut self, next: Option<[u8; 32]>) {
        if self.emoji_hash != next {
            self.emoji_hash = next;
            self.actions.push(E2eAction::EmojiHashChanged(next));
        }
    }

    fn flush_outbound_broadcasts(&mut self) {
        for msg in self.blockchain.pull_outbound_broadcasts() {
            self.actions.push(E2eAction::SendOutboundBlock(msg));
        }
    }

    fn fail(&mut self, reason: CallFailure) {
        self.encrypt_decrypt.clear();
        self.update_emoji_hash(None);
        self.failure = Some(reason);
        self.actions.push(E2eAction::Failed(reason));
    }
}

fn validate_subchain(subchain: i32) -> Result<usize, E2eError> {
    if !(0..SUBCHAINS_COUNT as i32).contains(&subchain) {
        return Err(E2eError::InvalidSubchain(subchain));
    }
    Ok(subchain as usize)
}

// ---------------------------------------------------------------------------
// Layer 233 `phone.*` Conference RPC Builders (`calls_group_call.cpp`)
// ---------------------------------------------------------------------------

/// Build `phone.createConferenceCall#7d0444bb` (creating and immediately joining a conference call).
///
/// Port of `GroupCall::startConference` (`calls_group_call.cpp:1732-1753`).
#[must_use]
pub fn build_create_conference_call(
    random_id: i32,
    muted: bool,
    video_stopped: bool,
    public_key: PublicKey,
    zero_block: Vec<u8>,
    params_json: String,
) -> tl_fns::phone::CreateConferenceCall {
    tl_fns::phone::CreateConferenceCall {
        muted,
        video_stopped,
        join: true,
        random_id,
        public_key: Some(public_key.to_tl_int256()),
        block: Some(zero_block),
        params: Some(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
            data: params_json,
        })),
    }
}

/// Build `phone.joinGroupCall#8fb53057` with conference E2E `public_key` and `block`.
///
/// Port of `GroupCall::sendJoinRequest` (`calls_group_call.cpp:1660-1675`).
#[allow(clippy::too_many_arguments)]
pub fn build_join_conference_call(
    call: InputGroupCall,
    join_as: tl_enums::InputPeer,
    muted: bool,
    video_stopped: bool,
    invite_hash: Option<String>,
    public_key: PublicKey,
    join_block: Vec<u8>,
    params_json: String,
) -> Result<tl_fns::phone::JoinGroupCall, TlConversionError> {
    Ok(tl_fns::phone::JoinGroupCall {
        muted,
        video_stopped,
        call: call.try_into()?,
        join_as,
        invite_hash,
        public_key: Some(public_key.to_tl_int256()),
        block: Some(join_block),
        params: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: params_json }),
    })
}

/// Build `phone.deleteConferenceCallParticipants#8ca60525`.
///
/// Port of `GroupCall::removeConferenceParticipants` (`calls_group_call.cpp:840-846`).
pub fn build_delete_conference_participants(
    call: InputGroupCall,
    user_ids: &[UserId],
    remove_block: Vec<u8>,
    removing_stale: bool,
) -> Result<tl_fns::phone::DeleteConferenceCallParticipants, TlConversionError> {
    Ok(tl_fns::phone::DeleteConferenceCallParticipants {
        only_left: removing_stale,
        kick: !removing_stale,
        call: call.try_into()?,
        ids: user_ids.iter().map(|u| u.to_tl()).collect(),
        block: remove_block,
    })
}

/// Build `phone.sendConferenceCallBroadcast#c6701900`.
///
/// Port of `GroupCall::sendOutboundBlock` (`calls_group_call.cpp:1923-1926`).
pub fn build_send_conference_broadcast(
    call: InputGroupCall,
    block: Vec<u8>,
) -> Result<tl_fns::phone::SendConferenceCallBroadcast, TlConversionError> {
    Ok(tl_fns::phone::SendConferenceCallBroadcast {
        call: call.try_into()?,
        block,
    })
}

/// Build `phone.getGroupCallChainBlocks#ee9f88a6`.
///
/// Port of `GroupCall::requestSubchainBlocks` (`calls_group_call.cpp:1894-1899`).
pub fn build_get_chain_blocks(
    call: InputGroupCall,
    sub_chain_id: i32,
    offset: i32,
    limit: i32,
) -> Result<tl_fns::phone::GetGroupCallChainBlocks, TlConversionError> {
    Ok(tl_fns::phone::GetGroupCallChainBlocks {
        call: call.try_into()?,
        sub_chain_id,
        offset,
        limit,
    })
}

/// Build `phone.getGroupCallChainBlocks(sub_chain_id = 0, offset = -1, limit = 1)` used to
/// fetch the latest subchain-0 block before joining an existing conference
/// (`GroupCall::refreshLastBlockAndJoin`, `calls_group_call.cpp:1696-1701`).
pub fn build_get_last_chain_block(
    call: InputGroupCall,
) -> Result<tl_fns::phone::GetGroupCallChainBlocks, TlConversionError> {
    build_get_chain_blocks(call, SUBCHAIN_STATE, -1, 1)
}

/// Build `phone.inviteConferenceCallParticipant#bcf22685`.
///
/// Port of `GroupCall::inviteToConference` (`calls_group_call.cpp:4054-4058`).
pub fn build_invite_conference_participant(
    call: InputGroupCall,
    user_id: tl_enums::InputUser,
    video: bool,
) -> Result<tl_fns::phone::InviteConferenceCallParticipant, TlConversionError> {
    Ok(tl_fns::phone::InviteConferenceCallParticipant {
        video,
        call: call.try_into()?,
        user_id,
    })
}

/// Build `phone.declineConferenceCallInvite#3c479971`.
pub fn build_decline_conference_invite(
    msg_id: MsgId,
) -> Result<tl_fns::phone::DeclineConferenceCallInvite, TlConversionError> {
    let raw = msg_id
        .as_i32()
        .ok_or(TlConversionError::MsgIdOutOfRange(msg_id.0))?;
    Ok(tl_fns::phone::DeclineConferenceCallInvite { msg_id: raw })
}

/// Helper matching `GroupCall::joinFail` (`calls_group_call.cpp:1865`): returns `true` when
/// the server rejected our join block because the chain advanced and we must refresh block 0.
#[must_use]
pub fn is_chain_invalid_error(error_type: &str) -> bool {
    error_type.starts_with("CONF_WRITE_CHAIN_INVALID")
}

/// Helper matching `GroupCall::sendOutboundBlock` (`calls_group_call.cpp:1935-1936`):
/// returns `true` when a broadcast error is permanent and should not be retried.
#[must_use]
pub fn is_permanent_broadcast_error(error_type: &str) -> bool {
    error_type == "BLOCK_INVALID" || is_chain_invalid_error(error_type)
}
