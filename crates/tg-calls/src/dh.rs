//! Diffie-Hellman key exchange and key fingerprint helpers for 1:1 calls.
//!
//! Port of `calls/calls_call.cpp` (`ComputeFingerprint`, `generateModExpFirst`,
//! `getKeyShaForFingerprint`, `confirmAcceptedCall`, `startConfirmedCall`) and
//! `calls/calls_instance.cpp` (`updateDhConfig`).

use tg_crypto::auth_key::{AUTH_KEY_SIZE, AuthKey};
use tg_crypto::dh::{
    GOOD_PRIME, ModExpFirst, create_auth_key, create_mod_exp, is_good_mod_exp_first_bytes,
    is_prime_and_good, mod_exp,
};
use tg_crypto::hash::sha256;

use crate::error::DhError;
use crate::types::DhConfigResponse;

/// Size in bytes of the random DH exponent and server seed (`kRandomPowerSize = 256`).
pub const RANDOM_POWER_SIZE: usize = 256;

/// Size in bytes of `g_a_hash` (`kSha256Size` in `calls_call.cpp:51`).
pub const SHA256_SIZE: usize = 32;

pub use tg_crypto::auth_key::AUTH_KEY_SIZE as CALL_AUTH_KEY_SIZE;
pub use tg_crypto::dh::{
    GOOD_PRIME as DH_GOOD_PRIME, ModExpFirst as DhModExpFirst,
    is_good_mod_exp_first_bytes as is_good_dh_mod_exp_first, is_prime_and_good as is_dh_prime_good,
};

/// Cached Diffie-Hellman parameters (`Calls::DhConfig` in `calls_call.h:46-50`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DhConfig {
    /// Server-assigned DH configuration version.
    pub version: i32,
    /// Generator `g`.
    pub g: i32,
    /// 2048-bit safe prime `p` (big-endian bytes).
    pub p: Vec<u8>,
}

impl DhConfig {
    /// Returns `true` if `g` and `p` are populated.
    pub fn is_valid(&self) -> bool {
        self.g != 0 && !self.p.is_empty()
    }
}

/// Cache for `messages.getDhConfig` responses.
///
/// Port of `Calls::Instance::_cachedDhConfig` and `Instance::updateDhConfig`
/// (`calls_instance.cpp:544-575`).
#[derive(Debug, Clone, Default)]
pub struct DhConfigCache {
    config: DhConfig,
}

impl DhConfigCache {
    /// Create an empty cache (version = 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// Current cached version to send in `messages.getDhConfig(version, 256)`.
    pub fn version(&self) -> i32 {
        self.config.version
    }

    /// Snapshot of the current cached DH config.
    pub fn config(&self) -> DhConfig {
        self.config.clone()
    }

    /// Validate and apply a `messages.DhConfig` response, returning the 256-byte
    /// server random seed on success.
    ///
    /// Mirrors `Instance::updateDhConfig` (`calls_instance.cpp:544-575`).
    pub fn update(&mut self, response: DhConfigResponse) -> Result<Vec<u8>, DhError> {
        match response {
            DhConfigResponse::Config {
                g,
                p,
                version,
                random,
            } => {
                if !is_prime_and_good(&p, g) {
                    return Err(DhError::InvalidParameters);
                }
                if random.len() != RANDOM_POWER_SIZE {
                    return Err(DhError::InvalidRandomLength {
                        expected: RANDOM_POWER_SIZE,
                        actual: random.len(),
                    });
                }
                self.config.g = g;
                self.config.p = p;
                self.config.version = version;
                Ok(random)
            }
            DhConfigResponse::NotModified { random } => {
                if !self.config.is_valid() {
                    return Err(DhError::MissingCachedConfig);
                }
                if random.len() != RANDOM_POWER_SIZE {
                    return Err(DhError::InvalidRandomLength {
                        expected: RANDOM_POWER_SIZE,
                        actual: random.len(),
                    });
                }
                Ok(random)
            }
        }
    }
}

/// Compute `g_a_hash = SHA256(g_a)` sent by the caller in `phone.requestCall`.
///
/// Port of `calls_call.cpp:324`.
pub fn compute_ga_hash(g_a: &[u8]) -> [u8; SHA256_SIZE] {
    sha256(&[g_a])
}

/// Verify that `SHA256(g_a)` equals the previously committed `g_a_hash`.
///
/// Port of `calls_call.cpp:1075`.
pub fn verify_ga_hash(g_a: &[u8], expected_hash: &[u8]) -> Result<(), DhError> {
    if expected_hash.len() != SHA256_SIZE {
        return Err(DhError::GaHashMismatch);
    }
    let actual = compute_ga_hash(g_a);
    if !tg_crypto::hash::const_time_eq(&actual, expected_hash) {
        return Err(DhError::GaHashMismatch);
    }
    Ok(())
}

/// Left-pad big-endian DH key bytes to 256 bytes using [`tg_crypto::auth_key::AuthKey::from_bytes`].
pub fn fill_auth_key_data(computed: &[u8]) -> Result<[u8; AUTH_KEY_SIZE], DhError> {
    if computed.len() > AUTH_KEY_SIZE {
        return Err(DhError::AuthKeyTooLarge {
            actual: computed.len(),
        });
    }
    let key = AuthKey::from_bytes(computed).map_err(|_| DhError::AuthKeyTooLarge {
        actual: computed.len(),
    })?;
    Ok(key.data)
}

/// Compute the 64-bit call key fingerprint: lower 64 bits (`hash[12..20]`, little-endian)
/// of `SHA1(auth_key)` via [`tg_crypto::auth_key::AuthKey::key_id_u64`].
///
/// Port of `ComputeFingerprint` in `calls/calls_call.cpp:189-201`.
pub fn compute_key_fingerprint(auth_key: &[u8; AUTH_KEY_SIZE]) -> u64 {
    AuthKey::new(*auth_key).key_id_u64()
}

/// Compute `SHA256(auth_key || g_a)` used as input to the 4-emoji fingerprint table.
///
/// Port of `Call::getKeyShaForFingerprint` in `calls/calls_call.cpp:795-811`.
pub fn compute_key_sha_for_fingerprint(
    auth_key: &[u8; AUTH_KEY_SIZE],
    g_a: &[u8],
) -> [u8; SHA256_SIZE] {
    sha256(&[auth_key.as_slice(), g_a])
}

/// Generate the first DH step (`g_a` + `a` or `g_b` + `b`) using the OS CSPRNG
/// via [`tg_crypto::dh::create_mod_exp`].
pub fn generate_mod_exp_first(
    config: &DhConfig,
    random_seed: &[u8],
) -> Result<ModExpFirst, DhError> {
    if !config.is_valid() || !is_prime_and_good(&config.p, config.g) {
        return Err(DhError::InvalidParameters);
    }
    let seed: &[u8; RANDOM_POWER_SIZE] =
        random_seed
            .try_into()
            .map_err(|_| DhError::InvalidRandomLength {
                expected: RANDOM_POWER_SIZE,
                actual: random_seed.len(),
            })?;
    create_mod_exp(config.g, &config.p, seed).map_err(|_| DhError::InvalidParameters)
}

/// Generate the first DH step with a caller-supplied random generator (for deterministic tests).
pub fn generate_mod_exp_first_with_rng<F>(
    config: &DhConfig,
    random_seed: &[u8],
    mut fill_local_random: F,
) -> Result<ModExpFirst, DhError>
where
    F: FnMut(&mut [u8; RANDOM_POWER_SIZE]),
{
    if !config.is_valid() || !is_prime_and_good(&config.p, config.g) {
        return Err(DhError::InvalidParameters);
    }
    if random_seed.len() != RANDOM_POWER_SIZE {
        return Err(DhError::InvalidRandomLength {
            expected: RANDOM_POWER_SIZE,
            actual: random_seed.len(),
        });
    }

    let g_bytes = (config.g as u32).to_be_bytes();
    let mut random_power = [0u8; RANDOM_POWER_SIZE];
    for _ in 0..1024 {
        fill_local_random(&mut random_power);
        for (dst, &seed) in random_power.iter_mut().zip(random_seed.iter()) {
            *dst ^= seed;
        }
        let modexp =
            mod_exp(&g_bytes, &random_power, &config.p).map_err(|_| DhError::InvalidParameters)?;
        if is_good_mod_exp_first_bytes(&modexp, &config.p) {
            return Ok(ModExpFirst {
                modexp,
                random_power,
            });
        }
    }
    Err(DhError::InvalidParameters)
}

/// Finish DH key agreement: compute `peer_public^random_power mod p`, left-pad to 256 bytes,
/// and return `(auth_key, key_fingerprint)`.
///
/// Port of `confirmAcceptedCall` (`calls_call.cpp:1027-1039`) and
/// `startConfirmedCall` (`calls_call.cpp:1082-1093`).
pub fn finish_dh_key_exchange(
    config: &DhConfig,
    peer_public: &[u8],
    random_power: &[u8],
) -> Result<([u8; AUTH_KEY_SIZE], u64), DhError> {
    if !is_prime_and_good(&config.p, config.g) {
        return Err(DhError::InvalidParameters);
    }
    let computed = create_auth_key(peer_public, random_power, &config.p)
        .map_err(|_| DhError::InvalidPublicValue)?;
    let auth_key = fill_auth_key_data(&computed)?;
    let fingerprint = compute_key_fingerprint(&auth_key);
    Ok((auth_key, fingerprint))
}

/// Re-export of the known-good 2048-bit Telegram DH safe prime from `tg-crypto`.
#[must_use]
pub const fn good_prime() -> &'static [u8; 256] {
    &GOOD_PRIME
}
