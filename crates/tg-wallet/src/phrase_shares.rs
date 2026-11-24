//! Recovery-phrase secret sharing: GF(256) Shamir `(k, n)` threshold splitting and
//! reconstruction with per-share and inner-secret SHA-256 checksums, plus tdesktop's
//! `Wallet::PhraseShares` 215-byte space-padded seed encoding, XOR split/combine,
//! TL `mnemonic.decryptedKeyPart` codec, and holder NaCl-box share sealing.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_phrase_shares.h`,
//! `Telegram/SourceFiles/wallet/wallet_phrase_shares.cpp`.

use std::collections::HashSet;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use crypto_box::aead::Aead;
use crypto_box::{PublicKey as BoxPublicKey, SalsaBox, SecretKey as BoxSecretKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::WalletError;
use crate::mnemonic::{MNEMONIC_WORD_COUNT, is_wordlist_word};

/// X25519 public key length in bytes (`Wallet::PhraseShares::kPublicKeySize`).
pub const PHRASE_SHARE_PUBLIC_KEY_SIZE: usize = 32;

/// Fixed space-padded recovery-phrase seed length in bytes (`Wallet::PhraseShares::kSeedLength`).
pub const PHRASE_SEED_LENGTH: usize = 215;

/// TL constructor ID for `mnemonic.decryptedKeyPart` (`Wallet::PhraseShares::kDecryptedKeyPartId`).
pub const DECRYPTED_KEY_PART_TL_ID: u32 = 0x8B90_DD08;

/// Format version tag for [`PhraseShare`] Shamir shares.
pub const PHRASE_SHARE_VERSION: u8 = 1;

/// Human-readable string prefix for serialized [`PhraseShare`] tokens.
pub const PHRASE_SHARE_PREFIX: &str = "tgshare1:";

const SHARE_CHECKSUM_DOMAIN: &[u8] = b"tg-wallet/shamir-share/v1";
const SECRET_CHECKSUM_DOMAIN: &[u8] = b"tg-wallet/shamir-secret/v1";
const INNER_CHECKSUM_LEN: usize = 4;
const HOLDER_NONCE_LEN: usize = 24;

// ============================================================================
// GF(256) finite-field arithmetic (AES/Rijndael polynomial x^8 + x^4 + x^3 + x + 1)
// ============================================================================

/// Addition / subtraction in GF(2^8).
#[inline]
#[must_use]
pub const fn gf256_add(a: u8, b: u8) -> u8 {
    a ^ b
}

/// Multiplication in GF(2^8) modulo `x^8 + x^4 + x^3 + x + 1` (`0x11B`).
#[must_use]
pub const fn gf256_mul(mut a: u8, mut b: u8) -> u8 {
    let mut acc = 0_u8;
    let mut i = 0;
    while i < 8 {
        let mask = 0_u8.wrapping_sub(b & 1);
        acc ^= a & mask;
        let hi = a & 0x80;
        a <<= 1;
        let reduce = 0_u8.wrapping_sub(hi >> 7) & 0x1B;
        a ^= reduce;
        b >>= 1;
        i += 1;
    }
    acc
}

/// Multiplicative inverse in GF(2^8) via Fermat's little theorem (`a^254`).
///
/// Returns `None` for `a == 0`.
#[must_use]
pub const fn gf256_inv(a: u8) -> Option<u8> {
    if a == 0 {
        return None;
    }
    // 254 = 0b1111_1110 = 2 + 4 + 8 + 16 + 32 + 64 + 128
    let a2 = gf256_mul(a, a);
    let a4 = gf256_mul(a2, a2);
    let a8 = gf256_mul(a4, a4);
    let a16 = gf256_mul(a8, a8);
    let a32 = gf256_mul(a16, a16);
    let a64 = gf256_mul(a32, a32);
    let a128 = gf256_mul(a64, a64);
    let mut res = gf256_mul(a2, a4);
    res = gf256_mul(res, a8);
    res = gf256_mul(res, a16);
    res = gf256_mul(res, a32);
    res = gf256_mul(res, a64);
    res = gf256_mul(res, a128);
    Some(res)
}

/// Division `a / b` in GF(2^8). Returns `None` if `b == 0`.
#[must_use]
pub const fn gf256_div(a: u8, b: u8) -> Option<u8> {
    match gf256_inv(b) {
        Some(inv) => Some(gf256_mul(a, inv)),
        None => None,
    }
}

fn eval_polynomial_gf256(coeffs: &[u8], x: u8) -> u8 {
    // Horner's method from highest-degree coefficient down to constant term `coeffs[0]`.
    let mut acc = 0_u8;
    for &c in coeffs.iter().rev() {
        acc = gf256_add(gf256_mul(acc, x), c);
    }
    acc
}

fn lagrange_interpolate_at_zero(points: &[(u8, u8)]) -> Result<u8, WalletError> {
    let mut result = 0_u8;
    for (j, &(xj, yj)) in points.iter().enumerate() {
        let mut basis = 1_u8;
        for (m, &(xm, _)) in points.iter().enumerate() {
            if m == j {
                continue;
            }
            let denom = gf256_add(xm, xj);
            let factor = gf256_div(xm, denom)
                .ok_or_else(|| WalletError::Mnemonic("duplicate share index".to_owned()))?;
            basis = gf256_mul(basis, factor);
        }
        result = gf256_add(result, gf256_mul(yj, basis));
    }
    Ok(result)
}

fn lagrange_interpolate_at(points: &[(u8, u8)], target_x: u8) -> Result<u8, WalletError> {
    let mut result = 0_u8;
    for (j, &(xj, yj)) in points.iter().enumerate() {
        let mut basis = 1_u8;
        for (m, &(xm, _)) in points.iter().enumerate() {
            if m == j {
                continue;
            }
            let num = gf256_add(target_x, xm);
            let denom = gf256_add(xj, xm);
            let factor = gf256_div(num, denom)
                .ok_or_else(|| WalletError::Mnemonic("duplicate share index".to_owned()))?;
            basis = gf256_mul(basis, factor);
        }
        result = gf256_add(result, gf256_mul(yj, basis));
    }
    Ok(result)
}

// ============================================================================
// Shamir Phrase Shares with integrity checksums
// ============================================================================

/// A single Shamir `(k, n)` share over GF(256) with an integrity checksum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhraseShare {
    /// Share format version (`PHRASE_SHARE_VERSION`).
    pub version: u8,
    /// Non-zero evaluation x-coordinate (`1..=total_shares`).
    pub index: u8,
    /// Minimum number of shares required to reconstruct (`k >= 2`).
    pub threshold: u8,
    /// Total number of shares generated (`n >= k`).
    pub total_shares: u8,
    /// Share payload bytes (`secret.len() + 4` including the inner secret checksum).
    pub data: Vec<u8>,
    /// 4-byte SHA-256 checksum over header fields and `data`.
    pub checksum: [u8; 4],
}

impl PhraseShare {
    /// Computes the expected 4-byte SHA-256 share checksum for the given fields.
    #[must_use]
    pub fn compute_checksum(
        version: u8,
        index: u8,
        threshold: u8,
        total_shares: u8,
        data: &[u8],
    ) -> [u8; 4] {
        let mut hasher = Sha256::new();
        hasher.update(SHARE_CHECKSUM_DOMAIN);
        hasher.update([version, index, threshold, total_shares]);
        hasher.update((data.len() as u32).to_be_bytes());
        hasher.update(data);
        let digest = hasher.finalize();
        [digest[0], digest[1], digest[2], digest[3]]
    }

    /// Returns `true` if version, parameters, and 4-byte share checksum are valid.
    #[must_use]
    pub fn verify_checksum(&self) -> bool {
        if self.version != PHRASE_SHARE_VERSION
            || self.index == 0
            || self.threshold < 2
            || self.total_shares < self.threshold
            || self.index > self.total_shares
            || self.data.len() <= INNER_CHECKSUM_LEN
        {
            return false;
        }
        let expected = Self::compute_checksum(
            self.version,
            self.index,
            self.threshold,
            self.total_shares,
            &self.data,
        );
        self.checksum == expected
    }

    /// Serializes the share into compact binary framing:
    /// `version(1) || index(1) || threshold(1) || total_shares(1) || len_be(2) || data || checksum(4)`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, WalletError> {
        let len_u16 = u16::try_from(self.data.len())
            .map_err(|_| WalletError::Mnemonic("share payload exceeds 65535 bytes".to_owned()))?;
        let mut out = Vec::with_capacity(6 + self.data.len() + 4);
        out.push(self.version);
        out.push(self.index);
        out.push(self.threshold);
        out.push(self.total_shares);
        out.extend_from_slice(&len_u16.to_be_bytes());
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&self.checksum);
        Ok(out)
    }

    /// Deserializes a share from compact binary bytes and verifies its checksum.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WalletError> {
        if bytes.len() < 10 {
            return Err(WalletError::Mnemonic("share data too short".to_owned()));
        }
        let version = bytes[0];
        let index = bytes[1];
        let threshold = bytes[2];
        let total_shares = bytes[3];
        let data_len = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        if bytes.len() != 6 + data_len + 4 {
            return Err(WalletError::Mnemonic(format!(
                "share length mismatch: expected {}, got {}",
                6 + data_len + 4,
                bytes.len()
            )));
        }
        let data = bytes[6..6 + data_len].to_vec();
        let mut checksum = [0_u8; 4];
        checksum.copy_from_slice(&bytes[6 + data_len..]);
        let share = Self {
            version,
            index,
            threshold,
            total_shares,
            data,
            checksum,
        };
        if !share.verify_checksum() {
            return Err(WalletError::Mnemonic(
                "share checksum verification failed".to_owned(),
            ));
        }
        Ok(share)
    }

    /// Encodes the share as a portable `tgshare1:<base64url>` string.
    pub fn encode_string(&self) -> Result<String, WalletError> {
        let bytes = self.to_bytes()?;
        Ok(format!(
            "{PHRASE_SHARE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(bytes)
        ))
    }

    /// Decodes a `tgshare1:<base64url>` string and verifies its checksum.
    pub fn decode_string(encoded: &str) -> Result<Self, WalletError> {
        let body = encoded
            .trim()
            .strip_prefix(PHRASE_SHARE_PREFIX)
            .ok_or_else(|| WalletError::Mnemonic("missing tgshare1: prefix".to_owned()))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|e| WalletError::Mnemonic(format!("invalid share base64url: {e}")))?;
        Self::from_bytes(&bytes)
    }
}

fn secret_inner_checksum(secret: &[u8]) -> [u8; INNER_CHECKSUM_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(SECRET_CHECKSUM_DOMAIN);
    hasher.update((secret.len() as u32).to_be_bytes());
    hasher.update(secret);
    let digest = hasher.finalize();
    [digest[0], digest[1], digest[2], digest[3]]
}

/// Splits an arbitrary non-empty byte secret into `total_shares` GF(256) Shamir shares,
/// requiring at least `threshold` shares to reconstruct.
pub fn split_secret(
    secret: &[u8],
    threshold: u8,
    total_shares: u8,
) -> Result<Vec<PhraseShare>, WalletError> {
    let mut rng = OsRng;
    split_secret_with_rng(secret, threshold, total_shares, &mut rng)
}

/// Deterministic/injectable-RNG variant of [`split_secret`] for known-answer tests.
pub fn split_secret_with_rng<R: RngCore>(
    secret: &[u8],
    threshold: u8,
    total_shares: u8,
    rng: &mut R,
) -> Result<Vec<PhraseShare>, WalletError> {
    if secret.is_empty() {
        return Err(WalletError::Mnemonic(
            "cannot split empty secret".to_owned(),
        ));
    }
    if threshold < 2 {
        return Err(WalletError::Mnemonic(
            "Shamir threshold k must be at least 2".to_owned(),
        ));
    }
    if total_shares < threshold {
        return Err(WalletError::Mnemonic(format!(
            "total_shares ({total_shares}) must be >= threshold ({threshold})"
        )));
    }
    if secret.len() + INNER_CHECKSUM_LEN > usize::from(u16::MAX) {
        return Err(WalletError::Mnemonic(
            "secret too large for Shamir share framing".to_owned(),
        ));
    }

    let inner_tag = secret_inner_checksum(secret);
    let mut payload = Zeroizing::new(Vec::with_capacity(secret.len() + INNER_CHECKSUM_LEN));
    payload.extend_from_slice(secret);
    payload.extend_from_slice(&inner_tag);

    let k = usize::from(threshold);
    let n = usize::from(total_shares);
    let mut share_buffers: Vec<Vec<u8>> =
        (0..n).map(|_| Vec::with_capacity(payload.len())).collect();
    let mut coeffs = Zeroizing::new(vec![0_u8; k]);

    for &byte in payload.iter() {
        coeffs[0] = byte;
        rng.fill_bytes(&mut coeffs[1..]);
        for (idx, buf) in share_buffers.iter_mut().enumerate() {
            let x = (idx + 1) as u8;
            buf.push(eval_polynomial_gf256(&coeffs, x));
        }
    }

    let mut shares = Vec::with_capacity(n);
    for (idx, data) in share_buffers.into_iter().enumerate() {
        let index = (idx + 1) as u8;
        let checksum = PhraseShare::compute_checksum(
            PHRASE_SHARE_VERSION,
            index,
            threshold,
            total_shares,
            &data,
        );
        shares.push(PhraseShare {
            version: PHRASE_SHARE_VERSION,
            index,
            threshold,
            total_shares,
            data,
            checksum,
        });
    }
    Ok(shares)
}

/// Reconstructs a secret from `>= threshold` GF(256) Shamir shares, verifying per-share
/// checksums, parameter agreement, extra-share polynomial consistency, and the inner secret
/// checksum.
pub fn reconstruct_secret(shares: &[PhraseShare]) -> Result<Zeroizing<Vec<u8>>, WalletError> {
    if shares.is_empty() {
        return Err(WalletError::Mnemonic("no shares provided".to_owned()));
    }
    let first = &shares[0];
    let threshold = first.threshold;
    let total_shares = first.total_shares;
    let data_len = first.data.len();

    let mut seen_indices = HashSet::with_capacity(shares.len());
    for share in shares {
        if !share.verify_checksum() {
            return Err(WalletError::Mnemonic(format!(
                "corrupted checksum or metadata on share #{}",
                share.index
            )));
        }
        if share.threshold != threshold || share.total_shares != total_shares {
            return Err(WalletError::Mnemonic(
                "share threshold/total mismatch".to_owned(),
            ));
        }
        if share.data.len() != data_len {
            return Err(WalletError::Mnemonic("share length mismatch".to_owned()));
        }
        if !seen_indices.insert(share.index) {
            return Err(WalletError::Mnemonic(format!(
                "duplicate share index {}",
                share.index
            )));
        }
    }

    let k = usize::from(threshold);
    if shares.len() < k {
        return Err(WalletError::Mnemonic(format!(
            "insufficient shares: need at least {k}, got {}",
            shares.len()
        )));
    }

    let basis_shares = &shares[..k];
    let extra_shares = &shares[k..];
    let mut points = Vec::with_capacity(k);
    let mut recovered = Zeroizing::new(Vec::with_capacity(data_len));

    for byte_idx in 0..data_len {
        points.clear();
        for share in basis_shares {
            points.push((share.index, share.data[byte_idx]));
        }
        // Verify any redundant shares lie on the exact same degree-(k-1) polynomial.
        for extra in extra_shares {
            let expected_y = lagrange_interpolate_at(&points, extra.index)?;
            if expected_y != extra.data[byte_idx] {
                return Err(WalletError::Mnemonic(format!(
                    "inconsistent share #{} detected during reconstruction",
                    extra.index
                )));
            }
        }
        recovered.push(lagrange_interpolate_at_zero(&points)?);
    }

    let secret_len = data_len - INNER_CHECKSUM_LEN;
    let (secret_bytes, tag) = recovered.split_at(secret_len);
    let expected_tag = secret_inner_checksum(secret_bytes);
    if tag != expected_tag {
        return Err(WalletError::Mnemonic(
            "reconstructed secret failed integrity checksum (mismatched or corrupted shares)"
                .to_owned(),
        ));
    }

    Ok(Zeroizing::new(secret_bytes.to_vec()))
}

/// Splits a 24-word TON recovery phrase into `(threshold, total_shares)` GF(256) Shamir shares.
///
/// The phrase is encoded using tdesktop's fixed 215-byte space-padded seed representation
/// (`seed_from_words`) before splitting, so share lengths never leak individual word lengths.
pub fn split_phrase(
    words: &[String],
    threshold: u8,
    total_shares: u8,
) -> Result<Vec<PhraseShare>, WalletError> {
    validate_phrase_words(words)?;
    let seed = seed_from_words(words)?;
    split_secret(&seed, threshold, total_shares)
}

/// Reconstructs a 24-word TON recovery phrase from `>= threshold` [`PhraseShare`]s.
pub fn reconstruct_phrase(shares: &[PhraseShare]) -> Result<Vec<String>, WalletError> {
    let seed = reconstruct_secret(shares)?;
    let words = words_from_seed(&seed)?;
    validate_phrase_words(&words)?;
    Ok(words)
}

fn validate_phrase_words(words: &[String]) -> Result<(), WalletError> {
    if words.len() != MNEMONIC_WORD_COUNT {
        return Err(WalletError::Mnemonic(format!(
            "expected {MNEMONIC_WORD_COUNT} mnemonic words, got {}",
            words.len()
        )));
    }
    for word in words {
        if !is_wordlist_word(word) {
            return Err(WalletError::Mnemonic(format!(
                "word `{word}` is not in the TON BIP-39 wordlist"
            )));
        }
    }
    Ok(())
}

// ============================================================================
// Direct port of `Telegram/SourceFiles/wallet/wallet_phrase_shares.cpp`
// ============================================================================

/// Joins `words` by single ASCII spaces and right-pads with spaces to [`PHRASE_SEED_LENGTH`]
/// (`Wallet::PhraseShares::SeedFromWords` in `wallet_phrase_shares.cpp:99-107`).
pub fn seed_from_words(words: &[String]) -> Result<Zeroizing<Vec<u8>>, WalletError> {
    if words.is_empty() {
        return Err(WalletError::Mnemonic("empty word list".to_owned()));
    }
    let normalized: Vec<String> = words
        .iter()
        .map(|w| w.trim().to_ascii_lowercase())
        .collect();
    let joined = Zeroizing::new(normalized.join(" "));
    if joined.len() > PHRASE_SEED_LENGTH {
        return Err(WalletError::Mnemonic(format!(
            "joined phrase length {} exceeds {PHRASE_SEED_LENGTH} bytes",
            joined.len()
        )));
    }
    let mut padded = Zeroizing::new(vec![b' '; PHRASE_SEED_LENGTH]);
    padded[..joined.len()].copy_from_slice(joined.as_bytes());
    Ok(padded)
}

/// Decodes a space-padded seed buffer back into normalized recovery phrase words.
pub fn words_from_seed(seed: &[u8]) -> Result<Vec<String>, WalletError> {
    if seed.len() != PHRASE_SEED_LENGTH {
        return Err(WalletError::Mnemonic(format!(
            "expected {PHRASE_SEED_LENGTH}-byte seed, got {} bytes",
            seed.len()
        )));
    }
    let text = std::str::from_utf8(seed)
        .map_err(|e| WalletError::Mnemonic(format!("seed is not valid UTF-8: {e}")))?;
    let trimmed = text.trim_end_matches(' ');
    if trimmed.is_empty() {
        return Err(WalletError::Mnemonic("seed contains no words".to_owned()));
    }
    Ok(trimmed.split(' ').map(ToOwned::to_owned).collect())
}

/// Splits `seed` into `count >= 2` equal-length XOR shares where the first `count - 1`
/// shares are drawn from `OsRng` and their XOR equals `seed`
/// (`Wallet::PhraseShares::SplitSeed` in `wallet_phrase_shares.cpp:109-126`).
pub fn split_seed_xor(seed: &[u8], count: usize) -> Result<Vec<Vec<u8>>, WalletError> {
    if count < 2 || seed.is_empty() {
        return Err(WalletError::Mnemonic(
            "XOR split requires non-empty seed and count >= 2".to_owned(),
        ));
    }
    let len = seed.len();
    let mut shares = Vec::with_capacity(count);
    let mut last = seed.to_vec();
    for _ in 0..(count - 1) {
        let mut share = vec![0_u8; len];
        OsRng.fill_bytes(&mut share);
        for (dst, &src) in last.iter_mut().zip(share.iter()) {
            *dst ^= src;
        }
        shares.push(share);
    }
    shares.push(last);
    Ok(shares)
}

/// Combines equal-length XOR shares back into the original seed
/// (`Wallet::PhraseShares::CombineShares` in `wallet_phrase_shares.cpp:72-97`).
pub fn combine_shares_xor(shares: &[Vec<u8>]) -> Result<Zeroizing<Vec<u8>>, WalletError> {
    let Some(first) = shares.first() else {
        return Err(WalletError::Mnemonic("empty XOR share list".to_owned()));
    };
    if first.is_empty() {
        return Err(WalletError::Mnemonic("empty XOR share payload".to_owned()));
    }
    let len = first.len();
    let mut result = Zeroizing::new(vec![0_u8; len]);
    for share in shares {
        if share.len() != len {
            return Err(WalletError::Mnemonic(format!(
                "XOR share length mismatch: expected {len}, got {}",
                share.len()
            )));
        }
        for (dst, &src) in result.iter_mut().zip(share.iter()) {
            *dst ^= src;
        }
    }
    Ok(result)
}

/// Serializes `share` as a TL `mnemonic.decryptedKeyPart#8b90dd08 share:bytes` buffer
/// (`SerializeDecryptedKeyPart` in `wallet_phrase_shares.cpp:42-49`).
#[must_use]
pub fn serialize_decrypted_key_part(share: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + share.len() + 4);
    out.extend_from_slice(&DECRYPTED_KEY_PART_TL_ID.to_le_bytes());
    let len = share.len();
    let header_len = if len < 254 {
        out.push(len as u8);
        1
    } else {
        out.push(254);
        out.push((len & 0xFF) as u8);
        out.push(((len >> 8) & 0xFF) as u8);
        out.push(((len >> 16) & 0xFF) as u8);
        4
    };
    out.extend_from_slice(share);
    let pad = (4 - ((header_len + len) % 4)) % 4;
    out.resize(out.len() + pad, 0);
    out
}

/// Parses a TL `mnemonic.decryptedKeyPart#8b90dd08 share:bytes` buffer
/// (`ParseDecryptedKeyPart` in `wallet_phrase_shares.cpp:17-40`).
pub fn parse_decrypted_key_part(plain: &[u8]) -> Result<Vec<u8>, WalletError> {
    if plain.len() < 8 || !plain.len().is_multiple_of(4) {
        return Err(WalletError::Mnemonic(format!(
            "invalid TL decryptedKeyPart size: {}",
            plain.len()
        )));
    }
    let ctor = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    if ctor != DECRYPTED_KEY_PART_TL_ID {
        return Err(WalletError::Mnemonic(format!(
            "unexpected TL constructor 0x{ctor:08x}, expected 0x{DECRYPTED_KEY_PART_TL_ID:08x}"
        )));
    }
    let first_byte = plain[4];
    let (header_len, data_len) = if first_byte < 254 {
        (1_usize, usize::from(first_byte))
    } else if first_byte == 254 {
        if plain.len() < 8 {
            return Err(WalletError::Mnemonic(
                "truncated extended TL bytes header".to_owned(),
            ));
        }
        let len =
            usize::from(plain[5]) | (usize::from(plain[6]) << 8) | (usize::from(plain[7]) << 16);
        (4_usize, len)
    } else {
        return Err(WalletError::Mnemonic(
            "invalid TL bytes marker 255".to_owned(),
        ));
    };

    let data_start = 4 + header_len;
    let data_end = data_start
        .checked_add(data_len)
        .ok_or_else(|| WalletError::Mnemonic("TL bytes length overflow".to_owned()))?;
    let pad = (4 - ((header_len + data_len) % 4)) % 4;
    if data_end + pad != plain.len() {
        return Err(WalletError::Mnemonic(format!(
            "TL bytes trailing data or truncation: expected {}, got {}",
            data_end + pad,
            plain.len()
        )));
    }
    Ok(plain[data_start..data_end].to_vec())
}

/// Seals a share for a holder's 32-byte X25519 public key using an ephemeral X25519 keypair
/// and NaCl box (`32-byte ephemeral pubkey || 24-byte nonce || ciphertext(TL decryptedKeyPart)`),
/// mirroring `Wallet::PhraseShares::EncryptShare`.
pub fn encrypt_share_for_holder(
    holder_public_key: &[u8; PHRASE_SHARE_PUBLIC_KEY_SIZE],
    share: &[u8],
) -> Result<Vec<u8>, WalletError> {
    let eph_sk = BoxSecretKey::generate(&mut OsRng);
    let eph_pk = eph_sk.public_key();
    let mut nonce = [0_u8; HOLDER_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);

    let tl_plain = Zeroizing::new(serialize_decrypted_key_part(share));
    let salsa = SalsaBox::new(&BoxPublicKey::from(*holder_public_key), &eph_sk);
    let ciphertext = salsa
        .encrypt((&nonce).into(), tl_plain.as_slice())
        .map_err(|_| WalletError::Vault("holder share encryption failed".to_owned()))?;

    let mut out =
        Vec::with_capacity(PHRASE_SHARE_PUBLIC_KEY_SIZE + HOLDER_NONCE_LEN + ciphertext.len());
    out.extend_from_slice(eph_pk.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Opens a holder share sealed by [`encrypt_share_for_holder`], returning the inner share bytes,
/// mirroring `Wallet::PhraseShares::DecryptShare`.
pub fn decrypt_share_for_holder(
    holder_secret_key: &[u8; PHRASE_SHARE_PUBLIC_KEY_SIZE],
    encrypted: &[u8],
) -> Result<Vec<u8>, WalletError> {
    let min_len = PHRASE_SHARE_PUBLIC_KEY_SIZE + HOLDER_NONCE_LEN + 1;
    if encrypted.len() < min_len {
        return Err(WalletError::Vault(format!(
            "encrypted holder share too short: {} bytes",
            encrypted.len()
        )));
    }
    let mut sender_pk = [0_u8; PHRASE_SHARE_PUBLIC_KEY_SIZE];
    sender_pk.copy_from_slice(&encrypted[..PHRASE_SHARE_PUBLIC_KEY_SIZE]);
    let nonce =
        &encrypted[PHRASE_SHARE_PUBLIC_KEY_SIZE..PHRASE_SHARE_PUBLIC_KEY_SIZE + HOLDER_NONCE_LEN];
    let ciphertext = &encrypted[PHRASE_SHARE_PUBLIC_KEY_SIZE + HOLDER_NONCE_LEN..];

    let salsa = SalsaBox::new(
        &BoxPublicKey::from(sender_pk),
        &BoxSecretKey::from(*holder_secret_key),
    );
    let plain = Zeroizing::new(
        salsa
            .decrypt(nonce.into(), ciphertext)
            .map_err(|_| WalletError::Vault("holder share decryption/auth failed".to_owned()))?,
    );
    parse_decrypted_key_part(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mnemonic::generate_mnemonic;

    #[test]
    fn gf256_field_axioms_and_all_inverses() {
        assert_eq!(gf256_inv(0), None);
        for a in 1_u8..=255 {
            let inv = gf256_inv(a).unwrap();
            assert_eq!(gf256_mul(a, inv), 1, "failed inverse for {a}");
            assert_eq!(gf256_div(a, a), Some(1));
        }
        // Standard AES GF(256) KAT: 0x57 * 0x83 = 0xC1.
        assert_eq!(gf256_mul(0x57, 0x83), 0xC1);
        assert_eq!(gf256_mul(0x57, 0x13), 0xFE);
    }

    #[test]
    fn shamir_2_of_3_mnemonic_all_subsets_and_rejections() {
        let words = generate_mnemonic("").unwrap();
        let shares = split_phrase(&words, 2, 3).unwrap();
        assert_eq!(shares.len(), 3);
        for s in &shares {
            assert!(s.verify_checksum());
            let encoded = s.encode_string().unwrap();
            let decoded = PhraseShare::decode_string(&encoded).unwrap();
            assert_eq!(&decoded, s);
        }

        // Every 2-of-3 subset reconstructs the exact 24 words.
        for pair in [
            [shares[0].clone(), shares[1].clone()],
            [shares[0].clone(), shares[2].clone()],
            [shares[1].clone(), shares[2].clone()],
        ] {
            assert_eq!(reconstruct_phrase(&pair).unwrap(), words);
        }
        // All 3 shares also succeed.
        assert_eq!(reconstruct_phrase(&shares).unwrap(), words);

        // 1 share rejected.
        assert!(reconstruct_phrase(&shares[..1]).is_err());

        // Corrupted payload byte detected via share checksum.
        let mut bad = shares[0].clone();
        bad.data[10] ^= 0x01;
        assert!(!bad.verify_checksum());
        assert!(reconstruct_phrase(&[bad, shares[1].clone()]).is_err());
    }

    #[test]
    fn tdesktop_xor_and_holder_envelope_roundtrip() {
        let words = generate_mnemonic("").unwrap();
        let seed = seed_from_words(&words).unwrap();
        assert_eq!(seed.len(), PHRASE_SEED_LENGTH);
        assert_eq!(words_from_seed(&seed).unwrap(), words);

        let xor_shares = split_seed_xor(&seed, 3).unwrap();
        assert_eq!(xor_shares.len(), 3);
        assert_eq!(&*combine_shares_xor(&xor_shares).unwrap(), &*seed);

        let tl = serialize_decrypted_key_part(&xor_shares[0]);
        assert_eq!(parse_decrypted_key_part(&tl).unwrap(), xor_shares[0]);

        let holder_sk = BoxSecretKey::generate(&mut OsRng);
        let holder_pk = *holder_sk.public_key().as_bytes();
        let sealed = encrypt_share_for_holder(&holder_pk, &xor_shares[0]).unwrap();
        let opened = decrypt_share_for_holder(&holder_sk.to_bytes(), &sealed).unwrap();
        assert_eq!(opened, xor_shares[0]);
    }
}
