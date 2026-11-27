//! `tg-passport`: Pure-logic Telegram Passport authorization, encryption, and validation.
//!
//! Rust port of `Telegram/SourceFiles/passport/`:
//! - [`encryption`]: AES-256-CBC data/file/secret encryption, padding, value hashes, and RSA-OAEP
//!   credentials encryption (`passport_encryption.cpp`).
//! - [`types`]: Passport value types, file metadata, scopes, form model, and TL conversions
//!   (`passport_form_controller.h/.cpp`, `passport_form_view_controller.h`).
//! - [`validation`]: Identity, address, and contact field validation rules, scope resolution,
//!   and form validation (`passport_panel_controller.cpp`, `passport_form_view_controller.cpp`).
//! - [`controller`]: Authorization form state machine, SRP/secret password flow, value saving,
//!   verification, and `account.acceptAuthorization` payload builder (`passport_form_controller.cpp`).

pub mod controller;
pub mod encryption;
pub mod types;
pub mod validation;

use thiserror::Error;

/// Errors returned by Telegram Passport cryptography, validation, and form operations.
#[derive(Debug, Error)]
pub enum PassportError {
    /// Underlying cryptographic error from `tg-crypto`.
    #[error("crypto error: {0}")]
    Crypto(#[from] tg_crypto::CryptoError),

    /// Ciphertext buffer was empty.
    #[error("ciphertext is empty")]
    EmptyCiphertext,

    /// Ciphertext or padded plaintext length was not a multiple of 16 bytes.
    #[error("length {0} is not 16-byte aligned")]
    InvalidAlignment(usize),

    /// Padding prefix length was outside `32..=255` or exceeded decrypted buffer length.
    #[error("invalid passport padding length: {0}")]
    InvalidPadding(usize),

    /// Hash length was not 32 bytes.
    #[error("invalid SHA-256 hash length: {0} (expected 32)")]
    InvalidHashLength(usize),

    /// Secret length was not 32 bytes.
    #[error("invalid secret length: {0} (expected 32)")]
    InvalidSecretLength(usize),

    /// Secret bytes failed Telegram Passport's `sum(bytes) % 255 == 239` checksum.
    #[error("passport secret failed mod-255 checksum")]
    InvalidSecretChecksum,

    /// Decrypted plaintext SHA-256 did not match `data_hash`.
    #[error("decrypted data SHA-256 mismatch")]
    HashMismatch,

    /// Unlocked master secret ID did not match server's `secure_secret_id`.
    #[error("secure secret ID mismatch: expected {expected}, computed {actual}")]
    SecretIdMismatch {
        /// Server-reported `secure_secret_id`.
        expected: u64,
        /// Locally computed `CountSecureSecretId(secret)`.
        actual: u64,
    },

    /// JSON serialization or deserialization error.
    #[error("json error: {0}")]
    Json(String),

    /// Authorization form violated protocol invariants.
    #[error("invalid authorization form: {0}")]
    InvalidForm(String),

    /// Operation requires an unlocked master secret first.
    #[error("passport master secret is not unlocked")]
    SecretNotReady,

    /// Operation requires a non-empty nonce / payload.
    #[error("missing authorization nonce/payload: {0}")]
    MissingNonce(&'static str),

    /// Server returned an unsupported KDF algorithm requiring an app update.
    #[error("unsupported KDF algorithm (app update required)")]
    UnsupportedKdfAlgo,

    /// Form submission failed because one or more scopes are incomplete or have errors.
    #[error("incomplete or invalid passport scopes: {0:?}")]
    IncompleteScopes(Vec<types::ValueType>),

    /// Invalid state transition or precondition in [`controller::PassportController`].
    #[error("invalid state: {0}")]
    InvalidState(String),
}

#[cfg(test)]
mod fuzz_untrusted;
