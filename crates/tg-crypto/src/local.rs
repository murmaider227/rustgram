//! Local storage (`tdata`) key derivation, AES-IGE local encryption, TDF MD5 framing,
//! and offset-based storage CTR encryption.
//!
//! Port of `storage/details/storage_file_utilities.cpp`,
//! `lib_storage/storage/storage_encryption.cpp`, `config.h`, and
//! `mtproto/mtproto_auth_key.h` (`aesEncryptLocal` / `aesDecryptLocal`).

use aes::Aes256;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::aes::{AES_BLOCK_SIZE, increment_be_128, try_aes_ige_decrypt, try_aes_ige_encrypt};
use crate::auth_key::{AUTH_KEY_SIZE, AuthKey, Side};
use crate::error::CryptoError;
use crate::hash::{
    Md5Hasher, const_time_eq, pbkdf2_hmac_sha1, pbkdf2_hmac_sha512, sha1, sha256, sha512,
};
use crate::random::random_bytes;

/// Magic header for tdesktop `tdata` files (`TdfMagic` in `storage_file_utilities.cpp`).
pub const TDF_MAGIC: [u8; 4] = *b"TDF$";

/// Iteration count for modern PBKDF2-HMAC-SHA512 local key derivation with non-empty passcode
/// (`kStrongIterationsCount` in `storage_file_utilities.cpp`).
pub const LOCAL_ENCRYPT_STRONG_ITER_COUNT: u32 = 100_000;

/// Iteration count for legacy PBKDF2-HMAC-SHA1 local key derivation with non-empty passcode
/// (`LocalEncryptIterCount` in `config.h`).
pub const LOCAL_ENCRYPT_ITER_COUNT: u32 = 4_000;

/// Iteration count for legacy PBKDF2-HMAC-SHA1 local key derivation with empty passcode
/// (`LocalEncryptNoPwdIterCount` in `config.h`).
pub const LOCAL_ENCRYPT_NO_PWD_ITER_COUNT: u32 = 4;

/// Default salt size in bytes for `tdata` local keys (`LocalEncryptSaltSize` in `config.h`).
pub const LOCAL_ENCRYPT_SALT_SIZE: usize = 32;

/// Salt size in bytes for `lib_storage` CTR encryption (`Storage::kSaltSize`).
pub const STORAGE_CTR_SALT_SIZE: usize = 64;

/// Minimum salt size for memory-hard passcode KDFs (`kPasscodeSaltMinSize`).
pub const PASSCODE_SALT_MIN_SIZE: usize = 8;

/// Memory-hard passcode KDF family identifier (`storage_file_utilities.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PasscodeKdfKind {
    /// Argon2id (`kPasscodeKdfArgon2id = 1`).
    Argon2id = 1,
    /// Scrypt (`kPasscodeKdfScrypt = 2`).
    Scrypt = 2,
}

impl PasscodeKdfKind {
    /// Convert a raw `u32` tag from `tdata` into [`PasscodeKdfKind`].
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Argon2id),
            2 => Some(Self::Scrypt),
            _ => None,
        }
    }
}

/// Memory-hard passcode KDF parameters stored alongside a wrapped `tdata` passcode
/// (`Storage::details::PasscodeKdf` in `storage/details/storage_file_utilities.h/.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasscodeKdf {
    /// KDF family (`Argon2id` or `Scrypt`).
    pub kind: PasscodeKdfKind,
    /// Memory cost: KiB for Argon2id, `N` (power of 2) for Scrypt.
    pub memory: u32,
    /// Time cost: iterations for Argon2id, `r` for Scrypt.
    pub time: u32,
    /// Parallelism: lanes for Argon2id, `p` for Scrypt.
    pub parallel: u32,
}

impl PasscodeKdf {
    /// Default Argon2id parameters matching 64-bit tdesktop (`DefaultPasscodeKdf`).
    pub const fn default_argon2id() -> Self {
        Self {
            kind: PasscodeKdfKind::Argon2id,
            memory: 65_536,
            time: 8,
            parallel: 1,
        }
    }

    /// Default Scrypt parameters matching 64-bit tdesktop (`DefaultPasscodeKdf` fallback).
    pub const fn default_scrypt() -> Self {
        Self {
            kind: PasscodeKdfKind::Scrypt,
            memory: 131_072,
            time: 8,
            parallel: 1,
        }
    }

    /// Validate structural parameter invariants (`PasscodeKdf::valid`).
    pub const fn is_valid(&self) -> bool {
        if self.time == 0 || self.parallel == 0 {
            return false;
        }
        match self.kind {
            PasscodeKdfKind::Argon2id => match self.parallel.checked_mul(8) {
                Some(min_mem) => self.memory >= min_mem,
                None => false,
            },
            PasscodeKdfKind::Scrypt => self.memory >= 2 && self.memory.is_power_of_two(),
        }
    }

    /// Check that memory/time/parallelism stay within tdesktop's read ceilings
    /// (`PasscodeKdf::costWithinLimits` + `kPasscodeScryptMaxMem`).
    pub const fn cost_within_limits(&self) -> bool {
        const ARGON2_MAX_MEMORY_KIB: u32 = 262_144;
        const ARGON2_MAX_TIME: u32 = 64;
        const ARGON2_MAX_LANES: u32 = 16;
        const SCRYPT_MAX_MEM_BYTES: u64 = 192 * 1024 * 1024;

        match self.kind {
            PasscodeKdfKind::Argon2id => {
                self.memory <= ARGON2_MAX_MEMORY_KIB
                    && self.time <= ARGON2_MAX_TIME
                    && self.parallel <= ARGON2_MAX_LANES
            }
            PasscodeKdfKind::Scrypt => {
                let n = self.memory as u64;
                let r = self.time as u64;
                match n.checked_mul(r) {
                    Some(nr) => match nr.checked_mul(128) {
                        Some(bytes) => bytes <= SCRYPT_MAX_MEM_BYTES,
                        None => false,
                    },
                    None => false,
                }
            }
        }
    }
}

/// Derive a 256-byte local storage key using modern tdesktop PBKDF2-HMAC-SHA512
/// (`Storage::details::CreateLocalKey` in `storage_file_utilities.cpp:445-465`).
///
/// Computes `hash = SHA512(salt || passcode || salt)` then derives 256 bytes via
/// `PBKDF2-HMAC-SHA512(hash, salt, iterations)` where `iterations = 1` if `passcode` is empty
/// and `100_000` otherwise.
pub fn create_local_key(passcode: &[u8], salt: &[u8]) -> AuthKey {
    let mut hash = sha512(&[salt, passcode, salt]);
    let iterations = if passcode.is_empty() {
        1
    } else {
        LOCAL_ENCRYPT_STRONG_ITER_COUNT
    };
    let mut key = [0u8; AUTH_KEY_SIZE];
    pbkdf2_hmac_sha512(&hash, salt, iterations, &mut key);
    hash.zeroize();
    AuthKey::new(key)
}

/// Derive a 256-byte local storage key using legacy tdesktop PBKDF2-HMAC-SHA1
/// (`Storage::details::CreateLegacyLocalKey` in `storage_file_utilities.cpp:467-485`).
///
/// Uses `4` iterations when `passcode` is empty and `4_000` iterations otherwise.
pub fn create_legacy_local_key(passcode: &[u8], salt: &[u8]) -> AuthKey {
    let iterations = if passcode.is_empty() {
        LOCAL_ENCRYPT_NO_PWD_ITER_COUNT
    } else {
        LOCAL_ENCRYPT_ITER_COUNT
    };
    let mut key = [0u8; AUTH_KEY_SIZE];
    pbkdf2_hmac_sha1(passcode, salt, iterations, &mut key);
    AuthKey::new(key)
}

/// Derive a 256-byte local storage key using Argon2id or Scrypt
/// (`Storage::details::CreatePasscodeKey` in `storage_file_utilities.cpp:535-619`).
pub fn create_passcode_key(
    passcode: &[u8],
    salt: &[u8],
    kdf: PasscodeKdf,
) -> Result<AuthKey, CryptoError> {
    if passcode.is_empty()
        || salt.len() < PASSCODE_SALT_MIN_SIZE
        || !kdf.is_valid()
        || !kdf.cost_within_limits()
    {
        return Err(CryptoError::LocalStorageFailed(
            "invalid passcode, salt, or KDF parameters",
        ));
    }

    let mut key = [0u8; AUTH_KEY_SIZE];
    match kdf.kind {
        PasscodeKdfKind::Argon2id => {
            let params = Params::new(kdf.memory, kdf.time, kdf.parallel, Some(AUTH_KEY_SIZE))
                .map_err(|_| CryptoError::LocalStorageFailed("invalid Argon2id parameters"))?;
            let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
            argon2
                .hash_password_into(passcode, salt, &mut key)
                .map_err(|_| CryptoError::LocalStorageFailed("Argon2id derivation failed"))?;
        }
        PasscodeKdfKind::Scrypt => {
            let log_n = kdf.memory.trailing_zeros() as u8;
            // `scrypt::scrypt` uses `output.len()` directly (256 bytes); `Params::new`'s
            // `len` parameter is only used by the `PasswordHasher` trait and caps at 64.
            let params = scrypt::Params::new(
                log_n,
                kdf.time,
                kdf.parallel,
                scrypt::Params::RECOMMENDED_LEN,
            )
            .map_err(|_| CryptoError::LocalStorageFailed("invalid Scrypt parameters"))?;
            scrypt::scrypt(passcode, salt, &params, &mut key)
                .map_err(|_| CryptoError::LocalStorageFailed("Scrypt derivation failed"))?;
        }
    }
    Ok(AuthKey::new(key))
}

/// Raw local storage AES-256-IGE encryption (`MTP::aesEncryptLocal` in `mtproto_auth_key.h`).
///
/// Derives `(aes_key, aes_iv)` using legacy MTProto 1.0 key derivation with `x = 8`
/// (`Side::Server`, matching `prepareAES_oldmtp(..., send = false)` in tdesktop).
pub fn aes_encrypt_local(
    data: &[u8],
    auth_key: &AuthKey,
    key128: &[u8; 16],
) -> Result<Vec<u8>, CryptoError> {
    let (mut aes_key, mut aes_iv) = auth_key.prepare_aes_v1(key128, Side::Server);
    let res = try_aes_ige_encrypt(data, &aes_key, &aes_iv);
    aes_key.zeroize();
    aes_iv.zeroize();
    res
}

/// Raw local storage AES-256-IGE decryption (`MTP::aesDecryptLocal` in `mtproto_auth_key.h`).
///
/// Derives `(aes_key, aes_iv)` using legacy MTProto 1.0 key derivation with `x = 8`
/// (`Side::Server`, matching `prepareAES_oldmtp(..., send = false)` in tdesktop).
pub fn aes_decrypt_local(
    data: &[u8],
    auth_key: &AuthKey,
    key128: &[u8; 16],
) -> Result<Vec<u8>, CryptoError> {
    let (mut aes_key, mut aes_iv) = auth_key.prepare_aes_v1(key128, Side::Server);
    let res = try_aes_ige_decrypt(data, &aes_key, &aes_iv);
    aes_key.zeroize();
    aes_iv.zeroize();
    res
}

/// Encrypt a `tdata` payload block using tdesktop's local encrypted format
/// (`Storage::details::PrepareEncrypted` in `storage_file_utilities.cpp:732-751`).
///
/// Wire format:
/// `key128 (16 bytes = SHA1(plaintext_block)[0..16]) || aes_encrypt_local(plaintext_block, key, key128)`
/// where `plaintext_block = (4 + payload.len() as u32).to_le_bytes() || payload || random_padding`
/// padded to a multiple of 16 bytes.
pub fn encrypt_local_data(payload: &[u8], key: &AuthKey) -> Result<Vec<u8>, CryptoError> {
    let size = payload
        .len()
        .checked_add(4)
        .and_then(|s| u32::try_from(s).ok())
        .ok_or(CryptoError::InvalidLength("local payload exceeds u32"))? as usize;

    let rem = size % AES_BLOCK_SIZE;
    let full_size = if rem == 0 {
        size
    } else {
        size + (AES_BLOCK_SIZE - rem)
    };

    let mut to_encrypt = vec![0u8; full_size];
    to_encrypt[0..4].copy_from_slice(&(size as u32).to_le_bytes());
    to_encrypt[4..size].copy_from_slice(payload);
    if full_size > size {
        random_bytes(&mut to_encrypt[size..]);
    }

    let digest = sha1(&[&to_encrypt]);
    let mut key128 = [0u8; 16];
    key128.copy_from_slice(&digest[0..16]);

    let ciphertext = aes_encrypt_local(&to_encrypt, key, &key128)?;
    to_encrypt.zeroize();

    let mut out = Vec::with_capacity(16 + ciphertext.len());
    out.extend_from_slice(&key128);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a `tdata` encrypted block and verify its SHA-1 `key128` prefix and length header
/// (`Storage::details::DecryptLocal` in `storage_file_utilities.cpp:864-901`).
pub fn decrypt_local_data(encrypted: &[u8], key: &AuthKey) -> Result<Vec<u8>, CryptoError> {
    if encrypted.len() <= 16 || !encrypted.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::LocalStorageFailed(
            "bad encrypted local block size",
        ));
    }
    let mut key128 = [0u8; 16];
    key128.copy_from_slice(&encrypted[0..16]);
    let ciphertext = &encrypted[16..];
    let full_len = ciphertext.len();

    let mut decrypted = aes_decrypt_local(ciphertext, key, &key128)?;
    let digest = sha1(&[&decrypted]);
    if !const_time_eq(&digest[0..16], &key128) {
        decrypted.zeroize();
        return Err(CryptoError::LocalStorageFailed(
            "SHA-1 verification failed (wrong local key or corrupted data)",
        ));
    }

    let data_len = u32::from_le_bytes(
        decrypted[0..4]
            .try_into()
            .map_err(|_| CryptoError::LocalStorageFailed("missing decrypted length header"))?,
    ) as usize;
    if data_len < 4 || data_len > full_len || data_len <= full_len - AES_BLOCK_SIZE {
        decrypted.zeroize();
        return Err(CryptoError::LocalStorageFailed(
            "decrypted length header out of valid range",
        ));
    }

    let payload = decrypted[4..data_len].to_vec();
    decrypted.zeroize();
    Ok(payload)
}

/// Compute the 16-byte trailing MD5 checksum of a `TDF$` file
/// (`FileWriteDescriptor::finish` and `ReadFile` in `storage_file_utilities.cpp:711-715, 834-839`).
///
/// Computes `MD5(data || (data.len() as i32).to_le_bytes() || version.to_le_bytes() || b"TDF$")`.
pub fn tdf_checksum(data: &[u8], version: i32) -> Result<[u8; 16], CryptoError> {
    let data_len = i32::try_from(data.len())
        .map_err(|_| CryptoError::InvalidLength("TDF data exceeds i32::MAX"))?;
    let mut md5 = Md5Hasher::new();
    md5.update(data);
    md5.update(&data_len.to_le_bytes());
    md5.update(&version.to_le_bytes());
    md5.update(&TDF_MAGIC);
    Ok(md5.finalize())
}

/// Verify the 16-byte trailing MD5 checksum of a `TDF$` file in constant time.
pub fn verify_tdf_checksum(data: &[u8], version: i32, expected_md5: &[u8; 16]) -> bool {
    match tdf_checksum(data, version) {
        Ok(computed) => const_time_eq(&computed, expected_md5),
        Err(_) => false,
    }
}

/// Offset-based AES-256-CTR state used by tdesktop's encrypted cache database
/// (`Storage::CtrState` in `lib_storage/storage/storage_encryption.cpp`).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct StorageCtrState {
    key: [u8; 32],
    iv: [u8; AES_BLOCK_SIZE],
}

impl StorageCtrState {
    /// Construct a new `StorageCtrState` from a 32-byte AES key and 16-byte base IV.
    pub const fn new(key: [u8; 32], iv: [u8; AES_BLOCK_SIZE]) -> Self {
        Self { key, iv }
    }

    /// Encrypt `data` in place at byte `offset` (both `data.len()` and `offset` must be
    /// multiples of 16 bytes).
    pub fn encrypt(&self, data: &mut [u8], offset: u64) -> Result<(), CryptoError> {
        self.process(data, offset)
    }

    /// Decrypt `data` in place at byte `offset` (both `data.len()` and `offset` must be
    /// multiples of 16 bytes).
    pub fn decrypt(&self, data: &mut [u8], offset: u64) -> Result<(), CryptoError> {
        self.process(data, offset)
    }

    fn process(&self, data: &mut [u8], offset: u64) -> Result<(), CryptoError> {
        if !data.len().is_multiple_of(AES_BLOCK_SIZE) {
            return Err(CryptoError::InvalidBlockLength { len: data.len() });
        }
        if !offset.is_multiple_of(AES_BLOCK_SIZE as u64) {
            return Err(CryptoError::LocalStorageFailed(
                "CTR offset must be a multiple of 16 bytes",
            ));
        }

        let block_index = offset / (AES_BLOCK_SIZE as u64);
        let mut iv = self.incremented_iv(block_index);
        let cipher = Aes256::new(GenericArray::from_slice(&self.key));

        for chunk in data.as_chunks_mut::<AES_BLOCK_SIZE>().0 {
            let mut keystream = iv;
            cipher.encrypt_block(GenericArray::from_mut_slice(&mut keystream));
            increment_be_128(&mut iv);
            for (b, k) in chunk.iter_mut().zip(keystream.iter()) {
                *b ^= *k;
            }
        }
        Ok(())
    }

    fn incremented_iv(&self, block_index: u64) -> [u8; AES_BLOCK_SIZE] {
        if block_index == 0 {
            return self.iv;
        }
        let mut result = self.iv;
        let mut carry = block_index;
        for byte in result.iter_mut().rev() {
            carry += u64::from(*byte);
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
            if carry == 0 {
                break;
            }
        }
        result
    }
}

/// Encrypted cache database key (`Storage::EncryptionKey` in `lib_storage/storage/storage_encryption.cpp`).
///
/// Accepts either v1 (256-byte) or v2 (64-byte) key material.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct StorageEncryptionKey {
    data: Vec<u8>,
}

impl StorageEncryptionKey {
    /// Legacy v1 key length (256 bytes).
    pub const SIZE_V1: usize = 256;
    /// Current v2 key length (64 bytes).
    pub const SIZE_V2: usize = 64;

    /// Construct a `StorageEncryptionKey` from 64-byte (v2) or 256-byte (v1) key material.
    pub fn new(data: Vec<u8>) -> Result<Self, CryptoError> {
        if data.len() != Self::SIZE_V1 && data.len() != Self::SIZE_V2 {
            return Err(CryptoError::InvalidLength(
                "StorageEncryptionKey must be 64 or 256 bytes",
            ));
        }
        Ok(Self { data })
    }

    /// Return the raw key bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Derive a per-entry [`StorageCtrState`] from a 64-byte salt
    /// (`Storage::EncryptionKey::prepareCtrState`).
    pub fn prepare_ctr_state(&self, salt: &[u8; STORAGE_CTR_SALT_SIZE]) -> StorageCtrState {
        let half_key = self.data.len() / 2;
        let half_salt = STORAGE_CTR_SALT_SIZE / 2;

        let key = sha256(&[&self.data[..half_key], &salt[..half_salt]]);
        let iv_full = sha256(&[&self.data[half_key..], &salt[half_salt..]]);
        let mut iv = [0u8; AES_BLOCK_SIZE];
        iv.copy_from_slice(&iv_full[..AES_BLOCK_SIZE]);

        StorageCtrState::new(key, iv)
    }
}
