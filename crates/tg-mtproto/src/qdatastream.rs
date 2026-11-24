//! Qt `QDataStream` (`QDataStream::Qt_5_1`) compatible big-endian binary reader and writer.
//!
//! Port of the `QDataStream` / `Serialize::bytes` helpers from
//! `Telegram/SourceFiles/storage/serialize_common.h` used by `mtproto_dc_options.cpp` and
//! `mtproto_config.cpp` so serialized bytes are byte-for-byte compatible with `tg-storage` and
//! Telegram Desktop `tdata`.

/// Null marker for `QByteArray` and `QString` in `QDataStream`.
pub const QDATASTREAM_NULL_LEN: u32 = 0xFFFF_FFFF;

/// Maximum single allocation permitted when decoding untrusted stream lengths (16 MiB).
const MAX_ALLOC_BYTES: usize = 16 * 1024 * 1024;

/// Big-endian `QDataStream::Qt_5_1` writer.
#[derive(Debug, Clone, Default)]
pub(crate) struct QDataWriter {
    buf: Vec<u8>,
}

impl QDataWriter {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn write_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Writes a `QByteArray` / `Serialize::bytes` matching tdesktop (`empty => 0xFFFFFFFF`).
    pub fn write_byte_array(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            self.write_u32(QDATASTREAM_NULL_LEN);
        } else {
            self.write_u32(bytes.len() as u32);
            self.buf.extend_from_slice(bytes);
        }
    }

    /// Writes a `QString` (UTF-16BE): empty string writes `0xFFFFFFFF` (null `QString()`).
    pub fn write_qstring(&mut self, s: &str) {
        if s.is_empty() {
            self.write_u32(QDATASTREAM_NULL_LEN);
        } else {
            let utf16: Vec<u16> = s.encode_utf16().collect();
            self.write_u32((utf16.len() * 2) as u32);
            for unit in utf16 {
                self.write_u16(unit);
            }
        }
    }
}

/// Big-endian `QDataStream::Qt_5_1` reader.
#[derive(Debug, Clone)]
pub(crate) struct QDataReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> QDataReader<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub const fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn read_raw(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        if end > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    pub fn read_i32(&mut self) -> Option<i32> {
        let raw = self.read_raw(4)?;
        Some(i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    pub fn read_u32(&mut self) -> Option<u32> {
        let raw = self.read_raw(4)?;
        Some(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    pub fn read_u64(&mut self) -> Option<u64> {
        let raw = self.read_raw(8)?;
        Some(u64::from_be_bytes([
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
        ]))
    }

    pub fn read_byte_array(&mut self) -> Option<Vec<u8>> {
        let len = self.read_u32()?;
        if len == QDATASTREAM_NULL_LEN {
            return Some(Vec::new());
        }
        let len_usize = len as usize;
        if len_usize > MAX_ALLOC_BYTES {
            return None;
        }
        Some(self.read_raw(len_usize)?.to_vec())
    }

    pub fn read_qstring(&mut self) -> Option<String> {
        let byte_len = self.read_u32()?;
        if byte_len == QDATASTREAM_NULL_LEN {
            return Some(String::new());
        }
        let byte_len_usize = byte_len as usize;
        if !byte_len_usize.is_multiple_of(2) || byte_len_usize > MAX_ALLOC_BYTES {
            return None;
        }
        let raw = self.read_raw(byte_len_usize)?;
        let mut units = Vec::with_capacity(byte_len_usize / 2);
        for &pair in raw.as_chunks::<2>().0 {
            units.push(u16::from_be_bytes(pair));
        }
        String::from_utf16(&units).ok()
    }
}
