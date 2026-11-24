//! Qt `QDataStream` (`QDataStream::Qt_5_1` / `Qt_5_3`) compatible big-endian binary reader and writer.
//!
//! Port of `Telegram/SourceFiles/storage/serialize_common.h` and `serialize_common.cpp`,
//! implementing Qt 5 `QDataStream` wire encoding rules used across `tg-storage`, `tg-core`,
//! and `tg-lang`.

use thiserror::Error;

/// Null marker for `QByteArray` and `QString` in `QDataStream`.
pub const QDATASTREAM_NULL_LEN: u32 = 0xFFFF_FFFF;

/// Maximum single allocation permitted when decoding untrusted stream lengths (64 MiB).
const MAX_ALLOC_BYTES: usize = 64 * 1024 * 1024;

/// Maximum collection item count permitted when decoding untrusted streams.
const MAX_COLLECTION_COUNT: usize = 1_000_000;

/// Errors returned while decoding a `QDataStream` payload.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DataStreamError {
    /// Reached end of input before completing the requested read.
    #[error("unexpected end of QDataStream (needed {needed} bytes, {remaining} remaining)")]
    UnexpectedEof {
        /// Bytes required by the read operation.
        needed: usize,
        /// Bytes remaining in the buffer.
        remaining: usize,
    },
    /// A `QString` had an odd byte length (cannot hold UTF-16 code units).
    #[error("invalid QString UTF-16 byte length: {0}")]
    OddStringByteLength(u32),
    /// A `QString` contained an invalid UTF-16 sequence.
    #[error("corrupt QString data in QDataStream")]
    CorruptString,
    /// A count or length field exceeded safe bounds or was negative.
    #[error("invalid length or count in QDataStream: {0}")]
    InvalidLength(i64),
}

/// Qt 5.1 `QDateTime` wire representation (13 bytes: `qint64` Julian day, `quint32` ms, `qint8` spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QtDateTime {
    /// Julian day number (`i64::MIN` represents a null/invalid `QDate`).
    pub julian_day: i64,
    /// Milliseconds since start of day (`u32::MAX` represents a null/invalid `QTime`).
    pub msecs_since_midnight: u32,
    /// `Qt::TimeSpec`: `0` = LocalTime, `1` = UTC.
    pub time_spec: i8,
}

impl Default for QtDateTime {
    fn default() -> Self {
        Self::null()
    }
}

impl QtDateTime {
    /// Null / invalid `QDateTime`.
    #[must_use]
    pub const fn null() -> Self {
        Self {
            julian_day: i64::MIN,
            msecs_since_midnight: u32::MAX,
            time_spec: 0,
        }
    }

    /// Construct a UTC `QtDateTime` from Unix timestamp in milliseconds.
    #[must_use]
    pub const fn from_unix_millis_utc(millis: i64) -> Self {
        // Julian day of 1970-01-01 is 2_440_588.
        const UNIX_EPOCH_JD: i64 = 2_440_588;
        const MS_PER_DAY: i64 = 86_400_000;
        let days = millis.div_euclid(MS_PER_DAY);
        let rem = millis.rem_euclid(MS_PER_DAY) as u32;
        Self {
            julian_day: UNIX_EPOCH_JD + days,
            msecs_since_midnight: rem,
            time_spec: 1,
        }
    }

    /// Convert to Unix timestamp in milliseconds if valid.
    #[must_use]
    pub const fn to_unix_millis(&self) -> Option<i64> {
        const UNIX_EPOCH_JD: i64 = 2_440_588;
        const MS_PER_DAY: i64 = 86_400_000;
        if self.julian_day == i64::MIN || self.msecs_since_midnight == u32::MAX {
            return None;
        }
        let Some(days) = self.julian_day.checked_sub(UNIX_EPOCH_JD) else {
            return None;
        };
        let Some(day_ms) = days.checked_mul(MS_PER_DAY) else {
            return None;
        };
        day_ms.checked_add(self.msecs_since_midnight as i64)
    }
}

/// RGBA color packed as in `Serialize::writeColor` / `Serialize::readColor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QtColor {
    /// Red channel (0..=255).
    pub r: u8,
    /// Green channel (0..=255).
    pub g: u8,
    /// Blue channel (0..=255).
    pub b: u8,
    /// Alpha channel (0..=255).
    pub a: u8,
}

impl QtColor {
    /// Construct an RGBA color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Construct an opaque RGB color (`a = 255`).
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

/// Serialized size of a `QString` in a `QDataStream` (`Serialize::stringSize`).
#[must_use]
pub fn string_size(s: &str) -> usize {
    4 + s.encode_utf16().count() * 2
}

/// Serialized size of a `QByteArray` in a `QDataStream` (`Serialize::bytearraySize`).
#[must_use]
pub fn bytearray_size(bytes: &[u8]) -> usize {
    4 + bytes.len()
}

/// Serialized size of a `QColor` in `Serialize::writeColor`.
#[must_use]
pub const fn color_size() -> usize {
    4
}

/// Serialized size of a `QDateTime` in `QDataStream::Qt_5_1` (`Serialize::dateTimeSize`).
#[must_use]
pub const fn datetime_size() -> usize {
    8 + 4 + 1
}

/// Big-endian `QDataStream::Qt_5_1` / `Qt_5_3` writer (`Serialize::ByteArrayWriter`).
#[derive(Debug, Clone, Default)]
pub struct QDataStreamWriter {
    buf: Vec<u8>,
}

impl QDataStreamWriter {
    /// Create a new empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Create a writer with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    /// Consume the writer and return the accumulated bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Borrow the accumulated bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Current byte length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether no bytes have been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Write raw bytes without any length prefix (`QDataStream::writeRawData`).
    pub fn write_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Write a Qt `bool` (1 byte: `0` or `1`).
    pub fn write_bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    /// Write a boolean using tdesktop's `qint32` convention (`1` for `true`, `0` for `false`).
    pub fn write_bool_i32(&mut self, v: bool) {
        self.write_i32(i32::from(v));
    }

    /// Write `qint8`.
    pub fn write_i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    /// Write `quint8`.
    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Write `qint16` (big-endian).
    pub fn write_i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Write `quint16` (big-endian).
    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Write `qint32` (big-endian).
    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Write `quint32` (big-endian).
    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Write `qint64` (big-endian).
    pub fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Write `quint64` (big-endian).
    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Write IEEE 754 `f64` (`qreal`, big-endian).
    pub fn write_f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Write a null `QByteArray` (`0xFFFFFFFF`).
    pub fn write_null_byte_array(&mut self) {
        self.write_u32(QDATASTREAM_NULL_LEN);
    }

    /// Write an optional `QByteArray`: `None` writes `0xFFFFFFFF`, `Some(b)` writes `len` + `b`.
    pub fn write_opt_byte_array(&mut self, bytes: Option<&[u8]>) {
        match bytes {
            None => self.write_u32(QDATASTREAM_NULL_LEN),
            Some(b) => {
                let len = u32::try_from(b.len()).unwrap_or(u32::MAX - 1);
                self.write_u32(len);
                self.buf.extend_from_slice(&b[..len as usize]);
            }
        }
    }

    /// Write a `QByteArray` following tdesktop's convention: empty slices write `0xFFFFFFFF`
    /// (matching default-constructed `QByteArray()` and `Serialize::WriteBytesWrap`).
    pub fn write_byte_array(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            self.write_u32(QDATASTREAM_NULL_LEN);
        } else {
            self.write_opt_byte_array(Some(bytes));
        }
    }

    /// Write a `QByteArray` where empty writes `0x00000000` (non-null empty `QByteArray("")`).
    pub fn write_non_null_byte_array(&mut self, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX - 1);
        self.write_u32(len);
        self.buf.extend_from_slice(&bytes[..len as usize]);
    }

    /// Write an optional `QString` (UTF-16BE): `None` writes `0xFFFFFFFF`.
    pub fn write_opt_qstring(&mut self, s: Option<&str>) {
        match s {
            None => self.write_u32(QDATASTREAM_NULL_LEN),
            Some(text) => {
                let unit_count = text.encode_utf16().count();
                let byte_len = u32::try_from(unit_count.saturating_mul(2)).unwrap_or(u32::MAX - 1);
                self.write_u32(byte_len);
                for unit in text.encode_utf16() {
                    self.write_u16(unit);
                }
            }
        }
    }

    /// Write a `QString` (UTF-16BE): empty string writes `0xFFFFFFFF` (matching `QString()`).
    pub fn write_qstring(&mut self, s: &str) {
        if s.is_empty() {
            self.write_u32(QDATASTREAM_NULL_LEN);
        } else {
            self.write_opt_qstring(Some(s));
        }
    }

    /// Write a `QDateTime` in `QDataStream::Qt_5_1` format (13 bytes).
    pub fn write_datetime(&mut self, dt: QtDateTime) {
        self.write_i64(dt.julian_day);
        self.write_u32(dt.msecs_since_midnight);
        self.write_i8(dt.time_spec);
    }

    /// Write a `QColor` using `Serialize::writeColor` RGBA bit-packing.
    pub fn write_color(&mut self, color: QtColor) {
        let packed = u32::from(color.r)
            | (u32::from(color.g) << 8)
            | (u32::from(color.b) << 16)
            | (u32::from(color.a) << 24);
        self.write_u32(packed);
    }
}

/// Big-endian `QDataStream::Qt_5_1` / `Qt_5_3` reader (`Serialize::ByteArrayReader`).
#[derive(Debug, Clone)]
pub struct QDataStreamReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> QDataStreamReader<'a> {
    /// Create a new reader positioned at byte 0.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Current cursor position in bytes.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Remaining unread byte count.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Return `true` when all bytes have been consumed (`QDataStream::atEnd`).
    #[must_use]
    pub const fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Borrow remaining unread bytes without advancing the cursor.
    #[must_use]
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    /// Read and consume all remaining bytes from the stream.
    #[must_use]
    pub fn read_rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }

    /// Read exactly `len` raw bytes (`QDataStream::readRawData`).
    pub fn read_raw(&mut self, len: usize) -> Result<&'a [u8], DataStreamError> {
        let rem = self.remaining();
        if len > rem {
            return Err(DataStreamError::UnexpectedEof {
                needed: len,
                remaining: rem,
            });
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    /// Read a fixed-size byte array.
    pub fn read_exact_array<const N: usize>(&mut self) -> Result<[u8; N], DataStreamError> {
        let raw = self.read_raw(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(raw);
        Ok(out)
    }

    /// Read a Qt `bool` (1 byte, non-zero is `true`).
    pub fn read_bool(&mut self) -> Result<bool, DataStreamError> {
        Ok(self.read_u8()? != 0)
    }

    /// Read `qint8`.
    pub fn read_i8(&mut self) -> Result<i8, DataStreamError> {
        Ok(self.read_exact_array::<1>()?[0] as i8)
    }

    /// Read `quint8`.
    pub fn read_u8(&mut self) -> Result<u8, DataStreamError> {
        Ok(self.read_exact_array::<1>()?[0])
    }

    /// Read `qint16` (big-endian).
    pub fn read_i16(&mut self) -> Result<i16, DataStreamError> {
        Ok(i16::from_be_bytes(self.read_exact_array::<2>()?))
    }

    /// Read `quint16` (big-endian).
    pub fn read_u16(&mut self) -> Result<u16, DataStreamError> {
        Ok(u16::from_be_bytes(self.read_exact_array::<2>()?))
    }

    /// Read `qint32` (big-endian).
    pub fn read_i32(&mut self) -> Result<i32, DataStreamError> {
        Ok(i32::from_be_bytes(self.read_exact_array::<4>()?))
    }

    /// Read `quint32` (big-endian).
    pub fn read_u32(&mut self) -> Result<u32, DataStreamError> {
        Ok(u32::from_be_bytes(self.read_exact_array::<4>()?))
    }

    /// Read `qint64` (big-endian).
    pub fn read_i64(&mut self) -> Result<i64, DataStreamError> {
        Ok(i64::from_be_bytes(self.read_exact_array::<8>()?))
    }

    /// Read `quint64` (big-endian).
    pub fn read_u64(&mut self) -> Result<u64, DataStreamError> {
        Ok(u64::from_be_bytes(self.read_exact_array::<8>()?))
    }

    /// Read IEEE 754 `f64` (`qreal`, big-endian).
    pub fn read_f64(&mut self) -> Result<f64, DataStreamError> {
        Ok(f64::from_be_bytes(self.read_exact_array::<8>()?))
    }

    /// Read a `QByteArray`, distinguishing null (`0xFFFFFFFF` -> `None`) from non-null (`Some`).
    pub fn read_opt_byte_array(&mut self) -> Result<Option<Vec<u8>>, DataStreamError> {
        let len = self.read_u32()?;
        if len == QDATASTREAM_NULL_LEN {
            return Ok(None);
        }
        let len_usize = len as usize;
        if len_usize > MAX_ALLOC_BYTES {
            return Err(DataStreamError::InvalidLength(i64::from(len)));
        }
        let raw = self.read_raw(len_usize)?;
        Ok(Some(raw.to_vec()))
    }

    /// Read a `QByteArray`, collapsing null (`0xFFFFFFFF`) to an empty `Vec<u8>`.
    pub fn read_byte_array(&mut self) -> Result<Vec<u8>, DataStreamError> {
        Ok(self.read_opt_byte_array()?.unwrap_or_default())
    }

    /// Read a `QString` (UTF-16BE), distinguishing null (`0xFFFFFFFF` -> `None`) from non-null.
    pub fn read_opt_qstring(&mut self) -> Result<Option<String>, DataStreamError> {
        let byte_len = self.read_u32()?;
        if byte_len == QDATASTREAM_NULL_LEN {
            return Ok(None);
        }
        if !byte_len.is_multiple_of(2) {
            return Err(DataStreamError::OddStringByteLength(byte_len));
        }
        let byte_len_usize = byte_len as usize;
        if byte_len_usize > MAX_ALLOC_BYTES {
            return Err(DataStreamError::InvalidLength(i64::from(byte_len)));
        }
        let raw = self.read_raw(byte_len_usize)?;
        let mut units = Vec::with_capacity(byte_len_usize / 2);
        for &pair in raw.as_chunks::<2>().0 {
            units.push(u16::from_be_bytes(pair));
        }
        let s = String::from_utf16(&units).map_err(|_| DataStreamError::CorruptString)?;
        Ok(Some(s))
    }

    /// Read a `QString` (UTF-16BE), collapsing null (`0xFFFFFFFF`) to an empty `String`.
    pub fn read_qstring(&mut self) -> Result<String, DataStreamError> {
        Ok(self.read_opt_qstring()?.unwrap_or_default())
    }

    /// Read a `QString` (UTF-16BE) with lossy replacement for unpaired surrogates,
    /// collapsing null (`0xFFFFFFFF`) and `0` to an empty `String`.
    pub fn read_qstring_lossy(&mut self) -> Result<String, DataStreamError> {
        let byte_len = self.read_u32()?;
        if byte_len == QDATASTREAM_NULL_LEN || byte_len == 0 {
            return Ok(String::new());
        }
        if !byte_len.is_multiple_of(2) {
            return Err(DataStreamError::OddStringByteLength(byte_len));
        }
        let byte_len_usize = byte_len as usize;
        if byte_len_usize > MAX_ALLOC_BYTES {
            return Err(DataStreamError::InvalidLength(i64::from(byte_len)));
        }
        let raw = self.read_raw(byte_len_usize)?;
        let mut units = Vec::with_capacity(byte_len_usize / 2);
        for &pair in raw.as_chunks::<2>().0 {
            units.push(u16::from_be_bytes(pair));
        }
        Ok(String::from_utf16_lossy(&units))
    }

    /// Read a `QDateTime` in `QDataStream::Qt_5_1` format (13 bytes).
    pub fn read_datetime(&mut self) -> Result<QtDateTime, DataStreamError> {
        let julian_day = self.read_i64()?;
        let msecs_since_midnight = self.read_u32()?;
        let time_spec = self.read_i8()?;
        Ok(QtDateTime {
            julian_day,
            msecs_since_midnight,
            time_spec,
        })
    }

    /// Read a `QColor` using `Serialize::readColor` RGBA unpacking.
    pub fn read_color(&mut self) -> Result<QtColor, DataStreamError> {
        let value = self.read_u32()?;
        Ok(QtColor {
            r: (value & 0xFF) as u8,
            g: ((value >> 8) & 0xFF) as u8,
            b: ((value >> 16) & 0xFF) as u8,
            a: ((value >> 24) & 0xFF) as u8,
        })
    }

    /// Read a `QVector<T>` / `QList<T>` (`quint32` count followed by `count` elements).
    pub fn read_qvector<T, F>(&mut self, mut read_elem: F) -> Result<Vec<T>, DataStreamError>
    where
        F: FnMut(&mut Self) -> Result<T, DataStreamError>,
    {
        let count = self.read_u32()? as usize;
        if count > MAX_COLLECTION_COUNT {
            return Err(DataStreamError::InvalidLength(count as i64));
        }
        let mut out = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            out.push(read_elem(self)?);
        }
        Ok(out)
    }

    /// Read a `QMap<K, V>` (`quint32` count followed by `count` `(K, V)` pairs).
    pub fn read_qmap<K, V, FK, FV>(
        &mut self,
        mut read_k: FK,
        mut read_v: FV,
    ) -> Result<Vec<(K, V)>, DataStreamError>
    where
        FK: FnMut(&mut Self) -> Result<K, DataStreamError>,
        FV: FnMut(&mut Self) -> Result<V, DataStreamError>,
    {
        let count = self.read_u32()? as usize;
        if count > MAX_COLLECTION_COUNT {
            return Err(DataStreamError::InvalidLength(count as i64));
        }
        let mut out = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let k = read_k(self)?;
            let v = read_v(self)?;
            out.push((k, v));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qdatastream_exact_byte_layout() {
        let mut w = QDataStreamWriter::new();
        w.write_i32(0x0102_0304);
        w.write_u64(0x1122_3344_5566_7788);
        w.write_null_byte_array();
        w.write_byte_array(b"abc");
        w.write_qstring("");
        w.write_qstring("Hi");
        w.write_color(QtColor::new(0x10, 0x20, 0x30, 0xFF));

        let bytes = w.into_bytes();
        let expected: Vec<u8> = vec![
            // i32
            0x01, 0x02, 0x03, 0x04, // u64
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, // null QByteArray
            0xFF, 0xFF, 0xFF, 0xFF, // "abc" QByteArray
            0x00, 0x00, 0x00, 0x03, b'a', b'b', b'c', // null QString
            0xFF, 0xFF, 0xFF, 0xFF, // "Hi" QString (4 bytes UTF-16BE)
            0x00, 0x00, 0x00, 0x04, 0x00, b'H', 0x00, b'i',
            // color: r | (g << 8) | (b << 16) | (a << 24) = 0xFF302010 in BE
            0xFF, 0x30, 0x20, 0x10,
        ];
        assert_eq!(bytes, expected);

        let mut r = QDataStreamReader::new(&bytes);
        assert_eq!(r.read_i32().expect("i32"), 0x0102_0304);
        assert_eq!(r.read_u64().expect("u64"), 0x1122_3344_5566_7788);
        assert_eq!(r.read_opt_byte_array().expect("null ba"), None);
        assert_eq!(r.read_byte_array().expect("ba"), b"abc");
        assert_eq!(r.read_opt_qstring().expect("null str"), None);
        assert_eq!(r.read_qstring().expect("str"), "Hi");
        assert_eq!(
            r.read_color().expect("color"),
            QtColor::new(0x10, 0x20, 0x30, 0xFF)
        );
        assert!(r.at_end());
    }

    #[test]
    fn qstring_surrogate_pair_roundtrip() {
        let mut w = QDataStreamWriter::new();
        w.write_qstring("Привет 🚀");
        let bytes = w.into_bytes();
        // 6 Cyrillic + 1 space + 2 UTF-16 code units for rocket = 9 code units = 18 bytes + 4 len
        assert_eq!(bytes.len(), 4 + 18);
        assert_eq!(string_size("Привет 🚀"), 22);

        let mut r = QDataStreamReader::new(&bytes);
        assert_eq!(r.read_qstring().expect("decode"), "Привет 🚀");
    }

    #[test]
    fn qdatetime_roundtrip() {
        let dt = QtDateTime::from_unix_millis_utc(1_700_000_000_123);
        let mut w = QDataStreamWriter::new();
        w.write_datetime(dt);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), datetime_size());

        let mut r = QDataStreamReader::new(&bytes);
        let decoded = r.read_datetime().expect("dt");
        assert_eq!(decoded, dt);
        assert_eq!(decoded.to_unix_millis(), Some(1_700_000_000_123));
    }

    #[test]
    fn bool_rest_and_error_cases() {
        let mut w = QDataStreamWriter::new();
        w.write_bool(true);
        w.write_bool(false);
        w.write_bool_i32(true);
        w.write_bool_i32(false);
        w.write_raw(b"tail");
        let bytes = w.into_bytes();

        let mut r = QDataStreamReader::new(&bytes);
        assert!(r.read_bool().expect("bool true"));
        assert!(!r.read_bool().expect("bool false"));
        assert_eq!(r.read_i32().expect("bool_i32 true"), 1);
        assert_eq!(r.read_i32().expect("bool_i32 false"), 0);
        assert_eq!(r.read_rest(), b"tail");
        assert!(r.at_end());

        let mut r_odd = QDataStreamReader::new(&[0, 0, 0, 3, 0, b'A', 0]);
        assert_eq!(
            r_odd.read_qstring().unwrap_err(),
            DataStreamError::OddStringByteLength(3)
        );

        let mut r_eof = QDataStreamReader::new(&[0, 0, 0, 10, 1, 2]);
        assert_eq!(
            r_eof.read_byte_array().unwrap_err(),
            DataStreamError::UnexpectedEof {
                needed: 10,
                remaining: 2
            }
        );
    }
}
