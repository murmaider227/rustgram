//! Two-Factor Authentication (SRP-6a) and Telegram Passport secret cryptography.
//!
//! Port of `core/core_cloud_password.cpp` (`PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow`,
//! `SecureSecretAlgoSHA512`, `SecureSecretAlgoPBKDF2`) and `passport/passport_encryption.cpp`.

use num_bigint::BigUint;
use num_traits::Zero;
use zeroize::Zeroize;

use crate::aes::{aes_cbc_decrypt, aes_cbc_encrypt};
use crate::dh::{is_good_mod_exp_first, is_prime_and_good};
use crate::error::CryptoError;
use crate::hash::{pbkdf2_hmac_sha512_64, sha256, sha512};
use crate::random::random_bytes;

/// Required PBKDF2 iteration count for Telegram SRP 2FA and Passport PBKDF2 KDF.
pub const SRP_PBKDF2_ITERATIONS: u32 = 100_000;

/// Additional random salt bytes appended when validating a new password/passport KDF algo.
pub const ADDITIONAL_SALT_SIZE: usize = 32;

/// Fixed 256-byte padded representation length used in Telegram SRP hashing.
const SRP_NUM_SIZE: usize = 256;

/// Parameters for `passwordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow`
/// (`Core::CloudPasswordAlgoModPow` in `core/core_cloud_password.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordKdfAlgoModPow {
    /// First salt (`salt1`).
    pub salt1: Vec<u8>,
    /// Second salt (`salt2`).
    pub salt2: Vec<u8>,
    /// Diffie-Hellman generator `g`.
    pub g: i32,
    /// 2048-bit big-endian safe prime `p`.
    pub p: Vec<u8>,
}

/// Output fields for TL `inputCheckPasswordSRP` (`srp_id`, `A`, `M1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputCheckPasswordSrp {
    /// Server-provided SRP session ID.
    pub srp_id: i64,
    /// Client ephemeral public value `A = g^a mod p` (256-byte big-endian).
    pub a: [u8; 256],
    /// Client SRP proof `M1` (32-byte SHA-256).
    pub m1: [u8; 32],
}

/// Append 32 random bytes to `algo.salt1` when preparing a new cloud password
/// (port of `Core::ValidateNewCloudPasswordAlgo`).
pub fn validate_new_algo(mut algo: PasswordKdfAlgoModPow) -> PasswordKdfAlgoModPow {
    let old_len = algo.salt1.len();
    algo.salt1.resize(old_len + ADDITIONAL_SALT_SIZE, 0);
    random_bytes(&mut algo.salt1[old_len..]);
    algo
}

/// Compute the 32-byte primary password hash `x` (`Core::ComputeCloudPasswordHash`):
///
/// ```text
/// hash1 = SHA256(salt1 || password || salt1)
/// hash2 = SHA256(salt2 || hash1 || salt2)
/// hash3 = PBKDF2-HMAC-SHA512(hash2, salt1, 100_000, 64)
/// x     = SHA256(salt2 || hash3 || salt2)
/// ```
pub fn compute_hash(algo: &PasswordKdfAlgoModPow, password: &[u8]) -> [u8; 32] {
    let mut hash1 = sha256(&[&algo.salt1, password, &algo.salt1]);
    let mut hash2 = sha256(&[&algo.salt2, &hash1, &algo.salt2]);
    let mut hash3 = pbkdf2_hmac_sha512_64(&hash2, &algo.salt1, SRP_PBKDF2_ITERATIONS);
    let x = sha256(&[&algo.salt2, &hash3, &algo.salt2]);
    hash1.zeroize();
    hash2.zeroize();
    hash3.zeroize();
    x
}

/// Compute the 256-byte SRP verifier `v = g^x mod p` for setting a new 2FA password
/// (port of `Core::ComputeCloudPasswordDigest`).
pub fn compute_digest(
    algo: &PasswordKdfAlgoModPow,
    password: &[u8],
) -> Result<[u8; 256], CryptoError> {
    if !is_prime_and_good(&algo.p, algo.g) {
        return Err(CryptoError::SrpFailed("invalid p or g in SRP algo"));
    }
    let mut hash = compute_hash(algo, password);
    let g = BigUint::from(algo.g as u32);
    let x = BigUint::from_bytes_be(&hash);
    hash.zeroize();
    let p = BigUint::from_bytes_be(&algo.p);
    let v = g.modpow(&x, &p);
    pad_biguint_256(&v)
}

/// Compute Telegram SRP-6a `InputCheckPasswordSrp` (`srp_id`, `A`, `M1`) from a plaintext
/// password (port of `Core::ComputeCloudPasswordHash` + `Core::ComputeCloudPasswordCheck`).
pub fn compute_check(
    algo: &PasswordKdfAlgoModPow,
    srp_b: &[u8],
    srp_id: i64,
    password: &[u8],
) -> Result<InputCheckPasswordSrp, CryptoError> {
    let mut hash = compute_hash(algo, password);
    let res = compute_check_with_hash(algo, srp_b, srp_id, &hash);
    hash.zeroize();
    res
}

/// Compute Telegram SRP-6a `InputCheckPasswordSrp` from a precomputed 32-byte password hash `x`
/// (port of `Core::ComputeCloudPasswordCheck`).
pub fn compute_check_with_hash(
    algo: &PasswordKdfAlgoModPow,
    srp_b: &[u8],
    srp_id: i64,
    password_hash: &[u8; 32],
) -> Result<InputCheckPasswordSrp, CryptoError> {
    let mut a_bytes = [0u8; SRP_NUM_SIZE];
    for _ in 0..1024 {
        random_bytes(&mut a_bytes);
        match compute_check_with_random(algo, srp_b, srp_id, password_hash, &a_bytes) {
            Ok(check) => {
                a_bytes.zeroize();
                return Ok(check);
            }
            Err(CryptoError::SrpFailed("ephemeral A or u invalid")) => continue,
            Err(e) => {
                a_bytes.zeroize();
                return Err(e);
            }
        }
    }
    a_bytes.zeroize();
    Err(CryptoError::SrpFailed(
        "failed to sample valid ephemeral secret a",
    ))
}

/// Deterministic SRP-6a computation using an explicit 256-byte client ephemeral secret `a_bytes`.
///
/// Matches `ComputeCheck` in `core/core_cloud_password.cpp:86-157` step by step.
pub fn compute_check_with_random(
    algo: &PasswordKdfAlgoModPow,
    srp_b: &[u8],
    srp_id: i64,
    password_hash: &[u8; 32],
    a_bytes: &[u8; 256],
) -> Result<InputCheckPasswordSrp, CryptoError> {
    if !is_prime_and_good(&algo.p, algo.g) {
        return Err(CryptoError::SrpFailed("invalid p or g in SRP algo"));
    }
    let p = BigUint::from_bytes_be(&algo.p);
    let g = BigUint::from(algo.g as u32);
    let b_val = BigUint::from_bytes_be(srp_b);

    // IsGoodLarge(B, p): 0 < B < p
    if b_val.is_zero() || b_val >= p {
        return Err(CryptoError::SrpFailed("srp_B out of range (0 < B < p)"));
    }

    let p_for_hash = pad_bytes_256(&algo.p)?;
    let g_for_hash = pad_biguint_256(&g)?;
    let b_for_hash = pad_bytes_256(srp_b)?;

    let x = BigUint::from_bytes_be(password_hash);
    let g_x = g.modpow(&x, &p);
    let k = BigUint::from_bytes_be(&sha256(&[&p_for_hash, &g_for_hash]));
    let kg_x = (&k * &g_x) % &p;

    let a = BigUint::from_bytes_be(a_bytes);
    let a_pub = g.modpow(&a, &p);
    if !is_good_mod_exp_first(&a_pub, &p) {
        return Err(CryptoError::SrpFailed("ephemeral A or u invalid"));
    }
    let a_for_hash = pad_biguint_256(&a_pub)?;
    let u = BigUint::from_bytes_be(&sha256(&[&a_for_hash, &b_for_hash]));
    if u.is_zero() {
        return Err(CryptoError::SrpFailed("ephemeral A or u invalid"));
    }

    // g_b = (B - k * g^x) mod p
    let g_b = if b_val >= kg_x {
        &b_val - &kg_x
    } else {
        (&b_val + &p) - &kg_x
    };
    if !is_good_mod_exp_first(&g_b, &p) {
        return Err(CryptoError::SrpFailed("g_b failed IsGoodModExpFirst check"));
    }

    let a_ux = &a + (&u * &x);
    let s = g_b.modpow(&a_ux, &p);
    let mut s_for_hash = pad_biguint_256(&s)?;
    let mut k_session = sha256(&[&s_for_hash]);
    s_for_hash.zeroize();

    let h_p = sha256(&[&p_for_hash]);
    let h_g = sha256(&[&g_for_hash]);
    let mut h_p_xor_h_g = [0u8; 32];
    for i in 0..32 {
        h_p_xor_h_g[i] = h_p[i] ^ h_g[i];
    }

    let h_salt1 = sha256(&[&algo.salt1]);
    let h_salt2 = sha256(&[&algo.salt2]);
    let m1 = sha256(&[
        &h_p_xor_h_g,
        &h_salt1,
        &h_salt2,
        &a_for_hash,
        &b_for_hash,
        &k_session,
    ]);
    k_session.zeroize();

    Ok(InputCheckPasswordSrp {
        srp_id,
        a: a_for_hash,
        m1,
    })
}

/// Telegram Passport secure secret KDF algorithm (`Core::SecureSecretAlgo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecureSecretAlgo {
    /// Legacy `securePasswordKdfAlgoSHA512`: `SHA512(salt || password || salt)`.
    Sha512 {
        /// Salt bytes.
        salt: Vec<u8>,
    },
    /// Modern `securePasswordKdfAlgoPBKDF2HMACSHA512iter100000`:
    /// `PBKDF2-HMAC-SHA512(password, salt, 100_000, 64)`.
    Pbkdf2HmacSha512Iter100000 {
        /// Salt bytes.
        salt: Vec<u8>,
    },
}

/// Append 32 random bytes to a new PBKDF2 secure secret salt
/// (port of `Core::ValidateNewSecureSecretAlgo`).
pub fn validate_new_secure_secret_algo(
    algo: SecureSecretAlgo,
) -> Result<SecureSecretAlgo, CryptoError> {
    match algo {
        SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { mut salt } => {
            let old_len = salt.len();
            salt.resize(old_len + ADDITIONAL_SALT_SIZE, 0);
            random_bytes(&mut salt[old_len..]);
            Ok(SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt })
        }
        SecureSecretAlgo::Sha512 { .. } => Err(CryptoError::PassportFailed(
            "new secure secret must use PBKDF2-HMAC-SHA512",
        )),
    }
}

/// Compute the 64-byte Telegram Passport password hash for encrypting/decrypting the
/// Passport secure secret (`Core::ComputeSecureSecretHash`).
pub fn compute_secure_secret_hash(algo: &SecureSecretAlgo, password: &[u8]) -> [u8; 64] {
    match algo {
        SecureSecretAlgo::Sha512 { salt } => sha512(&[salt, password, salt]),
        SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt } => {
            pbkdf2_hmac_sha512_64(password, salt, SRP_PBKDF2_ITERATIONS)
        }
    }
}

/// Check that Passport secret bytes satisfy the Telegram Passport checksum (`sum(bytes) % 255 == 239`).
///
/// Port of `Passport::CheckSecretBytes` in `passport/passport_encryption.cpp`.
pub fn check_secret_bytes(secret: &[u8]) -> bool {
    let sum: u64 = secret.iter().map(|&b| u64::from(b)).sum();
    (sum % 255) == 239
}

/// Generate a valid 32-byte Telegram Passport secret whose byte sum mod 255 equals 239
/// (`Passport::GenerateSecretBytes`).
pub fn generate_secret_bytes() -> [u8; 32] {
    let mut secret = [0u8; 32];
    random_bytes(&mut secret);
    let sum: u64 = secret.iter().map(|&b| u64::from(b)).sum();
    let modulo = sum % 255;
    let add = 255 + 239 - modulo;
    secret[0] = ((u64::from(secret[0]) + add) % 255) as u8;
    secret
}

/// Encrypt a 32-byte Passport secret with the 64-byte password hash
/// (`Passport::EncryptSecureSecret`: AES-256-CBC with `key = hash[0..32]`, `iv = hash[32..48]`).
pub fn encrypt_secure_secret(
    secret: &[u8; 32],
    password_hash: &[u8; 64],
) -> Result<[u8; 32], CryptoError> {
    if !check_secret_bytes(secret) {
        return Err(CryptoError::PassportFailed(
            "passport secret failed mod-255 checksum",
        ));
    }
    let mut key = [0u8; 32];
    let mut iv = [0u8; 16];
    key.copy_from_slice(&password_hash[0..32]);
    iv.copy_from_slice(&password_hash[32..48]);
    let enc = aes_cbc_encrypt(secret, &key, &iv)?;
    key.zeroize();
    iv.zeroize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&enc);
    Ok(out)
}

/// Decrypt a 32-byte Passport secret with the 64-byte password hash and verify its checksum
/// (`Passport::DecryptSecureSecret`).
pub fn decrypt_secure_secret(
    encrypted_secret: &[u8],
    password_hash: &[u8; 64],
) -> Result<[u8; 32], CryptoError> {
    if encrypted_secret.len() != 32 {
        return Err(CryptoError::PassportFailed(
            "encrypted passport secret must be 32 bytes",
        ));
    }
    let mut key = [0u8; 32];
    let mut iv = [0u8; 16];
    key.copy_from_slice(&password_hash[0..32]);
    iv.copy_from_slice(&password_hash[32..48]);
    let dec = aes_cbc_decrypt(encrypted_secret, &key, &iv)?;
    key.zeroize();
    iv.zeroize();
    if !check_secret_bytes(&dec) {
        return Err(CryptoError::PassportFailed(
            "decrypted passport secret failed mod-255 checksum",
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&dec);
    Ok(out)
}

/// Compute the 64-bit Passport secret ID (`Passport::CountSecureSecretId`: first 8 bytes of
/// `SHA256(secret)` as little-endian `u64`).
pub fn count_secure_secret_id(secret: &[u8]) -> u64 {
    let digest = sha256(&[secret]);
    let mut id = [0u8; 8];
    id.copy_from_slice(&digest[0..8]);
    u64::from_le_bytes(id)
}

fn pad_bytes_256(bytes: &[u8]) -> Result<[u8; SRP_NUM_SIZE], CryptoError> {
    if bytes.len() > SRP_NUM_SIZE {
        return Err(CryptoError::SrpFailed("integer exceeds 256 bytes"));
    }
    let mut out = [0u8; SRP_NUM_SIZE];
    out[SRP_NUM_SIZE - bytes.len()..].copy_from_slice(bytes);
    Ok(out)
}

fn pad_biguint_256(value: &BigUint) -> Result<[u8; SRP_NUM_SIZE], CryptoError> {
    pad_bytes_256(&value.to_bytes_be())
}
