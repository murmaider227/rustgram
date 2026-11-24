//! Local passcode-encrypted wallet key vault.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_vault.h` and
//! `Telegram/SourceFiles/wallet/wallet_vault.cpp`.
//!
//! Preserves tdesktop's exact constants, HKDF-SHA256 labels, AES-256-GCM AAD layouts,
//! keyring/record magic headers (`0x574B5247` / `0x574B5243`), and key-rotation semantics.

use std::collections::HashMap;
use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use rand_core::{OsRng, RngCore};
use sha2::{Sha256, Sha512};
use zeroize::Zeroizing;

use crate::WalletError;

type HmacSha256 = Hmac<Sha256>;

/// Device keyring magic (`"WKRG"` = `0x574B5247`, `kDeviceKeyringMagic` in `wallet_vault.cpp`).
pub const DEVICE_KEYRING_MAGIC: u32 = 0x574B_5247;
/// Device keyring format version (`kDeviceKeyringFormatVersion`).
pub const DEVICE_KEYRING_FORMAT_VERSION: u32 = 1;
/// Vault record magic (`"WKRC"` = `0x574B5243`, `kVaultRecordMagic`).
pub const VAULT_RECORD_MAGIC: u32 = 0x574B_5243;
/// Vault record format version (`kVaultRecordFormatVersion`).
pub const VAULT_RECORD_FORMAT_VERSION: u32 = 1;
/// AEAD suite identifier (`kVaultAeadAesGcm`).
pub const VAULT_AEAD_AES_GCM: u32 = 1;

/// Symmetric key size (32 bytes = AES-256).
pub const VAULT_KEY_SIZE: usize = 32;
/// Keyring entry identifier size (16 bytes).
pub const VAULT_KEY_ID_SIZE: usize = 16;
/// AES-GCM nonce size (12 bytes).
pub const VAULT_NONCE_SIZE: usize = 12;
/// AES-GCM authentication tag size (16 bytes).
pub const VAULT_TAG_SIZE: usize = 16;
/// KDF salt size (32 bytes).
pub const VAULT_SALT_SIZE: usize = 32;
/// Open-vault random secret size (32 bytes).
pub const VAULT_OPEN_SECRET_SIZE: usize = 32;
/// Wrapped device key blob length (`12 + 32 + 16 = 60` bytes).
pub const VAULT_BLOB_SIZE: usize = VAULT_NONCE_SIZE + VAULT_KEY_SIZE + VAULT_TAG_SIZE;

const VAULT_REQUIRE_USER_PRESENCE_FLAG: u32 = 1 << 0;

// Exact HKDF-SHA256 / AAD domain separation labels from `wallet_vault.cpp:49-54`.
const LABEL_VAULT_KEY: &[u8] = b"tdesktop-wallet-keyring/key/v1";
const LABEL_PASSCODE_WRAP: &[u8] = b"tdesktop-wallet-keyring/passcode-wrap/v1";
const LABEL_OPEN_WRAP: &[u8] = b"tdesktop-wallet-keyring/open-wrap/v1";
const LABEL_KEYRING_ENTRY: &[u8] = b"tdesktop-wallet-keyring/entry/v1";
const LABEL_VAULT_RECORD: &[u8] = b"tdesktop-wallet-keyring/record/v1";

/// Vault protection mechanism (`Wallet::VaultKind` in `wallet_vault.h:50-55`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VaultKind {
    /// Protected by a user passcode.
    Passcode = 1,
    /// Open (unprotected) vault using a random stored secret.
    Open = 2,
    /// macOS Touch ID hardware provider.
    TouchId = 3,
    /// Windows Hello hardware provider.
    WindowsHello = 4,
}

impl VaultKind {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Passcode),
            2 => Some(Self::Open),
            3 => Some(Self::TouchId),
            4 => Some(Self::WindowsHello),
            _ => None,
        }
    }
}

/// Passcode KDF parameters (`Storage::details::PasscodeKdf`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasscodeKdf {
    /// `1` = Argon2id, `2` = PBKDF2-HMAC-SHA512, `0` = none (for Open wrap).
    pub kind: u32,
    /// Memory cost in KiB (Argon2id).
    pub memory: u32,
    /// Iterations / time cost.
    pub time: u32,
    /// Parallelism lanes (Argon2id).
    pub parallel: u32,
}

impl PasscodeKdf {
    /// Empty KDF parameters for non-passcode wraps.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            kind: 0,
            memory: 0,
            time: 0,
            parallel: 0,
        }
    }

    /// Default Argon2id parameters (64 MiB, 3 iterations, 4 lanes).
    #[must_use]
    pub const fn default_argon2id() -> Self {
        Self {
            kind: 1,
            memory: 64 * 1024,
            time: 3,
            parallel: 4,
        }
    }

    /// Lightweight Argon2id parameters for fast unit tests.
    #[must_use]
    pub const fn fast_test_kdf() -> Self {
        Self {
            kind: 1,
            memory: 1024,
            time: 1,
            parallel: 1,
        }
    }

    /// PBKDF2-HMAC-SHA512 fallback parameters.
    #[must_use]
    pub const fn pbkdf2(iterations: u32) -> Self {
        Self {
            kind: 2,
            memory: 0,
            time: iterations,
            parallel: 1,
        }
    }

    /// Checks whether the KDF parameters are within safe limits.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        match self.kind {
            1 => {
                self.memory >= 64
                    && self.memory <= 512 * 1024
                    && self.time >= 1
                    && self.time <= 64
                    && self.parallel >= 1
                    && self.parallel <= 16
            }
            2 => self.time >= 1 && self.time <= 2_000_000,
            _ => false,
        }
    }
}

/// Move-friendly zeroizing byte buffer (`Wallet::SecureBytes`).
#[derive(Clone, Default)]
pub struct SecureBytes(Zeroizing<Vec<u8>>);

impl std::fmt::Debug for SecureBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecureBytes")
            .field(&format!("[{} bytes redacted]", self.0.len()))
            .finish()
    }
}

impl SecureBytes {
    /// Allocates `size` zeroed bytes.
    #[must_use]
    pub fn zeroed(size: usize) -> Self {
        Self(Zeroizing::new(vec![0_u8; size]))
    }

    /// Generates `size` cryptographically random bytes.
    #[must_use]
    pub fn random(size: usize) -> Self {
        let mut buf = vec![0_u8; size];
        OsRng.fill_bytes(&mut buf);
        Self(Zeroizing::new(buf))
    }

    /// Copies bytes into a zeroizing buffer.
    #[must_use]
    pub fn from_slice(data: &[u8]) -> Self {
        Self(Zeroizing::new(data.to_vec()))
    }

    /// Borrows the secret bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Encrypted device key wrapper (`Wallet::VaultWrap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultWrap {
    /// Protection kind.
    pub kind: VaultKind,
    /// KDF parameters (for `Passcode`).
    pub kdf: PasscodeKdf,
    /// 32-byte salt.
    pub salt: [u8; VAULT_SALT_SIZE],
    /// Open-vault random secret (32 bytes for `Open`, empty for `Passcode`).
    pub open_secret: Vec<u8>,
    /// `nonce(12) || ciphertext(32) || tag(16)` = 60 bytes.
    pub blob: Vec<u8>,
}

/// Per-secret keyring entry (`Wallet::DeviceKeyringEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceKeyringEntry {
    /// 16-byte random key identifier.
    pub key_id: [u8; VAULT_KEY_ID_SIZE],
    /// Account identifier (`!= 0`).
    pub account_id: u64,
    /// 12-byte AES-GCM nonce.
    pub nonce: [u8; VAULT_NONCE_SIZE],
    /// Sealed 32-byte record key (`32 + 16 = 48` bytes).
    pub sealed: Vec<u8>,
}

/// Full serialized device keyring (`Wallet::DeviceKeyring`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceKeyring {
    /// Master device key wrap.
    pub wrap: VaultWrap,
    /// Per-record wrapped keys `K`.
    pub entries: Vec<DeviceKeyringEntry>,
}

/// Prepared wrap parameters + derived wrap key (`Wallet::VaultPreparedWrap`).
#[derive(Debug, Clone)]
pub struct VaultPreparedWrap {
    /// Wrap metadata (blob filled once a device key is wrapped).
    pub wrap: VaultWrap,
    /// 32-byte derived wrap key.
    pub wrap_key: SecureBytes,
}

/// Decrypted vault record payload (`Wallet::VaultSecretRecord`).
#[derive(Debug, Clone)]
pub struct VaultSecretRecord {
    /// Whether this record required user presence.
    pub require_user_presence: bool,
    /// Decrypted secret payload bytes.
    pub bytes: SecureBytes,
}

/// Result of rotating a device keyring (`Wallet::DeviceKeyringRotation`).
#[derive(Debug, Clone)]
pub struct DeviceKeyringRotation {
    /// Updated keyring sealed under the new device key and new wrap.
    pub keyring: DeviceKeyring,
    /// Newly minted 32-byte device key `D`.
    pub device_key: SecureBytes,
}

/// RFC 5869 HKDF-SHA256 matching `Wallet::HkdfSha256` (`wallet_vault.cpp:394-433`).
pub fn hkdf_sha256(
    ikm: &[u8],
    salt: &[u8],
    info: &[u8],
    len: usize,
) -> Result<SecureBytes, WalletError> {
    if ikm.is_empty() || len == 0 || len > 255 * 32 {
        return Err(WalletError::Vault("invalid HKDF parameters".to_owned()));
    }
    let zero_salt = [0_u8; 32];
    let effective_salt = if salt.is_empty() {
        &zero_salt[..]
    } else {
        salt
    };
    let mut extract = <HmacSha256 as Mac>::new_from_slice(effective_salt)
        .map_err(|e| WalletError::Vault(format!("HKDF extract init: {e}")))?;
    extract.update(ikm);
    let prk = Zeroizing::new(extract.finalize().into_bytes());

    let mut out = Vec::with_capacity(len);
    let mut prev: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < len {
        let mut expand = <HmacSha256 as Mac>::new_from_slice(&prk)
            .map_err(|e| WalletError::Vault(format!("HKDF expand init: {e}")))?;
        expand.update(&prev);
        expand.update(info);
        expand.update(&[counter]);
        let block = expand.finalize().into_bytes();
        let take = (len - out.len()).min(block.len());
        out.extend_from_slice(&block[..take]);
        prev = block.to_vec();
        counter = counter
            .checked_add(1)
            .ok_or_else(|| WalletError::Vault("HKDF counter overflow".to_owned()))?;
    }
    Ok(SecureBytes(Zeroizing::new(out)))
}

fn aes_gcm_seal(
    key32: &[u8],
    nonce12: &[u8],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, WalletError> {
    if key32.len() != VAULT_KEY_SIZE || nonce12.len() != VAULT_NONCE_SIZE {
        return Err(WalletError::Vault(
            "invalid AES-GCM key or nonce size".to_owned(),
        ));
    }
    let cipher = Aes256Gcm::new_from_slice(key32)
        .map_err(|e| WalletError::Vault(format!("AES-GCM init: {e}")))?;
    let nonce = Nonce::from_slice(nonce12);
    cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| WalletError::Vault(format!("AES-GCM seal failed: {e}")))
}

fn aes_gcm_open(
    key32: &[u8],
    nonce12: &[u8],
    sealed: &[u8],
    aad: &[u8],
) -> Result<SecureBytes, WalletError> {
    if key32.len() != VAULT_KEY_SIZE
        || nonce12.len() != VAULT_NONCE_SIZE
        || sealed.len() < VAULT_TAG_SIZE
    {
        return Err(WalletError::Vault(
            "invalid AES-GCM open parameters".to_owned(),
        ));
    }
    let cipher = Aes256Gcm::new_from_slice(key32)
        .map_err(|e| WalletError::Vault(format!("AES-GCM init: {e}")))?;
    let nonce = Nonce::from_slice(nonce12);
    let plain = cipher
        .decrypt(nonce, Payload { msg: sealed, aad })
        .map_err(|_| {
            WalletError::Vault(
                "AES-GCM authentication failed (wrong passcode or corrupt data)".to_owned(),
            )
        })?;
    Ok(SecureBytes(Zeroizing::new(plain)))
}

fn write_bytes_field(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
}

fn read_u32_be(data: &[u8], pos: &mut usize) -> Result<u32, WalletError> {
    if data.len().saturating_sub(*pos) < 4 {
        return Err(WalletError::Vault("unexpected EOF reading u32".to_owned()));
    }
    let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

fn read_u64_be(data: &[u8], pos: &mut usize) -> Result<u64, WalletError> {
    if data.len().saturating_sub(*pos) < 8 {
        return Err(WalletError::Vault("unexpected EOF reading u64".to_owned()));
    }
    let val = u64::from_be_bytes([
        data[*pos],
        data[*pos + 1],
        data[*pos + 2],
        data[*pos + 3],
        data[*pos + 4],
        data[*pos + 5],
        data[*pos + 6],
        data[*pos + 7],
    ]);
    *pos += 8;
    Ok(val)
}

fn read_bytes_field(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, WalletError> {
    let len = read_u32_be(data, pos)? as usize;
    if data.len().saturating_sub(*pos) < len {
        return Err(WalletError::Vault(
            "unexpected EOF reading byte field".to_owned(),
        ));
    }
    let out = data[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(out)
}

fn wrap_aad(wrap: &VaultWrap) -> Vec<u8> {
    let mut aad = Vec::with_capacity(LABEL_VAULT_KEY.len() + 4);
    aad.extend_from_slice(LABEL_VAULT_KEY);
    aad.extend_from_slice(&(wrap.kind as u32).to_be_bytes());
    aad
}

fn entry_aad(key_id: &[u8; VAULT_KEY_ID_SIZE], account_id: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(LABEL_KEYRING_ENTRY.len() + 4 + VAULT_KEY_ID_SIZE + 8);
    aad.extend_from_slice(LABEL_KEYRING_ENTRY);
    write_bytes_field(&mut aad, key_id);
    aad.extend_from_slice(&account_id.to_be_bytes());
    aad
}

fn record_aad(storage_key: &str, key_id: &[u8; VAULT_KEY_ID_SIZE]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(
        LABEL_VAULT_RECORD.len() + 4 + VAULT_KEY_ID_SIZE + 4 + storage_key.len(),
    );
    aad.extend_from_slice(LABEL_VAULT_RECORD);
    write_bytes_field(&mut aad, key_id);
    write_bytes_field(&mut aad, storage_key.as_bytes());
    aad
}

/// Derives the 32-byte wrap key from a [`VaultWrap`] and user passcode (`DeriveVaultWrapKey`).
pub fn derive_vault_wrap_key(
    wrap: &VaultWrap,
    passcode: &[u8],
) -> Result<SecureBytes, WalletError> {
    match wrap.kind {
        VaultKind::Passcode => {
            if passcode.is_empty() {
                return Err(WalletError::Vault("passcode cannot be empty".to_owned()));
            }
            if !wrap.kdf.is_valid() {
                return Err(WalletError::Vault(
                    "invalid passcode KDF parameters".to_owned(),
                ));
            }
            let mut ikm = Zeroizing::new([0_u8; 32]);
            match wrap.kdf.kind {
                1 => {
                    let params =
                        Params::new(wrap.kdf.memory, wrap.kdf.time, wrap.kdf.parallel, Some(32))
                            .map_err(|e| WalletError::Vault(format!("Argon2 params: {e}")))?;
                    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
                    argon
                        .hash_password_into(passcode, &wrap.salt, &mut *ikm)
                        .map_err(|e| WalletError::Vault(format!("Argon2 derive: {e}")))?;
                }
                2 => {
                    pbkdf2_hmac::<Sha512>(passcode, &wrap.salt, wrap.kdf.time, &mut *ikm);
                }
                _ => {
                    return Err(WalletError::Vault("unsupported KDF family".to_owned()));
                }
            }
            hkdf_sha256(&*ikm, &[], LABEL_PASSCODE_WRAP, VAULT_KEY_SIZE)
        }
        VaultKind::Open => {
            if wrap.open_secret.len() != VAULT_OPEN_SECRET_SIZE {
                return Err(WalletError::Vault("invalid open secret length".to_owned()));
            }
            hkdf_sha256(
                &wrap.open_secret,
                &wrap.salt,
                LABEL_OPEN_WRAP,
                VAULT_KEY_SIZE,
            )
        }
        VaultKind::TouchId | VaultKind::WindowsHello => Err(WalletError::Vault(
            "hardware vault provider requires OS credential unwrap".to_owned(),
        )),
    }
}

/// Prepares a new passcode wrap using the given KDF (`PrepareVaultPasscodeWrap`).
pub fn prepare_vault_passcode_wrap_with_kdf(
    passcode: &[u8],
    kdf: PasscodeKdf,
) -> Result<VaultPreparedWrap, WalletError> {
    let mut salt = [0_u8; VAULT_SALT_SIZE];
    OsRng.fill_bytes(&mut salt);
    let wrap = VaultWrap {
        kind: VaultKind::Passcode,
        kdf,
        salt,
        open_secret: Vec::new(),
        blob: Vec::new(),
    };
    let wrap_key = derive_vault_wrap_key(&wrap, passcode)?;
    Ok(VaultPreparedWrap { wrap, wrap_key })
}

/// Prepares a new passcode wrap using Argon2id defaults (`PrepareVaultPasscodeWrap`).
pub fn prepare_vault_passcode_wrap(passcode: &[u8]) -> Result<VaultPreparedWrap, WalletError> {
    prepare_vault_passcode_wrap_with_kdf(passcode, PasscodeKdf::default_argon2id())
}

/// Prepares an open (non-passcode) wrap (`PrepareVaultOpenWrap`).
pub fn prepare_vault_open_wrap() -> Result<VaultPreparedWrap, WalletError> {
    let mut salt = [0_u8; VAULT_SALT_SIZE];
    let mut open_secret = vec![0_u8; VAULT_OPEN_SECRET_SIZE];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut open_secret);
    let wrap = VaultWrap {
        kind: VaultKind::Open,
        kdf: PasscodeKdf::empty(),
        salt,
        open_secret,
        blob: Vec::new(),
    };
    let wrap_key = derive_vault_wrap_key(&wrap, &[])?;
    Ok(VaultPreparedWrap { wrap, wrap_key })
}

/// Wraps a 32-byte device key under `wrap_key` (`WrapVaultKey`).
pub fn wrap_vault_key(
    device_key: &SecureBytes,
    wrap: &VaultWrap,
    wrap_key: &SecureBytes,
) -> Result<Vec<u8>, WalletError> {
    if device_key.len() != VAULT_KEY_SIZE || wrap_key.len() != VAULT_KEY_SIZE {
        return Err(WalletError::Vault("expected 32-byte keys".to_owned()));
    }
    let mut nonce = [0_u8; VAULT_NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    let aad = wrap_aad(wrap);
    let sealed = aes_gcm_seal(wrap_key.as_slice(), &nonce, device_key.as_slice(), &aad)?;
    let mut blob = Vec::with_capacity(VAULT_BLOB_SIZE);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&sealed);
    Ok(blob)
}

/// Unwraps the 32-byte device key from `wrap.blob` (`UnwrapVaultKey`).
pub fn unwrap_vault_key(
    wrap: &VaultWrap,
    wrap_key: &SecureBytes,
) -> Result<SecureBytes, WalletError> {
    if wrap.blob.len() != VAULT_BLOB_SIZE || wrap_key.len() != VAULT_KEY_SIZE {
        return Err(WalletError::Vault("malformed vault wrap blob".to_owned()));
    }
    let nonce = &wrap.blob[..VAULT_NONCE_SIZE];
    let sealed = &wrap.blob[VAULT_NONCE_SIZE..];
    let aad = wrap_aad(wrap);
    let key = aes_gcm_open(wrap_key.as_slice(), nonce, sealed, &aad)?;
    if key.len() != VAULT_KEY_SIZE {
        return Err(WalletError::Vault("unwrapped key size mismatch".to_owned()));
    }
    Ok(key)
}

/// Seals a per-record key `K` into a [`DeviceKeyringEntry`] (`SealDeviceKeyringEntry`).
pub fn seal_keyring_entry(
    device_key: &SecureBytes,
    key_id: [u8; VAULT_KEY_ID_SIZE],
    account_id: u64,
    record_key: &SecureBytes,
) -> Result<DeviceKeyringEntry, WalletError> {
    if device_key.len() != VAULT_KEY_SIZE || record_key.len() != VAULT_KEY_SIZE || account_id == 0 {
        return Err(WalletError::Vault(
            "invalid keyring entry inputs".to_owned(),
        ));
    }
    let mut nonce = [0_u8; VAULT_NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    let aad = entry_aad(&key_id, account_id);
    let sealed = aes_gcm_seal(device_key.as_slice(), &nonce, record_key.as_slice(), &aad)?;
    Ok(DeviceKeyringEntry {
        key_id,
        account_id,
        nonce,
        sealed,
    })
}

/// Opens a [`DeviceKeyringEntry`] with `device_key` (`OpenDeviceKeyringEntry`).
pub fn open_keyring_entry(
    device_key: &SecureBytes,
    entry: &DeviceKeyringEntry,
) -> Result<SecureBytes, WalletError> {
    if device_key.len() != VAULT_KEY_SIZE || entry.account_id == 0 {
        return Err(WalletError::Vault("invalid keyring entry".to_owned()));
    }
    let aad = entry_aad(&entry.key_id, entry.account_id);
    aes_gcm_open(device_key.as_slice(), &entry.nonce, &entry.sealed, &aad)
}

/// Rotates the device keyring to a new passcode/wrap without re-encrypting stored records
/// (`RotateDeviceKeyring` in `wallet_vault.cpp:1227-1262`).
pub fn rotate_device_keyring(
    current: &DeviceKeyring,
    old_device_key: &SecureBytes,
    next: &VaultPreparedWrap,
) -> Result<DeviceKeyringRotation, WalletError> {
    let new_device_key = SecureBytes::random(VAULT_KEY_SIZE);
    let mut new_wrap = next.wrap.clone();
    new_wrap.blob = wrap_vault_key(&new_device_key, &new_wrap, &next.wrap_key)?;

    let mut new_entries = Vec::with_capacity(current.entries.len());
    for entry in &current.entries {
        let record_key = open_keyring_entry(old_device_key, entry)?;
        let re_sealed =
            seal_keyring_entry(&new_device_key, entry.key_id, entry.account_id, &record_key)?;
        new_entries.push(re_sealed);
    }

    Ok(DeviceKeyringRotation {
        keyring: DeviceKeyring {
            wrap: new_wrap,
            entries: new_entries,
        },
        device_key: new_device_key,
    })
}

/// Serializes a [`DeviceKeyring`] to binary format (`SerializeDeviceKeyring`).
pub fn serialize_device_keyring(keyring: &DeviceKeyring) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&DEVICE_KEYRING_MAGIC.to_be_bytes());
    out.extend_from_slice(&DEVICE_KEYRING_FORMAT_VERSION.to_be_bytes());
    out.extend_from_slice(&VAULT_AEAD_AES_GCM.to_be_bytes());

    out.extend_from_slice(&(keyring.wrap.kind as u32).to_be_bytes());
    out.extend_from_slice(&keyring.wrap.kdf.kind.to_be_bytes());
    out.extend_from_slice(&keyring.wrap.kdf.memory.to_be_bytes());
    out.extend_from_slice(&keyring.wrap.kdf.time.to_be_bytes());
    out.extend_from_slice(&keyring.wrap.kdf.parallel.to_be_bytes());
    write_bytes_field(&mut out, &keyring.wrap.salt);
    write_bytes_field(&mut out, &keyring.wrap.open_secret);
    write_bytes_field(&mut out, &keyring.wrap.blob);

    out.extend_from_slice(&(keyring.entries.len() as u32).to_be_bytes());
    for entry in &keyring.entries {
        write_bytes_field(&mut out, &entry.key_id);
        out.extend_from_slice(&entry.account_id.to_be_bytes());
        write_bytes_field(&mut out, &entry.nonce);
        write_bytes_field(&mut out, &entry.sealed);
    }
    out
}

/// Deserializes a [`DeviceKeyring`] (`ParseDeviceKeyring`).
pub fn parse_device_keyring(data: &[u8]) -> Result<DeviceKeyring, WalletError> {
    let mut pos = 0;
    let magic = read_u32_be(data, &mut pos)?;
    let version = read_u32_be(data, &mut pos)?;
    let aead = read_u32_be(data, &mut pos)?;
    if magic != DEVICE_KEYRING_MAGIC {
        return Err(WalletError::Vault("invalid keyring magic".to_owned()));
    }
    if version != DEVICE_KEYRING_FORMAT_VERSION || aead != VAULT_AEAD_AES_GCM {
        return Err(WalletError::Vault(
            "unsupported keyring version or AEAD".to_owned(),
        ));
    }

    let kind_raw = read_u32_be(data, &mut pos)?;
    let kind = VaultKind::from_u32(kind_raw)
        .ok_or_else(|| WalletError::Vault(format!("unsupported vault kind {kind_raw}")))?;
    let kdf = PasscodeKdf {
        kind: read_u32_be(data, &mut pos)?,
        memory: read_u32_be(data, &mut pos)?,
        time: read_u32_be(data, &mut pos)?,
        parallel: read_u32_be(data, &mut pos)?,
    };
    let salt_vec = read_bytes_field(data, &mut pos)?;
    let salt: [u8; VAULT_SALT_SIZE] = salt_vec
        .try_into()
        .map_err(|_| WalletError::Vault("invalid salt length".to_owned()))?;
    let open_secret = read_bytes_field(data, &mut pos)?;
    let blob = read_bytes_field(data, &mut pos)?;
    if blob.len() != VAULT_BLOB_SIZE {
        return Err(WalletError::Vault("invalid wrap blob length".to_owned()));
    }

    let entry_count = read_u32_be(data, &mut pos)? as usize;
    let mut entries = Vec::with_capacity(entry_count.min(256));
    for _ in 0..entry_count {
        let key_id: [u8; VAULT_KEY_ID_SIZE] = read_bytes_field(data, &mut pos)?
            .try_into()
            .map_err(|_| WalletError::Vault("invalid key_id length".to_owned()))?;
        let account_id = read_u64_be(data, &mut pos)?;
        let nonce: [u8; VAULT_NONCE_SIZE] = read_bytes_field(data, &mut pos)?
            .try_into()
            .map_err(|_| WalletError::Vault("invalid nonce length".to_owned()))?;
        let sealed = read_bytes_field(data, &mut pos)?;
        if account_id == 0 || sealed.len() != VAULT_KEY_SIZE + VAULT_TAG_SIZE {
            return Err(WalletError::Vault("malformed keyring entry".to_owned()));
        }
        entries.push(DeviceKeyringEntry {
            key_id,
            account_id,
            nonce,
            sealed,
        });
    }
    if pos != data.len() {
        return Err(WalletError::Vault("trailing bytes in keyring".to_owned()));
    }

    Ok(DeviceKeyring {
        wrap: VaultWrap {
            kind,
            kdf,
            salt,
            open_secret,
            blob,
        },
        entries,
    })
}

/// Seals a secret record (`SealVaultRecord` in `wallet_vault.cpp:1273-1305`).
pub fn seal_vault_record(
    record_key: &SecureBytes,
    storage_key: &str,
    key_id: [u8; VAULT_KEY_ID_SIZE],
    require_user_presence: bool,
    secret: &[u8],
) -> Result<Vec<u8>, WalletError> {
    if record_key.len() != VAULT_KEY_SIZE || storage_key.is_empty() {
        return Err(WalletError::Vault(
            "invalid seal record parameters".to_owned(),
        ));
    }
    let mut nonce = [0_u8; VAULT_NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);

    let mut plain = Zeroizing::new(Vec::with_capacity(8 + secret.len()));
    let flags = if require_user_presence {
        VAULT_REQUIRE_USER_PRESENCE_FLAG
    } else {
        0
    };
    plain.extend_from_slice(&flags.to_be_bytes());
    write_bytes_field(&mut plain, secret);

    let aad = record_aad(storage_key, &key_id);
    let sealed = aes_gcm_seal(record_key.as_slice(), &nonce, &plain, &aad)?;

    let mut out = Vec::new();
    out.extend_from_slice(&VAULT_RECORD_MAGIC.to_be_bytes());
    out.extend_from_slice(&VAULT_RECORD_FORMAT_VERSION.to_be_bytes());
    write_bytes_field(&mut out, &key_id);
    write_bytes_field(&mut out, &nonce);
    write_bytes_field(&mut out, &sealed);
    Ok(out)
}

/// Reads the 16-byte `key_id` header from a serialized vault record (`ReadVaultRecordKeyId`).
pub fn read_vault_record_key_id(serialized: &[u8]) -> Option<[u8; VAULT_KEY_ID_SIZE]> {
    let mut pos = 0;
    let magic = read_u32_be(serialized, &mut pos).ok()?;
    let version = read_u32_be(serialized, &mut pos).ok()?;
    if magic != VAULT_RECORD_MAGIC || version != VAULT_RECORD_FORMAT_VERSION {
        return None;
    }
    read_bytes_field(serialized, &mut pos).ok()?.try_into().ok()
}

/// Opens a serialized vault record (`OpenVaultRecord` in `wallet_vault.cpp:1307-1344`).
pub fn open_vault_record(
    record_key: &SecureBytes,
    storage_key: &str,
    expected_key_id: &[u8; VAULT_KEY_ID_SIZE],
    serialized: &[u8],
) -> Result<VaultSecretRecord, WalletError> {
    if record_key.len() != VAULT_KEY_SIZE || storage_key.is_empty() {
        return Err(WalletError::Vault(
            "invalid open record parameters".to_owned(),
        ));
    }
    let mut pos = 0;
    let magic = read_u32_be(serialized, &mut pos)?;
    let version = read_u32_be(serialized, &mut pos)?;
    if magic != VAULT_RECORD_MAGIC || version != VAULT_RECORD_FORMAT_VERSION {
        return Err(WalletError::Vault("invalid vault record header".to_owned()));
    }
    let key_id: [u8; VAULT_KEY_ID_SIZE] = read_bytes_field(serialized, &mut pos)?
        .try_into()
        .map_err(|_| WalletError::Vault("invalid record key_id".to_owned()))?;
    if &key_id != expected_key_id {
        return Err(WalletError::Vault(
            "vault record key_id mismatch".to_owned(),
        ));
    }
    let nonce: [u8; VAULT_NONCE_SIZE] = read_bytes_field(serialized, &mut pos)?
        .try_into()
        .map_err(|_| WalletError::Vault("invalid record nonce".to_owned()))?;
    let sealed = read_bytes_field(serialized, &mut pos)?;
    if pos != serialized.len() {
        return Err(WalletError::Vault(
            "trailing bytes in vault record".to_owned(),
        ));
    }

    let aad = record_aad(storage_key, &key_id);
    let plaintext = aes_gcm_open(record_key.as_slice(), &nonce, &sealed, &aad)?;

    let mut inner_pos = 0;
    let flags = read_u32_be(plaintext.as_slice(), &mut inner_pos)?;
    let secret = read_bytes_field(plaintext.as_slice(), &mut inner_pos)?;
    if inner_pos != plaintext.len() || (flags & !VAULT_REQUIRE_USER_PRESENCE_FLAG) != 0 {
        return Err(WalletError::Vault(
            "corrupt vault record plaintext".to_owned(),
        ));
    }
    Ok(VaultSecretRecord {
        require_user_presence: (flags & VAULT_REQUIRE_USER_PRESENCE_FLAG) != 0,
        bytes: SecureBytes(Zeroizing::new(secret)),
    })
}

/// High-level local encrypted key vault holding a [`DeviceKeyring`] and sealed secret records.
#[derive(Debug, Clone)]
pub struct LocalKeyVault {
    account_id: u64,
    keyring: Option<DeviceKeyring>,
    records: HashMap<String, Vec<u8>>,
    unlocked_device_key: Option<SecureBytes>,
}

impl LocalKeyVault {
    /// Creates a new empty local key vault for `account_id` (must be non-zero).
    #[must_use]
    pub fn new(account_id: u64) -> Self {
        Self {
            account_id: account_id.max(1),
            keyring: None,
            records: HashMap::new(),
            unlocked_device_key: None,
        }
    }

    /// Initializes the vault with a passcode and unlocks it immediately.
    pub fn initialize_with_passcode(
        &mut self,
        passcode: &str,
        kdf: PasscodeKdf,
    ) -> Result<(), WalletError> {
        let prepared = prepare_vault_passcode_wrap_with_kdf(passcode.as_bytes(), kdf)?;
        let device_key = SecureBytes::random(VAULT_KEY_SIZE);
        let mut wrap = prepared.wrap;
        wrap.blob = wrap_vault_key(&device_key, &wrap, &prepared.wrap_key)?;
        self.keyring = Some(DeviceKeyring {
            wrap,
            entries: Vec::new(),
        });
        self.records.clear();
        self.unlocked_device_key = Some(device_key);
        Ok(())
    }

    /// Returns `true` if a keyring has been initialized.
    #[must_use]
    pub const fn is_initialized(&self) -> bool {
        self.keyring.is_some()
    }

    /// Returns `true` if the vault is currently unlocked in memory.
    #[must_use]
    pub const fn is_unlocked(&self) -> bool {
        self.unlocked_device_key.is_some()
    }

    /// Locks the vault and wipes the in-memory device key.
    pub fn lock(&mut self) {
        self.unlocked_device_key = None;
    }

    /// Unlocks the vault by verifying `passcode` against the keyring wrap.
    pub fn unlock(&mut self, passcode: &str) -> Result<(), WalletError> {
        let keyring = self
            .keyring
            .as_ref()
            .ok_or_else(|| WalletError::Vault("vault not initialized".to_owned()))?;
        let wrap_key = derive_vault_wrap_key(&keyring.wrap, passcode.as_bytes())?;
        let device_key = unwrap_vault_key(&keyring.wrap, &wrap_key)?;
        self.unlocked_device_key = Some(device_key);
        Ok(())
    }

    /// Rotates the vault passcode (re-seals keyring entries without touching records).
    pub fn change_passcode(
        &mut self,
        new_passcode: &str,
        kdf: PasscodeKdf,
    ) -> Result<(), WalletError> {
        let old_key = self
            .unlocked_device_key
            .as_ref()
            .ok_or_else(|| WalletError::Vault("vault is locked".to_owned()))?;
        let current = self
            .keyring
            .as_ref()
            .ok_or_else(|| WalletError::Vault("vault not initialized".to_owned()))?;
        let next = prepare_vault_passcode_wrap_with_kdf(new_passcode.as_bytes(), kdf)?;
        let rotated = rotate_device_keyring(current, old_key, &next)?;
        self.keyring = Some(rotated.keyring);
        self.unlocked_device_key = Some(rotated.device_key);
        Ok(())
    }

    /// Encrypts and stores a secret under `secret_ref` (keyed as `secret/<secret_ref>`).
    pub fn store_secret(&mut self, secret_ref: &str, secret: &[u8]) -> Result<(), WalletError> {
        let device_key = self
            .unlocked_device_key
            .as_ref()
            .ok_or_else(|| WalletError::Vault("vault is locked".to_owned()))?;
        let keyring = self
            .keyring
            .as_mut()
            .ok_or_else(|| WalletError::Vault("vault not initialized".to_owned()))?;

        let storage_key = format!("secret/{secret_ref}");
        let mut key_id = [0_u8; VAULT_KEY_ID_SIZE];
        OsRng.fill_bytes(&mut key_id);
        let record_key = SecureBytes::random(VAULT_KEY_SIZE);

        let entry = seal_keyring_entry(device_key, key_id, self.account_id, &record_key)?;
        let sealed_record = seal_vault_record(&record_key, &storage_key, key_id, false, secret)?;

        // Drop previous entry for this storage key if overwriting.
        if let Some(prev) = self.records.get(&storage_key)
            && let Some(prev_id) = read_vault_record_key_id(prev)
        {
            keyring.entries.retain(|e| e.key_id != prev_id);
        }
        keyring.entries.push(entry);
        self.records.insert(storage_key, sealed_record);
        Ok(())
    }

    /// Reads and decrypts a secret stored under `secret_ref`.
    pub fn read_secret(&self, secret_ref: &str) -> Result<SecureBytes, WalletError> {
        let device_key = self
            .unlocked_device_key
            .as_ref()
            .ok_or_else(|| WalletError::Vault("vault is locked".to_owned()))?;
        let keyring = self
            .keyring
            .as_ref()
            .ok_or_else(|| WalletError::Vault("vault not initialized".to_owned()))?;

        let storage_key = format!("secret/{secret_ref}");
        let serialized = self
            .records
            .get(&storage_key)
            .ok_or_else(|| WalletError::Vault(format!("secret not found: {secret_ref}")))?;
        let key_id = read_vault_record_key_id(serialized)
            .ok_or_else(|| WalletError::Vault("corrupt vault record header".to_owned()))?;
        let entry = keyring
            .entries
            .iter()
            .find(|e| e.key_id == key_id && e.account_id == self.account_id)
            .ok_or_else(|| WalletError::Vault("missing keyring entry for record".to_owned()))?;
        let record_key = open_keyring_entry(device_key, entry)?;
        let opened = open_vault_record(&record_key, &storage_key, &key_id, serialized)?;
        Ok(opened.bytes)
    }

    /// Saves the encrypted keyring and sealed records into `dir` (plaintext is never written).
    pub fn save_to_dir(&self, dir: &Path) -> Result<(), WalletError> {
        let keyring = self
            .keyring
            .as_ref()
            .ok_or_else(|| WalletError::Vault("vault not initialized".to_owned()))?;
        std::fs::create_dir_all(dir)
            .map_err(|e| WalletError::Vault(format!("create vault dir: {e}")))?;
        std::fs::write(dir.join("keyring.bin"), serialize_device_keyring(keyring))
            .map_err(|e| WalletError::Vault(format!("write keyring: {e}")))?;

        let mut records_blob = Vec::new();
        records_blob.extend_from_slice(&(self.records.len() as u32).to_be_bytes());
        for (k, v) in &self.records {
            write_bytes_field(&mut records_blob, k.as_bytes());
            write_bytes_field(&mut records_blob, v);
        }
        std::fs::write(dir.join("records.bin"), records_blob)
            .map_err(|e| WalletError::Vault(format!("write records: {e}")))?;
        Ok(())
    }

    /// Loads an encrypted vault from `dir` in a locked state.
    pub fn load_from_dir(account_id: u64, dir: &Path) -> Result<Self, WalletError> {
        let keyring_bytes = std::fs::read(dir.join("keyring.bin"))
            .map_err(|e| WalletError::Vault(format!("read keyring: {e}")))?;
        let keyring = parse_device_keyring(&keyring_bytes)?;

        let records_bytes = std::fs::read(dir.join("records.bin"))
            .map_err(|e| WalletError::Vault(format!("read records: {e}")))?;
        let mut pos = 0;
        let count = read_u32_be(&records_bytes, &mut pos)? as usize;
        let mut records = HashMap::with_capacity(count);
        for _ in 0..count {
            let k_bytes = read_bytes_field(&records_bytes, &mut pos)?;
            let k = String::from_utf8(k_bytes)
                .map_err(|e| WalletError::Vault(format!("invalid record key UTF-8: {e}")))?;
            let v = read_bytes_field(&records_bytes, &mut pos)?;
            records.insert(k, v);
        }
        if pos != records_bytes.len() {
            return Err(WalletError::Vault(
                "trailing bytes in records file".to_owned(),
            ));
        }

        Ok(Self {
            account_id: account_id.max(1),
            keyring: Some(keyring),
            records,
            unlocked_device_key: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_encrypt_lock_unlock_rotate_and_disk_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = LocalKeyVault::new(42);
        vault
            .initialize_with_passcode("correct-horse", PasscodeKdf::fast_test_kdf())
            .unwrap();
        vault
            .store_secret("mnemonic", b"synthetic test mnemonic phrase")
            .unwrap();
        assert_eq!(
            vault.read_secret("mnemonic").unwrap().as_slice(),
            b"synthetic test mnemonic phrase"
        );

        // Lock rejects reads.
        vault.lock();
        assert!(!vault.is_unlocked());
        assert!(vault.read_secret("mnemonic").is_err());

        // Wrong passcode fails; right passcode unlocks.
        assert!(vault.unlock("wrong-pass").is_err());
        vault.unlock("correct-horse").unwrap();
        assert_eq!(
            vault.read_secret("mnemonic").unwrap().as_slice(),
            b"synthetic test mnemonic phrase"
        );

        // Rotate passcode and verify persistence on disk.
        vault
            .change_passcode("new-passcode", PasscodeKdf::fast_test_kdf())
            .unwrap();
        vault.save_to_dir(dir.path()).unwrap();

        // Plaintext secret must not appear on disk.
        let disk_records = std::fs::read(dir.path().join("records.bin")).unwrap();
        assert!(!disk_records.windows(9).any(|w| w == b"synthetic"));

        let mut loaded = LocalKeyVault::load_from_dir(42, dir.path()).unwrap();
        assert!(!loaded.is_unlocked());
        assert!(loaded.unlock("correct-horse").is_err());
        loaded.unlock("new-passcode").unwrap();
        assert_eq!(
            loaded.read_secret("mnemonic").unwrap().as_slice(),
            b"synthetic test mnemonic phrase"
        );
    }

    #[test]
    fn open_wrap_and_hkdf_kat() {
        // RFC 5869 Test Case 1 for HKDF-SHA256.
        let ikm = hex::decode("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
        let salt = hex::decode("000102030405060708090a0b0c").unwrap();
        let info = hex::decode("f0f1f2f3f4f5f6f7f8f9").unwrap();
        let okm = hkdf_sha256(&ikm, &salt, &info, 42).unwrap();
        assert_eq!(
            hex::encode(okm.as_slice()),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );

        let prepared = prepare_vault_open_wrap().unwrap();
        let key = SecureBytes::random(VAULT_KEY_SIZE);
        let mut wrap = prepared.wrap.clone();
        wrap.blob = wrap_vault_key(&key, &wrap, &prepared.wrap_key).unwrap();
        let unwrapped = unwrap_vault_key(&wrap, &prepared.wrap_key).unwrap();
        assert_eq!(unwrapped.as_slice(), key.as_slice());
    }
}
