//! Error types for `tg-crypto`.

use thiserror::Error;

/// Errors returned by cryptographic operations in `tg-crypto`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// Input data length is not a multiple of the AES block size (16 bytes).
    #[error("invalid block length {len}: must be a multiple of 16")]
    InvalidBlockLength {
        /// The invalid byte length.
        len: usize,
    },

    /// Input buffer or key length does not match expected constraints.
    #[error("invalid length: {0}")]
    InvalidLength(&'static str),

    /// Encrypted packet does not match the expected `auth_key_id`.
    #[error("auth_key_id mismatch")]
    AuthKeyIdMismatch,

    /// Decrypted plaintext failed `msg_key` / integrity verification.
    #[error("msg_key verification failed")]
    MsgKeyMismatch,

    /// Decrypted MTProto message header or padding failed validation.
    #[error("invalid MTProto message framing: {0}")]
    InvalidMessageFraming(&'static str),

    /// PEM or ASN.1 DER RSA public key parsing failed.
    #[error("invalid RSA public key: {0}")]
    InvalidRsaKey(&'static str),

    /// RSA input data is invalid (e.g. too long or >= modulus).
    #[error("RSA operation failed: {0}")]
    RsaFailed(&'static str),

    /// PQ factorization failed to find non-trivial factors.
    #[error("failed to factorize pq")]
    FactorizationFailed,

    /// Diffie-Hellman parameters (`p`, `g`, `g_a`, `g_b`) failed safety validation.
    #[error("invalid DH parameters: {0}")]
    InvalidDhParams(&'static str),

    /// SRP 2FA check computation failed due to invalid server parameters.
    #[error("SRP computation failed: {0}")]
    SrpFailed(&'static str),

    /// Local storage decryption or passcode KDF failed.
    #[error("local storage crypto failed: {0}")]
    LocalStorageFailed(&'static str),

    /// Passport secret validation or decryption failed.
    #[error("passport secret crypto failed: {0}")]
    PassportFailed(&'static str),

    /// Conference / `tde2e` Ed25519, X25519, or envelope crypto failed.
    #[error("e2e crypto failed: {0}")]
    E2eFailed(&'static str),
}
