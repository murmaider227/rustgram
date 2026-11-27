//! MTProto WebView Proxy framing, local HTTP/WebSocket bridge, WebView carrier protocol,
//! and multiplexed stream session state machine.
//!
//! Port of:
//! - `Telegram/SourceFiles/mtproto/web_proxy/web_proxy_frame.{h,cpp}`
//! - `Telegram/SourceFiles/mtproto/web_proxy/web_proxy_transport.{h,cpp}`
//! - `Telegram/SourceFiles/mtproto/web_proxy/web_proxy_webview.{h,cpp}`

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use rand::RngCore as _;
use tg_transport::{
    Connection, ConnectionConfig, Error as TransportError, ProxyData, ProxyStatus, ProxyType,
    TransportKind, compute_web_proxy_bridge_capability,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::transport_adapter::{BoxFuture, PacketConnection, TgTcpConnection, TransportFactory};

/// Fixed 8-byte binary frame header size (`type:1 | stream_id:3 | length:4`).
pub const FRAME_HEADER_SIZE: usize = 8;

/// Maximum binary payload size in a single relay frame (1 MiB).
pub const MAX_FRAME_PAYLOAD: usize = 1024 * 1024;

/// Maximum number of frames accepted in a single parse batch.
pub const MAX_BATCH_FRAMES: usize = 4096;

/// Maximum 24-bit stream identifier (`0x00FF_FFFF`).
pub const MAX_STREAM_ID: u32 = 0x00FF_FFFF;

/// Initial per-stream flow-control window (4 MiB).
pub const INITIAL_STREAM_WINDOW: u32 = 4 * 1024 * 1024;

/// Chunk size used when slicing outbound stream data into `Data` frames (64 KiB).
pub const DATA_FRAME_SIZE: usize = 64 * 1024;

/// Receive-window accumulation threshold before flushing a `Window` grant (256 KiB).
pub const WINDOW_FLUSH_BYTES: u32 = 256 * 1024;

/// Maximum HTTP header block size accepted on the local loopback bridge server (16 KiB).
pub const MAX_HTTP_HEADER_SIZE: usize = 16 * 1024;

/// Maximum reassembled local WebSocket frame/message size (2 MiB).
pub const MAX_LOCAL_WEBSOCKET_PAYLOAD: usize = 2 * 1024 * 1024;

/// Maximum encoded script message size exchanged with a hidden WebView carrier (2 MiB).
pub const MAX_WEBVIEW_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

/// Maximum closed stream IDs remembered for tolerating late in-flight frames.
pub const MAX_CLOSED_STREAM_IDS: usize = 4096;

/// RFC 6455 WebSocket handshake GUID.
pub const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Relay binary frame opcodes (`MTP::WebProxy::FrameType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameType {
    /// Open a new multiplexed stream.
    Open = 0x01,
    /// Stream payload data.
    Data = 0x02,
    /// Gracefully close a stream.
    Close = 0x03,
    /// Flow-control window increment (4-byte big-endian u32 payload).
    Window = 0x04,
    /// Heartbeat ping on stream 0 (payload up to 64 bytes).
    Ping = 0x05,
    /// Heartbeat pong on stream 0 echoing the ping payload.
    Pong = 0x06,
    /// Client protocol hello on stream 0 (`[0x01]`).
    Hello = 0x10,
    /// Relay welcome acknowledgement on stream 0 (empty payload).
    Welcome = 0x11,
    /// Optional auth challenge on stream 0.
    AuthChallenge = 0x12,
    /// Optional auth response on stream 0.
    AuthResponse = 0x13,
    /// Relay goodbye / session teardown on stream 0.
    Bye = 0x1F,
}

impl TryFrom<u8> for FrameType {
    type Error = WebProxyError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::Open),
            0x02 => Ok(Self::Data),
            0x03 => Ok(Self::Close),
            0x04 => Ok(Self::Window),
            0x05 => Ok(Self::Ping),
            0x06 => Ok(Self::Pong),
            0x10 => Ok(Self::Hello),
            0x11 => Ok(Self::Welcome),
            0x12 => Ok(Self::AuthChallenge),
            0x13 => Ok(Self::AuthResponse),
            0x1F => Ok(Self::Bye),
            other => Err(WebProxyError::UnknownFrameType(other)),
        }
    }
}

/// Errors produced by WebView proxy framing, HTTP/WS handshake, or stream state validation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebProxyError {
    /// Stream ID exceeded 24-bit range (`0..=0x00FF_FFFF`).
    #[error("stream id {0:#x} exceeds 24-bit limit")]
    StreamIdOutOfRange(u32),
    /// Frame payload exceeded [`MAX_FRAME_PAYLOAD`].
    #[error("frame payload length {0} exceeds maximum {MAX_FRAME_PAYLOAD}")]
    PayloadTooLarge(usize),
    /// Unrecognized binary frame opcode byte.
    #[error("unknown web proxy frame type {0:#04x}")]
    UnknownFrameType(u8),
    /// A single parse call produced more than [`MAX_BATCH_FRAMES`] frames.
    #[error("batch frame limit ({MAX_BATCH_FRAMES}) exceeded")]
    BatchLimitExceeded,
    /// `Window` frame payload was not 4 bytes or carried a zero increment.
    #[error("invalid window increment payload")]
    InvalidWindowPayload,
    /// Relay or bridge protocol violation.
    #[error("web proxy protocol error: {0}")]
    Protocol(&'static str),
    /// Local loopback HTTP request failed validation.
    #[error("http error {status}: {reason}")]
    Http {
        /// HTTP status code (e.g. 400, 403, 404, 431).
        status: u16,
        /// Short diagnostic message.
        reason: &'static str,
    },
    /// Local loopback WebSocket framing violation.
    #[error("websocket error (close code {code}): {reason}")]
    WebSocket {
        /// RFC 6455 close status code (e.g. 1002, 1008, 1009).
        code: u16,
        /// Short diagnostic message.
        reason: &'static str,
    },
    /// Invalid Web Proxy configuration (host, port, path, or secret).
    #[error("invalid web proxy configuration: {0}")]
    InvalidProxy(String),
    /// Network or loopback I/O failure.
    #[error("web proxy I/O error: {0}")]
    Io(String),
    /// Timed out waiting for Web Proxy handshake or response.
    #[error("web proxy request timed out")]
    Timeout,
}

/// A decoded WebView proxy binary frame (`MTP::WebProxy::Frame`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Frame opcode.
    pub frame_type: FrameType,
    /// 24-bit multiplexed stream ID (`0` for transport-level control frames).
    pub stream_id: u32,
    /// Frame payload bytes.
    pub payload: Vec<u8>,
}

impl Frame {
    /// Constructs a new frame.
    #[must_use]
    pub fn new(frame_type: FrameType, stream_id: u32, payload: Vec<u8>) -> Self {
        Self {
            frame_type,
            stream_id,
            payload,
        }
    }

    /// Serializes this frame into wire bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, WebProxyError> {
        serialize_frame(self.frame_type, self.stream_id, &self.payload)
    }
}

/// Serializes a WebView proxy frame into its 8-byte big-endian header + payload.
///
/// Wire format (`web_proxy_frame.cpp:33-55`):
/// - `byte[0]`: `FrameType as u8`
/// - `byte[1..4]`: 24-bit big-endian `stream_id`
/// - `byte[4..8]`: 32-bit big-endian `payload.len()`
/// - `byte[8..]`: `payload`
pub fn serialize_frame(
    frame_type: FrameType,
    stream_id: u32,
    payload: &[u8],
) -> Result<Vec<u8>, WebProxyError> {
    if stream_id > MAX_STREAM_ID {
        return Err(WebProxyError::StreamIdOutOfRange(stream_id));
    }
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(WebProxyError::PayloadTooLarge(payload.len()));
    }
    let size = payload.len() as u32;
    let mut out = Vec::with_capacity(FRAME_HEADER_SIZE + payload.len());
    out.push(frame_type as u8);
    out.push(((stream_id >> 16) & 0xFF) as u8);
    out.push(((stream_id >> 8) & 0xFF) as u8);
    out.push((stream_id & 0xFF) as u8);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Parses as many complete frames as available from `buffer`, removing consumed bytes.
///
/// Any trailing incomplete frame remains untouched in `buffer` so callers can stream chunks
/// incrementally (`web_proxy_frame.cpp:57-92`).
pub fn parse_frames(buffer: &mut Vec<u8>) -> Result<Vec<Frame>, WebProxyError> {
    let mut output = Vec::new();
    let mut offset = 0usize;

    while buffer.len().saturating_sub(offset) >= FRAME_HEADER_SIZE {
        let header = &buffer[offset..offset + FRAME_HEADER_SIZE];
        let frame_type = FrameType::try_from(header[0])?;
        let stream_id =
            (u32::from(header[1]) << 16) | (u32::from(header[2]) << 8) | u32::from(header[3]);
        let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if size > MAX_FRAME_PAYLOAD {
            return Err(WebProxyError::PayloadTooLarge(size));
        }
        let full = FRAME_HEADER_SIZE + size;
        if buffer.len().saturating_sub(offset) < full {
            break;
        }
        if output.len() >= MAX_BATCH_FRAMES {
            return Err(WebProxyError::BatchLimitExceeded);
        }
        let payload = buffer[offset + FRAME_HEADER_SIZE..offset + full].to_vec();
        output.push(Frame {
            frame_type,
            stream_id,
            payload,
        });
        offset += full;
    }

    if offset > 0 {
        buffer.drain(..offset);
    }
    Ok(output)
}

/// Encodes a 4-byte big-endian flow-control window increment payload.
#[must_use]
pub const fn encode_window_payload(amount: u32) -> [u8; 4] {
    amount.to_be_bytes()
}

/// Decodes a 4-byte big-endian flow-control window increment payload (must be non-zero).
pub fn decode_window_payload(payload: &[u8]) -> Result<u32, WebProxyError> {
    if payload.len() != 4 {
        return Err(WebProxyError::InvalidWindowPayload);
    }
    let amount = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    if amount == 0 {
        return Err(WebProxyError::InvalidWindowPayload);
    }
    Ok(amount)
}

/// Computes the RFC 6455 `Sec-WebSocket-Accept` header value for a client `Sec-WebSocket-Key`.
#[must_use]
pub fn compute_websocket_accept(key: &str) -> String {
    let digest = tg_crypto::sha1(&[key.as_bytes(), WEBSOCKET_GUID.as_bytes()]);
    BASE64_STANDARD.encode(digest)
}

/// Action to take after validating a loopback HTTP request (`web_proxy_transport.cpp:546-637`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpRequestAction {
    /// Request header block is incomplete (`\r\n\r\n` not yet received).
    Incomplete,
    /// Serve the local browser bridge page at `/`.
    ServeBridgePage,
    /// Upgrade `/transport` to a WebSocket using the computed `Sec-WebSocket-Accept` response.
    UpgradeWebSocket {
        /// Complete `HTTP/1.1 101 Switching Protocols` response bytes.
        response: Vec<u8>,
        /// Number of header bytes consumed from the input buffer.
        consumed: usize,
    },
}

fn header_has_token(value: &str, token: &str) -> bool {
    value
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case(token))
}

/// Validates a loopback HTTP request directed at the local WebView proxy bridge server.
///
/// Mirrors `Transport::Private::processHttp` (`web_proxy_transport.cpp:546-637`), enforcing:
/// - Maximum header size (`16 KiB`) -> HTTP 431
/// - `GET <path> HTTP/1.1`, no duplicate headers, no `Content-Length` / `Transfer-Encoding` -> HTTP 400
/// - Exact `Host: 127.0.0.1:<port>` match -> HTTP 403
/// - `GET /` serves the bridge page; `GET /transport` requires RFC 6455 WebSocket upgrade headers,
///   matching `Origin: http://127.0.0.1:<port>`, and a 16-byte base64 `Sec-WebSocket-Key`.
pub fn parse_local_http_request(
    input: &[u8],
    server_port: u16,
) -> Result<HttpRequestAction, WebProxyError> {
    if input.len() > MAX_HTTP_HEADER_SIZE {
        return Err(WebProxyError::Http {
            status: 431,
            reason: "Request Header Fields Too Large",
        });
    }
    let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(HttpRequestAction::Incomplete);
    };
    let consumed = end + 4;
    let header_str = std::str::from_utf8(&input[..end]).map_err(|_| WebProxyError::Http {
        status: 400,
        reason: "Bad Request",
    })?;
    let mut lines = header_str.split('\n');
    let Some(first_line) = lines.next() else {
        return Err(WebProxyError::Http {
            status: 400,
            reason: "Bad Request",
        });
    };
    let request_parts: Vec<&str> = first_line.trim().split(' ').collect();
    if request_parts.len() != 3 || request_parts[0] != "GET" || request_parts[2] != "HTTP/1.1" {
        return Err(WebProxyError::Http {
            status: 400,
            reason: "Bad Request",
        });
    }

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for raw_line in lines {
        let line = raw_line.trim();
        let Some((name_raw, val_raw)) = line.split_once(':') else {
            return Err(WebProxyError::Http {
                status: 400,
                reason: "Bad Request",
            });
        };
        let name = name_raw.trim().to_ascii_lowercase();
        if name.is_empty() || headers.contains_key(&name) {
            return Err(WebProxyError::Http {
                status: 400,
                reason: "Bad Request",
            });
        }
        headers.insert(name, val_raw.trim().to_owned());
    }

    if headers.contains_key("content-length") || headers.contains_key("transfer-encoding") {
        return Err(WebProxyError::Http {
            status: 400,
            reason: "Bad Request",
        });
    }

    let expected_host = format!("127.0.0.1:{server_port}");
    if headers.get("host").map(String::as_str) != Some(expected_host.as_str()) {
        return Err(WebProxyError::Http {
            status: 403,
            reason: "Forbidden",
        });
    }

    let path = request_parts[1];
    if path == "/" {
        return Ok(HttpRequestAction::ServeBridgePage);
    }

    let upgrade_ok = headers
        .get("upgrade")
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let connection_ok = headers
        .get("connection")
        .is_some_and(|v| header_has_token(v, "upgrade"));
    let version_ok = headers
        .get("sec-websocket-version")
        .is_some_and(|v| v == "13");

    if path != "/transport" || !upgrade_ok || !connection_ok || !version_ok {
        return Err(WebProxyError::Http {
            status: 404,
            reason: "Not Found",
        });
    }

    let expected_origin = format!("http://{expected_host}");
    let key = headers
        .get("sec-websocket-key")
        .map(String::as_str)
        .unwrap_or_default();
    let decoded_key_len = BASE64_STANDARD.decode(key).map(|b| b.len()).unwrap_or(0);
    if headers.get("origin").map(String::as_str) != Some(expected_origin.as_str())
        || decoded_key_len != 16
    {
        return Err(WebProxyError::Http {
            status: 403,
            reason: "Forbidden",
        });
    }

    let accept = compute_websocket_accept(key);
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )
    .into_bytes();

    Ok(HttpRequestAction::UpgradeWebSocket { response, consumed })
}

/// Encodes an unmasked server-to-client RFC 6455 WebSocket frame (`WebSocketFrame` in
/// `web_proxy_transport.cpp:108-127`).
#[must_use]
pub fn encode_websocket_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | (opcode & 0x0F));
    let len = payload.len() as u64;
    if len < 126 {
        out.push(len as u8);
    } else if len <= 0xFFFF {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&len.to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

/// Decoded client-to-server WebSocket message (`web_proxy_transport.cpp:639-758`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketMessage {
    /// Text message (opcode `0x01`, used for JSON control messages).
    Text(Vec<u8>),
    /// Binary message (opcode `0x02`, carrying relay binary frames).
    Binary(Vec<u8>),
    /// Ping control frame (opcode `0x09`).
    Ping(Vec<u8>),
    /// Pong control frame (opcode `0x0A`).
    Pong(Vec<u8>),
    /// Close control frame (opcode `0x08`).
    Close(Vec<u8>),
}

/// Incremental masked client-to-server RFC 6455 WebSocket frame decoder.
#[derive(Debug, Default)]
pub struct WebSocketDecoder {
    fragmented_opcode: u8,
    fragmented: Vec<u8>,
}

impl WebSocketDecoder {
    /// Decodes all complete client WebSocket frames from `input`, draining consumed bytes.
    ///
    /// Frames are read at an offset and the consumed prefix is drained once (also when an
    /// error stops decoding), so a read holding many small frames moves each byte once.
    pub fn decode(&mut self, input: &mut Vec<u8>) -> Result<Vec<WebSocketMessage>, WebProxyError> {
        let mut messages = Vec::new();
        let mut consumed = 0usize;
        let result = self.decode_from(input, &mut consumed, &mut messages);
        if consumed > 0 {
            input.drain(..consumed);
        }
        result.map(|()| messages)
    }

    /// Decodes frames from `input[*consumed..]`, adding each complete frame's length to
    /// `consumed`.
    fn decode_from(
        &mut self,
        input: &[u8],
        consumed: &mut usize,
        messages: &mut Vec<WebSocketMessage>,
    ) -> Result<(), WebProxyError> {
        loop {
            let rest = &input[*consumed..];
            if rest.len() < 2 {
                break;
            }
            let b0 = rest[0];
            let b1 = rest[1];
            let fin = (b0 & 0x80) != 0;
            let rsv = b0 & 0x70;
            let opcode = b0 & 0x0F;
            let masked = (b1 & 0x80) != 0;
            if rsv != 0 || !masked {
                return Err(WebProxyError::WebSocket {
                    code: 1002,
                    reason: "unmasked or reserved bits set",
                });
            }

            let mut size = u64::from(b1 & 0x7F);
            let mut header = 2usize;
            if size == 126 {
                if rest.len() < 4 {
                    break;
                }
                size = u64::from(u16::from_be_bytes([rest[2], rest[3]]));
                if size < 126 {
                    return Err(WebProxyError::WebSocket {
                        code: 1002,
                        reason: "non-canonical 16-bit length",
                    });
                }
                header = 4;
            } else if size == 127 {
                if rest.len() < 10 {
                    break;
                }
                if (rest[2] & 0x80) != 0 {
                    return Err(WebProxyError::WebSocket {
                        code: 1002,
                        reason: "64-bit length MSB set",
                    });
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&rest[2..10]);
                size = u64::from_be_bytes(bytes);
                if size <= 0xFFFF {
                    return Err(WebProxyError::WebSocket {
                        code: 1002,
                        reason: "non-canonical 64-bit length",
                    });
                }
                header = 10;
            }

            let is_control = (opcode & 0x08) != 0;
            if size > MAX_LOCAL_WEBSOCKET_PAYLOAD as u64 || (is_control && (!fin || size > 125)) {
                return Err(WebProxyError::WebSocket {
                    code: 1009,
                    reason: "frame too large or fragmented control frame",
                });
            }

            let full = header + 4 + size as usize;
            if rest.len() < full {
                break;
            }

            let mask = [
                rest[header],
                rest[header + 1],
                rest[header + 2],
                rest[header + 3],
            ];
            let mut payload = rest[header + 4..full].to_vec();
            for (idx, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[idx % 4];
            }
            *consumed += full;

            match opcode {
                0x08 => {
                    if payload.len() == 1 {
                        return Err(WebProxyError::WebSocket {
                            code: 1002,
                            reason: "1-byte close payload",
                        });
                    }
                    messages.push(WebSocketMessage::Close(payload));
                    break;
                }
                0x09 => messages.push(WebSocketMessage::Ping(payload)),
                0x0A => messages.push(WebSocketMessage::Pong(payload)),
                0x00 => {
                    if self.fragmented_opcode == 0 {
                        return Err(WebProxyError::WebSocket {
                            code: 1002,
                            reason: "unexpected continuation frame",
                        });
                    }
                    self.fragmented.extend_from_slice(&payload);
                    if self.fragmented.len() > MAX_LOCAL_WEBSOCKET_PAYLOAD {
                        return Err(WebProxyError::WebSocket {
                            code: 1009,
                            reason: "fragmented message too large",
                        });
                    }
                    if fin {
                        let op = std::mem::take(&mut self.fragmented_opcode);
                        let assembled = std::mem::take(&mut self.fragmented);
                        messages.push(if op == 0x01 {
                            WebSocketMessage::Text(assembled)
                        } else {
                            WebSocketMessage::Binary(assembled)
                        });
                    }
                }
                0x01 | 0x02 => {
                    if self.fragmented_opcode != 0 {
                        return Err(WebProxyError::WebSocket {
                            code: 1002,
                            reason: "interleaved data frame during fragmentation",
                        });
                    }
                    if fin {
                        messages.push(if opcode == 0x01 {
                            WebSocketMessage::Text(payload)
                        } else {
                            WebSocketMessage::Binary(payload)
                        });
                    } else {
                        self.fragmented_opcode = opcode;
                        self.fragmented = payload;
                    }
                }
                _ => {
                    return Err(WebProxyError::WebSocket {
                        code: 1002,
                        reason: "unsupported opcode",
                    });
                }
            }
        }
        Ok(())
    }
}

/// Convenience helper decoding masked client WebSocket frames in a single shot.
pub fn decode_websocket_client_frames(
    input: &mut Vec<u8>,
) -> Result<Vec<WebSocketMessage>, WebProxyError> {
    let mut decoder = WebSocketDecoder::default();
    decoder.decode(input)
}

/// Returns the JavaScript bridge injected into a hidden WebView carrier window
/// (`BridgeScript()` in `web_proxy_webview.cpp:76-119`).
#[must_use]
pub const fn bridge_init_script() -> &'static str {
    r#"(()=>{
if(window!==window.top||Object.prototype.hasOwnProperty.call(window,'TelegramWebProxy'))return;
let receiver=null;
const send=value=>window.external.invoke(value);
const encode=value=>{
 const bytes=new Uint8Array(value),parts=[];
 for(let i=0;i<bytes.length;i+=32768)parts.push(String.fromCharCode(...bytes.subarray(i,i+32768)));
 return btoa(parts.join(''));
};
const decode=value=>{
 const binary=atob(value),bytes=new Uint8Array(binary.length);
 for(let i=0;i<binary.length;i++)bytes[i]=binary.charCodeAt(i);
 return bytes.buffer;
};
const bridge={
 postMessage(value){
  if(value instanceof ArrayBuffer)send('b'+encode(value));
  else if(typeof value==='string')send('c'+value);
  else send('f');
 },
 receive(sequence,value){
  try{
   if(typeof receiver!=='function')throw new Error();
   receiver({data:decode(value)});
   send('a'+sequence);
  }catch(error){send('f')}
 },
 receiveControl(sequence,value){
  try{
   if(typeof receiver!=='function')throw new Error();
   receiver({data:value});
   send('a'+sequence);
  }catch(error){send('f')}
 },
 get onmessage(){return receiver},
 set onmessage(value){receiver=typeof value==='function'?value:null}
};
Object.defineProperty(window,'TelegramWebProxy',{
 value:Object.freeze(bridge),configurable:false,writable:false
});
send('h');
})()"#
}

/// Outbound JavaScript evaluation command emitted by [`WebviewBridgeState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOutbound {
    /// Monotonic write sequence acknowledged by `'a<sequence>'`.
    pub sequence: u64,
    /// JavaScript snippet to evaluate in the WebView.
    pub script: String,
    /// Byte size of the underlying binary frame.
    pub frame_bytes: usize,
}

/// Events emitted by [`WebviewBridgeState`] when processing messages from the hidden WebView.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeEvent {
    /// Heartbeat `'h'` received from the page.
    Heartbeat,
    /// Handshake extended while the relay reports `connecting` / `reconnecting`.
    ExtendHandshake,
    /// WebView bridge completed `tproxy-android-init` + `Hello`/`Welcome` and is ready.
    Ready,
    /// Relay binary payload received after adoption.
    RelayPayload(Vec<u8>),
    /// Previous outbound write acknowledged with its byte count.
    Written(usize),
    /// Outbound script queued to evaluate in the WebView.
    Eval(BridgeOutbound),
}

/// Protocol state machine for `MTP::WebProxy::WebviewCarrier` (`web_proxy_webview.cpp`).
#[derive(Debug)]
pub struct WebviewBridgeState {
    nonce: String,
    expected_host: String,
    expected_path: String,
    initialized: bool,
    adopted: bool,
    failed: bool,
    write_sequence: u64,
    in_flight: Option<(Vec<u8>, bool)>,
    pending: VecDeque<(Vec<u8>, bool)>,
}

impl WebviewBridgeState {
    /// Creates a new WebView bridge state expecting `nonce` from `https://<host><path>`.
    #[must_use]
    pub fn new(
        nonce: impl Into<String>,
        expected_host: impl Into<String>,
        expected_path: impl Into<String>,
    ) -> Self {
        Self {
            nonce: nonce.into(),
            expected_host: expected_host.into(),
            expected_path: expected_path.into(),
            initialized: false,
            adopted: false,
            failed: false,
            write_sequence: 0,
            in_flight: None,
            pending: VecDeque::new(),
        }
    }

    /// Returns whether the bridge has completed the `Welcome` handshake.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.adopted && !self.failed
    }

    /// Validates that a script message originated from the expected HTTPS bridge URL.
    #[must_use]
    pub fn is_valid_source(&self, source_url: &str) -> bool {
        let Some(rest) = source_url.strip_prefix("https://") else {
            return false;
        };
        let (authority, path_and_more) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };
        if authority.contains('@') {
            return false;
        }
        let host = authority.strip_suffix(":443").unwrap_or(authority);
        if host != self.expected_host {
            return false;
        }
        let path_part = path_and_more
            .split(['?', '#'])
            .next()
            .unwrap_or(path_and_more);
        if path_part != self.expected_path {
            return false;
        }
        if let Some((_, fragment)) = path_and_more.split_once('#') {
            fragment.is_empty() || fragment == format!("android={}", self.nonce)
        } else {
            true
        }
    }

    /// Enqueues a binary relay frame for delivery via `window.TelegramWebProxy.receive(...)`.
    pub fn send_frame(&mut self, frame: Vec<u8>) -> Option<BridgeOutbound> {
        if self.failed || frame.is_empty() {
            return None;
        }
        self.pending.push_back((frame, true));
        self.drain_next()
    }

    /// Handles an incoming script message (`'h'`, `'f'`, `'a'`, `'c'`, `'b'`) from the WebView.
    pub fn handle_message(
        &mut self,
        message: &str,
        source_url: &str,
    ) -> Result<Vec<BridgeEvent>, WebProxyError> {
        if self.failed {
            return Err(WebProxyError::Protocol("carrier already failed"));
        }
        if !self.is_valid_source(source_url) {
            self.failed = true;
            return Err(WebProxyError::Protocol("invalid message source"));
        }
        if message.is_empty() || message.len() > MAX_WEBVIEW_MESSAGE_BYTES {
            self.failed = true;
            return Err(WebProxyError::Protocol("invalid message size"));
        }

        let mut events = Vec::new();
        let bytes = message.as_bytes();
        match bytes[0] {
            b'h' => {
                if bytes.len() != 1 {
                    self.failed = true;
                    return Err(WebProxyError::Protocol("invalid heartbeat"));
                }
                events.push(BridgeEvent::Heartbeat);
            }
            b'f' => {
                self.failed = true;
                return Err(WebProxyError::Protocol("bridge script failure"));
            }
            b'a' => {
                let seq: u64 = message[1..].parse().map_err(|_| {
                    self.failed = true;
                    WebProxyError::Protocol("invalid write acknowledgement")
                })?;
                let Some((in_flight_frame, notify)) = self.in_flight.take() else {
                    self.failed = true;
                    return Err(WebProxyError::Protocol("unexpected write acknowledgement"));
                };
                if seq != self.write_sequence {
                    self.failed = true;
                    return Err(WebProxyError::Protocol(
                        "mismatched write acknowledgement sequence",
                    ));
                }
                if notify {
                    events.push(BridgeEvent::Written(in_flight_frame.len()));
                }
                if let Some(outbound) = self.drain_next() {
                    events.push(BridgeEvent::Eval(outbound));
                }
            }
            b'c' => {
                let json: serde_json::Value =
                    serde_json::from_str(&message[1..]).map_err(|_| {
                        self.failed = true;
                        WebProxyError::Protocol("invalid control message")
                    })?;
                let Some(obj) = json.as_object() else {
                    self.failed = true;
                    return Err(WebProxyError::Protocol("control message not object"));
                };
                let msg_type = obj.get("t").and_then(serde_json::Value::as_str);
                match msg_type {
                    Some("tproxy-android-init") if !self.initialized => {
                        let version_ok =
                            obj.get("v").and_then(serde_json::Value::as_i64) == Some(1);
                        let nonce_ok = obj.get("nonce").and_then(serde_json::Value::as_str)
                            == Some(self.nonce.as_str());
                        if !version_ok || !nonce_ok {
                            self.failed = true;
                            return Err(WebProxyError::Protocol("invalid bridge initialization"));
                        }
                        self.initialized = true;
                        let hello = serialize_frame(FrameType::Hello, 0, &[1])?;
                        self.pending.push_back((hello, false));
                        if let Some(outbound) = self.drain_next() {
                            events.push(BridgeEvent::Eval(outbound));
                        }
                    }
                    Some("close") => {
                        self.failed = true;
                        return Err(WebProxyError::Protocol("bridge closed"));
                    }
                    Some("status") => {
                        let state = obj.get("state").and_then(serde_json::Value::as_str);
                        if state == Some("failed") {
                            self.failed = true;
                            return Err(WebProxyError::Protocol("bridge reported failure"));
                        } else if !self.adopted
                            && matches!(state, Some("connecting" | "reconnecting"))
                        {
                            events.push(BridgeEvent::ExtendHandshake);
                        }
                    }
                    _ => {}
                }
            }
            b'b' => {
                if !self.initialized {
                    self.failed = true;
                    return Err(WebProxyError::Protocol(
                        "binary message before initialization",
                    ));
                }
                let decoded = BASE64_STANDARD.decode(&message[1..]).map_err(|_| {
                    self.failed = true;
                    WebProxyError::Protocol("invalid binary base64")
                })?;
                if decoded.is_empty() {
                    self.failed = true;
                    return Err(WebProxyError::Protocol("empty binary message"));
                }
                if !self.adopted {
                    let mut buf = decoded;
                    let frames = parse_frames(&mut buf)?;
                    if !buf.is_empty()
                        || frames.len() != 1
                        || frames[0].frame_type != FrameType::Welcome
                        || frames[0].stream_id != 0
                        || !frames[0].payload.is_empty()
                    {
                        self.failed = true;
                        return Err(WebProxyError::Protocol("invalid bridge welcome"));
                    }
                    self.adopted = true;
                    events.push(BridgeEvent::Ready);
                } else {
                    events.push(BridgeEvent::RelayPayload(decoded));
                }
            }
            _ => {
                self.failed = true;
                return Err(WebProxyError::Protocol("invalid message type"));
            }
        }

        Ok(events)
    }

    fn drain_next(&mut self) -> Option<BridgeOutbound> {
        if self.failed || self.in_flight.is_some() {
            return None;
        }
        let (frame, notify) = self.pending.pop_front()?;
        self.write_sequence += 1;
        let encoded = BASE64_STANDARD.encode(&frame);
        let script = format!(
            "window.TelegramWebProxy?.receive({},'{encoded}')",
            self.write_sequence
        );
        let frame_bytes = frame.len();
        self.in_flight = Some((frame, notify));
        Some(BridgeOutbound {
            sequence: self.write_sequence,
            script,
            frame_bytes,
        })
    }
}

/// High-level connection state for the WebView proxy transport (`Transport::State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebProxyState {
    /// No WebView proxy active.
    #[default]
    Idle,
    /// Waiting for an external browser to open the local bridge URL.
    WaitingForBrowser,
    /// Connecting / waiting for relay `Welcome`.
    Connecting,
    /// Connected and multiplexing streams.
    Connected,
    /// Failed / disconnected by relay.
    Failed,
}

/// Events produced by [`WebProxySession`] for stream owners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Stream opened on the relay and ready for data.
    Connected(u32),
    /// Data bytes received from the relay for `stream_id`.
    Data {
        /// Stream identifier.
        stream_id: u32,
        /// Payload bytes.
        payload: Vec<u8>,
    },
    /// Stream closed remotely by the relay.
    Disconnected(u32),
    /// Stream failed locally or because the relay dropped.
    Failed(u32),
    /// Relay sent `Bye` and terminated the session.
    SessionClosedByRelay,
}

#[derive(Debug)]
struct StreamState {
    opened: bool,
    send_window: u64,
    receive_window: u32,
    pending_window: u32,
    pending_outbound: VecDeque<u8>,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            opened: false,
            send_window: u64::from(INITIAL_STREAM_WINDOW),
            receive_window: INITIAL_STREAM_WINDOW,
            pending_window: 0,
            pending_outbound: VecDeque::new(),
        }
    }
}

/// Multiplexed stream session state machine (`Transport::Private` in `web_proxy_transport.cpp`).
#[derive(Debug, Default)]
pub struct WebProxySession {
    state: WebProxyState,
    welcomed: bool,
    next_stream_id: u32,
    streams: BTreeMap<u32, StreamState>,
    closed_streams: BTreeSet<u32>,
    closed_order: VecDeque<u32>,
    outbound: VecDeque<Vec<u8>>,
}

impl WebProxySession {
    /// Creates a new idle [`WebProxySession`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current transport state.
    #[must_use]
    pub const fn state(&self) -> WebProxyState {
        self.state
    }

    /// Allocates the next non-zero 24-bit stream ID.
    pub fn next_stream_id(&mut self) -> u32 {
        loop {
            self.next_stream_id = (self.next_stream_id + 1) & MAX_STREAM_ID;
            if self.next_stream_id != 0
                && !self.streams.contains_key(&self.next_stream_id)
                && !self.closed_streams.contains(&self.next_stream_id)
            {
                return self.next_stream_id;
            }
        }
    }

    /// Starts the session and queues the initial `Hello` frame (`version = 1`).
    pub fn start_connecting(&mut self) -> Result<(), WebProxyError> {
        self.state = WebProxyState::Connecting;
        self.welcomed = false;
        let hello = serialize_frame(FrameType::Hello, 0, &[1])?;
        self.outbound.push_back(hello);
        Ok(())
    }

    /// Registers a new logical stream. If already welcomed, queues `Open` immediately.
    pub fn register_stream(&mut self, stream_id: u32) -> Result<Vec<StreamEvent>, WebProxyError> {
        if stream_id == 0 || stream_id > MAX_STREAM_ID {
            return Err(WebProxyError::StreamIdOutOfRange(stream_id));
        }
        if self.streams.contains_key(&stream_id) || self.closed_streams.contains(&stream_id) {
            return Ok(vec![StreamEvent::Failed(stream_id)]);
        }
        let mut stream = StreamState::default();
        let mut events = Vec::new();
        if self.welcomed {
            stream.opened = true;
            self.outbound
                .push_back(serialize_frame(FrameType::Open, stream_id, &[])?);
            events.push(StreamEvent::Connected(stream_id));
        }
        self.streams.insert(stream_id, stream);
        Ok(events)
    }

    /// Queues outbound data for `stream_id`, slicing into [`DATA_FRAME_SIZE`] chunks while
    /// respecting the remote stream send window.
    pub fn send_data(&mut self, stream_id: u32, data: &[u8]) -> Result<(), WebProxyError> {
        if data.is_empty() {
            return Ok(());
        }
        let Some(stream) = self.streams.get_mut(&stream_id) else {
            return Ok(());
        };
        stream.pending_outbound.extend(data);
        self.flush_stream(stream_id)
    }

    /// Grants additional receive window back to the relay for `stream_id`.
    pub fn grant_window(&mut self, stream_id: u32, amount: u32) -> Result<(), WebProxyError> {
        if amount == 0 {
            return Ok(());
        }
        let Some(stream) = self.streams.get_mut(&stream_id) else {
            return Ok(());
        };
        stream.pending_window = stream.pending_window.saturating_add(amount);
        if stream.opened && stream.pending_window >= WINDOW_FLUSH_BYTES {
            let grant = std::mem::take(&mut stream.pending_window);
            stream.receive_window = stream.receive_window.saturating_add(grant);
            self.outbound.push_back(serialize_frame(
                FrameType::Window,
                stream_id,
                &encode_window_payload(grant),
            )?);
        }
        Ok(())
    }

    /// Closes a local stream and queues a `Close` frame if it had been opened on the relay.
    pub fn close_stream(&mut self, stream_id: u32) -> Result<(), WebProxyError> {
        if let Some(stream) = self.streams.remove(&stream_id) {
            self.remember_closed_stream(stream_id);
            if stream.opened && self.welcomed {
                self.outbound
                    .push_back(serialize_frame(FrameType::Close, stream_id, &[])?);
            }
        }
        Ok(())
    }

    /// Processes a raw binary relay payload containing one or more frames.
    pub fn process_relay_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<Vec<StreamEvent>, WebProxyError> {
        let mut buf = payload.to_vec();
        let frames = parse_frames(&mut buf)?;
        if !buf.is_empty() || frames.is_empty() {
            return Err(WebProxyError::Protocol(
                "truncated or empty relay frame batch",
            ));
        }
        let mut events = Vec::new();
        for frame in frames {
            self.process_frame(frame, &mut events)?;
        }
        Ok(events)
    }

    /// Drains all queued outbound serialized frames ready to transmit to the relay.
    pub fn drain_outbound_frames(&mut self) -> Vec<Vec<u8>> {
        self.outbound.drain(..).collect()
    }

    fn process_frame(
        &mut self,
        frame: Frame,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), WebProxyError> {
        if !self.welcomed && (frame.frame_type != FrameType::Welcome || frame.stream_id != 0) {
            return Err(WebProxyError::Protocol("frame received before Welcome"));
        }

        if frame.stream_id == 0 {
            return match frame.frame_type {
                FrameType::Welcome => {
                    if self.welcomed || !frame.payload.is_empty() {
                        return Err(WebProxyError::Protocol("unexpected Welcome frame"));
                    }
                    self.welcomed = true;
                    self.state = WebProxyState::Connected;
                    let ids: Vec<u32> = self.streams.keys().copied().collect();
                    for id in ids {
                        if let Some(stream) = self.streams.get_mut(&id) {
                            stream.opened = true;
                        }
                        self.outbound
                            .push_back(serialize_frame(FrameType::Open, id, &[])?);
                        events.push(StreamEvent::Connected(id));
                        self.flush_stream(id)?;
                    }
                    Ok(())
                }
                FrameType::Ping => {
                    if frame.payload.len() > 64 {
                        return Err(WebProxyError::Protocol("ping payload exceeds 64 bytes"));
                    }
                    self.outbound
                        .push_back(serialize_frame(FrameType::Pong, 0, &frame.payload)?);
                    Ok(())
                }
                FrameType::Bye => {
                    self.welcomed = false;
                    self.state = WebProxyState::Failed;
                    let ids: Vec<u32> = self.streams.keys().copied().collect();
                    self.streams.clear();
                    for id in ids {
                        events.push(StreamEvent::Failed(id));
                    }
                    events.push(StreamEvent::SessionClosedByRelay);
                    Ok(())
                }
                _ => Err(WebProxyError::Protocol("invalid stream-0 control frame")),
            };
        }

        if !self.streams.contains_key(&frame.stream_id) {
            if !self.closed_streams.contains(&frame.stream_id) {
                return Err(WebProxyError::Protocol("frame for unknown stream"));
            }
            return match frame.frame_type {
                FrameType::Data if !frame.payload.is_empty() => Ok(()),
                FrameType::Window => {
                    let _ = decode_window_payload(&frame.payload)?;
                    Ok(())
                }
                FrameType::Close if frame.payload.is_empty() => Ok(()),
                _ => Err(WebProxyError::Protocol("invalid frame for closed stream")),
            };
        }

        let opened = self.streams[&frame.stream_id].opened;
        if !opened {
            return Err(WebProxyError::Protocol("frame for unopened stream"));
        }

        match frame.frame_type {
            FrameType::Data => {
                let stream = self
                    .streams
                    .get_mut(&frame.stream_id)
                    .ok_or(WebProxyError::Protocol("missing stream"))?;
                let len = frame.payload.len() as u32;
                if len == 0 || len > stream.receive_window {
                    return Err(WebProxyError::Protocol(
                        "empty Data frame or receive window exceeded",
                    ));
                }
                stream.receive_window -= len;
                events.push(StreamEvent::Data {
                    stream_id: frame.stream_id,
                    payload: frame.payload,
                });
                Ok(())
            }
            FrameType::Window => {
                let amount = decode_window_payload(&frame.payload)?;
                if let Some(stream) = self.streams.get_mut(&frame.stream_id) {
                    stream.send_window = stream
                        .send_window
                        .saturating_add(u64::from(amount))
                        .min(u64::from(u32::MAX));
                }
                self.flush_stream(frame.stream_id)
            }
            FrameType::Close => {
                if !frame.payload.is_empty() {
                    return Err(WebProxyError::Protocol("non-empty Close payload"));
                }
                self.streams.remove(&frame.stream_id);
                self.remember_closed_stream(frame.stream_id);
                events.push(StreamEvent::Disconnected(frame.stream_id));
                Ok(())
            }
            _ => Err(WebProxyError::Protocol("unexpected stream frame type")),
        }
    }

    fn flush_stream(&mut self, stream_id: u32) -> Result<(), WebProxyError> {
        if !self.welcomed {
            return Ok(());
        }
        loop {
            let chunk: Vec<u8> = {
                let Some(stream) = self.streams.get_mut(&stream_id) else {
                    break;
                };
                if !stream.opened || stream.pending_outbound.is_empty() || stream.send_window == 0 {
                    break;
                }
                let take = stream
                    .pending_outbound
                    .len()
                    .min(DATA_FRAME_SIZE)
                    .min(stream.send_window as usize);
                stream.send_window -= take as u64;
                stream.pending_outbound.drain(..take).collect()
            };
            if chunk.is_empty() {
                break;
            }
            self.outbound
                .push_back(serialize_frame(FrameType::Data, stream_id, &chunk)?);
        }
        Ok(())
    }

    fn remember_closed_stream(&mut self, stream_id: u32) {
        if self.closed_streams.insert(stream_id) {
            self.closed_order.push_back(stream_id);
            while self.closed_order.len() > MAX_CLOSED_STREAM_IDS {
                if let Some(old) = self.closed_order.pop_front() {
                    self.closed_streams.remove(&old);
                }
            }
        }
    }
}

// ============================================================================
// Bridge URL / Capability Helpers, Probe Functions & Live Loopback Bridge
// ============================================================================

/// Returns the canonical HTTP path for a Web Proxy bridge (`WebProxyBridgePath` in
/// `mtproto_proxy_data.cpp:432-437`).
#[must_use]
pub fn web_proxy_bridge_path(proxy: &ProxyData) -> String {
    let base = proxy.web_base_path();
    if base.is_empty() {
        "/".to_owned()
    } else {
        format!("/{base}/")
    }
}

/// Computes the HMAC-SHA256 base64url bridge capability query token for `proxy`
/// (`WebProxyBridgeCapability` in `mtproto_proxy_data.cpp:304-393`).
pub fn web_proxy_bridge_capability(proxy: &ProxyData) -> Result<String, WebProxyError> {
    if proxy.proxy_type != ProxyType::Web {
        return Err(WebProxyError::InvalidProxy(
            "expected ProxyType::Web".to_owned(),
        ));
    }
    if proxy.status() != ProxyStatus::Valid {
        return Err(WebProxyError::InvalidProxy(format!(
            "web proxy status is {:?}",
            proxy.status()
        )));
    }
    let secret = proxy.secret_from_mtproto_password();
    if secret.is_empty() {
        return Err(WebProxyError::InvalidProxy(
            "missing web proxy secret".to_owned(),
        ));
    }
    Ok(compute_web_proxy_bridge_capability(
        &proxy.host,
        proxy.web_base_path(),
        &secret,
    ))
}

/// Builds the canonical HTTPS bridge URL `https://<host><path>?bridge=<capability>`
/// (`WebProxyBridgeUrl` in `mtproto_proxy_data.cpp:439-446`).
pub fn web_proxy_bridge_url(proxy: &ProxyData) -> Result<String, WebProxyError> {
    let cap = web_proxy_bridge_capability(proxy)?;
    let path = web_proxy_bridge_path(proxy);
    Ok(format!("https://{}{path}?bridge={cap}", proxy.host))
}

/// Builds the hidden WebView carrier URL `https://<host><path>?bridge=<capability>#android=<nonce>`
/// (`BridgeUrl` in `web_proxy_webview.cpp:55-61`).
pub fn web_proxy_webview_url(proxy: &ProxyData, nonce: &str) -> Result<String, WebProxyError> {
    let base = web_proxy_bridge_url(proxy)?;
    Ok(format!("{base}#android={nonce}"))
}

/// Generates a 32-byte cryptographically random URL-safe bridge nonce (`RandomUrlToken(32)`).
#[must_use]
pub fn generate_bridge_nonce() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Encodes a masked client-to-server RFC 6455 WebSocket frame.
#[must_use]
pub fn encode_masked_websocket_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | (opcode & 0x0F));
    let len = payload.len() as u64;
    if len < 126 {
        out.push(0x80 | (len as u8));
    } else if len <= 0xFFFF {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&len.to_be_bytes());
    }
    out.extend_from_slice(&mask);
    for (idx, &b) in payload.iter().enumerate() {
        out.push(b ^ mask[idx % 4]);
    }
    out
}

/// Decodes unmasked server-to-client RFC 6455 WebSocket frames from `input`, draining consumed bytes.
///
/// Like [`WebSocketDecoder::decode`], the consumed prefix is drained once at the end.
pub fn decode_server_websocket_frames(
    input: &mut Vec<u8>,
) -> Result<Vec<WebSocketMessage>, WebProxyError> {
    let mut out = Vec::new();
    let mut consumed = 0usize;
    let result = decode_server_websocket_frames_from(input, &mut consumed, &mut out);
    if consumed > 0 {
        input.drain(..consumed);
    }
    result.map(|()| out)
}

fn decode_server_websocket_frames_from(
    input: &[u8],
    consumed: &mut usize,
    out: &mut Vec<WebSocketMessage>,
) -> Result<(), WebProxyError> {
    loop {
        let rest = &input[*consumed..];
        if rest.len() < 2 {
            break;
        }
        let b0 = rest[0];
        let b1 = rest[1];
        let fin = (b0 & 0x80) != 0;
        let rsv = b0 & 0x70;
        let opcode = b0 & 0x0F;
        let masked = (b1 & 0x80) != 0;
        if rsv != 0 || masked || !fin {
            return Err(WebProxyError::WebSocket {
                code: 1002,
                reason: "invalid server websocket frame",
            });
        }
        let mut size = u64::from(b1 & 0x7F);
        let mut header = 2usize;
        if size == 126 {
            if rest.len() < 4 {
                break;
            }
            size = u64::from(u16::from_be_bytes([rest[2], rest[3]]));
            header = 4;
        } else if size == 127 {
            if rest.len() < 10 {
                break;
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&rest[2..10]);
            size = u64::from_be_bytes(bytes);
            header = 10;
        }
        if size > MAX_LOCAL_WEBSOCKET_PAYLOAD as u64 {
            return Err(WebProxyError::WebSocket {
                code: 1009,
                reason: "server websocket frame too large",
            });
        }
        let full = header + size as usize;
        if rest.len() < full {
            break;
        }
        let payload = rest[header..full].to_vec();
        *consumed += full;
        match opcode {
            0x01 => out.push(WebSocketMessage::Text(payload)),
            0x02 => out.push(WebSocketMessage::Binary(payload)),
            0x08 => out.push(WebSocketMessage::Close(payload)),
            0x09 => out.push(WebSocketMessage::Ping(payload)),
            0x0A => out.push(WebSocketMessage::Pong(payload)),
            _ => {
                return Err(WebProxyError::WebSocket {
                    code: 1002,
                    reason: "unsupported server opcode",
                });
            }
        }
    }
    Ok(())
}

/// Probes a WebSocket WebProxy relay at `addr` (e.g. a local fake relay or loopback bridge),
/// verifying RFC 6455 101 upgrade, `Sec-WebSocket-Accept`, and binary `Hello` -> `Welcome`
/// handshake, returning the measured round-trip duration.
pub async fn probe_web_proxy_ws_addr(
    addr: SocketAddr,
    path: &str,
    host_header: &str,
    origin: &str,
    timeout: Duration,
) -> Result<Duration, WebProxyError> {
    let started = Instant::now();
    tokio::time::timeout(timeout, async move {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| WebProxyError::Io(format!("connect {addr}: {e}")))?;
        let mut key_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut key_bytes);
        let ws_key = BASE64_STANDARD.encode(key_bytes);
        let expected_accept = compute_websocket_accept(&ws_key);

        let req = format!(
            "GET {path} HTTP/1.1\r\n\
Host: {host_header}\r\n\
Origin: {origin}\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: {ws_key}\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .await
            .map_err(|e| WebProxyError::Io(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| WebProxyError::Io(e.to_string()))?;

        // Read HTTP 101 response headers.
        let mut buf = Vec::with_capacity(1024);
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            if buf.len() > MAX_HTTP_HEADER_SIZE {
                return Err(WebProxyError::Http {
                    status: 431,
                    reason: "response headers too large",
                });
            }
            let n = stream
                .read(&mut tmp)
                .await
                .map_err(|e| WebProxyError::Io(e.to_string()))?;
            if n == 0 {
                return Err(WebProxyError::Io("EOF before WebSocket upgrade".to_owned()));
            }
            buf.extend_from_slice(&tmp[..n]);
        };

        let header_text =
            std::str::from_utf8(&buf[..header_end]).map_err(|_| WebProxyError::Http {
                status: 400,
                reason: "non-UTF8 HTTP response",
            })?;
        let mut lines = header_text.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        if !status_line.contains(" 101 ") {
            return Err(WebProxyError::Http {
                status: 502,
                reason: "WebSocket upgrade rejected",
            });
        }
        let mut accept_matched = false;
        for line in lines {
            if let Some((k, v)) = line.split_once(':')
                && k.trim().eq_ignore_ascii_case("sec-websocket-accept")
                && v.trim() == expected_accept
            {
                accept_matched = true;
            }
        }
        if !accept_matched {
            return Err(WebProxyError::Protocol("Sec-WebSocket-Accept mismatch"));
        }
        buf.drain(..header_end + 4);

        // Send Hello frame via WebProxySession and await Welcome.
        let mut session = WebProxySession::new();
        session.start_connecting()?;
        let mut mask = [0u8; 4];
        rand::thread_rng().fill_bytes(&mut mask);
        for frame in session.drain_outbound_frames() {
            let ws = encode_masked_websocket_frame(0x02, &frame, mask);
            stream
                .write_all(&ws)
                .await
                .map_err(|e| WebProxyError::Io(e.to_string()))?;
        }
        stream
            .flush()
            .await
            .map_err(|e| WebProxyError::Io(e.to_string()))?;

        loop {
            for msg in decode_server_websocket_frames(&mut buf)? {
                match msg {
                    WebSocketMessage::Binary(payload) => {
                        let _ = session.process_relay_payload(&payload)?;
                        if session.state() == WebProxyState::Connected {
                            let _ = stream
                                .write_all(&encode_masked_websocket_frame(0x08, &[], mask))
                                .await;
                            return Ok(started.elapsed());
                        }
                    }
                    WebSocketMessage::Ping(data) => {
                        let pong = encode_masked_websocket_frame(0x0A, &data, mask);
                        let _ = stream.write_all(&pong).await;
                    }
                    WebSocketMessage::Close(_) => {
                        return Err(WebProxyError::Protocol("relay closed before Welcome"));
                    }
                    WebSocketMessage::Text(_) | WebSocketMessage::Pong(_) => {}
                }
            }
            let n = stream
                .read(&mut tmp)
                .await
                .map_err(|e| WebProxyError::Io(e.to_string()))?;
            if n == 0 {
                return Err(WebProxyError::Io("EOF before Welcome frame".to_owned()));
            }
            buf.extend_from_slice(&tmp[..n]);
        }
    })
    .await
    .map_err(|_| WebProxyError::Timeout)?
}

/// Probes a WebProxy HTTP/HTTPS bridge endpoint URL, returning the measured RTT if the server
/// responds with a valid HTTP status (`2xx`, `3xx`, or `101`).
pub async fn probe_web_proxy_http_url(
    url: &str,
    timeout: Duration,
) -> Result<Duration, WebProxyError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| WebProxyError::Io(format!("HTTP client init: {e}")))?;
    let started = Instant::now();
    let resp = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            WebProxyError::Timeout
        } else {
            WebProxyError::Io(e.to_string())
        }
    })?;
    let code = resp.status().as_u16();
    if resp.status().is_success() || resp.status().is_redirection() || code == 101 {
        Ok(started.elapsed())
    } else {
        Err(WebProxyError::Http {
            status: code,
            reason: "web proxy endpoint returned error status",
        })
    }
}

/// Validates a `ProxyType::Web` configuration and probes its canonical HTTPS bridge URL.
pub async fn check_web_proxy(
    proxy: &ProxyData,
    timeout: Duration,
) -> Result<Duration, WebProxyError> {
    let url = web_proxy_bridge_url(proxy)?;
    probe_web_proxy_http_url(&url, timeout).await
}

struct BridgeRuntimeState {
    session: WebProxySession,
    webview: Option<WebviewBridgeState>,
    pending_webview_evals: VecDeque<BridgeOutbound>,
    streams: HashMap<u32, mpsc::UnboundedSender<Result<Vec<u8>, TransportError>>>,
    active_ws_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    outbound_notifier: Option<OutboundNotifier>,
}

/// Callback invoked when new WebView carrier eval scripts are queued, so the UI thread that owns
/// the hidden carrier can wake up and drain them.
pub type OutboundNotifier = Arc<dyn Fn() + Send + Sync>;

impl BridgeRuntimeState {
    fn new() -> Self {
        Self {
            session: WebProxySession::new(),
            webview: None,
            pending_webview_evals: VecDeque::new(),
            streams: HashMap::new(),
            active_ws_tx: None,
            outbound_notifier: None,
        }
    }

    fn dispatch_stream_events(&mut self, events: Vec<StreamEvent>) {
        for ev in events {
            match ev {
                StreamEvent::Connected(_) => {}
                StreamEvent::Data { stream_id, payload } => {
                    if let Some(tx) = self.streams.get(&stream_id) {
                        let _ = tx.send(Ok(payload));
                    }
                }
                StreamEvent::Disconnected(stream_id) | StreamEvent::Failed(stream_id) => {
                    if let Some(tx) = self.streams.remove(&stream_id) {
                        let _ = tx.send(Err(TransportError::Disconnected));
                    }
                }
                StreamEvent::SessionClosedByRelay => {
                    for (_, tx) in self.streams.drain() {
                        let _ = tx.send(Err(TransportError::Disconnected));
                    }
                }
            }
        }
    }

    fn flush_outbound_to_ws(&mut self) {
        if let Some(tx) = self.active_ws_tx.as_ref() {
            for frame in self.session.drain_outbound_frames() {
                let ws_bytes = encode_websocket_frame(0x02, &frame);
                let _ = tx.send(ws_bytes);
            }
        }
    }

    fn flush_outbound_to_webview(&mut self) {
        let mut queued = false;
        if let Some(wv) = self.webview.as_mut()
            && wv.is_ready()
        {
            for frame in self.session.drain_outbound_frames() {
                if let Some(out) = wv.send_frame(frame) {
                    self.pending_webview_evals.push_back(out);
                    queued = true;
                }
            }
        }
        if queued && let Some(notify) = self.outbound_notifier.as_ref() {
            notify();
        }
    }

    fn drain_webview_evals(&mut self) -> Vec<BridgeOutbound> {
        self.flush_outbound_to_webview();
        self.pending_webview_evals.drain(..).collect()
    }
}

/// Live local loopback HTTP/WebSocket bridge and WebView carrier coordinator wrapping
/// [`WebProxySession`] and [`WebviewBridgeState`].
pub struct WebProxyBridgeServer {
    proxy: ProxyData,
    local_addr: SocketAddr,
    state_tx: watch::Sender<WebProxyState>,
    runtime: Arc<Mutex<BridgeRuntimeState>>,
    accept_task: Mutex<Option<JoinHandle<()>>>,
}

impl WebProxyBridgeServer {
    /// Binds a local loopback bridge server on `127.0.0.1:0` for `proxy`.
    pub async fn bind(proxy: ProxyData) -> Result<Self, WebProxyError> {
        if proxy.proxy_type != ProxyType::Web {
            return Err(WebProxyError::InvalidProxy(
                "WebProxyBridgeServer requires ProxyType::Web".to_owned(),
            ));
        }
        if proxy.status() != ProxyStatus::Valid {
            return Err(WebProxyError::InvalidProxy(format!(
                "invalid web proxy status: {:?}",
                proxy.status()
            )));
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| WebProxyError::Io(format!("bind loopback web proxy bridge: {e}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| WebProxyError::Io(e.to_string()))?;
        let (state_tx, _) = watch::channel(WebProxyState::WaitingForBrowser);
        let runtime = Arc::new(Mutex::new(BridgeRuntimeState::new()));

        let runtime_bg = Arc::clone(&runtime);
        let state_tx_bg = state_tx.clone();
        let proxy_bg = proxy.clone();
        let port = local_addr.port();

        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((stream, _peer)) = listener.accept().await else {
                    break;
                };
                let rt = Arc::clone(&runtime_bg);
                let stx = state_tx_bg.clone();
                let prx = proxy_bg.clone();
                tokio::spawn(async move {
                    let _ = handle_loopback_client(stream, port, &prx, rt, stx).await;
                });
            }
        });

        Ok(Self {
            proxy,
            local_addr,
            state_tx,
            runtime,
            accept_task: Mutex::new(Some(accept_task)),
        })
    }

    /// Returns the configured Web Proxy parameters.
    #[must_use]
    pub fn proxy(&self) -> &ProxyData {
        &self.proxy
    }

    /// Returns the bound loopback socket address (`127.0.0.1:<port>`).
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Returns the bound loopback port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Returns the local loopback HTTP bridge page URL (`http://127.0.0.1:<port>/`).
    #[must_use]
    pub fn bridge_page_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.local_addr.port())
    }

    /// Returns the local loopback WebSocket transport URL (`ws://127.0.0.1:<port>/transport`).
    #[must_use]
    pub fn transport_ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/transport", self.local_addr.port())
    }

    /// Returns the canonical remote HTTPS bridge URL (`https://<host><path>?bridge=<cap>`).
    pub fn remote_bridge_url(&self) -> Result<String, WebProxyError> {
        web_proxy_bridge_url(&self.proxy)
    }

    /// Returns the current [`WebProxyState`].
    #[must_use]
    pub fn state(&self) -> WebProxyState {
        *self.state_tx.borrow()
    }

    /// Subscribes to [`WebProxyState`] transitions.
    #[must_use]
    pub fn subscribe_state(&self) -> watch::Receiver<WebProxyState> {
        self.state_tx.subscribe()
    }

    /// Initializes the hidden WebView carrier state machine with `nonce` and returns the
    /// exact HTTPS URL (`https://<host><path>?bridge=<cap>#android=<nonce>`) to load.
    pub fn attach_webview_carrier(
        &self,
        nonce: impl Into<String>,
    ) -> Result<String, WebProxyError> {
        let nonce_str = nonce.into();
        let url = web_proxy_webview_url(&self.proxy, &nonce_str)?;
        let path = web_proxy_bridge_path(&self.proxy);
        let mut rt = self.runtime.lock();
        rt.webview = Some(WebviewBridgeState::new(
            nonce_str,
            self.proxy.host.clone(),
            path,
        ));
        rt.pending_webview_evals.clear();
        self.state_tx.send_replace(WebProxyState::Connecting);
        Ok(url)
    }

    /// Feeds a raw script message from the hidden WebView carrier (`tg-webview::WebMessage`)
    /// through [`WebviewBridgeState`] into [`WebProxySession`].
    pub fn feed_webview_message(
        &self,
        message: &str,
        source_url: &str,
    ) -> Result<Vec<BridgeEvent>, WebProxyError> {
        let mut rt = self.runtime.lock();
        let Some(wv) = rt.webview.as_mut() else {
            return Err(WebProxyError::Protocol("webview carrier not attached"));
        };
        let mut events = wv.handle_message(message, source_url)?;
        for ev in &events {
            match ev {
                BridgeEvent::Ready => {
                    if rt.session.state() == WebProxyState::Idle {
                        rt.session.start_connecting()?;
                        // Discard Hello already sent by WebviewBridgeState during adoption.
                        let _ = rt.session.drain_outbound_frames();
                    }
                    let welcome = serialize_frame(FrameType::Welcome, 0, &[])?;
                    let stream_events = rt.session.process_relay_payload(&welcome)?;
                    rt.dispatch_stream_events(stream_events);
                    self.state_tx.send_replace(rt.session.state());
                    rt.flush_outbound_to_webview();
                }
                BridgeEvent::RelayPayload(bytes) => {
                    let stream_events = rt.session.process_relay_payload(bytes)?;
                    rt.dispatch_stream_events(stream_events);
                    self.state_tx.send_replace(rt.session.state());
                    rt.flush_outbound_to_webview();
                }
                BridgeEvent::Heartbeat
                | BridgeEvent::ExtendHandshake
                | BridgeEvent::Written(_)
                | BridgeEvent::Eval(_) => {}
            }
        }
        for out in rt.drain_webview_evals() {
            events.push(BridgeEvent::Eval(out));
        }
        Ok(events)
    }

    /// Installs a callback invoked whenever new WebView carrier eval scripts are queued.
    pub fn set_outbound_notifier(&self, notifier: Option<OutboundNotifier>) {
        self.runtime.lock().outbound_notifier = notifier;
    }

    /// Opens a new multiplexed stream as a raw byte pipe ([`WebProxyByteStream`]), the
    /// equivalent of tdesktop's `WebProxySocket` (`mtproto/details/mtproto_web_proxy_socket.cpp`).
    pub fn open_byte_stream(&self) -> Result<WebProxyByteStream, WebProxyError> {
        let conn = self.open_stream()?;
        Ok(WebProxyByteStream {
            conn,
            read_buf: Vec::new(),
            read_pos: 0,
        })
    }

    /// Drains any pending outbound frames from [`WebProxySession`] as WebView eval scripts.
    pub fn drain_webview_outbound(&self) -> Vec<BridgeOutbound> {
        let mut rt = self.runtime.lock();
        rt.drain_webview_evals()
    }

    /// Opens a new multiplexed MTProto stream over this Web Proxy bridge, returning a
    /// [`WebProxyPacketConnection`] implementing [`PacketConnection`].
    pub fn open_stream(&self) -> Result<WebProxyPacketConnection, WebProxyError> {
        let mut rt = self.runtime.lock();
        let stream_id = rt.session.next_stream_id();
        let (tx, rx) = mpsc::unbounded_channel();
        rt.streams.insert(stream_id, tx);
        let events = rt.session.register_stream(stream_id)?;
        rt.dispatch_stream_events(events);
        rt.flush_outbound_to_ws();
        rt.flush_outbound_to_webview();
        Ok(WebProxyPacketConnection {
            stream_id,
            runtime: Arc::clone(&self.runtime),
            rx,
            closed: false,
        })
    }

    /// Shuts down the loopback bridge server and closes all active streams.
    pub fn shutdown(&self) {
        if let Some(handle) = self.accept_task.lock().take() {
            handle.abort();
        }
        let mut rt = self.runtime.lock();
        rt.active_ws_tx = None;
        rt.pending_webview_evals.clear();
        for (_, tx) in rt.streams.drain() {
            let _ = tx.send(Err(TransportError::Disconnected));
        }
        self.state_tx.send_replace(WebProxyState::Idle);
    }
}

impl Drop for WebProxyBridgeServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A multiplexed MTProto packet stream carried over [`WebProxyBridgeServer`].
pub struct WebProxyPacketConnection {
    stream_id: u32,
    runtime: Arc<Mutex<BridgeRuntimeState>>,
    rx: mpsc::UnboundedReceiver<Result<Vec<u8>, TransportError>>,
    closed: bool,
}

impl WebProxyPacketConnection {
    /// Returns the 24-bit multiplexed stream identifier assigned by [`WebProxySession`].
    #[must_use]
    pub const fn stream_id(&self) -> u32 {
        self.stream_id
    }
}

impl PacketConnection for WebProxyPacketConnection {
    fn send<'a>(&'a mut self, payload: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            if self.closed {
                return Err(TransportError::Disconnected);
            }
            let mut rt = self.runtime.lock();
            rt.session
                .send_data(self.stream_id, payload)
                .map_err(|e| TransportError::InvalidProxy(e.to_string()))?;
            rt.flush_outbound_to_ws();
            rt.flush_outbound_to_webview();
            Ok(())
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
            if self.closed {
                return Err(TransportError::Disconnected);
            }
            match self.rx.recv().await {
                Some(Ok(bytes)) => {
                    let len = bytes.len().min(u32::MAX as usize) as u32;
                    let mut rt = self.runtime.lock();
                    let _ = rt.session.grant_window(self.stream_id, len);
                    rt.flush_outbound_to_ws();
                    rt.flush_outbound_to_webview();
                    Ok(bytes)
                }
                Some(Err(err)) => Err(err),
                None => Err(TransportError::Disconnected),
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if !self.closed {
                self.closed = true;
                let mut rt = self.runtime.lock();
                rt.streams.remove(&self.stream_id);
                let _ = rt.session.close_stream(self.stream_id);
                rt.flush_outbound_to_ws();
                rt.flush_outbound_to_webview();
            }
        })
    }

    fn transport_tag(&self) -> &str {
        "WebProxy"
    }
}

/// [`TransportFactory`] routing MTProto DC connections through an active [`WebProxyBridgeServer`].
#[derive(Clone)]
pub struct WebProxyTransportFactory {
    server: Arc<WebProxyBridgeServer>,
}

impl WebProxyTransportFactory {
    /// Creates a [`WebProxyTransportFactory`] backed by `server`.
    #[must_use]
    pub fn new(server: Arc<WebProxyBridgeServer>) -> Self {
        Self { server }
    }

    /// Returns the underlying [`WebProxyBridgeServer`].
    #[must_use]
    pub fn server(&self) -> &Arc<WebProxyBridgeServer> {
        &self.server
    }
}

impl TransportFactory for WebProxyTransportFactory {
    fn connect<'a>(
        &'a self,
        _ip: &'a str,
        _port: u16,
        protocol_dc_id: i16,
        _secret: &'a [u8],
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            connect_over_web_proxy_bridge(&self.server, protocol_dc_id, TransportKind::Intermediate)
                .await
        })
    }
}

/// A multiplexed Web Proxy stream exposed as an async byte pipe, so the regular MTProto TCP
/// framing and obfuscation (`TcpConnection` over `WebProxySocket` in tdesktop) can run on it.
pub struct WebProxyByteStream {
    conn: WebProxyPacketConnection,
    read_buf: Vec<u8>,
    read_pos: usize,
}

impl WebProxyByteStream {
    /// Returns the 24-bit multiplexed stream identifier.
    #[must_use]
    pub const fn stream_id(&self) -> u32 {
        self.conn.stream_id
    }

    fn close_now(&mut self) {
        if !self.conn.closed {
            self.conn.closed = true;
            let mut rt = self.conn.runtime.lock();
            rt.streams.remove(&self.conn.stream_id);
            let _ = rt.session.close_stream(self.conn.stream_id);
            rt.flush_outbound_to_ws();
            rt.flush_outbound_to_webview();
        }
    }
}

impl AsyncRead for WebProxyByteStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.read_pos < this.read_buf.len() {
                let available = &this.read_buf[this.read_pos..];
                let n = available.len().min(buf.remaining());
                buf.put_slice(&available[..n]);
                this.read_pos += n;
                if this.read_pos >= this.read_buf.len() {
                    this.read_buf.clear();
                    this.read_pos = 0;
                }
                return Poll::Ready(Ok(()));
            }
            if this.conn.closed {
                return Poll::Ready(Ok(()));
            }
            match this.conn.rx.poll_recv(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                    {
                        let mut rt = this.conn.runtime.lock();
                        let _ = rt.session.grant_window(this.conn.stream_id, len);
                        rt.flush_outbound_to_ws();
                        rt.flush_outbound_to_webview();
                    }
                    this.read_buf = bytes;
                    this.read_pos = 0;
                }
                Poll::Ready(Some(Err(err))) => {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        err.to_string(),
                    )));
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for WebProxyByteStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.conn.closed {
            return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }
        let mut rt = this.conn.runtime.lock();
        if !rt.streams.contains_key(&this.conn.stream_id) {
            return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }
        rt.session
            .send_data(this.conn.stream_id, data)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        rt.flush_outbound_to_ws();
        rt.flush_outbound_to_webview();
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.get_mut().close_now();
        Poll::Ready(Ok(()))
    }
}

impl Drop for WebProxyByteStream {
    fn drop(&mut self) {
        self.close_now();
    }
}

/// Opens an MTProto TCP connection (obfuscated framing keyed by the Web proxy secret, as
/// `TcpConnection::connectToServer` does for `ProxyData::Type::Web`) over a new bridge stream.
pub async fn connect_over_web_proxy_bridge(
    server: &WebProxyBridgeServer,
    protocol_dc_id: i16,
    transport: TransportKind,
) -> Result<Box<dyn PacketConnection>, TransportError> {
    let stream = server
        .open_byte_stream()
        .map_err(|e| TransportError::InvalidProxy(e.to_string()))?;
    let cfg = ConnectionConfig {
        transport,
        obfuscated: true,
        dc_id: protocol_dc_id,
        protocol_secret: server.proxy().secret_from_mtproto_password(),
        proxy: ProxyData::none(),
        ..ConnectionConfig::default()
    };
    let conn = Connection::from_stream(stream, cfg).await?;
    Ok(Box::new(TgTcpConnection::new(conn)) as Box<dyn PacketConnection>)
}

/// Bridges registered for the process (`WebProxy::Transport::Instance()` in tdesktop), looked up
/// by the default transport factory when the client's proxy is a Web proxy.
static ACTIVE_WEB_PROXY_BRIDGES: Mutex<Vec<Weak<WebProxyBridgeServer>>> = Mutex::new(Vec::new());

fn same_web_proxy(a: &ProxyData, b: &ProxyData) -> bool {
    a.proxy_type == ProxyType::Web
        && b.proxy_type == ProxyType::Web
        && a.host == b.host
        && a.port == b.port
        && a.user == b.user
        && a.password == b.password
}

/// Registers `server` as the active bridge for its Web proxy. Dropped servers unregister
/// themselves (only a weak reference is kept).
pub fn register_active_web_proxy_bridge(server: &Arc<WebProxyBridgeServer>) {
    let mut list = ACTIVE_WEB_PROXY_BRIDGES.lock();
    list.retain(|w| {
        w.upgrade()
            .is_some_and(|s| !same_web_proxy(s.proxy(), server.proxy()))
    });
    list.push(Arc::downgrade(server));
}

/// Unregisters `server` if it is the active bridge for its Web proxy.
pub fn unregister_active_web_proxy_bridge(server: &Arc<WebProxyBridgeServer>) {
    ACTIVE_WEB_PROXY_BRIDGES
        .lock()
        .retain(|w| w.upgrade().is_some_and(|s| !Arc::ptr_eq(&s, server)));
}

/// Returns the active bridge registered for `proxy`, if any.
#[must_use]
pub fn active_web_proxy_bridge(proxy: &ProxyData) -> Option<Arc<WebProxyBridgeServer>> {
    ACTIVE_WEB_PROXY_BRIDGES
        .lock()
        .iter()
        .filter_map(Weak::upgrade)
        .find(|s| same_web_proxy(s.proxy(), proxy))
}

/// Connects through the bridge registered for `proxy` (see [`register_active_web_proxy_bridge`]).
pub async fn connect_via_active_web_proxy(
    proxy: &ProxyData,
    protocol_dc_id: i16,
    transport: TransportKind,
) -> Result<Box<dyn PacketConnection>, TransportError> {
    let server = active_web_proxy_bridge(proxy).ok_or_else(|| {
        TransportError::InvalidProxy("Web proxy bridge is not running".to_owned())
    })?;
    connect_over_web_proxy_bridge(&server, protocol_dc_id, transport).await
}

async fn handle_loopback_client(
    mut stream: TcpStream,
    port: u16,
    proxy: &ProxyData,
    runtime: Arc<Mutex<BridgeRuntimeState>>,
    state_tx: watch::Sender<WebProxyState>,
) -> Result<(), WebProxyError> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 4096];

    let action = loop {
        match parse_local_http_request(&buf, port) {
            Ok(HttpRequestAction::Incomplete) => {
                let n = stream
                    .read(&mut tmp)
                    .await
                    .map_err(|e| WebProxyError::Io(e.to_string()))?;
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            Ok(act) => break act,
            Err(WebProxyError::Http { status, reason }) => {
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                return Err(WebProxyError::Http { status, reason });
            }
            Err(e) => return Err(e),
        }
    };

    match action {
        HttpRequestAction::Incomplete => Ok(()),
        HttpRequestAction::ServeBridgePage => {
            let remote = web_proxy_bridge_url(proxy).unwrap_or_default();
            let body = format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><title>Rustgram Web Proxy Bridge</title></head>\
                 <body><script>window.__RUSTGRAM_WEB_PROXY_REMOTE__={:?};</script></body></html>",
                remote
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(resp.as_bytes())
                .await
                .map_err(|e| WebProxyError::Io(e.to_string()))?;
            let _ = stream.flush().await;
            Ok(())
        }
        HttpRequestAction::UpgradeWebSocket { response, consumed } => {
            buf.drain(..consumed);
            stream
                .write_all(&response)
                .await
                .map_err(|e| WebProxyError::Io(e.to_string()))?;
            stream
                .flush()
                .await
                .map_err(|e| WebProxyError::Io(e.to_string()))?;

            let (mut reader, mut writer) = stream.into_split();
            let (ws_tx, mut ws_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            {
                let mut rt = runtime.lock();
                rt.active_ws_tx = Some(ws_tx.clone());
                if rt.session.state() == WebProxyState::Idle {
                    rt.session.start_connecting()?;
                }
                state_tx.send_replace(rt.session.state());
                rt.flush_outbound_to_ws();
            }

            let writer_task = tokio::spawn(async move {
                while let Some(bytes) = ws_rx.recv().await {
                    if writer.write_all(&bytes).await.is_err() {
                        break;
                    }
                    if writer.flush().await.is_err() {
                        break;
                    }
                }
            });

            let mut decoder = WebSocketDecoder::default();
            loop {
                match decoder.decode(&mut buf) {
                    Ok(msgs) => {
                        let mut closed = false;
                        for msg in msgs {
                            match msg {
                                WebSocketMessage::Binary(payload) => {
                                    let mut rt = runtime.lock();
                                    let events = rt.session.process_relay_payload(&payload)?;
                                    rt.dispatch_stream_events(events);
                                    state_tx.send_replace(rt.session.state());
                                    rt.flush_outbound_to_ws();
                                }
                                WebSocketMessage::Ping(data) => {
                                    let _ = ws_tx.send(encode_websocket_frame(0x0A, &data));
                                }
                                WebSocketMessage::Close(_) => {
                                    closed = true;
                                    break;
                                }
                                WebSocketMessage::Text(_) | WebSocketMessage::Pong(_) => {}
                            }
                        }
                        if closed {
                            break;
                        }
                    }
                    Err(e) => {
                        writer_task.abort();
                        return Err(e);
                    }
                }
                let n = reader
                    .read(&mut tmp)
                    .await
                    .map_err(|e| WebProxyError::Io(e.to_string()))?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }

            writer_task.abort();
            let mut rt = runtime.lock();
            rt.active_ws_tx = None;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_and_partial_streaming() {
        let f1 = serialize_frame(FrameType::Hello, 0, &[1]).expect("hello");
        let f2 = serialize_frame(FrameType::Data, 0x01_0203, b"mtproto-bytes").expect("data");
        let f3 = serialize_frame(FrameType::Window, 0x01_0203, &encode_window_payload(65_536))
            .expect("window");

        let mut combined = Vec::new();
        combined.extend_from_slice(&f1);
        combined.extend_from_slice(&f2);
        combined.extend_from_slice(&f3);

        // Feed byte-by-byte / partial slices to confirm streaming parser retains partial tails.
        let mut buf = Vec::new();
        buf.extend_from_slice(&combined[..5]);
        assert!(parse_frames(&mut buf).expect("partial header").is_empty());
        assert_eq!(buf.len(), 5);

        buf.extend_from_slice(&combined[5..f1.len() + 10]);
        let first = parse_frames(&mut buf).expect("first complete frame");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0], Frame::new(FrameType::Hello, 0, vec![1]));
        assert_eq!(buf.len(), 10);

        buf.extend_from_slice(&combined[f1.len() + 10..]);
        let rest = parse_frames(&mut buf).expect("remaining frames");
        assert!(buf.is_empty());
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].frame_type, FrameType::Data);
        assert_eq!(rest[0].stream_id, 0x01_0203);
        assert_eq!(rest[0].payload, b"mtproto-bytes");
        assert_eq!(rest[1].frame_type, FrameType::Window);
        assert_eq!(decode_window_payload(&rest[1].payload).unwrap(), 65_536);
    }

    #[test]
    fn frame_validation_rejects_bad_headers() {
        assert_eq!(
            serialize_frame(FrameType::Data, MAX_STREAM_ID + 1, b"x"),
            Err(WebProxyError::StreamIdOutOfRange(MAX_STREAM_ID + 1))
        );
        let oversized = vec![0u8; MAX_FRAME_PAYLOAD + 1];
        assert_eq!(
            serialize_frame(FrameType::Data, 1, &oversized),
            Err(WebProxyError::PayloadTooLarge(MAX_FRAME_PAYLOAD + 1))
        );

        let mut unknown = vec![0x99, 0, 0, 1, 0, 0, 0, 0];
        assert_eq!(
            parse_frames(&mut unknown),
            Err(WebProxyError::UnknownFrameType(0x99))
        );

        let mut bad_len = vec![FrameType::Data as u8, 0, 0, 1, 0x01, 0, 0, 0];
        assert_eq!(
            parse_frames(&mut bad_len),
            Err(WebProxyError::PayloadTooLarge(0x0100_0000))
        );
    }

    #[test]
    fn local_http_upgrade_and_websocket_masked_frame_decode() {
        // Standard RFC 6455 test vector key.
        let accept = compute_websocket_accept("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");

        let req = concat!(
            "GET /transport HTTP/1.1\r\n",
            "Host: 127.0.0.1:41234\r\n",
            "Origin: http://127.0.0.1:41234\r\n",
            "Upgrade: websocket\r\n",
            "Connection: keep-alive, Upgrade\r\n",
            "Sec-WebSocket-Version: 13\r\n",
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        let action = parse_local_http_request(req.as_bytes(), 41234).expect("valid ws upgrade");
        match action {
            HttpRequestAction::UpgradeWebSocket { response, consumed } => {
                assert_eq!(consumed, req.len());
                let text = String::from_utf8(response).unwrap();
                assert!(text.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
                assert!(text.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
            }
            other => panic!("unexpected action: {other:?}"),
        }

        // Wrong host is rejected with 403.
        let bad_host = "GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n";
        assert_eq!(
            parse_local_http_request(bad_host.as_bytes(), 41234),
            Err(WebProxyError::Http {
                status: 403,
                reason: "Forbidden",
            })
        );

        // Decode a masked binary WebSocket frame carrying a serialized Ping frame.
        let ping = serialize_frame(FrameType::Ping, 0, b"abc").unwrap();
        let mask = [0x37u8, 0xFA, 0x21, 0x3D];
        let mut wire = vec![0x82, 0x80 | (ping.len() as u8)];
        wire.extend_from_slice(&mask);
        for (i, b) in ping.iter().enumerate() {
            wire.push(b ^ mask[i % 4]);
        }
        let msgs = decode_websocket_client_frames(&mut wire).expect("decode masked binary");
        assert!(wire.is_empty());
        assert_eq!(msgs, vec![WebSocketMessage::Binary(ping)]);
    }

    #[test]
    fn webview_bridge_handshake_and_session_multiplexing() {
        let mut bridge = WebviewBridgeState::new("secret-nonce", "proxy.example", "/bridge");
        let source = "https://proxy.example/bridge#android=secret-nonce";
        assert!(bridge.is_valid_source(source));
        assert!(!bridge.is_valid_source("http://proxy.example/bridge#android=secret-nonce"));
        assert!(!bridge.is_valid_source("https://other.example/bridge#android=secret-nonce"));

        // 1. Heartbeat
        let ev = bridge.handle_message("h", source).unwrap();
        assert_eq!(ev, vec![BridgeEvent::Heartbeat]);

        // 2. Init control message triggers Hello frame eval
        let init = r#"c{"t":"tproxy-android-init","v":1,"nonce":"secret-nonce"}"#;
        let ev = bridge.handle_message(init, source).unwrap();
        assert_eq!(ev.len(), 1);
        let seq = match &ev[0] {
            BridgeEvent::Eval(out) => {
                assert!(out.script.contains("window.TelegramWebProxy?.receive(1,'"));
                out.sequence
            }
            other => panic!("expected eval, got {other:?}"),
        };

        // 3. Acknowledge Hello write
        let ev = bridge.handle_message(&format!("a{seq}"), source).unwrap();
        assert!(ev.is_empty());

        // 4. Relay sends Welcome frame in base64 -> Ready
        let welcome = serialize_frame(FrameType::Welcome, 0, &[]).unwrap();
        let welcome_msg = format!("b{}", BASE64_STANDARD.encode(&welcome));
        let ev = bridge.handle_message(&welcome_msg, source).unwrap();
        assert_eq!(ev, vec![BridgeEvent::Ready]);
        assert!(bridge.is_ready());

        // 5. Session multiplexing: start -> Welcome -> Open/Data/Ping/Pong/Close
        let mut session = WebProxySession::new();
        session.start_connecting().unwrap();
        let stream_id = session.next_stream_id();
        assert!(session.register_stream(stream_id).unwrap().is_empty());
        session.send_data(stream_id, b"hello relay").unwrap();

        let outbound_before_welcome = session.drain_outbound_frames();
        assert_eq!(outbound_before_welcome.len(), 1); // Only Hello so far

        let events = session.process_relay_payload(&welcome).unwrap();
        assert_eq!(session.state(), WebProxyState::Connected);
        assert_eq!(events, vec![StreamEvent::Connected(stream_id)]);

        // Now Open + Data frames were queued.
        let mut out_bytes = Vec::new();
        for f in session.drain_outbound_frames() {
            out_bytes.extend_from_slice(&f);
        }
        let parsed = parse_frames(&mut out_bytes).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], Frame::new(FrameType::Open, stream_id, vec![]));
        assert_eq!(
            parsed[1],
            Frame::new(FrameType::Data, stream_id, b"hello relay".to_vec())
        );

        // Relay sends Ping + Data for stream_id.
        let mut inbound = serialize_frame(FrameType::Ping, 0, b"42").unwrap();
        inbound.extend_from_slice(&serialize_frame(FrameType::Data, stream_id, b"reply").unwrap());
        let events = session.process_relay_payload(&inbound).unwrap();
        assert_eq!(
            events,
            vec![StreamEvent::Data {
                stream_id,
                payload: b"reply".to_vec(),
            }]
        );
        let pongs = session.drain_outbound_frames();
        assert_eq!(pongs.len(), 1);
        assert_eq!(
            pongs[0],
            serialize_frame(FrameType::Pong, 0, b"42").unwrap()
        );

        // Grant enough window to trigger Window frame flush.
        session.grant_window(stream_id, WINDOW_FLUSH_BYTES).unwrap();
        let win_frames = session.drain_outbound_frames();
        assert_eq!(win_frames.len(), 1);
        assert_eq!(
            win_frames[0],
            serialize_frame(
                FrameType::Window,
                stream_id,
                &encode_window_payload(WINDOW_FLUSH_BYTES)
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn web_proxy_bridge_url_loopback_server_and_packet_stream_roundtrip() {
        let mut proxy = ProxyData::none();
        proxy.proxy_type = ProxyType::Web;
        proxy.port = 443;
        proxy.set_web_address("https://proxy.example.com/dobry-cola-super-app/");
        proxy.password = "000102030405060708090a0b0c0d0e0f".to_owned();

        let url = web_proxy_bridge_url(&proxy).expect("bridge url");
        assert_eq!(
            url,
            "https://proxy.example.com/dobry-cola-super-app/?bridge=hHz99Xs93EN1j91G9gpNepXwGNNt5YdAFkEVk_LlqdQ"
        );

        let server = Arc::new(
            WebProxyBridgeServer::bind(proxy)
                .await
                .expect("bind bridge"),
        );
        let port = server.port();

        // 1. GET / serves bridge HTML page.
        let mut http_client = TcpStream::connect(server.local_addr())
            .await
            .expect("connect http");
        let http_req = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
        http_client.write_all(http_req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        http_client.read_to_end(&mut resp).await.unwrap();
        let resp_str = String::from_utf8(resp).unwrap();
        assert!(resp_str.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(resp_str.contains("Rustgram Web Proxy Bridge"));

        // 2. Connect as a simulated browser relay on /transport, verify Hello -> Welcome + Data.
        let mut ws_client = TcpStream::connect(server.local_addr())
            .await
            .expect("connect ws");
        let ws_req = format!(
            "GET /transport HTTP/1.1\r\n\
Host: 127.0.0.1:{port}\r\n\
Origin: http://127.0.0.1:{port}\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        ws_client.write_all(ws_req.as_bytes()).await.unwrap();

        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let hdr_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            let n = ws_client.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
        };
        let hdr = std::str::from_utf8(&buf[..hdr_end]).unwrap();
        assert!(hdr.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        buf.drain(..hdr_end + 4);

        // Expect Hello binary frame from bridge server.
        let mut relay_in = Vec::new();
        let hello = loop {
            for msg in decode_server_websocket_frames(&mut buf).unwrap() {
                if let WebSocketMessage::Binary(b) = msg {
                    relay_in.extend_from_slice(&b);
                }
            }
            let frames = parse_frames(&mut relay_in).unwrap();
            if !frames.is_empty() {
                break frames;
            }
            let n = ws_client.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
        };
        assert_eq!(hello[0], Frame::new(FrameType::Hello, 0, vec![1]));

        // Send Welcome from simulated relay.
        let mask = [0x11, 0x22, 0x33, 0x44];
        let welcome = serialize_frame(FrameType::Welcome, 0, &[]).unwrap();
        ws_client
            .write_all(&encode_masked_websocket_frame(0x02, &welcome, mask))
            .await
            .unwrap();

        // Wait for state == Connected.
        let mut state_rx = server.subscribe_state();
        while *state_rx.borrow() != WebProxyState::Connected {
            state_rx.changed().await.unwrap();
        }

        // Open a PacketConnection stream and send/receive MTProto payload.
        let mut conn = server.open_stream().expect("open stream");
        let sid = conn.stream_id();
        conn.send(b"ping-mtproto").await.expect("send");

        let mut got_open = false;
        let mut got_data = false;
        while !(got_open && got_data) {
            for msg in decode_server_websocket_frames(&mut buf).unwrap() {
                if let WebSocketMessage::Binary(b) = msg {
                    relay_in.extend_from_slice(&b);
                }
            }
            for f in parse_frames(&mut relay_in).unwrap() {
                if f.frame_type == FrameType::Open && f.stream_id == sid {
                    got_open = true;
                } else if f.frame_type == FrameType::Data && f.stream_id == sid {
                    assert_eq!(f.payload, b"ping-mtproto");
                    got_data = true;
                }
            }
            if !(got_open && got_data) {
                let n = ws_client.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
        }

        // Relay replies with Data on stream `sid`.
        let reply = serialize_frame(FrameType::Data, sid, b"pong-mtproto").unwrap();
        ws_client
            .write_all(&encode_masked_websocket_frame(0x02, &reply, mask))
            .await
            .unwrap();
        let received = conn.recv().await.expect("recv");
        assert_eq!(received, b"pong-mtproto");
        conn.close().await;
        server.shutdown();
    }

    async fn attach_fake_relay(server: &WebProxyBridgeServer) -> (TcpStream, Vec<u8>, Vec<u8>) {
        let port = server.port();
        let mut ws = TcpStream::connect(server.local_addr())
            .await
            .expect("connect ws");
        let req = format!(
            "GET /transport HTTP/1.1\r\n\
Host: 127.0.0.1:{port}\r\n\
Origin: http://127.0.0.1:{port}\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        ws.write_all(req.as_bytes()).await.expect("upgrade");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let hdr_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            let n = ws.read(&mut tmp).await.expect("read upgrade");
            assert!(n > 0, "relay connection closed during upgrade");
            buf.extend_from_slice(&tmp[..n]);
        };
        buf.drain(..hdr_end + 4);
        let welcome = serialize_frame(FrameType::Welcome, 0, &[]).expect("welcome");
        ws.write_all(&encode_masked_websocket_frame(0x02, &welcome, [1, 2, 3, 4]))
            .await
            .expect("send welcome");
        let mut state_rx = server.subscribe_state();
        while *state_rx.borrow() != WebProxyState::Connected {
            state_rx.changed().await.expect("state");
        }
        (ws, buf, Vec::new())
    }

    async fn read_stream_data(
        ws: &mut TcpStream,
        buf: &mut Vec<u8>,
        relay_in: &mut Vec<u8>,
        stream_id: u32,
        want: usize,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut tmp = [0u8; 4096];
        while out.len() < want {
            for msg in decode_server_websocket_frames(buf).expect("ws decode") {
                if let WebSocketMessage::Binary(b) = msg {
                    relay_in.extend_from_slice(&b);
                }
            }
            for f in parse_frames(relay_in).expect("frames") {
                if f.frame_type == FrameType::Data && f.stream_id == stream_id {
                    out.extend_from_slice(&f.payload);
                }
            }
            if out.len() < want {
                let n = ws.read(&mut tmp).await.expect("read relay");
                assert!(n > 0, "relay connection closed");
                buf.extend_from_slice(&tmp[..n]);
            }
        }
        out
    }

    #[tokio::test]
    async fn default_transport_factory_routes_web_proxy_through_registered_bridge() {
        use crate::config::ClientConfig;
        use crate::dc_options::{DcOptions, DcType};
        use crate::transport_adapter::TgTransportFactory;
        use tg_transport::{OBFUSCATED_HEADER_SIZE, Obfuscated2};

        let mut proxy = ProxyData::none();
        proxy.proxy_type = ProxyType::Web;
        proxy.port = 443;
        proxy.set_web_address("https://routing-check.example.com/relay/");
        proxy.password = "000102030405060708090a0b0c0d0e0f".to_owned();
        let factory = TgTransportFactory::default();
        let config = ClientConfig::default();
        let dc_options = DcOptions::new(config.environment);

        // Without a registered bridge the Web proxy cannot be dialed.
        let missing = factory
            .connect_dc(2, DcType::Regular, &dc_options, &config, &proxy)
            .await;
        assert!(missing.is_err());

        let server = Arc::new(
            WebProxyBridgeServer::bind(proxy.clone())
                .await
                .expect("bind bridge"),
        );
        register_active_web_proxy_bridge(&server);
        assert!(active_web_proxy_bridge(&proxy).is_some());
        let (mut ws, mut buf, mut relay_in) = attach_fake_relay(&server).await;

        let mut conn = factory
            .connect_dc(2, DcType::Regular, &dc_options, &config, &proxy)
            .await
            .expect("connect through web proxy bridge");
        assert_eq!(conn.transport_tag(), "TCP/Stream");
        let packet = *b"0123456789abcdef";
        conn.send(&packet).await.expect("send packet");

        // Stream 1 carries the obfuscated2 header keyed by the proxy secret, then the
        // abridged-framed packet (1 length byte + 16 bytes).
        let sent = read_stream_data(
            &mut ws,
            &mut buf,
            &mut relay_in,
            1,
            OBFUSCATED_HEADER_SIZE + 1 + packet.len(),
        )
        .await;
        let mut header = [0u8; OBFUSCATED_HEADER_SIZE];
        header.copy_from_slice(&sent[..OBFUSCATED_HEADER_SIZE]);
        let key: [u8; 16] = std::array::from_fn(|i| i as u8);
        let (mut relay_obf, tag, dc) = Obfuscated2::accept_client_header(&header, Some(&key));
        assert_eq!(tag, [0xef, 0xef, 0xef, 0xef]);
        assert_eq!(dc, 2);
        let mut body = sent[OBFUSCATED_HEADER_SIZE..].to_vec();
        relay_obf.decrypt_incoming(&mut body);
        assert_eq!(body[0] as usize, packet.len() / 4);
        assert_eq!(&body[1..], &packet);

        // The relay answers with an abridged frame on the same stream.
        let mut reply = vec![4u8];
        reply.extend_from_slice(b"pongpongpongpong");
        relay_obf.encrypt_outgoing(&mut reply);
        let data = serialize_frame(FrameType::Data, 1, &reply).expect("data frame");
        ws.write_all(&encode_masked_websocket_frame(0x02, &data, [5, 6, 7, 8]))
            .await
            .expect("send reply");
        let received = conn.recv().await.expect("recv reply");
        assert_eq!(received, b"pongpongpongpong");

        conn.close().await;
        unregister_active_web_proxy_bridge(&server);
        assert!(active_web_proxy_bridge(&proxy).is_none());
        server.shutdown();
    }
}
