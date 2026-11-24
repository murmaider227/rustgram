//! Core TL serialization traits, primitive implementations, and wrapper types.
//!
//! Port of `lib_tl/tl/tl_basic_types.h` and `lib_tl/tl/tl_boxed.h`.

use crate::cursor::Cursor;
use crate::error::DeserializeError;

/// Constructor ID for TL `boolFalse`.
pub const BOOL_FALSE_ID: u32 = 0xbc79_9737;
/// Constructor ID for TL `boolTrue`.
pub const BOOL_TRUE_ID: u32 = 0x9972_75b5;
/// Constructor ID for boxed TL `vector#1cb5c415`.
pub const VECTOR_ID: u32 = 0x1cb5_c415;

/// Trait implemented by all TL types and RPC functions that can be serialized to bytes.
pub trait Serializable {
    /// Appends this value's TL wire representation to `buf`.
    fn serialize(&self, buf: &mut Vec<u8>);

    /// Serializes this value into a freshly allocated `Vec<u8>`.
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize(&mut buf);
        buf
    }
}

/// Trait implemented by all TL types and RPC functions that can be deserialized from bytes.
pub trait Deserializable: Sized {
    /// Reads this value from `buf`, advancing the cursor past its wire representation.
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError>;

    /// Deserializes a value from a byte slice starting at offset 0.
    fn from_bytes(b: &[u8]) -> Result<Self, DeserializeError> {
        let mut cursor = Cursor::new(b);
        Self::deserialize(&mut cursor)
    }
}

/// Trait providing the 32-bit TL constructor ID for bare types and RPC functions.
pub trait Identifiable {
    /// 32-bit TL constructor/function ID (CRC32 of normalized TL definition).
    const CONSTRUCTOR_ID: u32;
}

/// Trait implemented by all TL RPC functions, associating the request with its expected return type.
pub trait RemoteCall: Serializable {
    /// Expected boxed response type returned by the Telegram server.
    type Return: Deserializable;
}

/// Writes raw bytes using TL string/bytes length prefixing and 4-byte zero padding.
///
/// Matches `tl::string_type::write` in `lib_tl/tl/tl_basic_types.h`.
pub fn write_tl_bytes(bytes: &[u8], buf: &mut Vec<u8>) {
    let len = bytes.len();
    assert!(
        len <= 0x00FF_FFFF,
        "TL string/bytes length {len} exceeds 24-bit maximum (16777215)"
    );
    if len < 254 {
        buf.push(len as u8);
        buf.extend_from_slice(bytes);
        let pad = (4 - ((1 + len) % 4)) % 4;
        buf.extend_from_slice(&[0u8; 3][..pad]);
    } else {
        buf.push(0xfe);
        buf.push((len & 0xff) as u8);
        buf.push(((len >> 8) & 0xff) as u8);
        buf.push(((len >> 16) & 0xff) as u8);
        buf.extend_from_slice(bytes);
        let pad = (4 - (len % 4)) % 4;
        buf.extend_from_slice(&[0u8; 3][..pad]);
    }
}

/// Serializes a slice as a **bare** TL `vector<T>` (4-byte count followed by elements, no `0x1cb5c415` ID).
pub fn serialize_bare_vec<T: Serializable>(items: &[T], buf: &mut Vec<u8>) {
    let len = i32::try_from(items.len()).expect("TL vector length exceeds i32::MAX");
    buf.extend_from_slice(&len.to_le_bytes());
    for item in items {
        item.serialize(buf);
    }
}

/// Deserializes a **bare** TL `vector<T>` (4-byte count followed by elements, no `0x1cb5c415` ID).
pub fn deserialize_bare_vec<T: Deserializable>(
    buf: &mut Cursor<'_>,
) -> Result<Vec<T>, DeserializeError> {
    let count = buf.read_i32()?;
    if count < 0 {
        return Err(DeserializeError::NegativeLength(count));
    }
    let count_usize = count as usize;
    // Cap initial allocation to defend against truncated/malicious large lengths.
    let prealloc = count_usize.min(buf.remaining() / 4).min(4096);
    let mut out = Vec::with_capacity(prealloc);
    for _ in 0..count_usize {
        out.push(T::deserialize(buf)?);
    }
    Ok(out)
}

// --- Primitive implementations ---

impl Serializable for i32 {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.to_le_bytes());
    }
}

impl Deserializable for i32 {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        buf.read_i32()
    }
}

impl Serializable for i64 {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.to_le_bytes());
    }
}

impl Deserializable for i64 {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        buf.read_i64()
    }
}

impl Serializable for f64 {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.to_le_bytes());
    }
}

impl Deserializable for f64 {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        buf.read_f64()
    }
}

impl Serializable for [u8; 16] {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(self);
    }
}

impl Deserializable for [u8; 16] {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        buf.read_exact::<16>()
    }
}

impl Serializable for [u8; 32] {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(self);
    }
}

impl Deserializable for [u8; 32] {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        buf.read_exact::<32>()
    }
}

/// Boxed TL `Bool` maps directly to Rust `bool` (`boolTrue#997275b5` / `boolFalse#bc799737`).
impl Serializable for bool {
    fn serialize(&self, buf: &mut Vec<u8>) {
        let id = if *self { BOOL_TRUE_ID } else { BOOL_FALSE_ID };
        buf.extend_from_slice(&id.to_le_bytes());
    }
}

impl Deserializable for bool {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        match buf.read_u32()? {
            BOOL_TRUE_ID => Ok(true),
            BOOL_FALSE_ID => Ok(false),
            id => Err(DeserializeError::UnexpectedConstructor { id }),
        }
    }
}

impl Serializable for String {
    fn serialize(&self, buf: &mut Vec<u8>) {
        write_tl_bytes(self.as_bytes(), buf);
    }
}

impl Deserializable for String {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        buf.read_tl_string()
    }
}

/// TL `bytes` (`Vec<u8>`) uses TL length-prefixed + 4-byte-padded byte-string encoding.
impl Serializable for Vec<u8> {
    fn serialize(&self, buf: &mut Vec<u8>) {
        write_tl_bytes(self, buf);
    }
}

impl Deserializable for Vec<u8> {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        buf.read_tl_bytes()
    }
}

/// Boxed TL `Vector<T>` (`vector#1cb5c415` followed by count and elements).
///
/// Note: `u8` does not implement `Serializable`/`Deserializable`, so `Vec<u8>` uses the
/// `bytes` impl above while `Vec<Vec<u8>>` (`Vector<bytes>`) uses this boxed vector impl.
impl<T: Serializable> Serializable for Vec<T> {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&VECTOR_ID.to_le_bytes());
        serialize_bare_vec(self, buf);
    }
}

impl<T: Deserializable> Deserializable for Vec<T> {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let id = buf.read_u32()?;
        if id != VECTOR_ID {
            return Err(DeserializeError::UnexpectedConstructor { id });
        }
        deserialize_bare_vec(buf)
    }
}

impl<T: Serializable> Serializable for Box<T> {
    fn serialize(&self, buf: &mut Vec<u8>) {
        (**self).serialize(buf);
    }
}

impl<T: Deserializable> Deserializable for Box<T> {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        Ok(Box::new(T::deserialize(buf)?))
    }
}

// --- Wrapper types ---

/// Raw untyped TL `Object` payload represented as raw wire bytes.
///
/// Used for TL `Object` parameters (such as `rpc_result.result` and `CoreMessage.body`)
/// so `tg-mtproto` can inspect constructor IDs, decompress `gzip_packed`, or deserialize
/// responses with the caller's known `RemoteCall::Return` type.
///
/// - [`Serializable::serialize`] appends `self.0` verbatim (no length prefix).
/// - [`Deserializable::deserialize`] consumes all remaining bytes from the [`Cursor`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawObject(pub Vec<u8>);

impl RawObject {
    /// Peeks the leading 32-bit constructor ID of the raw TL object, if at least 4 bytes exist.
    pub fn constructor_id(&self) -> Option<u32> {
        if self.0.len() < 4 {
            return None;
        }
        Some(u32::from_le_bytes([
            self.0[0], self.0[1], self.0[2], self.0[3],
        ]))
    }

    /// Attempts to deserialize this raw object as a concrete TL type `T`.
    pub fn deserialize_as<T: Deserializable>(&self) -> Result<T, DeserializeError> {
        T::from_bytes(&self.0)
    }
}

impl Serializable for RawObject {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.0);
    }
}

impl Deserializable for RawObject {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        Ok(Self(buf.read_remaining().to_vec()))
    }
}

/// Wrapper that serializes and deserializes any bare [`Identifiable`] type in **boxed** form
/// (prefixed by `T::CONSTRUCTOR_ID`).
///
/// Port of `tl::boxed<T>` from `lib_tl/tl/tl_boxed.h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boxed<T>(pub T);

impl<T: Identifiable + Serializable> Serializable for Boxed<T> {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&T::CONSTRUCTOR_ID.to_le_bytes());
        self.0.serialize(buf);
    }
}

impl<T: Identifiable + Deserializable> Deserializable for Boxed<T> {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let id = buf.read_u32()?;
        if id != T::CONSTRUCTOR_ID {
            return Err(DeserializeError::UnexpectedConstructor { id });
        }
        Ok(Self(T::deserialize(buf)?))
    }
}

/// Wrapper that serializes and deserializes a `Vec<T>` as a **bare** TL `vector<T>`
/// (omitting the `0x1cb5c415` constructor ID).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BareVec<T>(pub Vec<T>);

impl<T: Serializable> Serializable for BareVec<T> {
    fn serialize(&self, buf: &mut Vec<u8>) {
        serialize_bare_vec(&self.0, buf);
    }
}

impl<T: Deserializable> Deserializable for BareVec<T> {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        Ok(Self(deserialize_bare_vec(buf)?))
    }
}
