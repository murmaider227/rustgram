//! TDF$ file format, file key naming, and AES-IGE encrypted descriptor I/O.
//!
//! Port of `Telegram/SourceFiles/storage/details/storage_file_utilities.h` and `.cpp`.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use tg_crypto::{
    AuthKey, TDF_MAGIC as CRYPTO_TDF_MAGIC, aes_encrypt_local, decrypt_local_data,
    encrypt_local_data, md5, random_u32, random_u64, sha1, tdf_checksum, verify_tdf_checksum,
};

use crate::error::{Result, StorageError};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};

/// 4-byte magic header at the start of every TDF file (`TdfMagic`).
pub const TDF_MAGIC: [u8; 4] = CRYPTO_TDF_MAGIC;

/// Reference tdesktop application version (`AppVersion` in `core/version.h`, v7.4.5).
pub const APP_VERSION: i32 = 7_004_005;

/// 64-bit random identifier for an encrypted file inside an account directory (`Storage::FileKey`).
pub type FileKey = u64;

/// Encode a 64-bit `FileKey` into tdesktop's 16-character uppercase hex filename part (`ToFilePart`).
///
/// Important: tdesktop encodes the **least-significant nibble first**.
/// For example, `0x0123_4567_89AB_CDEF` encodes to `"FEDCBA9876543210"`.
#[must_use]
pub fn to_file_part(mut val: FileKey) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(16);
    for _ in 0..16 {
        let v = (val & 0x0F) as usize;
        out.push(HEX[v] as char);
        val >>= 4;
    }
    out
}

/// Inverse of [`to_file_part`]: parse a 16-character uppercase/lowercase hex filename part into a `FileKey`.
#[must_use]
pub fn from_file_part(s: &str) -> Option<FileKey> {
    let bytes = s.as_bytes();
    if bytes.len() != 16 {
        return None;
    }
    let mut val = 0u64;
    for (i, &b) in bytes.iter().enumerate() {
        let nibble = match b {
            b'0'..=b'9' => (b - b'0') as u64,
            b'A'..=b'F' => (b - b'A' + 10) as u64,
            b'a'..=b'f' => (b - b'a' + 10) as u64,
            _ => return None,
        };
        val |= nibble << (i * 4);
    }
    Some(val)
}

/// Compute the 64-bit `FileKey` for an account's `data_name` (`ComputeDataNameKey`).
///
/// Computes `MD5(data_name.as_bytes())` and interprets the first 8 bytes as a little-endian `u64`.
/// For the default account `"data"`, `to_file_part(compute_data_name_key("data"))` yields
/// `"D877F783D5D3EF8C"`.
#[must_use]
pub fn compute_data_name_key(data_name: &str) -> FileKey {
    let digest = md5(&[data_name.as_bytes()]);
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&digest[0..8]);
    u64::from_le_bytes(first8)
}

/// Compose the account data name for slot `index` (`ComposeDataString` in `main_account.cpp`).
///
/// Strips `'#'` characters from `data_name`, and for `index > 0` appends `"#<index + 1>"`.
#[must_use]
pub fn compose_data_string(data_name: &str, index: i32) -> String {
    let mut result = data_name.replace('#', "");
    if index > 0 {
        result.push('#');
        result.push_str(&(index + 1).to_string());
    }
    result
}

/// Check if any suffix (`'0'`, `'1'`, `'s'`) already exists for `base_path / name` (`KeyAlreadyUsed`).
#[must_use]
pub fn key_already_used(base_path: &Path, name: &str) -> bool {
    for suffix in ['0', '1', 's'] {
        if base_path.join(format!("{name}{suffix}")).exists() {
            return true;
        }
    }
    false
}

/// Generate a non-zero random `FileKey` unused in `base_path` (`GenerateKey`).
#[must_use]
pub fn generate_key(base_path: &Path) -> FileKey {
    loop {
        let key = random_u64();
        if key != 0 && !key_already_used(base_path, &to_file_part(key)) {
            return key;
        }
    }
}

/// Remove `'0'`, `'1'`, and `'s'` files for `key` in `base_path` (`ClearKey`).
pub fn clear_key(key: FileKey, base_path: &Path) {
    clear_name(&to_file_part(key), base_path);
}

/// Remove `'0'`, `'1'`, and `'s'` files for `name` in `base_path`.
pub fn clear_name(name: &str, base_path: &Path) {
    for suffix in ['0', '1', 's'] {
        let _ = fs::remove_file(base_path.join(format!("{name}{suffix}")));
    }
}

/// Decoded contents of a TDF$ file (`Storage::details::FileReadDescriptor`).
#[derive(Debug, Clone)]
pub struct FileReadDescriptor {
    /// Application version stored in the TDF header.
    pub version: i32,
    /// Verified payload bytes (or decrypted inner bytes after `read_encrypted_file`).
    pub data: Vec<u8>,
}

impl FileReadDescriptor {
    /// Create a `QDataStreamReader` positioned at the start of the payload.
    #[must_use]
    pub fn stream(&self) -> QDataStreamReader<'_> {
        QDataStreamReader::new(&self.data)
    }
}

/// Buffer for constructing or reading an AES-IGE locally-encrypted blob (`EncryptedDescriptor`).
#[derive(Debug, Clone, Default)]
pub struct EncryptedDescriptor {
    /// Inner unencrypted QDataStream payload bytes (without the 4-byte length prefix).
    pub writer: QDataStreamWriter,
}

impl EncryptedDescriptor {
    /// Create an empty encrypted descriptor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            writer: QDataStreamWriter::new(),
        }
    }

    /// Create an encrypted descriptor with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(size: usize) -> Self {
        Self {
            writer: QDataStreamWriter::with_capacity(size),
        }
    }

    /// Encrypt the accumulated payload bytes using `key` (`PrepareEncrypted`).
    pub fn prepare_encrypted(&self, key: &AuthKey) -> Result<Vec<u8>> {
        prepare_encrypted(self.writer.as_bytes(), key)
    }
}

/// Encrypt a plaintext payload into tdesktop's local encrypted blob format (`PrepareEncrypted`).
///
/// Layout of plaintext before encryption:
/// - `u32` little-endian `size = 4 + payload.len()`
/// - `payload` bytes
/// - `0..15` random padding bytes so total length `full_size` is a multiple of 16
///
/// Output layout:
/// - `key128 = SHA1(padded_plaintext)[0..16]` (16 bytes)
/// - `aes_encrypt_local(padded_plaintext, key, &key128)` (`full_size` bytes)
pub fn prepare_encrypted(payload: &[u8], key: &AuthKey) -> Result<Vec<u8>> {
    encrypt_local_data(payload, key).map_err(|e| StorageError::CorruptData(e.to_string()))
}

/// Deterministic helper for testing: encrypt an already length-prefixed and padded buffer.
#[allow(dead_code)]
pub(crate) fn prepare_encrypted_with_padded(
    padded_plaintext: &[u8],
    key: &AuthKey,
) -> Result<Vec<u8>> {
    let digest = sha1(&[padded_plaintext]);
    let mut key128 = [0u8; 16];
    key128.copy_from_slice(&digest[0..16]);

    let ciphertext = aes_encrypt_local(padded_plaintext, key, &key128)
        .map_err(|e| StorageError::CorruptData(e.to_string()))?;
    let mut encrypted = Vec::with_capacity(16 + ciphertext.len());
    encrypted.extend_from_slice(&key128);
    encrypted.extend_from_slice(&ciphertext);
    Ok(encrypted)
}

/// Decrypt a tdesktop local encrypted blob (`DecryptLocal`) and return the inner payload bytes
/// (with the leading 4-byte length prefix stripped and validated).
pub fn decrypt_local(encrypted: &[u8], key: &AuthKey) -> Result<Vec<u8>> {
    decrypt_local_data(encrypted, key).map_err(|_| StorageError::DecryptionFailed)
}

/// Serialize raw TDF$ file bytes (magic + LE version + payload + MD5 checksum).
///
/// MD5 is computed over `payload || (payload.len() as i32).to_le_bytes() || version.to_le_bytes() || b"TDF$"`.
pub fn encode_tdf_bytes(version: i32, payload: &[u8]) -> Result<Vec<u8>> {
    let digest =
        tdf_checksum(payload, version).map_err(|e| StorageError::CorruptData(e.to_string()))?;

    let mut out = Vec::with_capacity(4 + 4 + payload.len() + 16);
    out.extend_from_slice(&TDF_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&digest);
    Ok(out)
}

/// Parse and verify raw TDF$ file bytes against `max_version`.
pub fn decode_tdf_bytes(raw: &[u8], max_version: i32) -> Result<FileReadDescriptor> {
    if raw.len() < 4 + 4 + 16 {
        return Err(StorageError::InvalidMagic);
    }
    if raw[0..4] != TDF_MAGIC {
        return Err(StorageError::InvalidMagic);
    }
    let version = i32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
    if version > max_version {
        return Err(StorageError::UnsupportedVersion(version));
    }

    let payload_end = raw.len() - 16;
    let payload = &raw[8..payload_end];
    let mut stored_md5 = [0u8; 16];
    stored_md5.copy_from_slice(&raw[payload_end..]);

    if !verify_tdf_checksum(payload, version, &stored_md5) {
        return Err(StorageError::ChecksumMismatch);
    }

    Ok(FileReadDescriptor {
        version,
        data: payload.to_vec(),
    })
}

/// Builder for writing a TDF$ file (`Storage::details::FileWriteDescriptor`).
#[derive(Debug)]
pub struct FileWriteDescriptor {
    base_path: PathBuf,
    name: String,
    version: i32,
    stream: QDataStreamWriter,
    finished: bool,
}

impl FileWriteDescriptor {
    /// Create a descriptor targeting `<base_path>/<ToFilePart(key)>s`.
    #[must_use]
    pub fn from_key(key: FileKey, base_path: impl Into<PathBuf>) -> Self {
        Self::new(to_file_part(key), base_path)
    }

    /// Create a descriptor targeting `<base_path>/<name>s`.
    #[must_use]
    pub fn new(name: impl Into<String>, base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            name: name.into(),
            version: APP_VERSION,
            stream: QDataStreamWriter::new(),
            finished: false,
        }
    }

    /// Override the TDF header version (defaults to [`APP_VERSION`]).
    #[must_use]
    pub fn with_version(mut self, version: i32) -> Self {
        self.version = version;
        self
    }

    /// Append a `QByteArray` blob to the TDF payload (`FileWriteDescriptor::writeData`).
    ///
    /// Empty slices are encoded with the Qt null `QByteArray` marker (`0xFFFFFFFF`), matching
    /// tdesktop's `QByteArray()` default.
    pub fn write_data(&mut self, data: &[u8]) {
        self.stream.write_byte_array(data);
    }

    /// Append an explicit `Option<&[u8]>` `QByteArray` blob (`None` -> `0xFFFFFFFF`, `Some(b)` -> `len + b`).
    pub fn write_opt_data(&mut self, data: Option<&[u8]>) {
        self.stream.write_opt_byte_array(data);
    }

    /// Encrypt `payload` with `key` and append it as a `QByteArray` blob (`FileWriteDescriptor::writeEncrypted`).
    pub fn write_encrypted(&mut self, payload: &[u8], key: &AuthKey) -> Result<()> {
        let encrypted = prepare_encrypted(payload, key)?;
        self.write_data(&encrypted);
        Ok(())
    }

    /// Encode the full TDF$ bytes without writing to disk.
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_tdf_bytes(self.version, self.stream.as_bytes())
    }

    /// Consume this descriptor and build a [`crate::write_manager::WriteEntry`] ready for
    /// background writing via [`crate::write_manager::AsyncWriteManager`].
    pub fn into_write_entry(mut self) -> Result<crate::write_manager::WriteEntry> {
        self.finished = true;
        let full_bytes = encode_tdf_bytes(self.version, self.stream.as_bytes())?;
        Ok(crate::write_manager::WriteEntry {
            base_path: self.base_path,
            name: self.name,
            full_bytes,
        })
    }

    /// Atomically commit the TDF$ file to `<base_path>/<name>s` and remove legacy `0`/`1` files.
    pub fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let full_bytes = encode_tdf_bytes(self.version, self.stream.as_bytes())?;
        write_tdf_file_atomic(&self.base_path, &self.name, &full_bytes)
    }
}

/// Atomically write an encoded TDF$ buffer to `<base_path>/<name>s`, removing legacy `0`/`1` files
/// and falling back to `<name>0` + sync + rename (matching `WriteManager::writeNow`).
pub(crate) fn write_tdf_file_atomic(base_path: &Path, name: &str, full_bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(base_path)?;

    let safe_path = base_path.join(format!("{name}s"));
    let simple_path = base_path.join(format!("{name}0"));
    let backup_path = base_path.join(format!("{name}1"));

    // Write to a sibling temporary file, flush/sync, then rename over `<name>s`
    // (matching QSaveFile::commit() + fallback to '0' -> rename in WriteManager::writeNow).
    let nonce: u32 = random_u32();
    let temp_path = base_path.join(format!("{name}s.{nonce:08x}.tmp"));

    let write_temp = (|| -> std::io::Result<()> {
        let mut file = File::create(&temp_path)?;
        file.write_all(full_bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    match write_temp.and_then(|()| replace_file(&temp_path, &safe_path)) {
        Ok(()) => {
            let _ = fs::remove_file(&simple_path);
            let _ = fs::remove_file(&backup_path);
            Ok(())
        }
        Err(_) => {
            let _ = fs::remove_file(&temp_path);
            // Fallback path: write `<name>0`, sync, remove `<name>1`, rename to `<name>s`.
            let mut plain = File::create(&simple_path)?;
            plain.write_all(full_bytes)?;
            plain.sync_all()?;
            drop(plain);
            let _ = fs::remove_file(&backup_path);
            replace_file(&simple_path, &safe_path)?;
            Ok(())
        }
    }
}

fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            if to.exists() {
                fs::remove_file(to)?;
                fs::rename(from, to)
            } else {
                Err(first_err)
            }
        }
    }
}

/// Read and verify a TDF$ file from `base_path` (`Storage::details::ReadFile`).
///
/// Checks `<name>s` first if it exists; otherwise tries legacy `<name>0` and `<name>1`
/// ordered by most recent modification time first.
pub fn read_file(name: &str, base_path: &Path) -> Result<FileReadDescriptor> {
    read_file_with_max_version(name, base_path, APP_VERSION)
}

/// Read and verify a TDF$ file allowing versions up to `max_version`.
pub fn read_file_with_max_version(
    name: &str,
    base_path: &Path,
    max_version: i32,
) -> Result<FileReadDescriptor> {
    let modern = base_path.join(format!("{name}s"));
    let candidates: Vec<PathBuf> = if modern.exists() {
        vec![modern]
    } else {
        let p0 = base_path.join(format!("{name}0"));
        let p1 = base_path.join(format!("{name}1"));
        match (p0.exists(), p1.exists()) {
            (true, true) => {
                let m0 = fs::metadata(&p0).and_then(|m| m.modified()).ok();
                let m1 = fs::metadata(&p1).and_then(|m| m.modified()).ok();
                if m0 < m1 { vec![p1, p0] } else { vec![p0, p1] }
            }
            (true, false) => vec![p0],
            (false, true) => vec![p1],
            (false, false) => return Err(StorageError::NotFound(modern)),
        }
    };

    let mut last_err = StorageError::NotFound(base_path.join(format!("{name}s")));
    for candidate in &candidates {
        match fs::read(candidate) {
            Ok(raw) => match decode_tdf_bytes(&raw, max_version) {
                Ok(desc) => return Ok(desc),
                Err(err) => last_err = err,
            },
            Err(err) => last_err = StorageError::Io(err),
        }
    }
    Err(last_err)
}

/// Read and decrypt an encrypted TDF$ file (`Storage::details::ReadEncryptedFile`).
pub fn read_encrypted_file(
    name: &str,
    base_path: &Path,
    key: &AuthKey,
) -> Result<FileReadDescriptor> {
    let desc = read_file(name, base_path)?;
    let mut reader = desc.stream();
    let encrypted = reader.read_byte_array()?;
    let inner = decrypt_local(&encrypted, key)?;
    Ok(FileReadDescriptor {
        version: desc.version,
        data: inner,
    })
}

/// Read and decrypt an encrypted TDF$ file addressed by `FileKey`.
pub fn read_encrypted_file_by_key(
    key_id: FileKey,
    base_path: &Path,
    key: &AuthKey,
) -> Result<FileReadDescriptor> {
    read_encrypted_file(&to_file_part(key_id), base_path, key)
}

/// Write a single encrypted TDF$ file addressed by `name`.
pub fn write_encrypted_file(
    name: &str,
    base_path: &Path,
    payload: &[u8],
    key: &AuthKey,
) -> Result<()> {
    let mut desc = FileWriteDescriptor::new(name, base_path);
    desc.write_encrypted(payload, key)?;
    desc.finish()
}

/// Write a single encrypted TDF$ file addressed by `FileKey`.
pub fn write_encrypted_file_by_key(
    key_id: FileKey,
    base_path: &Path,
    payload: &[u8],
    key: &AuthKey,
) -> Result<()> {
    write_encrypted_file(&to_file_part(key_id), base_path, payload, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn to_file_part_known_answer_data_folder() {
        // Known tdesktop default account folder name for "data":
        // MD5("data") = 8d777f385d3dfec8815d20f7496026dc
        // LE u64 of first 8 bytes = 0xc8fe3d5d387f778d
        // LSB-nibble-first hex = "D877F783D5D3EF8C"
        let key = compute_data_name_key("data");
        assert_eq!(key, 0xC8FE_3D5D_387F_778D);
        let part = to_file_part(key);
        assert_eq!(part, "D877F783D5D3EF8C");
        assert_eq!(from_file_part(&part), Some(key));

        // Account #2 ("data#2")
        assert_eq!(compose_data_string("data", 0), "data");
        assert_eq!(compose_data_string("data", 1), "data#2");
        assert_eq!(compose_data_string("da#ta", 2), "data#3");
    }

    #[test]
    fn tdf_exact_byte_layout_and_md5() {
        let payload = b"\x00\x00\x00\x05hello";
        let version = 7_004_005i32;
        let encoded = encode_tdf_bytes(version, payload).expect("encode");

        assert_eq!(&encoded[0..4], b"TDF$");
        assert_eq!(&encoded[4..8], &version.to_le_bytes());
        assert_eq!(&encoded[8..8 + payload.len()], payload);
        assert_eq!(encoded.len(), 8 + payload.len() + 16);

        // Verify hand-computed MD5 order: payload || LE(len) || LE(version) || "TDF$"
        let expected_md5 = md5(&[
            payload,
            &(payload.len() as i32).to_le_bytes(),
            &version.to_le_bytes(),
            b"TDF$",
        ]);
        assert_eq!(&encoded[8 + payload.len()..], &expected_md5);

        let decoded = decode_tdf_bytes(&encoded, APP_VERSION).expect("decode");
        assert_eq!(decoded.version, version);
        assert_eq!(decoded.data, payload);

        // Tampering any byte fails checksum
        let mut tampered = encoded.clone();
        tampered[8] ^= 0x01;
        assert!(matches!(
            decode_tdf_bytes(&tampered, APP_VERSION),
            Err(StorageError::ChecksumMismatch)
        ));
    }

    #[test]
    fn encrypted_file_roundtrip_and_legacy_fallback() {
        let dir = TempDir::new().expect("tempdir");
        let key = AuthKey::new([0x3Cu8; 256]);
        let payload = b"secret tdesktop payload 12345";

        write_encrypted_file("testfile", dir.path(), payload, &key).expect("write");
        assert!(dir.path().join("testfiles").exists());

        let read = read_encrypted_file("testfile", dir.path(), &key).expect("read");
        assert_eq!(read.version, APP_VERSION);
        assert_eq!(read.data, payload);

        // Wrong key fails
        let wrong_key = AuthKey::new([0x99u8; 256]);
        assert!(matches!(
            read_encrypted_file("testfile", dir.path(), &wrong_key),
            Err(StorageError::DecryptionFailed)
        ));

        // Legacy '0'/'1' fallback: move 's' to '0' and put corrupt '1'
        fs::rename(dir.path().join("testfiles"), dir.path().join("testfile0")).expect("rename");
        fs::write(dir.path().join("testfile1"), b"TDF$corrupt").expect("write corrupt");

        let read_legacy = read_encrypted_file("testfile", dir.path(), &key).expect("legacy read");
        assert_eq!(read_legacy.data, payload);
    }
}
