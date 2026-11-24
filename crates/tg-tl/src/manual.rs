//! Manually handled MTProto framing types (`rpc_result`, `msg_container`, `message`, `gzip_packed`).
//!
//! Port of manual MTProto constructor constants and framing from `mtproto/core_types.h`
//! and the commented-out manual definitions in `schema/mtproto.tl`.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::cursor::Cursor;
use crate::error::DeserializeError;
use crate::traits::{
    Deserializable, Identifiable, RawObject, Serializable, deserialize_bare_vec, serialize_bare_vec,
};

/// Default maximum decompressed size accepted by [`GzipPacked::unpack`] (64 MiB).
pub const DEFAULT_MAX_UNPACK_BYTES: usize = 64 * 1024 * 1024;

/// MTProto `rpc_result#f35c6d01 req_msg_id:long result:Object = RpcResult;`.
///
/// Bare representation (use [`crate::Boxed<RpcResult>`] to include/verify `0xf35c6d01`).
/// `result` holds the remaining raw bytes so `tg-mtproto` can check for `rpc_error`,
/// `gzip_packed`, or deserialize the expected `RemoteCall::Return`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcResult {
    /// Message ID of the original RPC request.
    pub req_msg_id: i64,
    /// Raw TL-encoded response object (remaining bytes of the `rpc_result` payload).
    pub result: RawObject,
}

impl Identifiable for RpcResult {
    const CONSTRUCTOR_ID: u32 = 0xf35c_6d01;
}

impl RpcResult {
    /// Canonical TL constructor name.
    pub const NAME: &'static str = "rpc_result";
    /// Returns the canonical TL constructor name.
    pub const fn name(&self) -> &'static str {
        Self::NAME
    }
}

impl Serializable for RpcResult {
    fn serialize(&self, buf: &mut Vec<u8>) {
        self.req_msg_id.serialize(buf);
        self.result.serialize(buf);
    }
}

impl Deserializable for RpcResult {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let req_msg_id = i64::deserialize(buf)?;
        let result = RawObject::deserialize(buf)?;
        Ok(Self { req_msg_id, result })
    }
}

/// MTProto bare core message: `message msg_id:long seqno:int bytes:int body:Object = Message;`.
///
/// Unlike bare [`RawObject::deserialize`], `CoreMessage` reads the explicit `bytes:int` length
/// field and slices exactly `bytes` bytes into `body`, allowing multiple messages to be parsed
/// sequentially inside [`MsgContainer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreMessage {
    /// 64-bit MTProto message ID.
    pub msg_id: i64,
    /// 32-bit sequence number.
    pub seqno: i32,
    /// Raw TL-encoded message body (exact length encoded in `bytes:int`).
    pub body: RawObject,
}

impl CoreMessage {
    /// Canonical TL constructor name.
    pub const NAME: &'static str = "message";
    /// Returns the canonical TL constructor name.
    pub const fn name(&self) -> &'static str {
        Self::NAME
    }
}

impl Serializable for CoreMessage {
    fn serialize(&self, buf: &mut Vec<u8>) {
        self.msg_id.serialize(buf);
        self.seqno.serialize(buf);
        let len = i32::try_from(self.body.0.len()).expect("CoreMessage body exceeds i32::MAX");
        len.serialize(buf);
        self.body.serialize(buf);
    }
}

impl Deserializable for CoreMessage {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let msg_id = i64::deserialize(buf)?;
        let seqno = i32::deserialize(buf)?;
        let bytes = i32::deserialize(buf)?;
        if bytes < 0 {
            return Err(DeserializeError::NegativeLength(bytes));
        }
        let body = RawObject(buf.read_slice(bytes as usize)?.to_vec());
        Ok(Self {
            msg_id,
            seqno,
            body,
        })
    }
}

/// MTProto `msg_container#73f1f8dc messages:vector<message> = MessageContainer;`.
///
/// Bare representation (use [`crate::Boxed<MsgContainer>`] to include/verify `0x73f1f8dc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgContainer {
    /// Bare vector of framed [`CoreMessage`] entries.
    pub messages: Vec<CoreMessage>,
}

impl Identifiable for MsgContainer {
    const CONSTRUCTOR_ID: u32 = 0x73f1_f8dc;
}

impl MsgContainer {
    /// Canonical TL constructor name.
    pub const NAME: &'static str = "msg_container";
    /// Returns the canonical TL constructor name.
    pub const fn name(&self) -> &'static str {
        Self::NAME
    }
}

impl Serializable for MsgContainer {
    fn serialize(&self, buf: &mut Vec<u8>) {
        serialize_bare_vec(&self.messages, buf);
    }
}

impl Deserializable for MsgContainer {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let messages = deserialize_bare_vec(buf)?;
        Ok(Self { messages })
    }
}

/// MTProto `gzip_packed#3072cfa1 packed_data:string = Object;`.
///
/// Bare representation (use [`crate::Boxed<GzipPacked>`] to include/verify `0x3072cfa1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GzipPacked {
    /// Gzip-compressed inner TL bytes.
    pub packed_data: Vec<u8>,
}

impl Identifiable for GzipPacked {
    const CONSTRUCTOR_ID: u32 = 0x3072_cfa1;
}

impl GzipPacked {
    /// Canonical TL constructor name.
    pub const NAME: &'static str = "gzip_packed";
    /// Returns the canonical TL constructor name.
    pub const fn name(&self) -> &'static str {
        Self::NAME
    }

    /// Compresses `uncompressed` bytes with gzip and wraps them in [`GzipPacked`].
    pub fn pack(uncompressed: &[u8]) -> Result<Self, DeserializeError> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(uncompressed)
            .map_err(|e| DeserializeError::GzipError(e.to_string()))?;
        let packed_data = encoder
            .finish()
            .map_err(|e| DeserializeError::GzipError(e.to_string()))?;
        Ok(Self { packed_data })
    }

    /// Decompresses `packed_data` using [`DEFAULT_MAX_UNPACK_BYTES`] as the output size cap.
    pub fn unpack(&self) -> Result<Vec<u8>, DeserializeError> {
        self.unpack_with_limit(DEFAULT_MAX_UNPACK_BYTES)
    }

    /// Decompresses `packed_data`, failing with [`DeserializeError::GzipError`] if the
    /// decompressed stream exceeds `max_bytes`.
    pub fn unpack_with_limit(&self, max_bytes: usize) -> Result<Vec<u8>, DeserializeError> {
        let decoder = GzDecoder::new(self.packed_data.as_slice());
        let mut limited = decoder.take((max_bytes as u64).saturating_add(1));
        let mut out = Vec::new();
        limited
            .read_to_end(&mut out)
            .map_err(|e| DeserializeError::GzipError(e.to_string()))?;
        if out.len() > max_bytes {
            return Err(DeserializeError::GzipError(format!(
                "decompressed payload exceeded limit of {max_bytes} bytes"
            )));
        }
        Ok(out)
    }
}

impl Serializable for GzipPacked {
    fn serialize(&self, buf: &mut Vec<u8>) {
        self.packed_data.serialize(buf);
    }
}

impl Deserializable for GzipPacked {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let packed_data = Vec::<u8>::deserialize(buf)?;
        Ok(Self { packed_data })
    }
}
