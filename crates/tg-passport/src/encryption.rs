//! Telegram Passport secure value, file, secret, and credentials cryptography.
//!
//! Port of `passport/passport_encryption.h` and `passport/passport_encryption.cpp`,
//! built on top of `tg-crypto`'s AES-256-CBC, SHA-256/SHA-512, MD5, RSA-OAEP, and
//! secure secret helpers.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use tg_crypto::aes::{aes_cbc_decrypt, aes_cbc_encrypt};
use tg_crypto::hash::{md5_hex, sha256, sha512};
use tg_crypto::random::{random_bytes, random_u32};
use tg_crypto::rsa::PublicKey;
use tg_crypto::srp::{
    SecureSecretAlgo, check_secret_bytes, compute_secure_secret_hash, count_secure_secret_id,
    decrypt_secure_secret, encrypt_secure_secret, generate_secret_bytes,
    validate_new_secure_secret_algo,
};
use zeroize::Zeroize;

use crate::PassportError;

/// AES-256 key size in bytes (`kAesKeyLength`).
pub const AES_KEY_LENGTH: usize = 32;
/// AES-CBC IV size in bytes (`kAesIvLength`).
pub const AES_IV_LENGTH: usize = 16;
/// Passport secret size in bytes (`kSecretSize`).
pub const SECRET_SIZE: usize = 32;
/// SHA-256 data hash size in bytes (`kDataHashSize`).
pub const DATA_HASH_SIZE: usize = 32;
/// Minimum random padding prefix length in bytes (`kMinPadding`).
pub const MIN_PADDING: usize = 32;
/// Maximum random padding prefix length in bytes (`kMaxPadding`).
pub const MAX_PADDING: usize = 255;
/// AES block alignment in bytes (`kAlignTo`).
pub const ALIGN_TO: usize = 16;

/// Result of encrypting a Passport secure value payload or file (`Passport::EncryptedData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedData {
    /// 32-byte per-value/per-file random secret (satisfies `sum % 255 == 239`).
    pub secret: [u8; SECRET_SIZE],
    /// 32-byte SHA-256 hash of the padded plaintext (`padding_byte || random_pad || plaintext`).
    pub hash: [u8; DATA_HASH_SIZE],
    /// AES-256-CBC ciphertext of the padded plaintext.
    pub bytes: Vec<u8>,
}

/// Encrypted file payload ready for upload (`UploadScanData` crypto outputs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedFileUpload {
    /// Per-file 32-byte secret (must be encrypted with the user's master secret before saving).
    pub file_secret: [u8; SECRET_SIZE],
    /// SHA-256 hash of the padded file plaintext.
    pub hash: [u8; DATA_HASH_SIZE],
    /// AES-256-CBC encrypted file bytes.
    pub bytes: Vec<u8>,
    /// Lowercase 32-char hex MD5 checksum of `bytes` (`md5_checksum` in `inputSecureFileUploaded`).
    pub md5_checksum: String,
}

// Re-export the `tg-crypto` passport secret primitives so callers have a single module.
pub use tg_crypto::srp::{
    SecureSecretAlgo as SecretKdfAlgo, check_secret_bytes as is_valid_secret_bytes,
    compute_secure_secret_hash as compute_password_hash_for_secret,
    count_secure_secret_id as compute_secret_id, decrypt_secure_secret as decrypt_master_secret,
    encrypt_secure_secret as encrypt_master_secret, generate_secret_bytes as generate_secret,
    validate_new_secure_secret_algo as prepare_new_secret_kdf,
};

fn derive_aes_key_iv(seed_parts: &[&[u8]]) -> ([u8; AES_KEY_LENGTH], [u8; AES_IV_LENGTH]) {
    let mut digest = sha512(seed_parts);
    let mut key = [0u8; AES_KEY_LENGTH];
    let mut iv = [0u8; AES_IV_LENGTH];
    key.copy_from_slice(&digest[..AES_KEY_LENGTH]);
    iv.copy_from_slice(&digest[AES_KEY_LENGTH..AES_KEY_LENGTH + AES_IV_LENGTH]);
    digest.zeroize();
    (key, iv)
}

/// Computes a valid Telegram Passport padding length in `32..=255` such that
/// `(plaintext_len + padding) % 16 == 0` (exact formula from `passport_encryption.cpp:263-269`).
pub fn choose_padding_length(plaintext_len: usize, random_value: u32) -> usize {
    const FROM_PADDING: usize = MIN_PADDING + ALIGN_TO - 1; // 47
    const PADDING_DELTA: usize = MAX_PADDING - FROM_PADDING; // 208
    let random_padding = FROM_PADDING + ((random_value as usize) % PADDING_DELTA);
    let padding = random_padding - ((plaintext_len + random_padding) % ALIGN_TO);
    debug_assert!((MIN_PADDING..=MAX_PADDING).contains(&padding));
    debug_assert!((plaintext_len + padding).is_multiple_of(ALIGN_TO));
    padding
}

/// Encrypts `plaintext` with a freshly generated 32-byte secret (`Passport::EncryptData(bytes)`).
pub fn encrypt_data(plaintext: &[u8]) -> Result<EncryptedData, PassportError> {
    let secret = generate_secret_bytes();
    encrypt_data_with_secret(plaintext, &secret)
}

/// Encrypts `plaintext` with a caller-supplied 32-byte `data_secret` and OS CSPRNG padding
/// (`Passport::EncryptData(bytes, dataSecret)`).
pub fn encrypt_data_with_secret(
    plaintext: &[u8],
    data_secret: &[u8; SECRET_SIZE],
) -> Result<EncryptedData, PassportError> {
    let padding = choose_padding_length(plaintext.len(), random_u32());
    let mut random_pad = vec![0u8; padding - 1];
    random_bytes(&mut random_pad);
    let res = encrypt_data_deterministic(plaintext, data_secret, &random_pad);
    random_pad.zeroize();
    res
}

/// Deterministic variant of [`encrypt_data_with_secret`] using explicit `padding_tail` bytes.
///
/// The total prefix length is `padding = padding_tail.len() + 1`, which must lie in
/// `32..=255` and make `padding + plaintext.len()` a multiple of 16.
///
/// Format:
/// 1. `unencrypted = [padding as u8] || padding_tail || plaintext`
/// 2. `data_hash = SHA256(unencrypted)`
/// 3. `(key, iv) = SHA512(data_secret || data_hash)[0..32], [32..48]`
/// 4. `ciphertext = AES-256-CBC(unencrypted, key, iv)`
pub fn encrypt_data_deterministic(
    plaintext: &[u8],
    data_secret: &[u8; SECRET_SIZE],
    padding_tail: &[u8],
) -> Result<EncryptedData, PassportError> {
    let padding = padding_tail.len() + 1;
    if !(MIN_PADDING..=MAX_PADDING).contains(&padding) {
        return Err(PassportError::InvalidPadding(padding));
    }
    if !(padding + plaintext.len()).is_multiple_of(ALIGN_TO) {
        return Err(PassportError::InvalidAlignment(padding + plaintext.len()));
    }

    let mut unencrypted = Vec::with_capacity(padding + plaintext.len());
    unencrypted.push(padding as u8);
    unencrypted.extend_from_slice(padding_tail);
    unencrypted.extend_from_slice(plaintext);

    let data_hash = sha256(&[&unencrypted]);
    let (mut key, mut iv) = derive_aes_key_iv(&[data_secret, &data_hash]);
    let ciphertext = aes_cbc_encrypt(&unencrypted, &key, &iv)?;
    key.zeroize();
    iv.zeroize();
    unencrypted.zeroize();

    Ok(EncryptedData {
        secret: *data_secret,
        hash: data_hash,
        bytes: ciphertext,
    })
}

/// Decrypts a Passport encrypted payload (`Passport::DecryptData`).
pub fn decrypt_data(
    encrypted: &[u8],
    data_hash: &[u8],
    data_secret: &[u8],
) -> Result<Vec<u8>, PassportError> {
    if encrypted.is_empty() {
        return Err(PassportError::EmptyCiphertext);
    }
    if data_hash.len() != DATA_HASH_SIZE {
        return Err(PassportError::InvalidHashLength(data_hash.len()));
    }
    if data_secret.len() != SECRET_SIZE {
        return Err(PassportError::InvalidSecretLength(data_secret.len()));
    }
    if !encrypted.len().is_multiple_of(ALIGN_TO) {
        return Err(PassportError::InvalidAlignment(encrypted.len()));
    }

    let (mut key, mut iv) = derive_aes_key_iv(&[data_secret, data_hash]);
    let mut decrypted = aes_cbc_decrypt(encrypted, &key, &iv)?;
    key.zeroize();
    iv.zeroize();

    let computed_hash = sha256(&[&decrypted]);
    if !tg_crypto::const_time_eq(&computed_hash, data_hash) {
        decrypted.zeroize();
        return Err(PassportError::HashMismatch);
    }

    let padding = usize::from(decrypted[0]);
    if !(MIN_PADDING..=MAX_PADDING).contains(&padding) || padding > decrypted.len() {
        decrypted.zeroize();
        return Err(PassportError::InvalidPadding(padding));
    }

    let plaintext = decrypted[padding..].to_vec();
    decrypted.zeroize();
    Ok(plaintext)
}

/// Encrypts a per-value or per-file secret under the user's master secret and value/file hash
/// (`Passport::EncryptValueSecret`).
///
/// Derives `(key, iv)` from `SHA512(master_secret || value_hash)` and encrypts `value_secret`
/// (which must pass `sum(bytes) % 255 == 239`) with AES-256-CBC.
pub fn encrypt_value_secret(
    value_secret: &[u8; SECRET_SIZE],
    master_secret: &[u8; SECRET_SIZE],
    value_hash: &[u8],
) -> Result<[u8; SECRET_SIZE], PassportError> {
    if !check_secret_bytes(value_secret) {
        return Err(PassportError::InvalidSecretChecksum);
    }
    let (mut key, mut iv) = derive_aes_key_iv(&[master_secret, value_hash]);
    let enc = aes_cbc_encrypt(value_secret, &key, &iv)?;
    key.zeroize();
    iv.zeroize();
    let mut out = [0u8; SECRET_SIZE];
    out.copy_from_slice(&enc);
    Ok(out)
}

/// Decrypts a per-value or per-file secret under the user's master secret and value/file hash
/// (`Passport::DecryptValueSecret`).
pub fn decrypt_value_secret(
    encrypted_secret: &[u8],
    master_secret: &[u8; SECRET_SIZE],
    value_hash: &[u8],
) -> Result<[u8; SECRET_SIZE], PassportError> {
    if encrypted_secret.len() != SECRET_SIZE {
        return Err(PassportError::InvalidSecretLength(encrypted_secret.len()));
    }
    let (mut key, mut iv) = derive_aes_key_iv(&[master_secret, value_hash]);
    let mut dec = aes_cbc_decrypt(encrypted_secret, &key, &iv)?;
    key.zeroize();
    iv.zeroize();
    if !check_secret_bytes(&dec) {
        dec.zeroize();
        return Err(PassportError::InvalidSecretChecksum);
    }
    let mut out = [0u8; SECRET_SIZE];
    out.copy_from_slice(&dec);
    dec.zeroize();
    Ok(out)
}

/// Encrypts the 32-byte credentials secret with the bot's PEM-encoded RSA public key using
/// RSAES-OAEP (SHA-1, MGF1-SHA1) (`Passport::EncryptCredentialsSecret`).
pub fn encrypt_credentials_secret(
    secret: &[u8],
    public_key_pem: &str,
) -> Result<Vec<u8>, PassportError> {
    let normalized = public_key_pem.replace("\r\n", "\n");
    let key = PublicKey::from_pem(&normalized)?;
    Ok(key.encrypt_oaep(secret)?)
}

/// Encrypts a file's raw bytes for upload and computes its MD5 hex checksum
/// (port of `FormController::encryptFile`).
pub fn encrypt_file_for_upload(
    content: &[u8],
    file_secret: Option<[u8; SECRET_SIZE]>,
) -> Result<EncryptedFileUpload, PassportError> {
    let secret = file_secret.unwrap_or_else(generate_secret_bytes);
    let encrypted = encrypt_data_with_secret(content, &secret)?;
    let md5_checksum = md5_hex(&encrypted.bytes);
    Ok(EncryptedFileUpload {
        file_secret: encrypted.secret,
        hash: encrypted.hash,
        bytes: encrypted.bytes,
        md5_checksum,
    })
}

/// Serializes Passport key-value field map to compact UTF-8 JSON (`Passport::SerializeData`).
pub fn serialize_data(data: &BTreeMap<String, String>) -> Result<Vec<u8>, PassportError> {
    serde_json::to_vec(data).map_err(|e| PassportError::Json(e.to_string()))
}

/// Deserializes Passport key-value field map from decrypted UTF-8 JSON (`Passport::DeserializeData`).
///
/// Mirrors tdesktop's exact value-type handling:
/// - `String` -> preserved verbatim
/// - `Null` -> empty string
/// - `Number` -> formatted as string
/// - `Bool`, `Array`, `Object` -> error
pub fn deserialize_data(bytes: &[u8]) -> Result<BTreeMap<String, String>, PassportError> {
    let value: JsonValue =
        serde_json::from_slice(bytes).map_err(|e| PassportError::Json(e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| PassportError::Json("decrypted JSON root is not an object".to_string()))?;
    let mut result = BTreeMap::new();
    for (key, val) in obj {
        match val {
            JsonValue::String(s) => {
                result.insert(key.clone(), s.clone());
            }
            JsonValue::Null => {
                result.insert(key.clone(), String::new());
            }
            JsonValue::Number(n) => {
                result.insert(key.clone(), n.to_string());
            }
            JsonValue::Bool(_) | JsonValue::Array(_) | JsonValue::Object(_) => {
                return Err(PassportError::Json(format!(
                    "unsupported JSON value type for key '{key}'"
                )));
            }
        }
    }
    Ok(result)
}

/// Encrypts a freshly generated master secret under a password and new PBKDF2 KDF algorithm.
///
/// Returns `(secret, secret_id, encrypted_secret, hash_for_secret, prepared_algo)`.
#[allow(clippy::type_complexity)]
pub fn create_new_master_secret(
    new_secure_algo: SecureSecretAlgo,
    password: &[u8],
) -> Result<
    (
        [u8; SECRET_SIZE],
        u64,
        [u8; SECRET_SIZE],
        [u8; 64],
        SecureSecretAlgo,
    ),
    PassportError,
> {
    let prepared_algo = validate_new_secure_secret_algo(new_secure_algo)?;
    let secret = generate_secret_bytes();
    let secret_id = count_secure_secret_id(&secret);
    let hash_for_secret = compute_secure_secret_hash(&prepared_algo, password);
    let encrypted_secret = encrypt_secure_secret(&secret, &hash_for_secret)?;
    Ok((
        secret,
        secret_id,
        encrypted_secret,
        hash_for_secret,
        prepared_algo,
    ))
}

/// Decrypts and verifies a master secret from server `secureSecretSettings`
/// (`FormController::validateSecureSecret`).
pub fn unlock_master_secret(
    encrypted_secret: &[u8],
    password_hash_for_secret: &[u8; 64],
    expected_secret_id: u64,
) -> Result<[u8; SECRET_SIZE], PassportError> {
    let secret = decrypt_secure_secret(encrypted_secret, password_hash_for_secret)?;
    let computed_id = count_secure_secret_id(&secret);
    if computed_id != expected_secret_id {
        return Err(PassportError::SecretIdMismatch {
            expected: expected_secret_id,
            actual: computed_id,
        });
    }
    Ok(secret)
}
