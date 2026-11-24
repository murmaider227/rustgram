//! TON mnemonic generation, validation (PBKDF2-HMAC-SHA512 scheme), BIP-39 wordlist lookup,
//! and Ed25519 key derivation.
//!
//! Follows the official TON mnemonic specification (TEP-3 / `ton-crypto`) and ports wordlist
//! suggestion helpers from `Telegram/SourceFiles/wallet/wallet_session.h`.

use bip39::Language;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use rand_core::{OsRng, RngCore};
use sha2::Sha512;
use zeroize::Zeroizing;

use crate::WalletError;

type HmacSha512 = Hmac<Sha512>;

/// Standard number of words in a TON recovery phrase.
pub const MNEMONIC_WORD_COUNT: usize = 24;
/// PBKDF2 iteration count for deriving the TON master seed (`PBKDF_ITERATIONS`).
pub const PBKDF_ITERATIONS: u32 = 100_000;
/// PBKDF2 iteration count for validating a basic seed (`max(1, 100_000 / 256)`).
pub const PBKDF_BASIC_SEED_ITERATIONS: u32 = PBKDF_ITERATIONS / 256;

const TON_DEFAULT_SEED_SALT: &[u8] = b"TON default seed";
const TON_BASIC_SEED_SALT: &[u8] = b"TON seed version";
const TON_FAST_SEED_SALT: &[u8] = b"TON fast seed version";

/// Derived TON Ed25519 keypair. Secret seed bytes are automatically zeroized on drop.
#[derive(Clone)]
pub struct TonKeypair {
    seed: Zeroizing<[u8; 32]>,
    verifying: VerifyingKey,
}

impl std::fmt::Debug for TonKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TonKeypair")
            .field("public_key", &hex::encode(self.public_key_bytes()))
            .finish_non_exhaustive()
    }
}

impl TonKeypair {
    /// Builds a [`TonKeypair`] directly from a 32-byte Ed25519 seed.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&seed);
        let verifying = signing.verifying_key();
        Self {
            seed: Zeroizing::new(seed),
            verifying,
        }
    }

    /// Returns the 32-byte Ed25519 private seed.
    #[must_use]
    pub fn seed_bytes(&self) -> &[u8; 32] {
        &self.seed
    }

    /// Returns the 32-byte Ed25519 public key.
    #[must_use]
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying.to_bytes()
    }

    /// Returns the 64-byte NaCl-style Ed25519 secret key (`seed || public_key`).
    #[must_use]
    pub fn expanded_secret_key(&self) -> Zeroizing<[u8; 64]> {
        let mut out = Zeroizing::new([0_u8; 64]);
        out[..32].copy_from_slice(&*self.seed);
        out[32..].copy_from_slice(&self.verifying.to_bytes());
        out
    }

    /// Returns the [`SigningKey`] for signing TON messages and proofs.
    #[must_use]
    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.seed)
    }

    /// Returns the [`VerifyingKey`].
    #[must_use]
    pub const fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying
    }
}

/// Returns the standard 2048-word English BIP-39 wordlist used by TON.
#[must_use]
pub fn wordlist() -> &'static [&'static str; 2048] {
    Language::English.word_list()
}

/// Returns `true` if `word` (case-insensitive, trimmed) is in the 2048-word TON wordlist
/// (`Wallet::IsWordlistWord` in `wallet_session.h:497`).
#[must_use]
pub fn is_wordlist_word(word: &str) -> bool {
    let lower = word.trim().to_ascii_lowercase();
    wordlist().binary_search(&lower.as_str()).is_ok()
}

/// Returns up to `limit` wordlist suggestions starting with `prefix`
/// (`Wallet::WordlistSuggestions` in `wallet_session.h:498`).
#[must_use]
pub fn wordlist_suggestions(prefix: &str, limit: usize) -> Vec<&'static str> {
    let lower = prefix.trim().to_ascii_lowercase();
    if lower.is_empty() || limit == 0 {
        return Vec::new();
    }
    wordlist()
        .iter()
        .copied()
        .filter(|w| w.starts_with(lower.as_str()))
        .take(limit)
        .collect()
}

/// Computes the 64-byte TON mnemonic entropy via `HMAC-SHA512(key = words.join(" "), data = password)`.
pub fn mnemonic_to_entropy(
    words: &[String],
    password: &str,
) -> Result<Zeroizing<[u8; 64]>, WalletError> {
    if words.is_empty() {
        return Err(WalletError::Mnemonic("empty mnemonic".to_owned()));
    }
    let normalized: Vec<String> = words
        .iter()
        .map(|w| w.trim().to_ascii_lowercase())
        .collect();
    let phrase = Zeroizing::new(normalized.join(" "));
    let mut mac = HmacSha512::new_from_slice(phrase.as_bytes())
        .map_err(|e| WalletError::Mnemonic(format!("HMAC init failed: {e}")))?;
    mac.update(password.as_bytes());
    let result = mac.finalize().into_bytes();
    let mut out = Zeroizing::new([0_u8; 64]);
    out.copy_from_slice(&result);
    Ok(out)
}

/// Returns `true` when `entropy` satisfies TON's basic seed check
/// (`PBKDF2-HMAC-SHA512(entropy, "TON seed version", 390)[0] == 0`).
#[must_use]
pub fn is_basic_seed(entropy: &[u8; 64]) -> bool {
    let mut out = Zeroizing::new([0_u8; 64]);
    pbkdf2_hmac::<Sha512>(
        entropy,
        TON_BASIC_SEED_SALT,
        PBKDF_BASIC_SEED_ITERATIONS,
        &mut *out,
    );
    out[0] == 0
}

/// Returns `true` when `entropy` satisfies TON's password-seed marker
/// (`PBKDF2-HMAC-SHA512(entropy, "TON fast seed version", 1)[0] == 1`).
#[must_use]
pub fn is_password_seed(entropy: &[u8; 64]) -> bool {
    let mut out = Zeroizing::new([0_u8; 64]);
    pbkdf2_hmac::<Sha512>(entropy, TON_FAST_SEED_SALT, 1, &mut *out);
    out[0] == 1
}

/// Validates a TON mnemonic phrase (wordlist membership + TON PBKDF2 seed version check).
#[must_use]
pub fn validate_mnemonic(words: &[String], password: &str) -> bool {
    if words.len() != MNEMONIC_WORD_COUNT {
        return false;
    }
    if !words.iter().all(|w| is_wordlist_word(w)) {
        return false;
    }
    if !password.is_empty() {
        // Password-protected phrase must have password marker when evaluated without password.
        let Ok(no_pass) = mnemonic_to_entropy(words, "") else {
            return false;
        };
        if !is_password_seed(&no_pass) {
            return false;
        }
        let Ok(with_pass) = mnemonic_to_entropy(words, password) else {
            return false;
        };
        return is_basic_seed(&with_pass);
    }

    let Ok(entropy) = mnemonic_to_entropy(words, "") else {
        return false;
    };
    if is_password_seed(&entropy) {
        return false;
    }
    is_basic_seed(&entropy)
}

/// Generates a fresh 24-word TON mnemonic phrase satisfying TON's PBKDF2 seed version rules.
pub fn generate_mnemonic(password: &str) -> Result<Vec<String>, WalletError> {
    let words_table = wordlist();
    let mut rng = OsRng;
    let mut buf = [0_u8; MNEMONIC_WORD_COUNT * 2];

    loop {
        rng.fill_bytes(&mut buf);
        let mut candidate = Vec::with_capacity(MNEMONIC_WORD_COUNT);
        for chunk in buf.as_chunks::<2>().0 {
            let idx = (u16::from_be_bytes([chunk[0], chunk[1]]) as usize) & 0x07FF;
            candidate.push(words_table[idx].to_owned());
        }

        if password.is_empty() {
            let entropy = mnemonic_to_entropy(&candidate, "")?;
            if is_basic_seed(&entropy) && !is_password_seed(&entropy) {
                return Ok(candidate);
            }
        } else {
            let no_pass = mnemonic_to_entropy(&candidate, "")?;
            if !is_password_seed(&no_pass) {
                continue;
            }
            let with_pass = mnemonic_to_entropy(&candidate, password)?;
            if is_basic_seed(&with_pass) {
                return Ok(candidate);
            }
        }
    }
}

/// Derives the Ed25519 [`TonKeypair`] from a TON mnemonic phrase and optional password.
///
/// Uses `PBKDF2-HMAC-SHA512(HMAC-SHA512(words.join(" "), password), "TON default seed", 100_000)`.
pub fn mnemonic_to_keypair(words: &[String], password: &str) -> Result<TonKeypair, WalletError> {
    if words.is_empty() {
        return Err(WalletError::Mnemonic("empty mnemonic".to_owned()));
    }
    let entropy = mnemonic_to_entropy(words, password)?;
    let mut derived = Zeroizing::new([0_u8; 64]);
    pbkdf2_hmac::<Sha512>(
        &*entropy,
        TON_DEFAULT_SEED_SALT,
        PBKDF_ITERATIONS,
        &mut *derived,
    );
    let mut seed = [0_u8; 32];
    seed.copy_from_slice(&derived[..32]);
    Ok(TonKeypair::from_seed(seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_ton_spec_mnemonic_vectors() {
        // Public test vectors from `@ton/crypto` (`src/mnemonic/mnemonic.spec.ts`, MIT).
        // Never hold real funds.
        let vectors: [(&[&str], &str); 2] = [
            (
                &[
                    "hospital", "stove", "relief", "fringe", "tongue", "always", "charge", "angry",
                    "urge", "sentence", "again", "match", "nerve", "inquiry", "senior", "coconut",
                    "label", "tumble", "carry", "category", "beauty", "bean", "road", "solution",
                ],
                "9d659a6c2234db7f6e4f977e6e8653b9f5946d557163f31034011375d8f3f97df6c450a16bb1c514e22f1977e390a3025599aa1e7b00068a6aacf2119484c1bd",
            ),
            (
                &[
                    "dose", "ice", "enrich", "trigger", "test", "dove", "century", "still",
                    "betray", "gas", "diet", "dune", "use", "other", "base", "gym", "mad", "law",
                    "immense", "village", "world", "example", "praise", "game",
                ],
                "119dcf2840a3d56521d260b2f125eedc0d4f3795b9e627269a4b5a6dca8257bdc04ad1885c127fe863abb00752fa844e6439bb04f264d70de7cea580b32637ab",
            ),
        ];

        for (words_raw, expected_expanded_hex) in vectors {
            let words: Vec<String> = words_raw.iter().map(|s| (*s).to_owned()).collect();
            assert!(validate_mnemonic(&words, ""));
            let kp = mnemonic_to_keypair(&words, "").unwrap();
            assert_eq!(
                hex::encode(*kp.expanded_secret_key()),
                expected_expanded_hex
            );
        }
    }

    #[test]
    fn wordlist_and_generation_roundtrip() {
        assert_eq!(wordlist().len(), 2048);
        assert!(is_wordlist_word("abandon"));
        assert!(is_wordlist_word("ZOO"));
        assert!(!is_wordlist_word("notaword123"));
        assert_eq!(wordlist_suggestions("aban", 5), vec!["abandon"]);

        let generated = generate_mnemonic("").unwrap();
        assert_eq!(generated.len(), MNEMONIC_WORD_COUNT);
        assert!(validate_mnemonic(&generated, ""));
        let kp = mnemonic_to_keypair(&generated, "").unwrap();
        assert_eq!(kp.public_key_bytes().len(), 32);
    }
}
