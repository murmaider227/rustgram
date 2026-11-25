//! Rustgram self-updater library and `rustgram-updater` helper.
//!
//! Port of `Telegram/SourceFiles/core/update_checker.cpp`,
//! `Telegram/SourceFiles/core/update_verify.cpp`,
//! `Telegram/SourceFiles/core/update_unpack.cpp`,
//! `Telegram/SourceFiles/_other/updater_win.cpp`, and
//! `Telegram/SourceFiles/_other/packer.cpp` in a simplified, Ed25519-authenticated form.

pub mod apply;
pub mod checker;
pub mod github;
pub mod package;
pub mod version;

use ed25519_dalek::VerifyingKey;
use thiserror::Error;

/// Default GitHub repository slug (`owner/repo`) queried for releases.
pub const DEFAULT_GITHUB_REPO: &str = "murmaider227/rustgram";

/// Default main application executable name on Windows.
pub const DEFAULT_EXE_NAME: &str = "rustgram.exe";

/// Standalone updater executable name on Windows.
pub const UPDATER_EXE_NAME: &str = "rustgram-updater.exe";

/// Staging directory created inside the working directory, matching tdesktop.
pub const TUPDATES_DIR_NAME: &str = "tupdates";

/// Unique Inno Setup AppId GUID shared with `packaging/windows/tgfork.iss`.
pub const APP_ID_GUID: &str = "{46C2F7F5-7D28-455F-8E02-5102E303855E}";

/// Build-time environment variable configuring [`RELEASE_SIGNING_PUBLIC_KEY`] (64-char hex Ed25519).
///
/// Produce a keypair with `cargo run -p tg-xtask -- keygen`:
/// - Export `RUSTGRAM_UPDATE_PUBKEY=<64-char public key hex>` when building release binaries.
/// - Keep the private key seed out of version control (store only in CI secret
///   `RUSTGRAM_UPDATER_SIGNING_KEY`).
pub const UPDATE_PUBKEY_ENV_VAR: &str = "RUSTGRAM_UPDATE_PUBKEY";

include!(concat!(env!("OUT_DIR"), "/release_pubkey.rs"));

/// Returns `true` if `key` is the unconfigured all-zero placeholder key.
#[must_use]
pub fn is_placeholder_public_key(key: &[u8; 32]) -> bool {
    key.iter().all(|&b| b == 0)
}

/// Returns `true` when a non-placeholder Ed25519 release verifying key was compiled in via
/// [`UPDATE_PUBKEY_ENV_VAR`].
#[must_use]
pub fn has_configured_release_key() -> bool {
    RELEASE_SIGNING_PUBLIC_KEY_CONFIGURED && !is_placeholder_public_key(&RELEASE_SIGNING_PUBLIC_KEY)
}

/// Loads the compiled-in [`RELEASE_SIGNING_PUBLIC_KEY`] as a [`VerifyingKey`].
///
/// Fails closed with [`UpdateError::UnconfiguredPublicKey`] while the placeholder
/// all-zero key is still set.
pub fn release_verifying_key() -> Result<VerifyingKey, UpdateError> {
    verifying_key_from_bytes(&RELEASE_SIGNING_PUBLIC_KEY)
}

/// Decodes a 64-character hex string into 32 raw key bytes without rejecting the all-zero
/// placeholder (use [`verifying_key_from_hex`] to enforce fail-closed validation).
pub fn decode_public_key_hex(hex_str: &str) -> Result<[u8; 32], UpdateError> {
    let decoded = hex::decode(hex_str.trim())
        .map_err(|e| UpdateError::InvalidPublicKey(format!("invalid hex: {e}")))?;
    decoded
        .try_into()
        .map_err(|_| UpdateError::InvalidPublicKey("expected 32 bytes (64 hex chars)".into()))
}

/// Converts a raw 32-byte Ed25519 public key into a [`VerifyingKey`], failing closed on
/// the all-zero placeholder.
pub fn verifying_key_from_bytes(bytes: &[u8; 32]) -> Result<VerifyingKey, UpdateError> {
    if is_placeholder_public_key(bytes) {
        return Err(UpdateError::UnconfiguredPublicKey);
    }
    VerifyingKey::from_bytes(bytes).map_err(|e| UpdateError::InvalidPublicKey(e.to_string()))
}

/// Decodes a 64-character hex string into an Ed25519 [`VerifyingKey`], failing closed on
/// the all-zero placeholder.
pub fn verifying_key_from_hex(hex_str: &str) -> Result<VerifyingKey, UpdateError> {
    let bytes = decode_public_key_hex(hex_str)?;
    verifying_key_from_bytes(&bytes)
}

/// Errors produced across release checking, verification, unpacking, and applying.
#[derive(Debug, Error)]
pub enum UpdateError {
    /// Compiled-in release public key has not been configured by the maintainer yet.
    #[error("release verifying key is not configured (placeholder all-zero key)")]
    UnconfiguredPublicKey,

    /// Public key bytes or hex encoding could not be decoded.
    #[error("invalid Ed25519 public key: {0}")]
    InvalidPublicKey(String),

    /// Version string failed SemVer parsing.
    #[error("invalid version string '{0}'")]
    InvalidVersion(String),

    /// Network transport error reported by [`github::HttpClient`].
    #[error("network request failed: {0}")]
    Network(String),

    /// HTTP response exceeded the configured byte ceiling.
    #[error("response exceeded maximum allowed size of {limit} bytes")]
    ResponseTooLarge {
        /// Maximum permitted size in bytes.
        limit: usize,
    },

    /// GitHub Releases JSON could not be deserialized.
    #[error("invalid release metadata JSON: {0}")]
    InvalidReleaseJson(String),

    /// Envelope header or binary layout is malformed.
    #[error("malformed update package: {0}")]
    MalformedPackage(String),

    /// Ed25519 cryptographic signature verification failed.
    #[error("update signature verification failed")]
    InvalidSignature,

    /// SHA-256 hash verification failed against the expected digest.
    #[error("update SHA-256 mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        /// Expected hex-encoded SHA-256 digest.
        expected: String,
        /// Actual hex-encoded SHA-256 digest of the downloaded bytes.
        actual: String,
    },

    /// Applying staged files failed midway; indicates whether prior files were rolled back.
    #[error("failed to apply update (rolled_back={rolled_back}): {reason}")]
    ApplyFailed {
        /// Underlying failure details.
        reason: String,
        /// `true` if previously overwritten files and newly created files were restored/cleaned up.
        rolled_back: bool,
    },

    /// Package target OS/architecture does not match this installation.
    #[error("package target {actual} does not match expected {expected}")]
    TargetMismatch {
        /// Expected platform target.
        expected: String,
        /// Target declared in the signed envelope.
        actual: String,
    },

    /// Package channel is not allowed by the client's beta preference.
    #[error("beta update rejected on stable channel")]
    ChannelNotAllowed,

    /// Package version is not strictly newer than the running version.
    #[error("anti-downgrade: package version {package} is not newer than {current}")]
    NotNewer {
        /// Currently installed version.
        current: String,
        /// Version declared inside the package.
        package: String,
    },

    /// Archive entry path failed safety checks (traversal, Windows device names, collisions).
    #[error("unsafe or conflicting archive path: {0}")]
    UnsafePath(String),

    /// Archive exceeded uncompressed size or file-count limits.
    #[error("archive exceeds safety limits: {0}")]
    ArchiveLimitExceeded(String),

    /// ZIP decompression or encoding error.
    #[error("archive error: {0}")]
    Archive(String),

    /// Staged update illegally contained the updater executable itself.
    #[error("refusing update that attempts to overwrite updater binary '{0}'")]
    SelfOverwriteBlocked(String),

    /// Waiting for or terminating the parent application process failed.
    #[error("failed waiting for parent process {pid}: {reason}")]
    ProcessWait {
        /// Parent process ID.
        pid: u32,
        /// Platform error details.
        reason: String,
    },

    /// Relaunching the updated application failed.
    #[error("failed to relaunch application: {0}")]
    RelaunchFailed(String),

    /// Filesystem I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
