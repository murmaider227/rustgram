//! Zero-copy binary reader for TL streams.
//!
//! Port of `tl::Reader` and `tl::string_type::read` from `lib_tl/tl/tl_basic_types.h`
//! and `mtproto/core_types.h`.

use crate::error::DeserializeError;

/// Zero-copy cursor over a serialized TL byte slice.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Creates a new cursor positioned at the start of `data`.
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Returns the current byte offset from the start of the underlying slice.
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Returns the number of unread bytes remaining.
    pub const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Returns `true` if no unread bytes remain.
    pub const fn is_empty(&self) -> bool {
        self.pos == self.data.len()
    }

    /// Returns the unread tail of the underlying slice without advancing the cursor.
    pub fn as_slice(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    /// Peeks the next 32-bit little-endian unsigned integer without advancing the cursor.
    pub fn peek_u32(&self) -> Result<u32, DeserializeError> {
        let rem = self.remaining();
        if rem < 4 {
            return Err(DeserializeError::UnexpectedEof {
                needed: 4,
                remaining: rem,
            });
        }
        let s = &self.data[self.pos..self.pos + 4];
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// Reads a slice of exactly `len` bytes and advances the cursor.
    pub fn read_slice(&mut self, len: usize) -> Result<&'a [u8], DeserializeError> {
        let rem = self.remaining();
        if rem < len {
            return Err(DeserializeError::UnexpectedEof {
                needed: len,
                remaining: rem,
            });
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.data[start..self.pos])
    }

    /// Reads a fixed-size byte array `[u8; N]` and advances the cursor.
    pub fn read_exact<const N: usize>(&mut self) -> Result<[u8; N], DeserializeError> {
        let slice = self.read_slice(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }

    /// Consumes and returns all remaining unread bytes.
    pub fn read_remaining(&mut self) -> &'a [u8] {
        let tail = &self.data[self.pos..];
        self.pos = self.data.len();
        tail
    }

    /// Reads a 32-bit little-endian unsigned integer.
    pub fn read_u32(&mut self) -> Result<u32, DeserializeError> {
        Ok(u32::from_le_bytes(self.read_exact::<4>()?))
    }

    /// Reads a 32-bit little-endian signed integer (`int`).
    pub fn read_i32(&mut self) -> Result<i32, DeserializeError> {
        Ok(i32::from_le_bytes(self.read_exact::<4>()?))
    }

    /// Reads a 64-bit little-endian signed integer (`long`).
    pub fn read_i64(&mut self) -> Result<i64, DeserializeError> {
        Ok(i64::from_le_bytes(self.read_exact::<8>()?))
    }

    /// Reads a 64-bit IEEE 754 little-endian floating-point number (`double`).
    pub fn read_f64(&mut self) -> Result<f64, DeserializeError> {
        Ok(f64::from_le_bytes(self.read_exact::<8>()?))
    }

    /// Reads a TL-encoded byte string (`bytes` / `string`), validating 4-byte alignment padding.
    ///
    /// Encoding rules per TL spec and `lib_tl/tl/tl_basic_types.h`:
    /// - If first byte `len < 254`: `len` payload bytes follow, padded with zero bytes so
    ///   `1 + len + pad` is a multiple of 4.
    /// - If first byte `== 254`: 3 little-endian length bytes follow, then `len` payload bytes,
    ///   padded so `len + pad` is a multiple of 4.
    /// - First byte `== 255` is invalid (`DeserializeError::InvalidStringLength`).
    pub fn read_tl_bytes(&mut self) -> Result<Vec<u8>, DeserializeError> {
        let first = self.read_exact::<1>()?[0];
        let (len, pad) = if first < 254 {
            let len = usize::from(first);
            let pad = (4 - ((1 + len) % 4)) % 4;
            (len, pad)
        } else if first == 254 {
            let b = self.read_exact::<3>()?;
            let len = usize::from(b[0]) | (usize::from(b[1]) << 8) | (usize::from(b[2]) << 16);
            let pad = (4 - (len % 4)) % 4;
            (len, pad)
        } else {
            return Err(DeserializeError::InvalidStringLength { prefix: first });
        };

        let total = len.saturating_add(pad);
        let rem = self.remaining();
        if rem < total {
            return Err(DeserializeError::UnexpectedEof {
                needed: total,
                remaining: rem,
            });
        }
        let payload = self.read_slice(len)?.to_vec();
        let _ = self.read_slice(pad)?;
        Ok(payload)
    }

    /// Reads a TL-encoded string, decoding invalid UTF-8 sequences lossily (`U+FFFD`).
    pub fn read_tl_string(&mut self) -> Result<String, DeserializeError> {
        let bytes = self.read_tl_bytes()?;
        match String::from_utf8(bytes) {
            Ok(s) => Ok(s),
            Err(e) => Ok(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        }
    }
}
