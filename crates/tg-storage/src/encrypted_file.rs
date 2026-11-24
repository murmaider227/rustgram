//! `TDEF` AES-256-CTR encrypted files used by tdesktop's `lib_storage` binlog caches.
//!
//! Port of `Telegram/lib_storage/storage/storage_encrypted_file.h` and `.cpp`.
//!
//! On-disk layout:
//! - Bytes `0..4`: `"TDEF"` magic (also used for file-lock region `kSkipBytes = 4`)
//! - Bytes `4..68`: 64-byte random `salt` (plaintext)
//! - Bytes `68..116` (48 bytes = 3 AES blocks, encrypted with AES-256-CTR starting at offset 0):
//!   - `u32` LE `format (8 bits) | reserved1 (24 bits)` (`format = 0`)
//!   - `u32` LE `reserved2 = 0`
//!   - `u64` LE `applicationVersion = 0`
//!   - `[u8; 32]` SHA-256 checksum over `key.data() || salt || header[64..80]`
//! - Bytes `116..`: payload encrypted with AES-256-CTR starting at encryption offset 48,
//!   16-byte block-aligned (with random trailing padding when written via `write_with_padding`).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use tg_crypto::{
    AES_BLOCK_SIZE, STORAGE_CTR_SALT_SIZE, StorageCtrState, StorageEncryptionKey, random_bytes,
    sha256,
};

use crate::error::{Result, StorageError};

/// 4-byte magic prefix of `lib_storage` encrypted files (`"TDEF"`).
pub const TDEF_MAGIC: [u8; 4] = *b"TDEF";

/// Byte offset skipped at file start (`base::FileLock::kSkipBytes = 4`).
pub const TDEF_SKIP_BYTES: u64 = 4;

/// Size of the encrypted part of `BasicHeader` (`16` metadata bytes + `32` SHA-256 checksum bytes).
pub const TDEF_HEADER_ENCRYPTED_SIZE: usize = 48;

/// Total size of `BasicHeader` (`64` salt + `48` encrypted metadata/checksum = `112` bytes).
pub const TDEF_BASIC_HEADER_SIZE: usize = STORAGE_CTR_SALT_SIZE + TDEF_HEADER_ENCRYPTED_SIZE;

/// File offset where encrypted payload data begins (`4 + 112 = 116`).
pub const TDEF_DATA_START_OFFSET: u64 = TDEF_SKIP_BYTES + TDEF_BASIC_HEADER_SIZE as u64;

/// Open mode for [`EncryptedFile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Read-only access to an existing `TDEF` file.
    Read,
    /// Open existing `TDEF` file for reading/appending, or create a new one if empty/missing.
    ReadAppend,
    /// Create or truncate a `TDEF` file for writing.
    Write,
}

/// AES-256-CTR encrypted file compatible with `Storage::File` in `lib_storage`.
pub struct EncryptedFile {
    file: File,
    state: StorageCtrState,
    encryption_offset: u64,
    data_size: u64,
}

impl EncryptedFile {
    /// Open or create a `TDEF` encrypted file at `path` with `mode` and `key`.
    pub fn open(path: &Path, mode: OpenMode, key: &StorageEncryptionKey) -> Result<Self> {
        if mode != OpenMode::Read
            && let Some(parent) = path.parent()
        {
            fs::create_dir_all(parent)?;
        }

        let file = match mode {
            OpenMode::Read => File::open(path)?,
            OpenMode::ReadAppend => OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?,
            OpenMode::Write => OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?,
        };

        let existing_len = file.metadata()?.len();
        match mode {
            OpenMode::Read => Self::read_header(file, existing_len, key),
            OpenMode::Write => Self::write_header(file, key),
            OpenMode::ReadAppend => {
                if existing_len == 0 {
                    Self::write_header(file, key)
                } else {
                    Self::read_header(file, existing_len, key)
                }
            }
        }
    }

    /// Total aligned encrypted payload byte length (excluding magic + header).
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.data_size
    }

    /// Current logical byte offset within the payload (0 = right after header).
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.encryption_offset
            .saturating_sub(TDEF_HEADER_ENCRYPTED_SIZE as u64)
    }

    /// Seek to a 16-byte-aligned payload offset (`Storage::File::seek`).
    pub fn seek(&mut self, payload_offset: u64) -> Result<()> {
        if payload_offset > self.data_size || !payload_offset.is_multiple_of(AES_BLOCK_SIZE as u64)
        {
            return Err(StorageError::CorruptData(format!(
                "invalid TDEF seek offset {payload_offset} (size {})",
                self.data_size
            )));
        }
        self.file
            .seek(SeekFrom::Start(TDEF_DATA_START_OFFSET + payload_offset))?;
        self.encryption_offset = TDEF_HEADER_ENCRYPTED_SIZE as u64 + payload_offset;
        Ok(())
    }

    /// Read up to `buf.len()` bytes (must be a multiple of 16) and decrypt in-place (`Storage::File::read`).
    pub fn read_blocks(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !buf.len().is_multiple_of(AES_BLOCK_SIZE) {
            return Err(StorageError::CorruptData(
                "TDEF read_blocks buffer must be 16-byte aligned".into(),
            ));
        }
        let mut total = 0usize;
        while total < buf.len() {
            let n = self.file.read(&mut buf[total..])?;
            if n == 0 {
                break;
            }
            total += n;
        }
        let rem = total % AES_BLOCK_SIZE;
        if rem != 0 {
            total -= rem;
            self.file.seek(SeekFrom::Current(-(rem as i64)))?;
        }
        if total > 0 {
            self.state
                .decrypt(&mut buf[..total], self.encryption_offset)
                .map_err(|e| StorageError::CorruptData(e.to_string()))?;
            self.encryption_offset += total as u64;
        }
        Ok(total)
    }

    /// Encrypt and write a 16-byte-aligned slice (`Storage::File::write`).
    pub fn write_blocks(&mut self, data: &[u8]) -> Result<()> {
        if !data.len().is_multiple_of(AES_BLOCK_SIZE) {
            return Err(StorageError::CorruptData(
                "TDEF write_blocks slice must be 16-byte aligned".into(),
            ));
        }
        let mut encrypted = data.to_vec();
        self.state
            .encrypt(&mut encrypted, self.encryption_offset)
            .map_err(|e| StorageError::CorruptData(e.to_string()))?;
        self.file.write_all(&encrypted)?;
        self.encryption_offset += encrypted.len() as u64;
        self.data_size = self.data_size.max(self.offset());
        Ok(())
    }

    /// Read `buf.len()` bytes of arbitrary length, consuming trailing block padding (`Storage::File::readWithPadding`).
    pub fn read_with_padding(&mut self, buf: &mut [u8]) -> Result<usize> {
        let part = buf.len() % AES_BLOCK_SIZE;
        let good = buf.len() - part;
        if good > 0 {
            let n = self.read_blocks(&mut buf[..good])?;
            if n != good {
                return Ok(n);
            }
        }
        if part == 0 {
            return Ok(good);
        }
        let mut last_block = [0u8; AES_BLOCK_SIZE];
        let n = self.read_blocks(&mut last_block)?;
        if n < AES_BLOCK_SIZE {
            return Ok(good);
        }
        buf[good..].copy_from_slice(&last_block[..part]);
        Ok(buf.len())
    }

    /// Encrypt and write arbitrary-length `data`, padding the final partial block with random bytes
    /// (`Storage::File::writeWithPadding`).
    pub fn write_with_padding(&mut self, data: &[u8]) -> Result<()> {
        let part = data.len() % AES_BLOCK_SIZE;
        let good = data.len() - part;
        if good > 0 {
            self.write_blocks(&data[..good])?;
        }
        if part > 0 {
            let mut last_block = [0u8; AES_BLOCK_SIZE];
            last_block[..part].copy_from_slice(&data[good..]);
            random_bytes(&mut last_block[part..]);
            self.write_blocks(&last_block)?;
        }
        Ok(())
    }

    /// Flush underlying file buffers.
    pub fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        Ok(())
    }

    fn write_header(mut file: File, key: &StorageEncryptionKey) -> Result<Self> {
        file.write_all(&TDEF_MAGIC)?;

        let mut salt = [0u8; STORAGE_CTR_SALT_SIZE];
        random_bytes(&mut salt);
        let state = key.prepare_ctr_state(&salt);

        // Header encrypted portion: format(0) + reserved1(0) + reserved2(0) + appVersion(0) + sha256(32)
        let mut enc_part = [0u8; TDEF_HEADER_ENCRYPTED_SIZE];
        let checksum = sha256(&[key.as_bytes(), &salt, &enc_part[..16]]);
        enc_part[16..48].copy_from_slice(&checksum);

        state
            .encrypt(&mut enc_part, 0)
            .map_err(|e| StorageError::CorruptData(e.to_string()))?;

        file.write_all(&salt)?;
        file.write_all(&enc_part)?;

        Ok(Self {
            file,
            state,
            encryption_offset: TDEF_HEADER_ENCRYPTED_SIZE as u64,
            data_size: 0,
        })
    }

    fn read_header(mut file: File, file_len: u64, key: &StorageEncryptionKey) -> Result<Self> {
        if file_len < TDEF_DATA_START_OFFSET {
            return Err(StorageError::InvalidMagic);
        }
        file.seek(SeekFrom::Start(TDEF_SKIP_BYTES))?;
        let mut salt = [0u8; STORAGE_CTR_SALT_SIZE];
        file.read_exact(&mut salt)?;
        let mut enc_part = [0u8; TDEF_HEADER_ENCRYPTED_SIZE];
        file.read_exact(&mut enc_part)?;

        let state = key.prepare_ctr_state(&salt);
        state
            .decrypt(&mut enc_part, 0)
            .map_err(|e| StorageError::CorruptData(e.to_string()))?;

        let expected = sha256(&[key.as_bytes(), &salt, &enc_part[..16]]);
        if enc_part[16..48] != expected {
            return Err(StorageError::DecryptionFailed);
        }

        let format_word = u32::from_le_bytes([enc_part[0], enc_part[1], enc_part[2], enc_part[3]]);
        let format = format_word & 0xFF;
        if format != 0 {
            return Err(StorageError::UnsupportedVersion(format as i32));
        }

        let raw_data_size = file_len - TDEF_DATA_START_OFFSET;
        let aligned_data_size = raw_data_size - (raw_data_size % AES_BLOCK_SIZE as u64);

        Ok(Self {
            file,
            state,
            encryption_offset: TDEF_HEADER_ENCRYPTED_SIZE as u64,
            data_size: aligned_data_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn tdef_encrypted_file_roundtrip_and_wrong_key() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("binlog");
        let key = StorageEncryptionKey::new(vec![0x5Au8; 256]).expect("key");
        let wrong_key = StorageEncryptionKey::new(vec![0xA5u8; 256]).expect("wrong key");

        let payload = b"Hello TDEF encrypted cache payload with non-block length!";
        {
            let mut f = EncryptedFile::open(&path, OpenMode::Write, &key).expect("open write");
            f.write_with_padding(payload).expect("write");
            f.flush().expect("flush");
        }

        // Verify on-disk magic
        let raw = fs::read(&path).expect("raw read");
        assert_eq!(&raw[0..4], b"TDEF");

        // Wrong key rejected
        assert!(matches!(
            EncryptedFile::open(&path, OpenMode::Read, &wrong_key),
            Err(StorageError::DecryptionFailed)
        ));

        // Read back with right key
        let mut f = EncryptedFile::open(&path, OpenMode::Read, &key).expect("open read");
        let mut out = vec![0u8; payload.len()];
        let n = f.read_with_padding(&mut out).expect("read");
        assert_eq!(n, payload.len());
        assert_eq!(&out, payload);

        // Seek and re-read
        f.seek(0).expect("seek");
        let mut block = [0u8; 16];
        assert_eq!(f.read_blocks(&mut block).expect("read block"), 16);
        assert_eq!(&block, &payload[..16]);
    }
}
