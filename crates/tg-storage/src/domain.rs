//! Domain-level local storage (`key_datas`, passcode wraps, account index list).
//!
//! Port of `Telegram/SourceFiles/storage/storage_domain.h` and `storage_domain.cpp`.
//!
//! Supports both:
//! - Legacy 3-blob `key_datas` (`salt`, `keyEncrypted`, `infoEncrypted`)
//! - Modern v2 `key_datas` with the 4th trailer blob (`kKeyDataMagic = 0x4B443200`,
//!   format version 2, committed generation, Argon2id / Scrypt `PasscodeWrap` entries)

use std::path::{Path, PathBuf};

use tg_crypto::{
    AUTH_KEY_SIZE, AuthKey, LOCAL_ENCRYPT_SALT_SIZE, PASSCODE_SALT_MIN_SIZE, PasscodeKdf,
    PasscodeKdfKind, create_local_key, create_passcode_key, random_bytes,
};

use crate::error::{Result, StorageError};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};
use crate::tdf::{
    FileWriteDescriptor, clear_name, decrypt_local, prepare_encrypted, read_encrypted_file,
    read_file, write_encrypted_file,
};

/// Magic tag identifying the v2 `key_datas` trailer blob (`"KD2\0"` = `0x4B443200`).
pub const KEY_DATA_MAGIC: u32 = 0x4B44_3200;

/// Current `key_datas` trailer format version (`kKeyDataFormatVersion`).
pub const KEY_DATA_FORMAT_VERSION: u32 = 2;

/// Maximum number of passcode wraps permitted in a `key_datas` trailer (`kMaxWrapCount`).
pub const MAX_WRAP_COUNT: u32 = 2;

/// Maximum number of accounts in a tdesktop domain (`Main::Domain::kPremiumMaxAccounts`).
pub const PREMIUM_MAX_ACCOUNTS: i32 = 6;

/// Expected ciphertext size of a wrapped 256-byte local key:
/// `16 (key128) + ceil_16(4 + 256) = 16 + 272 = 288`.
pub const OPEN_WRAP_SIZE: usize = 16 + (4 + AUTH_KEY_SIZE).div_ceil(16) * 16;

/// A memory-hard passcode wrap recorded in the v2 `key_datas` trailer (`Storage::PasscodeWrap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasscodeWrap {
    /// KDF family and cost parameters (Argon2id or Scrypt).
    pub kdf: PasscodeKdf,
    /// Monotonic generation counter matched against `KeyData::committed`.
    pub generation: u32,
    /// Per-wrap random salt (minimum 8 bytes, normally 32 bytes).
    pub salt: Vec<u8>,
    /// Local key encrypted with the KDF-derived wrap key.
    pub key_encrypted: Vec<u8>,
}

/// Parsed contents of `key_<dataName>` (`Storage::KeyData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyData {
    /// 32-byte salt for the open (empty-passcode) wrap, or legacy passcode wrap.
    pub open_salt: Vec<u8>,
    /// Encrypted 256-byte local key (empty when app lock is armed in v2 format).
    pub open_key_encrypted: Vec<u8>,
    /// Encrypted accounts info blob (encrypted directly with `local_key`).
    pub info_encrypted: Vec<u8>,
    /// Modern passcode wraps (v2 format only).
    pub passcode_wraps: Vec<PasscodeWrap>,
    /// Committed wrap generation (v2 format only).
    pub committed: u32,
    /// `true` when the file contained only the 3 legacy blobs without a v2 trailer.
    pub legacy: bool,
    /// `true` when a legacy file was opened using a non-empty passcode.
    pub legacy_passcode: bool,
    /// `true` when `open_key_encrypted` was verified to unwrap to the live `local_key`.
    pub open_key_verified: bool,
}

/// Decrypted accounts index list stored inside `infoEncrypted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountsInfo {
    /// Ordered list of active account slot indices (0..`PREMIUM_MAX_ACCOUNTS`).
    pub indices: Vec<i32>,
    /// Currently active account slot index.
    pub active: i32,
}

impl AccountsInfo {
    /// Serialize into the inner QDataStream payload of `infoEncrypted` (`Domain::prepareAccountsInfo`).
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut w = QDataStreamWriter::with_capacity(4 + self.indices.len() * 4 + 4);
        w.write_i32(self.indices.len() as i32);
        for &idx in &self.indices {
            w.write_i32(idx);
        }
        w.write_i32(self.active);
        w.into_bytes()
    }

    /// Deserialize from the decrypted `infoEncrypted` payload (`Domain::startModern`).
    pub fn deserialize(bytes: &[u8]) -> Result<Self> {
        let mut r = QDataStreamReader::new(bytes);
        let count = r.read_i32()?;
        if count <= 0 || count > PREMIUM_MAX_ACCOUNTS {
            return Err(StorageError::CorruptData(format!(
                "bad accounts count: {count}"
            )));
        }
        let mut indices = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let idx = r.read_i32()?;
            if (0..PREMIUM_MAX_ACCOUNTS).contains(&idx) && !indices.contains(&idx) {
                indices.push(idx);
            }
        }
        if indices.is_empty() {
            return Err(StorageError::CorruptData(
                "no valid accounts in info".into(),
            ));
        }
        let active = if !r.at_end() {
            r.read_i32()?
        } else {
            indices[0]
        };
        Ok(Self { indices, active })
    }
}

/// Compute the TDF file base name for `key_data` (`ComputeKeyName`).
#[must_use]
pub fn compute_key_name(data_name: &str) -> String {
    format!("key_{data_name}")
}

/// Compute the TDF file base name for the device wallet keyring (`ComputeWalletKeyringName`).
#[must_use]
pub fn compute_wallet_keyring_name(data_name: &str) -> String {
    format!("wallet_keyring_{data_name}")
}

/// Generate a 32-byte random salt (`RandomSalt`).
#[must_use]
pub fn random_salt() -> Vec<u8> {
    let mut salt = vec![0u8; LOCAL_ENCRYPT_SALT_SIZE];
    random_bytes(&mut salt);
    salt
}

/// Wrap `local_key` under `wrap_key` using `PrepareEncrypted` (`WrapLocalKey`).
pub fn wrap_local_key(local_key: &AuthKey, wrap_key: &AuthKey) -> Result<Vec<u8>> {
    prepare_encrypted(local_key.data(), wrap_key)
}

/// Unwrap a 256-byte `local_key` from `key_encrypted` using `wrap_key` (`UnwrapLocalKey`).
pub fn unwrap_local_key(key_encrypted: &[u8], wrap_key: &AuthKey) -> Result<AuthKey> {
    let inner = decrypt_local(key_encrypted, wrap_key)?;
    if inner.len() != AUTH_KEY_SIZE {
        return Err(StorageError::CorruptData(format!(
            "unwrapped local key length {}, expected {AUTH_KEY_SIZE}",
            inner.len()
        )));
    }
    let mut key_bytes = [0u8; AUTH_KEY_SIZE];
    key_bytes.copy_from_slice(&inner);
    Ok(AuthKey::new(key_bytes))
}

/// Parse a `key_<dataName>` TDF payload stream into a [`KeyData`] (`ReadKeyData`).
pub fn read_key_data(payload: &[u8]) -> Result<KeyData> {
    let mut stream = QDataStreamReader::new(payload);
    let open_salt = stream.read_byte_array()?;
    let open_key_encrypted = stream.read_byte_array()?;
    let info_encrypted = stream.read_byte_array()?;

    if open_salt.len() != LOCAL_ENCRYPT_SALT_SIZE {
        return Err(StorageError::CorruptData(format!(
            "bad salt size in key_data: {}",
            open_salt.len()
        )));
    }

    if stream.at_end() {
        return Ok(KeyData {
            open_salt,
            open_key_encrypted,
            info_encrypted,
            passcode_wraps: Vec::new(),
            committed: 0,
            legacy: true,
            legacy_passcode: false,
            open_key_verified: false,
        });
    }

    let trailer = stream.read_byte_array()?;
    let mut inner = QDataStreamReader::new(&trailer);
    let magic = inner.read_u32()?;
    let format_version = inner.read_u32()?;
    let committed = inner.read_u32()?;
    let wrap_count = inner.read_u32()?;

    if magic != KEY_DATA_MAGIC {
        return Err(StorageError::CorruptData(format!(
            "bad key_data trailer magic: 0x{magic:08x}"
        )));
    }
    if format_version > KEY_DATA_FORMAT_VERSION {
        return Err(StorageError::UnsupportedVersion(format_version as i32));
    }
    if wrap_count > MAX_WRAP_COUNT {
        return Err(StorageError::CorruptData(format!(
            "bad key_data wrap count: {wrap_count}"
        )));
    }

    let mut passcode_wraps = Vec::with_capacity(wrap_count as usize);
    let mut unsupported_cost = false;
    for _ in 0..wrap_count {
        let kind = inner.read_u32()?;
        let generation = inner.read_u32()?;
        let memory = inner.read_u32()?;
        let time = inner.read_u32()?;
        let parallel = inner.read_u32()?;
        let salt = inner.read_byte_array()?;
        let key_encrypted = inner.read_byte_array()?;

        let Some(kdf_kind) = PasscodeKdfKind::from_u32(kind) else {
            return Err(StorageError::UnsupportedKdf(format!(
                "unknown key_data KDF family: {kind}"
            )));
        };
        let kdf = PasscodeKdf {
            kind: kdf_kind,
            memory,
            time,
            parallel,
        };
        if !kdf.cost_within_limits() {
            unsupported_cost = true;
        } else if !kdf.is_valid() {
            return Err(StorageError::UnsupportedKdf(format!(
                "invalid key_data KDF parameters: {kdf:?}"
            )));
        }
        if salt.len() < PASSCODE_SALT_MIN_SIZE {
            return Err(StorageError::CorruptData(format!(
                "bad key_data wrap salt size: {}",
                salt.len()
            )));
        }
        if passcode_wraps
            .iter()
            .any(|w: &PasscodeWrap| w.generation == generation)
        {
            return Err(StorageError::CorruptData(format!(
                "duplicate key_data wrap generation: {generation}"
            )));
        }
        passcode_wraps.push(PasscodeWrap {
            kdf,
            generation,
            salt,
            key_encrypted,
        });
    }

    if unsupported_cost {
        return Err(StorageError::UnsupportedKdf(
            "passcode KDF cost exceeds limits".into(),
        ));
    }

    Ok(KeyData {
        open_salt,
        open_key_encrypted,
        info_encrypted,
        passcode_wraps,
        committed,
        legacy: false,
        legacy_passcode: false,
        open_key_verified: false,
    })
}

/// Write a [`KeyData`] into a [`FileWriteDescriptor`] (`WriteKeyData`).
pub fn write_key_data_to_descriptor(
    file: &mut FileWriteDescriptor,
    data: &KeyData,
    info_encrypted: &[u8],
) {
    file.write_data(&data.open_salt);
    file.write_data(&data.open_key_encrypted);
    file.write_data(info_encrypted);
    if data.legacy {
        return;
    }

    let mut trailer = QDataStreamWriter::new();
    trailer.write_u32(KEY_DATA_MAGIC);
    trailer.write_u32(KEY_DATA_FORMAT_VERSION);
    trailer.write_u32(data.committed);
    trailer.write_u32(data.passcode_wraps.len() as u32);
    for wrap in &data.passcode_wraps {
        trailer.write_u32(wrap.kdf.kind as u32);
        trailer.write_u32(wrap.generation);
        trailer.write_u32(wrap.kdf.memory);
        trailer.write_u32(wrap.kdf.time);
        trailer.write_u32(wrap.kdf.parallel);
        trailer.write_byte_array(&wrap.salt);
        trailer.write_byte_array(&wrap.key_encrypted);
    }
    file.write_data(trailer.as_bytes());
}

/// Single-use verification token returned by [`DomainStorage::verify_passcode`]
/// (`Storage::PasscodeVerification` in `storage_domain.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasscodeVerification {
    nonce: u64,
}

/// High-level manager for a tdesktop `tdata` domain (`Storage::Domain`).
#[derive(Debug, Clone)]
pub struct DomainStorage {
    tdata_path: PathBuf,
    data_name: String,
    local_key: AuthKey,
    key_data: KeyData,
    accounts: AccountsInfo,
    old_version: i32,
    verification_nonce: u64,
}

impl DomainStorage {
    /// Inspect `tdata_path` without mutating disk to determine whether unlocking `data_name`
    /// requires a non-empty local passcode.
    ///
    /// Returns `Ok(false)` for a fresh/non-existent `tdata` directory or an unlocked domain,
    /// and `Ok(true)` if `key_<dataName>` (or a legacy single-account `map`) requires a passcode.
    pub fn needs_passcode(tdata_path: &Path, data_name: &str) -> Result<bool> {
        match read_file(&compute_key_name(data_name), tdata_path) {
            Ok(file) => {
                let key_data = read_key_data(&file.data)?;
                if key_data.legacy {
                    let wrap_key = create_local_key(b"", &key_data.open_salt);
                    Ok(unwrap_local_key(&key_data.open_key_encrypted, &wrap_key).is_err())
                } else if key_data.open_key_encrypted.is_empty() {
                    Ok(true)
                } else {
                    let wrap_key = create_local_key(b"", &key_data.open_salt);
                    match unwrap_local_key(&key_data.open_key_encrypted, &wrap_key) {
                        Ok(local_key) => {
                            Ok(decrypt_local(&key_data.info_encrypted, &local_key).is_err())
                        }
                        Err(_) => Ok(true),
                    }
                }
            }
            Err(StorageError::NotFound(_)) => {
                match crate::account::AccountStorage::open_legacy(tdata_path, data_name, b"") {
                    Ok(_) | Err(StorageError::NotFound(_)) => Ok(false),
                    Err(StorageError::IncorrectPasscode) => Ok(true),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Remove `key_<dataName>` and wallet keyring files from `tdata_path` (used during scratch reset).
    pub fn clear_files(tdata_path: &Path, data_name: &str) {
        clear_name(&compute_key_name(data_name), tdata_path);
        clear_name(&compute_wallet_keyring_name(data_name), tdata_path);
    }

    /// Open an existing `tdata` directory with an optional passcode (pass `&[]` for no passcode).
    ///
    /// Reads `key_<dataName>s` (or legacy `0`/`1`), unwraps the 256-byte `local_key`, and
    /// decrypts the accounts index list.
    pub fn open(
        tdata_path: impl Into<PathBuf>,
        data_name: impl Into<String>,
        passcode: &[u8],
    ) -> Result<Self> {
        let tdata_path = tdata_path.into();
        let data_name = data_name.into();
        let file = read_file(&compute_key_name(&data_name), &tdata_path)?;
        let old_version = file.version;
        let mut key_data = read_key_data(&file.data)?;

        let open_key = if key_data.open_key_encrypted.len() == OPEN_WRAP_SIZE {
            let empty_wrap = create_local_key(b"", &key_data.open_salt);
            unwrap_local_key(&key_data.open_key_encrypted, &empty_wrap).ok()
        } else {
            None
        };

        let local_key = if key_data.legacy {
            let wrap_key = create_local_key(passcode, &key_data.open_salt);
            let k = unwrap_local_key(&key_data.open_key_encrypted, &wrap_key)
                .map_err(|_| StorageError::IncorrectPasscode)?;
            key_data.legacy_passcode = !passcode.is_empty();
            k
        } else {
            // Keep only wraps matching the committed generation.
            key_data
                .passcode_wraps
                .retain(|w| w.generation == key_data.committed);

            if passcode.is_empty() {
                if key_data.open_key_encrypted.is_empty() {
                    return Err(StorageError::IncorrectPasscode);
                }
                let wrap_key = create_local_key(b"", &key_data.open_salt);
                unwrap_local_key(&key_data.open_key_encrypted, &wrap_key)
                    .map_err(|_| StorageError::IncorrectPasscode)?
            } else {
                let wrap = key_data
                    .passcode_wraps
                    .first()
                    .ok_or(StorageError::IncorrectPasscode)?;
                let wrap_key = create_passcode_key(passcode, &wrap.salt, wrap.kdf)
                    .map_err(|e| StorageError::UnsupportedKdf(e.to_string()))?;
                unwrap_local_key(&wrap.key_encrypted, &wrap_key)
                    .map_err(|_| StorageError::IncorrectPasscode)?
            }
        };

        key_data.open_key_verified = open_key.as_ref() == Some(&local_key);

        let info_bytes = decrypt_local(&key_data.info_encrypted, &local_key).map_err(|_| {
            if passcode.is_empty() && key_data.passcode_wraps.len() == 1 {
                StorageError::IncorrectPasscode
            } else {
                StorageError::DecryptionFailed
            }
        })?;
        let accounts = AccountsInfo::deserialize(&info_bytes)?;

        Ok(Self {
            tdata_path,
            data_name,
            local_key,
            key_data,
            accounts,
            old_version,
            verification_nonce: 0,
        })
    }

    /// Create a brand-new `tdata` domain from scratch with a randomly generated `local_key`
    /// and a single account slot (`index = 0`), writing `key_<dataName>s` to disk.
    pub fn create_fresh(
        tdata_path: impl Into<PathBuf>,
        data_name: impl Into<String>,
    ) -> Result<Self> {
        let tdata_path = tdata_path.into();
        let data_name = data_name.into();

        // Matches Domain::generateLocalKey(): derive localKey from 256 random bytes + 32-byte salt
        let mut pass = [0u8; AUTH_KEY_SIZE];
        random_bytes(&mut pass);
        let gen_salt = random_salt();
        let local_key = create_local_key(&pass, &gen_salt);

        Self::create_with_local_key(
            tdata_path,
            data_name,
            local_key,
            AccountsInfo {
                indices: vec![0],
                active: 0,
            },
        )
    }

    /// Create a `tdata` domain using a caller-supplied `local_key` and accounts list (useful for tests).
    pub fn create_with_local_key(
        tdata_path: impl Into<PathBuf>,
        data_name: impl Into<String>,
        local_key: AuthKey,
        accounts: AccountsInfo,
    ) -> Result<Self> {
        let tdata_path = tdata_path.into();
        let data_name = data_name.into();

        let open_salt = random_salt();
        let wrap_key = create_local_key(b"", &open_salt);
        let open_key_encrypted = wrap_local_key(&local_key, &wrap_key)?;

        let key_data = KeyData {
            open_salt,
            open_key_encrypted,
            info_encrypted: Vec::new(),
            passcode_wraps: Vec::new(),
            committed: 0,
            legacy: false,
            legacy_passcode: false,
            open_key_verified: true,
        };

        let mut domain = Self {
            tdata_path,
            data_name,
            local_key,
            key_data,
            accounts,
            old_version: crate::tdf::APP_VERSION,
            verification_nonce: 0,
        };
        domain.write_accounts()?;
        Ok(domain)
    }

    /// Path to the `tdata` directory.
    #[must_use]
    pub fn tdata_path(&self) -> &Path {
        &self.tdata_path
    }

    /// Configured data name (normally `"data"`).
    #[must_use]
    pub fn data_name(&self) -> &str {
        &self.data_name
    }

    /// The decrypted 256-byte local storage key.
    #[must_use]
    pub fn local_key(&self) -> &AuthKey {
        &self.local_key
    }

    /// Current `AccountsInfo` (slot indices + active index).
    #[must_use]
    pub fn accounts(&self) -> &AccountsInfo {
        &self.accounts
    }

    /// Mutable access to `AccountsInfo` (call [`Self::write_accounts`] to persist).
    pub fn accounts_mut(&mut self) -> &mut AccountsInfo {
        &mut self.accounts
    }

    /// Parsed `KeyData` metadata.
    #[must_use]
    pub fn key_data(&self) -> &KeyData {
        &self.key_data
    }

    /// TDF app version read from `key_datas`.
    #[must_use]
    pub const fn old_version(&self) -> i32 {
        self.old_version
    }

    /// Whether a passcode is configured (`Domain::hasPasscode`).
    #[must_use]
    pub fn has_passcode(&self) -> bool {
        !self.key_data.passcode_wraps.is_empty() || self.key_data.legacy_passcode
    }

    /// Whether app launch lock is enabled (`Domain::appLockEnabled` / `hasLocalPasscode`).
    #[must_use]
    pub fn app_lock_enabled(&self) -> bool {
        self.key_data.legacy_passcode || !self.key_data.open_key_verified
    }

    /// Verify a candidate passcode against the live wrap (`Domain::checkPasscode`).
    #[must_use]
    pub fn check_passcode(&self, passcode: &[u8]) -> bool {
        if self.key_data.legacy || self.key_data.legacy_passcode {
            let wrap_key = create_local_key(passcode, &self.key_data.open_salt);
            unwrap_local_key(&self.key_data.open_key_encrypted, &wrap_key)
                .map(|k| k == self.local_key)
                .unwrap_or(false)
        } else if let Some(wrap) = self.key_data.passcode_wraps.first() {
            create_passcode_key(passcode, &wrap.salt, wrap.kdf)
                .ok()
                .and_then(|wk| unwrap_local_key(&wrap.key_encrypted, &wk).ok())
                .map(|k| k == self.local_key)
                .unwrap_or(false)
        } else {
            false
        }
    }

    /// Verify `passcode` against the live wrap and mint a single-use [`PasscodeVerification`]
    /// token (`Domain::verifyPasscode` in `storage_domain.cpp`).
    #[must_use]
    pub fn verify_passcode(&mut self, passcode: &[u8]) -> Option<PasscodeVerification> {
        if !self.check_passcode(passcode) {
            return None;
        }
        let mut nonce = 0u64;
        while nonce == 0 {
            let mut buf = [0u8; 8];
            random_bytes(&mut buf);
            nonce = u64::from_le_bytes(buf);
        }
        self.verification_nonce = nonce;
        Some(PasscodeVerification { nonce })
    }

    /// Returns `true` if `verification` matches the active single-use nonce (`Domain::accepts`).
    #[must_use]
    pub const fn accepts_verification(&self, verification: PasscodeVerification) -> bool {
        self.verification_nonce != 0 && self.verification_nonce == verification.nonce
    }

    /// Set, change, or remove the local passcode after verifying an existing passcode via
    /// [`Self::verify_passcode`] (`Domain::changePasscode` in `storage_domain.cpp`).
    ///
    /// - If a passcode is already configured, `verification` must be `Some` and match the active
    ///   single-use nonce (otherwise returns [`StorageError::IncorrectPasscode`]).
    /// - Passing an empty `passcode` removes the passcode and restores an open wrap.
    /// - Creating a first passcode drops the open wrap so app launch lock is armed.
    /// - Changing an existing passcode preserves the current open-wrap state.
    pub fn set_passcode_verified(
        &mut self,
        passcode: &[u8],
        kdf: PasscodeKdf,
        verification: Option<PasscodeVerification>,
    ) -> Result<()> {
        let valid_verification = verification.is_some_and(|v| self.accepts_verification(v));
        self.verification_nonce = 0;

        if self.has_passcode() && !valid_verification {
            return Err(StorageError::IncorrectPasscode);
        }

        let drop_open_wrap = if passcode.is_empty() {
            false
        } else {
            !self.has_passcode() || self.app_lock_enabled()
        };
        self.set_passcode(passcode, kdf, drop_open_wrap)
    }

    /// Set or change the local passcode using the provided `kdf` (e.g., `PasscodeKdf::default_argon2id()`).
    ///
    /// Uses tdesktop's two-phase staged commit (`Domain::installPasscode` in `storage_domain.cpp`):
    /// 1. Writes the new wrap beside the old wrap with `committed` unchanged.
    /// 2. Reads `key_<dataName>` back from disk and proves the staged wrap unwraps `local_key`.
    /// 3. Commits by replacing `passcode_wraps`, advancing `committed`, and (if `drop_open_wrap`)
    ///    clearing the open wrap so app lock is armed.
    ///
    /// Passing an empty `passcode` removes the passcode and installs an open wrap.
    pub fn set_passcode(
        &mut self,
        passcode: &[u8],
        kdf: PasscodeKdf,
        drop_open_wrap: bool,
    ) -> Result<()> {
        self.verification_nonce = 0;
        if passcode.is_empty() {
            let mut updated = self.key_data.clone();
            updated.legacy = false;
            updated.legacy_passcode = false;
            updated.passcode_wraps.clear();
            updated.committed = updated.committed.wrapping_add(1);
            Self::install_open_wrap_into(&mut updated, &self.local_key)?;
            self.write_key_data_checked(&updated)?;
            self.key_data = updated;
            return Ok(());
        }

        let generation = self.key_data.committed.wrapping_add(1);
        let salt = random_salt();
        let wrap_key = create_passcode_key(passcode, &salt, kdf)
            .map_err(|e| StorageError::UnsupportedKdf(e.to_string()))?;
        let key_encrypted = wrap_local_key(&self.local_key, &wrap_key)?;

        // Verify in-memory roundtrip before touching disk.
        if unwrap_local_key(&key_encrypted, &wrap_key)? != self.local_key {
            return Err(StorageError::DecryptionFailed);
        }

        let new_wrap = PasscodeWrap {
            kdf,
            generation,
            salt,
            key_encrypted,
        };

        // Phase 1: stage the new wrap alongside any existing live wrap, keeping `committed` unchanged.
        let mut staged = self.key_data.clone();
        staged.legacy = false;
        staged.legacy_passcode = false;
        if staged.passcode_wraps.len() >= MAX_WRAP_COUNT as usize {
            staged.passcode_wraps.truncate(MAX_WRAP_COUNT as usize - 1);
        }
        staged.passcode_wraps.push(new_wrap.clone());
        self.write_key_data_checked(&staged)?;

        // Phase 2: read back from disk and verify the staged wrap unwraps `local_key`.
        if !self.wrap_on_disk_opens_local_key(&new_wrap, &wrap_key) {
            let _ = self.write_key_data_checked(&self.key_data);
            return Err(StorageError::DecryptionFailed);
        }

        // Phase 3: commit the new wrap and advance `committed`.
        let mut updated = self.key_data.clone();
        updated.legacy = false;
        updated.legacy_passcode = false;
        updated.passcode_wraps = vec![new_wrap];
        updated.committed = generation;
        if drop_open_wrap {
            updated.open_salt = random_salt();
            updated.open_key_encrypted.clear();
            updated.open_key_verified = false;
        }
        self.write_key_data_checked(&updated)?;
        self.key_data = updated;
        Ok(())
    }

    /// Set a legacy 3-blob passcode (PBKDF2-HMAC-SHA512 100k iters, no v2 trailer)
    /// for compatibility with older tdesktop builds.
    pub fn set_legacy_passcode(&mut self, passcode: &[u8]) -> Result<()> {
        let salt = random_salt();
        let wrap_key = create_local_key(passcode, &salt);
        let key_encrypted = wrap_local_key(&self.local_key, &wrap_key)?;
        self.key_data.open_salt = salt;
        self.key_data.open_key_encrypted = key_encrypted;
        self.key_data.passcode_wraps.clear();
        self.key_data.committed = 0;
        self.key_data.legacy = true;
        self.key_data.legacy_passcode = !passcode.is_empty();
        self.key_data.open_key_verified = passcode.is_empty();
        self.write_accounts()
    }

    /// Toggle app launch lock without removing the configured passcode (`Domain::setAppLockEnabled`).
    pub fn set_app_lock_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.key_data.legacy {
            return Err(StorageError::CorruptData(
                "cannot toggle app lock on legacy key_data".into(),
            ));
        }
        if enabled {
            if self.key_data.passcode_wraps.is_empty() {
                return Err(StorageError::IncorrectPasscode);
            }
            self.key_data.open_salt = random_salt();
            self.key_data.open_key_encrypted.clear();
            self.key_data.open_key_verified = false;
        } else {
            self.install_open_wrap()?;
        }
        self.write_accounts()
    }

    /// Persist `key_<dataName>s` with current `KeyData` and `AccountsInfo` (`Domain::writeAccounts`).
    pub fn write_accounts(&mut self) -> Result<()> {
        let info_encrypted = prepare_encrypted(&self.accounts.serialize(), &self.local_key)?;
        self.key_data.info_encrypted = info_encrypted.clone();

        let mut file =
            FileWriteDescriptor::new(compute_key_name(&self.data_name), &self.tdata_path);
        write_key_data_to_descriptor(&mut file, &self.key_data, &info_encrypted);
        file.finish()
    }

    /// Read the encrypted domain wallet keyring (`Domain::readWalletKeyring`).
    pub fn read_wallet_keyring(&self) -> Result<Option<Vec<u8>>> {
        let name = compute_wallet_keyring_name(&self.data_name);
        match read_encrypted_file(&name, &self.tdata_path, &self.local_key) {
            Ok(file) => {
                let mut r = file.stream();
                let bytes = r.read_byte_array()?;
                if !r.at_end() {
                    return Err(StorageError::CorruptData(
                        "trailing bytes in wallet keyring".into(),
                    ));
                }
                Ok(Some(bytes))
            }
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Write the encrypted domain wallet keyring (`Domain::writeWalletKeyring`).
    pub fn write_wallet_keyring(&self, bytes: &[u8]) -> Result<()> {
        let mut w = QDataStreamWriter::with_capacity(4 + bytes.len());
        w.write_byte_array(bytes);
        write_encrypted_file(
            &compute_wallet_keyring_name(&self.data_name),
            &self.tdata_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Remove the domain wallet keyring files (`Domain::removeWalletKeyring`).
    pub fn remove_wallet_keyring(&self) {
        clear_name(
            &compute_wallet_keyring_name(&self.data_name),
            &self.tdata_path,
        );
    }

    fn install_open_wrap(&mut self) -> Result<()> {
        Self::install_open_wrap_into(&mut self.key_data, &self.local_key)
    }

    fn install_open_wrap_into(key_data: &mut KeyData, local_key: &AuthKey) -> Result<()> {
        let open_salt = random_salt();
        let wrap_key = create_local_key(b"", &open_salt);
        let open_key_encrypted = wrap_local_key(local_key, &wrap_key)?;
        key_data.open_salt = open_salt;
        key_data.open_key_encrypted = open_key_encrypted;
        key_data.open_key_verified = true;
        Ok(())
    }

    fn write_key_data_checked(&self, data: &KeyData) -> Result<()> {
        let info_encrypted = prepare_encrypted(&self.accounts.serialize(), &self.local_key)?;
        let mut file =
            FileWriteDescriptor::new(compute_key_name(&self.data_name), &self.tdata_path);
        write_key_data_to_descriptor(&mut file, data, &info_encrypted);
        file.finish()
    }

    fn wrap_on_disk_opens_local_key(&self, staged: &PasscodeWrap, wrap_key: &AuthKey) -> bool {
        let Ok(file) = read_file(&compute_key_name(&self.data_name), &self.tdata_path) else {
            return false;
        };
        let Ok(parsed) = read_key_data(&file.data) else {
            return false;
        };
        if parsed.legacy {
            return false;
        }
        for wrap in &parsed.passcode_wraps {
            if wrap.generation != staged.generation {
                continue;
            }
            if wrap.kdf != staged.kdf
                || wrap.salt != staged.salt
                || wrap.key_encrypted != staged.key_encrypted
            {
                return false;
            }
            return unwrap_local_key(&wrap.key_encrypted, wrap_key)
                .map(|k| k == self.local_key)
                .unwrap_or(false);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn domain_create_open_and_passcode_flows() {
        let dir = TempDir::new().expect("tempdir");
        let mut domain = DomainStorage::create_fresh(dir.path(), "data").expect("create");
        assert!(!domain.has_passcode());
        assert!(!domain.app_lock_enabled());
        assert!(dir.path().join("key_datas").exists());

        // Add second account and switch active
        domain.accounts_mut().indices.push(1);
        domain.accounts_mut().active = 1;
        domain.write_accounts().expect("write accounts");

        let reopened = DomainStorage::open(dir.path(), "data", b"").expect("reopen");
        assert_eq!(reopened.local_key(), domain.local_key());
        assert_eq!(reopened.accounts().indices, vec![0, 1]);
        assert_eq!(reopened.accounts().active, 1);

        // Set modern Argon2id passcode with app lock armed (fast test parameters)
        let fast_argon = PasscodeKdf {
            kind: PasscodeKdfKind::Argon2id,
            memory: 64,
            time: 1,
            parallel: 1,
        };
        domain
            .set_passcode(b"my-secret-pass", fast_argon, true)
            .expect("set passcode");
        assert!(domain.has_passcode());
        assert!(domain.app_lock_enabled());
        assert!(domain.check_passcode(b"my-secret-pass"));
        assert!(!domain.check_passcode(b"wrong"));

        // Opening without passcode must fail
        assert!(matches!(
            DomainStorage::open(dir.path(), "data", b""),
            Err(StorageError::IncorrectPasscode)
        ));
        // Opening with wrong passcode must fail
        assert!(matches!(
            DomainStorage::open(dir.path(), "data", b"wrong"),
            Err(StorageError::IncorrectPasscode)
        ));
        // Opening with right passcode succeeds
        let unlocked = DomainStorage::open(dir.path(), "data", b"my-secret-pass").expect("unlock");
        assert_eq!(unlocked.local_key(), domain.local_key());
        assert!(unlocked.has_passcode());
        assert!(unlocked.app_lock_enabled());

        // Switch to Scrypt wrap while keeping open wrap (app lock disabled)
        let fast_scrypt = PasscodeKdf {
            kind: PasscodeKdfKind::Scrypt,
            memory: 16,
            time: 1,
            parallel: 1,
        };
        domain
            .set_passcode(b"scrypt-pass", fast_scrypt, false)
            .expect("scrypt");
        domain.set_app_lock_enabled(false).expect("disable lock");
        assert!(domain.has_passcode());
        assert!(!domain.app_lock_enabled());

        // Can open both with empty passcode AND with scrypt-pass
        let opened_empty = DomainStorage::open(dir.path(), "data", b"").expect("open empty");
        assert_eq!(opened_empty.local_key(), domain.local_key());
        let opened_scrypt =
            DomainStorage::open(dir.path(), "data", b"scrypt-pass").expect("open scrypt");
        assert_eq!(opened_scrypt.local_key(), domain.local_key());
    }

    #[test]
    fn domain_legacy_3blob_format_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let mut domain = DomainStorage::create_fresh(dir.path(), "data").expect("create");
        domain
            .set_legacy_passcode(b"")
            .expect("set legacy empty passcode");
        assert!(domain.key_data().legacy);
        assert!(!domain.has_passcode());

        let reopened = DomainStorage::open(dir.path(), "data", b"").expect("open legacy");
        assert!(reopened.key_data().legacy);
        assert_eq!(reopened.local_key(), domain.local_key());

        // Wallet keyring roundtrip
        domain
            .write_wallet_keyring(b"keyring-bytes")
            .expect("write keyring");
        assert_eq!(
            domain.read_wallet_keyring().expect("read keyring"),
            Some(b"keyring-bytes".to_vec())
        );
        domain.remove_wallet_keyring();
        assert_eq!(domain.read_wallet_keyring().expect("read absent"), None);
    }

    #[test]
    fn domain_verified_passcode_set_change_remove_and_reopen_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let mut domain = DomainStorage::create_fresh(dir.path(), "data").expect("create");
        let original_local_key = domain.local_key().clone();
        let fast_argon = PasscodeKdf {
            kind: PasscodeKdfKind::Argon2id,
            memory: 64,
            time: 1,
            parallel: 1,
        };

        // 1. Set first passcode without verification token (none exists yet).
        domain
            .set_passcode_verified(b"first-pass", fast_argon, None)
            .expect("set first passcode");
        assert!(domain.has_passcode());
        assert!(domain.app_lock_enabled());
        assert!(DomainStorage::needs_passcode(dir.path(), "data").expect("needs_passcode"));

        // 2. Attempting to change without verification fails and leaves passcode intact.
        assert!(matches!(
            domain.set_passcode_verified(b"second-pass", fast_argon, None),
            Err(StorageError::IncorrectPasscode)
        ));
        assert!(domain.check_passcode(b"first-pass"));

        // 3. Verify wrong passcode returns None; verify right passcode mints single-use token.
        assert!(domain.verify_passcode(b"wrong").is_none());
        let token = domain
            .verify_passcode(b"first-pass")
            .expect("verify first passcode");
        assert!(domain.accepts_verification(token));

        // 4. Change passcode using token; token is consumed (single-use).
        domain
            .set_passcode_verified(b"second-pass", fast_argon, Some(token))
            .expect("change passcode");
        assert!(!domain.accepts_verification(token));
        assert!(!domain.check_passcode(b"first-pass"));
        assert!(domain.check_passcode(b"second-pass"));
        assert_eq!(domain.local_key(), &original_local_key);

        // Reopen from disk confirms new passcode decrypts the exact same local_key.
        let reopened = DomainStorage::open(dir.path(), "data", b"second-pass").expect("reopen");
        assert_eq!(reopened.local_key(), &original_local_key);
        assert!(matches!(
            DomainStorage::open(dir.path(), "data", b"first-pass"),
            Err(StorageError::IncorrectPasscode)
        ));

        // 5. Remove passcode after verification; open wrap restored on disk.
        let remove_token = domain
            .verify_passcode(b"second-pass")
            .expect("verify second passcode");
        domain
            .set_passcode_verified(b"", fast_argon, Some(remove_token))
            .expect("remove passcode");
        assert!(!domain.has_passcode());
        assert!(!domain.app_lock_enabled());
        assert!(!DomainStorage::needs_passcode(dir.path(), "data").expect("needs_passcode false"));

        let reopened_open = DomainStorage::open(dir.path(), "data", b"").expect("reopen open");
        assert_eq!(reopened_open.local_key(), &original_local_key);
    }
}
