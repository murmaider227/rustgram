//! Curve25519 (Ed25519 + X25519) primitives and authenticated one-shot ECDH envelopes
//! for Telegram E2E conference calls (`tde2e`).
//!
//! Port of the cryptographic operations invoked by `tde2e/tde2e_api.cpp` through
//! `tde2e_api::key_*`, `encrypt_message_for_one`, and `decrypt_message_for_one`.
//!
//! # Key Representation
//! Each participant holds a single 32-byte Ed25519 seed (`E2eKeyPair`). Its public key is
//! the 32-byte compressed Edwards25519 verifying key (`int256` on the wire).
//! - **Signing / Verification**: standard RFC 8032 Ed25519 (`ed25519-dalek`), producing
//!   64-byte (`int512`) signatures.
//! - **Key Agreement (ECDH)**: converts the Ed25519 signing scalar and peer Edwards public
//!   key to Curve25519 Montgomery coordinates (`SigningKey::to_scalar_bytes` +
//!   `VerifyingKey::to_montgomery`) and runs X25519 Diffie-Hellman (`x25519-dalek`),
//!   rejecting low-order / all-zero shared secrets.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::aes::{CtrState, aes_ctr_encrypt};
use crate::error::CryptoError;
use crate::hash::{const_time_eq, hmac_sha256_parts, sha256};
use crate::random::random_array;

/// Size of an Ed25519 / X25519 public key or seed in bytes (`int256`).
pub const E2E_KEY_SIZE: usize = 32;

/// Size of an Ed25519 signature in bytes (`int512`).
pub const E2E_SIGNATURE_SIZE: usize = 64;

/// Size of the random nonce prepended to one-shot ECDH envelopes.
pub const E2E_ENVELOPE_NONCE_SIZE: usize = 16;

/// Size of the truncated HMAC-SHA256 authentication tag appended to one-shot envelopes.
pub const E2E_ENVELOPE_TAG_SIZE: usize = 16;

const ENVELOPE_DOMAIN: &[u8] = b"tde2e_for_one_v1";

/// Temporary or session Curve25519 key pair supporting both Ed25519 signing and X25519 ECDH.
///
/// Mirrors the private key handle behind `TdE2E::TemporaryKeyPair` and `TdE2E::Call::_myKeyId`
/// in `tde2e/tde2e_api.cpp`.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct E2eKeyPair {
    seed: [u8; E2E_KEY_SIZE],
}

impl std::fmt::Debug for E2eKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2eKeyPair")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

impl E2eKeyPair {
    /// Generate a fresh random key pair from the OS CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            seed: random_array::<E2E_KEY_SIZE>(),
        }
    }

    /// Construct a deterministic key pair from a 32-byte Ed25519 seed.
    #[must_use]
    pub const fn from_seed(seed: [u8; E2E_KEY_SIZE]) -> Self {
        Self { seed }
    }

    /// Access the raw 32-byte Ed25519 seed.
    #[must_use]
    pub const fn seed(&self) -> &[u8; E2E_KEY_SIZE] {
        &self.seed
    }

    /// Export the 32-byte compressed Ed25519 public key (`int256`).
    #[must_use]
    pub fn public_key(&self) -> [u8; E2E_KEY_SIZE] {
        let signing = SigningKey::from_bytes(&self.seed);
        signing.verifying_key().to_bytes()
    }

    /// Export the 32-byte X25519 (Montgomery u-coordinate) public key corresponding to this key pair.
    #[must_use]
    pub fn x25519_public_key(&self) -> [u8; E2E_KEY_SIZE] {
        let signing = SigningKey::from_bytes(&self.seed);
        signing.verifying_key().to_montgomery().to_bytes()
    }

    /// Sign `message` with Ed25519 (RFC 8032), returning a 64-byte (`int512`) signature.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> [u8; E2E_SIGNATURE_SIZE] {
        let signing = SigningKey::from_bytes(&self.seed);
        signing.sign(message).to_bytes()
    }

    /// Compute a 32-byte X25519 shared secret against a peer's 32-byte Ed25519 public key.
    ///
    /// Rejects invalid curve points and all-zero (low-order) shared secrets.
    pub fn ecdh(&self, peer_ed25519_public: &[u8; E2E_KEY_SIZE]) -> Result<[u8; 32], CryptoError> {
        let peer_vk = VerifyingKey::from_bytes(peer_ed25519_public)
            .map_err(|_| CryptoError::E2eFailed("invalid peer Ed25519 public key"))?;
        let peer_montgomery = peer_vk.to_montgomery().to_bytes();
        self.ecdh_x25519(&peer_montgomery)
    }

    /// Compute a 32-byte X25519 shared secret against a peer's raw 32-byte Montgomery public key.
    ///
    /// Rejects all-zero (low-order) shared secrets.
    pub fn ecdh_x25519(
        &self,
        peer_x25519_public: &[u8; E2E_KEY_SIZE],
    ) -> Result<[u8; 32], CryptoError> {
        let signing = SigningKey::from_bytes(&self.seed);
        let mut scalar_bytes = signing.to_scalar_bytes();
        let secret = x25519_dalek::StaticSecret::from(scalar_bytes);
        scalar_bytes.zeroize();
        let peer_pk = x25519_dalek::PublicKey::from(*peer_x25519_public);
        let shared = secret.diffie_hellman(&peer_pk);
        let bytes = shared.to_bytes();
        if bytes.iter().all(|&b| b == 0) {
            return Err(CryptoError::E2eFailed(
                "low-order X25519 point produced all-zero shared secret",
            ));
        }
        Ok(bytes)
    }

    /// Seal `plaintext` to `peer_ed25519_public` in an authenticated one-shot ECDH envelope.
    ///
    /// Port of `TdE2E::TemporaryKeyPair::encryptForOne` (`tde2e/tde2e_api.cpp:200-229`).
    /// Wire layout: `nonce (16 B) || ciphertext (N B) || hmac_sha256_tag[..16] (16 B)`.
    pub fn encrypt_for_one(
        &self,
        peer_ed25519_public: &[u8; E2E_KEY_SIZE],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let mut shared = self.ecdh(peer_ed25519_public)?;
        let nonce = random_array::<E2E_ENVELOPE_NONCE_SIZE>();
        let out = seal_with_shared_secret(&shared, &nonce, plaintext);
        shared.zeroize();
        Ok(out)
    }

    /// Open a one-shot ECDH envelope from `peer_ed25519_public`.
    ///
    /// Port of `TdE2E::TemporaryKeyPair::decryptForOne` (`tde2e/tde2e_api.cpp:169-198`).
    pub fn decrypt_for_one(
        &self,
        peer_ed25519_public: &[u8; E2E_KEY_SIZE],
        envelope: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let mut shared = self.ecdh(peer_ed25519_public)?;
        let res = open_with_shared_secret(&shared, envelope);
        shared.zeroize();
        res
    }
}

/// Verify an Ed25519 signature over `message` under `public_key`.
pub fn verify_ed25519(
    public_key: &[u8; E2E_KEY_SIZE],
    message: &[u8],
    signature: &[u8; E2E_SIGNATURE_SIZE],
) -> Result<(), CryptoError> {
    let vk = VerifyingKey::from_bytes(public_key)
        .map_err(|_| CryptoError::E2eFailed("invalid Ed25519 public key"))?;
    let sig = Signature::from_bytes(signature);
    vk.verify(message, &sig)
        .map_err(|_| CryptoError::E2eFailed("Ed25519 signature verification failed"))
}

/// Validate that `bytes` represents a valid Ed25519 compressed Edwards point.
pub fn validate_ed25519_public_key(bytes: &[u8; E2E_KEY_SIZE]) -> Result<(), CryptoError> {
    VerifyingKey::from_bytes(bytes)
        .map(|_| ())
        .map_err(|_| CryptoError::E2eFailed("invalid Ed25519 public key"))
}

/// Authenticated symmetric envelope using AES-256-CTR + truncated HMAC-SHA256
/// derived from a 32-byte shared secret and a 16-byte nonce.
pub fn seal_with_shared_secret(
    shared_secret: &[u8; 32],
    nonce: &[u8; E2E_ENVELOPE_NONCE_SIZE],
    plaintext: &[u8],
) -> Vec<u8> {
    let mut enc_key = sha256(&[ENVELOPE_DOMAIN, b"enc", shared_secret, nonce]);
    let mut mac_key = sha256(&[ENVELOPE_DOMAIN, b"mac", shared_secret, nonce]);

    let mut ciphertext = plaintext.to_vec();
    let mut ctr = CtrState::new(enc_key, *nonce);
    aes_ctr_encrypt(&mut ciphertext, &mut ctr);

    let full_tag = hmac_sha256_parts(&mac_key, &[ENVELOPE_DOMAIN, nonce, &ciphertext]);
    enc_key.zeroize();
    mac_key.zeroize();

    let mut out =
        Vec::with_capacity(E2E_ENVELOPE_NONCE_SIZE + ciphertext.len() + E2E_ENVELOPE_TAG_SIZE);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ciphertext);
    out.extend_from_slice(&full_tag[..E2E_ENVELOPE_TAG_SIZE]);
    out
}

/// Verify and decrypt an authenticated symmetric envelope created by [`seal_with_shared_secret`].
pub fn open_with_shared_secret(
    shared_secret: &[u8; 32],
    envelope: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let min_len = E2E_ENVELOPE_NONCE_SIZE + E2E_ENVELOPE_TAG_SIZE;
    if envelope.len() < min_len {
        return Err(CryptoError::E2eFailed("encrypted envelope too short"));
    }
    let nonce: [u8; E2E_ENVELOPE_NONCE_SIZE] = envelope[..E2E_ENVELOPE_NONCE_SIZE]
        .try_into()
        .map_err(|_| CryptoError::E2eFailed("invalid envelope nonce"))?;
    let ct_end = envelope.len() - E2E_ENVELOPE_TAG_SIZE;
    let ciphertext = &envelope[E2E_ENVELOPE_NONCE_SIZE..ct_end];
    let tag = &envelope[ct_end..];

    let mut enc_key = sha256(&[ENVELOPE_DOMAIN, b"enc", shared_secret, &nonce]);
    let mut mac_key = sha256(&[ENVELOPE_DOMAIN, b"mac", shared_secret, &nonce]);

    let expected_tag = hmac_sha256_parts(&mac_key, &[ENVELOPE_DOMAIN, &nonce, ciphertext]);
    mac_key.zeroize();
    if !const_time_eq(tag, &expected_tag[..E2E_ENVELOPE_TAG_SIZE]) {
        enc_key.zeroize();
        return Err(CryptoError::E2eFailed(
            "envelope authentication tag mismatch",
        ));
    }

    let mut plaintext = ciphertext.to_vec();
    let mut ctr = CtrState::new(enc_key, nonce);
    aes_ctr_encrypt(&mut plaintext, &mut ctr);
    enc_key.zeroize();
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ed25519_sign_verify_and_tamper_rejection() {
        let kp = E2eKeyPair::from_seed([0x11; 32]);
        let pk = kp.public_key();
        let msg = b"conference chain block payload";
        let sig = kp.sign(msg);

        assert!(verify_ed25519(&pk, msg, &sig).is_ok());

        let mut bad_msg = msg.to_vec();
        bad_msg[0] ^= 0x01;
        assert!(verify_ed25519(&pk, &bad_msg, &sig).is_err());

        let mut bad_sig = sig;
        bad_sig[63] ^= 0x80;
        assert!(verify_ed25519(&pk, msg, &bad_sig).is_err());
    }

    #[test]
    fn x25519_ecdh_and_for_one_envelope_roundtrip() {
        let alice = E2eKeyPair::from_seed([0x22; 32]);
        let bob = E2eKeyPair::from_seed([0x33; 32]);

        let s_ab = alice.ecdh(&bob.public_key()).expect("alice ecdh");
        let s_ba = bob.ecdh(&alice.public_key()).expect("bob ecdh");
        assert_eq!(s_ab, s_ba);
        assert!(s_ab.iter().any(|&b| b != 0));

        let plain = b"secret conference key material";
        let sealed = alice
            .encrypt_for_one(&bob.public_key(), plain)
            .expect("encrypt_for_one");
        let opened = bob
            .decrypt_for_one(&alice.public_key(), &sealed)
            .expect("decrypt_for_one");
        assert_eq!(opened, plain);

        // Tampering ciphertext must fail authentication.
        let mut tampered = sealed.clone();
        tampered[E2E_ENVELOPE_NONCE_SIZE] ^= 0xFF;
        assert!(bob.decrypt_for_one(&alice.public_key(), &tampered).is_err());

        // Wrong peer key must fail authentication.
        let charlie = E2eKeyPair::from_seed([0x44; 32]);
        assert!(
            charlie
                .decrypt_for_one(&alice.public_key(), &sealed)
                .is_err()
        );
    }

    #[test]
    fn low_order_x25519_point_rejected() {
        let alice = E2eKeyPair::from_seed([0x55; 32]);
        assert!(alice.ecdh_x25519(&[0u8; 32]).is_err());
    }
}
