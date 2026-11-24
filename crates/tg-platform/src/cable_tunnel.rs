//! caBLEv2 / CTAP 2.2 hybrid transport WebSocket tunnel client, Noise `KNpsk0`/`NKpsk0`
//! handshake driver, encrypted CTAP2 framing, and deterministic in-process test fakes.
//!
//! Port origin:
//! - `Telegram/SourceFiles/webauthn/cable_ceremony.cpp`
//! - `Telegram/SourceFiles/webauthn/cable_core.cpp`
//! - `Telegram/SourceFiles/core/websocket_client.cpp`
//! - CTAP 2.2 hybrid transport specification / Chromium `device/fido/cable/v2_handshake.cc`

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use p256::elliptic_curve::Generate;
use p256::{PublicKey as P256PublicKey, SecretKey as P256SecretKey};
use rustls::pki_types::ServerName;
use sha1::{Digest as Sha1Digest, Sha1};
use thiserror::Error;

use crate::webauthn::{
    CABLE_HANDSHAKE_RESPONSE_LEN, CABLE_PSK_LEN, CABLE_QR_SECRET_LEN, CABLE_TUNNEL_ID_LEN,
    CTAP_ERR_NO_CREDENTIALS, CTAP_ERR_OPERATION_DENIED, CTAP_ERR_USER_ACTION_TIMEOUT, CTAP_SUCCESS,
    CableCrypter, CableDerivedValueType, CableHandshakeInitiator, CableHandshakeResult,
    CableHandshakeType, CableMessageType, CableNoise, CablePostHandshakeMessage, CborValue,
    CtapGetAssertionResponse, CtapMakeCredentialResponse, LoginData, LoginResult, P256_X962_LEN,
    RegisterData, RegisterResult, WebAuthnErrorKind, build_ctap_get_assertion_request,
    build_ctap_make_credential_request, build_none_attestation_object, build_synthetic_auth_data,
    cable_derive, cable_tunnel_connect_path, cbor_decode, cbor_encode, p256_ecdh_shared_x,
    parse_ctap_get_assertion_response, parse_ctap_make_credential_response,
    parse_post_handshake_message, sha256_digest,
};

/// Default TLS port for caBLEv2 WebSocket tunnel servers (`kTunnelPort`).
pub const CABLE_TUNNEL_PORT: u16 = 443;

/// Required WebSocket subprotocol for caBLEv2 hybrid transport (`cable_ceremony.cpp:353`).
pub const CABLE_WS_SUBPROTOCOL: &str = "fido.cable";

/// Maximum WebSocket payload frame size accepted by the caBLE client (4 MiB, `websocket_client.cpp:24`).
pub const CABLE_WS_MAX_FRAME_PAYLOAD: usize = 4 * 1024 * 1024;

/// Maximum HTTP/1.1 Upgrade response header block size (8 KiB, `websocket_client.cpp:25`).
const CABLE_WS_MAX_UPGRADE_HEADER: usize = 8 * 1024;

/// RFC 6455 Section 4.2.2 WebSocket Accept GUID (`kAcceptGuid`).
const WS_ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const WS_OPCODE_CONTINUATION: u8 = 0x0;
const WS_OPCODE_BINARY: u8 = 0x2;
const WS_OPCODE_CLOSE: u8 = 0x8;
const WS_OPCODE_PING: u8 = 0x9;
const WS_OPCODE_PONG: u8 = 0xA;

// ============================================================================
// Errors & Endpoint Descriptor
// ============================================================================

/// Error returned by the caBLEv2 hybrid tunnel transport and ceremony state machine.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CableTunnelError {
    /// Invalid tunnel host, path, or URL parameters.
    #[error("invalid caBLE tunnel endpoint: {0}")]
    InvalidEndpoint(String),
    /// Underlying TCP, TLS, or stream I/O failure.
    #[error("caBLE tunnel transport error: {0}")]
    Transport(String),
    /// RFC 6455 HTTP Upgrade handshake failed or returned an invalid `Sec-WebSocket-Accept`.
    #[error("caBLE WebSocket upgrade failed: {0}")]
    WebSocketHandshake(String),
    /// Malformed or oversized RFC 6455 WebSocket frame.
    #[error("caBLE WebSocket protocol error: {0}")]
    WebSocketProtocol(String),
    /// Noise `KNpsk0` / `NKpsk0` handshake failed authentication or key exchange.
    #[error("caBLE Noise handshake failed")]
    NoiseHandshakeFailed,
    /// Post-handshake AES-256-GCM frame encryption failed.
    #[error("caBLE frame encryption failed")]
    EncryptionFailed,
    /// Post-handshake AES-256-GCM frame decryption or unpadding failed.
    #[error("caBLE frame decryption failed")]
    DecryptionFailed,
    /// Authenticator post-handshake CBOR capabilities message was invalid.
    #[error("invalid caBLE post-handshake message")]
    InvalidPostHandshake,
    /// Authenticator reported that it does not support CTAP over caBLE.
    #[error("authenticator does not support CTAP over caBLE")]
    CtapUnsupported,
    /// Authenticator closed the tunnel with a `Shutdown` control message.
    #[error("authenticator shut down the caBLE tunnel")]
    TunnelShutdown,
    /// Received an unexpected caBLE v2 message type byte while awaiting a CTAP reply.
    #[error("unexpected caBLE message type: 0x{0:02x}")]
    UnexpectedMessageType(u8),
    /// Authenticator returned a non-zero CTAP2 status byte.
    #[error("authenticator returned CTAP2 error 0x{code:02x}: {kind}")]
    CtapError {
        /// Raw CTAP2 status byte.
        code: u8,
        /// Mapped [`WebAuthnErrorKind`].
        kind: WebAuthnErrorKind,
    },
    /// CTAP2 response payload failed CBOR or authenticator-data validation.
    #[error("malformed CTAP2 response: {0}")]
    MalformedCtapResponse(String),
    /// WebSocket connection closed before the ceremony finished.
    #[error("caBLE tunnel connection closed unexpectedly")]
    Closed,
}

impl CableTunnelError {
    /// Converts a tunnel error into the corresponding [`WebAuthnErrorKind`].
    #[must_use]
    pub fn to_webauthn_error(&self) -> WebAuthnErrorKind {
        match self {
            Self::CtapError { kind, .. } => kind.clone(),
            other => WebAuthnErrorKind::Other(other.to_string()),
        }
    }
}

/// Target caBLEv2 WebSocket tunnel endpoint (`wss://<tunnel_domain>/cable/connect/<routing>/<tunnel>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CableTunnelEndpoint {
    /// Decoded tunnel server domain (e.g. `"cable.ua5v.com"`).
    pub tunnel_domain: String,
    /// TCP/TLS port (defaults to [`CABLE_TUNNEL_PORT`] = `443`).
    pub port: u16,
    /// WebSocket request target path (e.g. `"/cable/connect/<routing_hex>/<tunnel_hex>"`).
    pub path: String,
    /// WebSocket `Sec-WebSocket-Protocol` value (`"fido.cable"`).
    pub subprotocol: String,
}

impl CableTunnelEndpoint {
    /// Constructs a standard caBLEv2 tunnel endpoint from a decoded `tunnel_domain` and `path`.
    #[must_use]
    pub fn new(tunnel_domain: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            tunnel_domain: tunnel_domain.into(),
            port: CABLE_TUNNEL_PORT,
            path: path.into(),
            subprotocol: CABLE_WS_SUBPROTOCOL.to_owned(),
        }
    }

    /// Formats the canonical `wss://` URL for diagnostics and test assertions.
    #[must_use]
    pub fn wss_url(&self) -> String {
        if self.port == CABLE_TUNNEL_PORT {
            format!("wss://{}{}", self.tunnel_domain, self.path)
        } else {
            format!("wss://{}:{}{}", self.tunnel_domain, self.port, self.path)
        }
    }

    /// Validates that the domain and path are non-empty and free of CRLF injection characters.
    pub fn validate(&self) -> Result<(), CableTunnelError> {
        let domain = self.tunnel_domain.trim();
        if domain.is_empty()
            || domain.contains('/')
            || domain.contains('\r')
            || domain.contains('\n')
            || domain.contains(' ')
        {
            return Err(CableTunnelError::InvalidEndpoint(format!(
                "invalid tunnel domain '{}'",
                self.tunnel_domain
            )));
        }
        if !self.path.starts_with('/')
            || self.path.contains('\r')
            || self.path.contains('\n')
            || self.path.contains(' ')
        {
            return Err(CableTunnelError::InvalidEndpoint(format!(
                "invalid tunnel path '{}'",
                self.path
            )));
        }
        Ok(())
    }
}

// ============================================================================
// WebSocket Transport Traits & RFC 6455 Client Implementation
// ============================================================================

/// Binary WebSocket connection abstraction used by [`CableTunnelClient`].
pub trait CableWebSocket: Send {
    /// Sends one complete binary WebSocket message.
    fn send_binary(&mut self, data: &[u8]) -> Result<(), CableTunnelError>;

    /// Receives the next complete binary WebSocket message.
    fn recv_binary(&mut self) -> Result<Vec<u8>, CableTunnelError>;

    /// Sends an optional Close frame and closes the underlying transport.
    fn close(&mut self);
}

/// Factory trait opening a [`CableWebSocket`] to a caBLEv2 tunnel server.
///
/// Production code uses [`TlsCableTunnelConnector`]; tests inject [`FakeCableTunnelConnector`]
/// so no real tunnel server or network is ever contacted.
pub trait CableTunnelConnector: Send + Sync {
    /// Opens a WebSocket connection to `endpoint` within `timeout`.
    fn connect(
        &self,
        endpoint: &CableTunnelEndpoint,
        timeout: Duration,
    ) -> Result<Box<dyn CableWebSocket>, CableTunnelError>;
}

/// Computes the RFC 6455 `Sec-WebSocket-Accept` header value for a Base64 `Sec-WebSocket-Key`.
#[must_use]
pub fn compute_websocket_accept(key_b64: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key_b64.as_bytes());
    hasher.update(WS_ACCEPT_GUID.as_bytes());
    let digest = hasher.finalize();
    BASE64_STANDARD.encode(digest)
}

/// Minimal RFC 6455 binary WebSocket client over any synchronous byte stream
/// (port of `Telegram/SourceFiles/core/websocket_client.cpp`).
pub struct Rfc6455WebSocket<S: Read + Write + Send> {
    stream: S,
    read_buf: Vec<u8>,
    closed: bool,
}

impl<S: Read + Write + Send> Rfc6455WebSocket<S> {
    /// Performs the RFC 6455 HTTP/1.1 Upgrade handshake on `stream` using a random 16-byte key.
    pub fn client_handshake(
        stream: S,
        endpoint: &CableTunnelEndpoint,
    ) -> Result<Self, CableTunnelError> {
        let mut nonce = [0u8; 16];
        getrandom::getrandom(&mut nonce)
            .map_err(|e| CableTunnelError::Transport(format!("RNG failed: {e}")))?;
        Self::client_handshake_with_key(stream, endpoint, &nonce)
    }

    /// Performs the RFC 6455 HTTP/1.1 Upgrade handshake with a caller-supplied 16-byte key
    /// (for deterministic protocol unit tests).
    pub fn client_handshake_with_key(
        mut stream: S,
        endpoint: &CableTunnelEndpoint,
        key_bytes: &[u8; 16],
    ) -> Result<Self, CableTunnelError> {
        endpoint.validate()?;
        let key_b64 = BASE64_STANDARD.encode(key_bytes);
        let host_header = if endpoint.port == CABLE_TUNNEL_PORT {
            endpoint.tunnel_domain.clone()
        } else {
            format!("{}:{}", endpoint.tunnel_domain, endpoint.port)
        };
        let mut request = format!(
            "GET {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\n\
             Sec-WebSocket-Version: 13\r\n",
            endpoint.path, host_header, key_b64
        );
        if !endpoint.subprotocol.is_empty() {
            request.push_str(&format!(
                "Sec-WebSocket-Protocol: {}\r\n",
                endpoint.subprotocol
            ));
        }
        request.push_str("\r\n");

        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| CableTunnelError::Transport(format!("upgrade write failed: {e}")))?;

        let mut buf = Vec::with_capacity(512);
        let mut chunk = [0u8; 512];
        let header_end = loop {
            if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
                break pos;
            }
            if buf.len() >= CABLE_WS_MAX_UPGRADE_HEADER {
                return Err(CableTunnelError::WebSocketHandshake(
                    "HTTP upgrade response exceeded 8 KiB".to_owned(),
                ));
            }
            let n = stream
                .read(&mut chunk)
                .map_err(|e| CableTunnelError::Transport(format!("upgrade read failed: {e}")))?;
            if n == 0 {
                return Err(CableTunnelError::WebSocketHandshake(
                    "peer closed connection before completing HTTP 101 upgrade".to_owned(),
                ));
            }
            buf.extend_from_slice(&chunk[..n]);
        };

        let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| {
            CableTunnelError::WebSocketHandshake("non-UTF-8 HTTP upgrade headers".to_owned())
        })?;
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        if !status_line.contains(" 101") {
            return Err(CableTunnelError::WebSocketHandshake(format!(
                "expected HTTP 101 Switching Protocols, got '{status_line}'"
            )));
        }
        let expected_accept = compute_websocket_accept(&key_b64);
        let mut accept_ok = false;
        for line in lines {
            if let Some((name, value)) = line.split_once(':')
                && name.trim().eq_ignore_ascii_case("sec-websocket-accept")
            {
                accept_ok = value.trim() == expected_accept;
                break;
            }
        }
        if !accept_ok {
            return Err(CableTunnelError::WebSocketHandshake(
                "missing or mismatched Sec-WebSocket-Accept header".to_owned(),
            ));
        }

        let leftover = buf[header_end + 4..].to_vec();
        Ok(Self {
            stream,
            read_buf: leftover,
            closed: false,
        })
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), CableTunnelError> {
        if self.closed {
            return Err(CableTunnelError::Closed);
        }
        let mut mask = [0u8; 4];
        getrandom::getrandom(&mut mask)
            .map_err(|e| CableTunnelError::Transport(format!("RNG failed: {e}")))?;
        self.write_frame_with_mask(opcode, payload, mask)
    }

    fn write_frame_with_mask(
        &mut self,
        opcode: u8,
        payload: &[u8],
        mask: [u8; 4],
    ) -> Result<(), CableTunnelError> {
        let len = payload.len();
        if len > CABLE_WS_MAX_FRAME_PAYLOAD {
            return Err(CableTunnelError::WebSocketProtocol(format!(
                "outbound payload {len} exceeds maximum {CABLE_WS_MAX_FRAME_PAYLOAD}"
            )));
        }
        let mut frame = Vec::with_capacity(len + 14);
        frame.push(0x80 | (opcode & 0x0F));
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if len <= 0xFFFF {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (idx, &byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[idx % 4]);
        }
        self.stream
            .write_all(&frame)
            .and_then(|()| self.stream.flush())
            .map_err(|e| CableTunnelError::Transport(format!("frame write failed: {e}")))
    }

    fn ensure_buffered(&mut self, needed: usize) -> Result<(), CableTunnelError> {
        let mut tmp = [0u8; 4096];
        while self.read_buf.len() < needed {
            let n = self
                .stream
                .read(&mut tmp)
                .map_err(|e| CableTunnelError::Transport(format!("frame read failed: {e}")))?;
            if n == 0 {
                return Err(CableTunnelError::Closed);
            }
            self.read_buf.extend_from_slice(&tmp[..n]);
        }
        Ok(())
    }

    fn read_raw_frame(&mut self) -> Result<(bool, u8, Vec<u8>), CableTunnelError> {
        self.ensure_buffered(2)?;
        let first = self.read_buf[0];
        let second = self.read_buf[1];
        let fin = (first & 0x80) != 0;
        let opcode = first & 0x0F;
        let masked = (second & 0x80) != 0;
        let len_code = second & 0x7F;

        let (payload_len, mut header_len) = match len_code {
            0..=125 => (usize::from(len_code), 2usize),
            126 => {
                self.ensure_buffered(4)?;
                let len = u16::from_be_bytes([self.read_buf[2], self.read_buf[3]]);
                (usize::from(len), 4usize)
            }
            127 => {
                self.ensure_buffered(10)?;
                let len_u64 = u64::from_be_bytes([
                    self.read_buf[2],
                    self.read_buf[3],
                    self.read_buf[4],
                    self.read_buf[5],
                    self.read_buf[6],
                    self.read_buf[7],
                    self.read_buf[8],
                    self.read_buf[9],
                ]);
                let len = usize::try_from(len_u64).map_err(|_| {
                    CableTunnelError::WebSocketProtocol("frame length overflow".to_owned())
                })?;
                (len, 10usize)
            }
            _ => unreachable!(),
        };

        if payload_len > CABLE_WS_MAX_FRAME_PAYLOAD {
            return Err(CableTunnelError::WebSocketProtocol(format!(
                "inbound payload {payload_len} exceeds maximum {CABLE_WS_MAX_FRAME_PAYLOAD}"
            )));
        }

        let mask_offset = header_len;
        if masked {
            header_len += 4;
        }
        let total = header_len.checked_add(payload_len).ok_or_else(|| {
            CableTunnelError::WebSocketProtocol("frame total size overflow".to_owned())
        })?;
        self.ensure_buffered(total)?;

        let mut payload = self.read_buf[header_len..total].to_vec();
        if masked {
            let mask = [
                self.read_buf[mask_offset],
                self.read_buf[mask_offset + 1],
                self.read_buf[mask_offset + 2],
                self.read_buf[mask_offset + 3],
            ];
            for (idx, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[idx % 4];
            }
        }
        self.read_buf.drain(..total);
        Ok((fin, opcode, payload))
    }
}

impl<S: Read + Write + Send> CableWebSocket for Rfc6455WebSocket<S> {
    fn send_binary(&mut self, data: &[u8]) -> Result<(), CableTunnelError> {
        self.write_frame(WS_OPCODE_BINARY, data)
    }

    fn recv_binary(&mut self) -> Result<Vec<u8>, CableTunnelError> {
        if self.closed {
            return Err(CableTunnelError::Closed);
        }
        let mut assembled: Option<Vec<u8>> = None;
        loop {
            let (fin, opcode, payload) = self.read_raw_frame()?;
            match opcode {
                WS_OPCODE_PING => {
                    self.write_frame(WS_OPCODE_PONG, &payload)?;
                }
                WS_OPCODE_PONG => {}
                WS_OPCODE_CLOSE => {
                    self.closed = true;
                    return Err(CableTunnelError::Closed);
                }
                WS_OPCODE_BINARY => {
                    if assembled.is_some() {
                        return Err(CableTunnelError::WebSocketProtocol(
                            "interleaved binary frame during fragmented message".to_owned(),
                        ));
                    }
                    if fin {
                        return Ok(payload);
                    }
                    assembled = Some(payload);
                }
                WS_OPCODE_CONTINUATION => {
                    let Some(ref mut buf) = assembled else {
                        return Err(CableTunnelError::WebSocketProtocol(
                            "unexpected continuation frame without initial frame".to_owned(),
                        ));
                    };
                    if buf.len().saturating_add(payload.len()) > CABLE_WS_MAX_FRAME_PAYLOAD {
                        return Err(CableTunnelError::WebSocketProtocol(
                            "fragmented message exceeded 4 MiB limit".to_owned(),
                        ));
                    }
                    buf.extend_from_slice(&payload);
                    if fin {
                        return Ok(assembled.take().unwrap_or_default());
                    }
                }
                other => {
                    return Err(CableTunnelError::WebSocketProtocol(format!(
                        "unsupported WebSocket opcode 0x{other:x}"
                    )));
                }
            }
        }
    }

    fn close(&mut self) {
        if !self.closed {
            let _ = self.write_frame(WS_OPCODE_CLOSE, &[]);
            self.closed = true;
        }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Production TLS (`rustls` + `webpki-roots`) RFC 6455 WebSocket connector for caBLEv2 tunnel servers.
#[derive(Debug, Default, Clone, Copy)]
pub struct TlsCableTunnelConnector;

impl TlsCableTunnelConnector {
    /// Creates a new [`TlsCableTunnelConnector`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CableTunnelConnector for TlsCableTunnelConnector {
    fn connect(
        &self,
        endpoint: &CableTunnelEndpoint,
        timeout: Duration,
    ) -> Result<Box<dyn CableWebSocket>, CableTunnelError> {
        endpoint.validate()?;

        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| CableTunnelError::Transport(format!("TLS config error: {e}")))?
        .with_root_certificates(root_store)
        .with_no_client_auth();

        let server_name = ServerName::try_from(endpoint.tunnel_domain.clone()).map_err(|_| {
            CableTunnelError::InvalidEndpoint(format!(
                "invalid TLS SNI domain '{}'",
                endpoint.tunnel_domain
            ))
        })?;

        let addrs: Vec<_> = (endpoint.tunnel_domain.as_str(), endpoint.port)
            .to_socket_addrs()
            .map_err(|e| CableTunnelError::Transport(format!("DNS resolution failed: {e}")))?
            .collect();
        if addrs.is_empty() {
            return Err(CableTunnelError::Transport(format!(
                "no addresses resolved for {}:{}",
                endpoint.tunnel_domain, endpoint.port
            )));
        }

        let mut last_err = None;
        let mut connected_tcp = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(tcp) => {
                    connected_tcp = Some(tcp);
                    break;
                }
                Err(err) => last_err = Some(err),
            }
        }
        let tcp = connected_tcp.ok_or_else(|| {
            CableTunnelError::Transport(format!(
                "TCP connect to {}:{} failed: {}",
                endpoint.tunnel_domain,
                endpoint.port,
                last_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown error".to_owned())
            ))
        })?;
        let _ = tcp.set_read_timeout(Some(timeout));
        let _ = tcp.set_write_timeout(Some(timeout));

        let tls_conn = rustls::ClientConnection::new(Arc::new(tls_config), server_name)
            .map_err(|e| CableTunnelError::Transport(format!("TLS handshake init failed: {e}")))?;
        let tls_stream = rustls::StreamOwned::new(tls_conn, tcp);
        let ws = Rfc6455WebSocket::client_handshake(tls_stream, endpoint)?;
        Ok(Box::new(ws))
    }
}

// ============================================================================
// Noise Responder (`KNpsk0` / `NKpsk0`) & Encrypted CTAP2 Framing
// ============================================================================

enum CableHandshakeResponderMode {
    KNpsk0 {
        peer_identity_x962: [u8; P256_X962_LEN],
    },
    NKpsk0 {
        local_identity: P256SecretKey,
    },
}

/// Authenticator/phone side of the caBLEv2 `KNpsk0` / `NKpsk0` Noise handshake.
pub struct CableHandshakeResponder {
    psk: [u8; CABLE_PSK_LEN],
    mode: CableHandshakeResponderMode,
    noise: CableNoise,
}

impl CableHandshakeResponder {
    /// Creates a `KNpsk0` responder (QR flow) given `psk` and the desktop initiator's 65-byte
    /// uncompressed SEC1 P-256 identity key.
    #[must_use]
    pub fn new_knpsk0(psk: [u8; CABLE_PSK_LEN], peer_identity_x962: [u8; P256_X962_LEN]) -> Self {
        Self {
            psk,
            mode: CableHandshakeResponderMode::KNpsk0 { peer_identity_x962 },
            noise: CableNoise::new(CableHandshakeType::KNpsk0),
        }
    }

    /// Creates an `NKpsk0` responder (linked/paired device flow) given `psk` and the
    /// authenticator's own static P-256 identity private key.
    #[must_use]
    pub fn new_nkpsk0(psk: [u8; CABLE_PSK_LEN], local_identity: P256SecretKey) -> Self {
        Self {
            psk,
            mode: CableHandshakeResponderMode::NKpsk0 { local_identity },
            noise: CableNoise::new(CableHandshakeType::NKpsk0),
        }
    }

    /// Processes the initiator's 81-byte Noise message with a freshly generated ephemeral key.
    #[must_use]
    pub fn process_initial_message(
        &mut self,
        initial_msg: &[u8],
    ) -> Option<(Vec<u8>, CableHandshakeResult)> {
        let ephemeral = P256SecretKey::generate();
        self.process_initial_message_with_ephemeral(initial_msg, ephemeral)
    }

    /// Processes the initiator's 81-byte Noise message with an explicit responder ephemeral key
    /// (for deterministic known-answer and transcript tests).
    #[must_use]
    pub fn process_initial_message_with_ephemeral(
        &mut self,
        initial_msg: &[u8],
        ephemeral: P256SecretKey,
    ) -> Option<(Vec<u8>, CableHandshakeResult)> {
        if initial_msg.len() != CABLE_HANDSHAKE_RESPONSE_LEN {
            return None;
        }
        let init_eph_pub = &initial_msg[..P256_X962_LEN];
        let init_ct = &initial_msg[P256_X962_LEN..];

        match &self.mode {
            CableHandshakeResponderMode::KNpsk0 { peer_identity_x962 } => {
                self.noise = CableNoise::new(CableHandshakeType::KNpsk0);
                self.noise.mix_hash(&[1u8]);
                self.noise.mix_hash(peer_identity_x962);
                self.noise.mix_key_and_hash(&self.psk);
                self.noise.mix_hash(init_eph_pub);
                self.noise.mix_key(init_eph_pub);
            }
            CableHandshakeResponderMode::NKpsk0 { local_identity } => {
                self.noise = CableNoise::new(CableHandshakeType::NKpsk0);
                self.noise.mix_hash(&[0u8]);
                let local_pub = local_identity.public_key().to_sec1_bytes();
                self.noise.mix_hash(&local_pub);
                self.noise.mix_key_and_hash(&self.psk);
                self.noise.mix_hash(init_eph_pub);
                self.noise.mix_key(init_eph_pub);
                let shared_es = p256_ecdh_shared_x(local_identity, init_eph_pub)?;
                self.noise.mix_key(&shared_es);
            }
        }

        let init_pt = self.noise.decrypt_and_hash(init_ct)?;
        if !init_pt.is_empty() {
            return None;
        }

        let resp_eph_pub = ephemeral.public_key().to_sec1_bytes();
        let shared_ee = p256_ecdh_shared_x(&ephemeral, init_eph_pub)?;
        self.noise.mix_hash(&resp_eph_pub);
        self.noise.mix_key(&resp_eph_pub);
        self.noise.mix_key(&shared_ee);

        if let CableHandshakeResponderMode::KNpsk0 { peer_identity_x962 } = &self.mode {
            let shared_se = p256_ecdh_shared_x(&ephemeral, peer_identity_x962)?;
            self.noise.mix_key(&shared_se);
        }

        let resp_ct = self.noise.encrypt_and_hash(&[])?;
        let mut resp_msg = Vec::with_capacity(resp_eph_pub.len() + resp_ct.len());
        resp_msg.extend_from_slice(&resp_eph_pub);
        resp_msg.extend_from_slice(&resp_ct);

        let (read_key, write_key) = self.noise.traffic_keys();
        Some((
            resp_msg,
            CableHandshakeResult {
                crypter: CableCrypter::new(read_key, write_key),
                handshake_hash: self.noise.handshake_hash(),
            },
        ))
    }
}

/// Decoded post-handshake caBLEv2 tunnel frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CableTunnelFrame {
    /// Authenticator closed the tunnel (`CableMessageType::Shutdown`).
    Shutdown,
    /// CTAP2 command or response bytes (`CableMessageType::Ctap`).
    Ctap(Vec<u8>),
    /// Authenticator linking / metadata update (`CableMessageType::Update`).
    Update(Vec<u8>),
    /// JSON control payload (`CableMessageType::Json`).
    Json(Vec<u8>),
}

/// Frames raw CTAP2 bytes for encryption according to the negotiated `protocol_revision`.
///
/// Protocol revision `>= 1` prepends `CableMessageType::Ctap` (`0x01`); legacy revision `0`
/// sends raw CTAP2 bytes directly (`SendCtapRequest` in `cable_ceremony.cpp:187-194`).
#[must_use]
pub fn encode_tunnel_ctap_frame(protocol_revision: u8, ctap_payload: &[u8]) -> Vec<u8> {
    if protocol_revision >= 1 {
        let mut out = Vec::with_capacity(1 + ctap_payload.len());
        out.push(CableMessageType::Ctap as u8);
        out.extend_from_slice(ctap_payload);
        out
    } else {
        ctap_payload.to_vec()
    }
}

/// Decodes a decrypted post-handshake tunnel plaintext into a [`CableTunnelFrame`].
pub fn decode_tunnel_frame(
    protocol_revision: u8,
    plaintext: &[u8],
) -> Result<CableTunnelFrame, CableTunnelError> {
    if protocol_revision == 0 {
        return Ok(CableTunnelFrame::Ctap(plaintext.to_vec()));
    }
    let (&tag, rest) = plaintext
        .split_first()
        .ok_or(CableTunnelError::MalformedCtapResponse(
            "empty caBLE v2 tunnel frame".to_owned(),
        ))?;
    match tag {
        x if x == CableMessageType::Shutdown as u8 => Ok(CableTunnelFrame::Shutdown),
        x if x == CableMessageType::Ctap as u8 => Ok(CableTunnelFrame::Ctap(rest.to_vec())),
        x if x == CableMessageType::Update as u8 => Ok(CableTunnelFrame::Update(rest.to_vec())),
        x if x == CableMessageType::Json as u8 => Ok(CableTunnelFrame::Json(rest.to_vec())),
        other => Err(CableTunnelError::UnexpectedMessageType(other)),
    }
}

/// Maps a non-zero CTAP2 status byte to a categorized [`WebAuthnErrorKind`].
#[must_use]
pub fn map_ctap_status_error(status: u8) -> WebAuthnErrorKind {
    match status {
        CTAP_ERR_OPERATION_DENIED => WebAuthnErrorKind::Cancelled,
        CTAP_ERR_NO_CREDENTIALS => WebAuthnErrorKind::NoCredentials,
        CTAP_ERR_USER_ACTION_TIMEOUT => {
            WebAuthnErrorKind::Other("authenticator timed out waiting for user action".to_owned())
        }
        other => WebAuthnErrorKind::Other(format!("CTAP2 authenticator error 0x{other:02x}")),
    }
}

// ============================================================================
// High-Level caBLEv2 Tunnel Client
// ============================================================================

/// Result of a completed caBLEv2 hybrid `authenticatorMakeCredential` ceremony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CableRegisterOutcome {
    /// Ready-to-submit WebAuthn [`RegisterResult`].
    pub result: RegisterResult,
    /// Final Noise transcript hash `h`.
    pub handshake_hash: [u8; 32],
    /// Negotiated post-handshake authenticator capabilities.
    pub post_handshake: CablePostHandshakeMessage,
    /// Any `CableMessageType::Update` payloads received before the CTAP2 reply.
    pub update_payloads: Vec<Vec<u8>>,
}

/// Result of a completed caBLEv2 hybrid `authenticatorGetAssertion` ceremony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CableLoginOutcome {
    /// Ready-to-submit WebAuthn [`LoginResult`].
    pub result: LoginResult,
    /// Final Noise transcript hash `h`.
    pub handshake_hash: [u8; 32],
    /// Negotiated post-handshake authenticator capabilities.
    pub post_handshake: CablePostHandshakeMessage,
    /// Any `CableMessageType::Update` payloads received before the CTAP2 reply.
    pub update_payloads: Vec<Vec<u8>>,
}

/// End-to-end caBLEv2 hybrid transport client driving WebSocket connection, Noise handshake,
/// post-handshake negotiation, and encrypted CTAP2 `MakeCredential` / `GetAssertion` exchange.
pub struct CableTunnelClient {
    connector: Arc<dyn CableTunnelConnector>,
}

impl CableTunnelClient {
    /// Creates a new [`CableTunnelClient`] backed by `connector`.
    #[must_use]
    pub fn new(connector: Arc<dyn CableTunnelConnector>) -> Self {
        Self { connector }
    }

    /// Creates a production [`CableTunnelClient`] using [`TlsCableTunnelConnector`].
    #[must_use]
    pub fn tls() -> Self {
        Self::new(Arc::new(TlsCableTunnelConnector::new()))
    }

    /// Executes a complete caBLEv2 hybrid `MakeCredential` ceremony over `endpoint`.
    pub fn run_register(
        &self,
        endpoint: &CableTunnelEndpoint,
        handshake: CableHandshakeInitiator,
        client_data_json: String,
        client_data_hash: [u8; 32],
        data: &RegisterData,
        timeout: Duration,
    ) -> Result<CableRegisterOutcome, CableTunnelError> {
        self.run_register_inner(
            endpoint,
            handshake,
            None,
            client_data_json,
            client_data_hash,
            data,
            timeout,
        )
    }

    /// Executes a complete caBLEv2 hybrid `MakeCredential` ceremony with a fixed initiator
    /// ephemeral key (for deterministic tests).
    #[allow(clippy::too_many_arguments)]
    pub fn run_register_with_ephemeral(
        &self,
        endpoint: &CableTunnelEndpoint,
        handshake: CableHandshakeInitiator,
        initiator_ephemeral: P256SecretKey,
        client_data_json: String,
        client_data_hash: [u8; 32],
        data: &RegisterData,
        timeout: Duration,
    ) -> Result<CableRegisterOutcome, CableTunnelError> {
        self.run_register_inner(
            endpoint,
            handshake,
            Some(initiator_ephemeral),
            client_data_json,
            client_data_hash,
            data,
            timeout,
        )
    }

    /// Executes a complete caBLEv2 hybrid `GetAssertion` ceremony over `endpoint`.
    pub fn run_login(
        &self,
        endpoint: &CableTunnelEndpoint,
        handshake: CableHandshakeInitiator,
        client_data_json: String,
        client_data_hash: [u8; 32],
        data: &LoginData,
        timeout: Duration,
    ) -> Result<CableLoginOutcome, CableTunnelError> {
        self.run_login_inner(
            endpoint,
            handshake,
            None,
            client_data_json,
            client_data_hash,
            data,
            timeout,
        )
    }

    /// Executes a complete caBLEv2 hybrid `GetAssertion` ceremony with a fixed initiator
    /// ephemeral key (for deterministic tests).
    #[allow(clippy::too_many_arguments)]
    pub fn run_login_with_ephemeral(
        &self,
        endpoint: &CableTunnelEndpoint,
        handshake: CableHandshakeInitiator,
        initiator_ephemeral: P256SecretKey,
        client_data_json: String,
        client_data_hash: [u8; 32],
        data: &LoginData,
        timeout: Duration,
    ) -> Result<CableLoginOutcome, CableTunnelError> {
        self.run_login_inner(
            endpoint,
            handshake,
            Some(initiator_ephemeral),
            client_data_json,
            client_data_hash,
            data,
            timeout,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_register_inner(
        &self,
        endpoint: &CableTunnelEndpoint,
        handshake: CableHandshakeInitiator,
        initiator_ephemeral: Option<P256SecretKey>,
        client_data_json: String,
        client_data_hash: [u8; 32],
        data: &RegisterData,
        timeout: Duration,
    ) -> Result<CableRegisterOutcome, CableTunnelError> {
        let algorithms: Vec<i32> = data.pub_key_cred_params.iter().map(|p| p.alg).collect();
        let ctap_request = build_ctap_make_credential_request(
            &client_data_hash,
            &data.rp.id,
            &data.rp.name,
            &data.user.id,
            &data.user.name,
            &data.user.display_name,
            &algorithms,
        );
        let CtapExchangeResult {
            ctap_reply,
            handshake_hash,
            post_handshake,
            update_payloads,
        } = self.exchange_ctap(
            endpoint,
            handshake,
            initiator_ephemeral,
            &ctap_request,
            timeout,
        )?;

        let status = *ctap_reply
            .first()
            .ok_or_else(|| CableTunnelError::MalformedCtapResponse("empty CTAP reply".into()))?;
        if status != CTAP_SUCCESS {
            return Err(CableTunnelError::CtapError {
                code: status,
                kind: map_ctap_status_error(status),
            });
        }
        let parsed = parse_ctap_make_credential_response(&ctap_reply).ok_or_else(|| {
            CableTunnelError::MalformedCtapResponse(
                "invalid authenticatorMakeCredential CBOR response".to_owned(),
            )
        })?;
        let attestation_object = build_none_attestation_object(&parsed.auth_data);
        Ok(CableRegisterOutcome {
            result: RegisterResult {
                credential_id: parsed.credential_id,
                attestation_object,
                client_data_json: client_data_json.into_bytes(),
            },
            handshake_hash,
            post_handshake,
            update_payloads,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn run_login_inner(
        &self,
        endpoint: &CableTunnelEndpoint,
        handshake: CableHandshakeInitiator,
        initiator_ephemeral: Option<P256SecretKey>,
        client_data_json: String,
        client_data_hash: [u8; 32],
        data: &LoginData,
        timeout: Duration,
    ) -> Result<CableLoginOutcome, CableTunnelError> {
        let allow_ids: Vec<Vec<u8>> = data
            .allow_credentials
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let ctap_request =
            build_ctap_get_assertion_request(&data.rp_id, &client_data_hash, &allow_ids);
        let CtapExchangeResult {
            ctap_reply,
            handshake_hash,
            post_handshake,
            update_payloads,
        } = self.exchange_ctap(
            endpoint,
            handshake,
            initiator_ephemeral,
            &ctap_request,
            timeout,
        )?;

        let status = *ctap_reply
            .first()
            .ok_or_else(|| CableTunnelError::MalformedCtapResponse("empty CTAP reply".into()))?;
        if status != CTAP_SUCCESS {
            return Err(CableTunnelError::CtapError {
                code: status,
                kind: map_ctap_status_error(status),
            });
        }
        let parsed = parse_ctap_get_assertion_response(&ctap_reply).ok_or_else(|| {
            CableTunnelError::MalformedCtapResponse(
                "invalid authenticatorGetAssertion CBOR response".to_owned(),
            )
        })?;
        let credential_id = if parsed.credential_id.is_empty() && allow_ids.len() == 1 {
            allow_ids[0].clone()
        } else {
            parsed.credential_id
        };
        Ok(CableLoginOutcome {
            result: LoginResult {
                client_data_json: client_data_json.into_bytes(),
                credential_id,
                authenticator_data: parsed.auth_data,
                signature: parsed.signature,
                user_handle: parsed.user_handle,
            },
            handshake_hash,
            post_handshake,
            update_payloads,
        })
    }

    fn exchange_ctap(
        &self,
        endpoint: &CableTunnelEndpoint,
        mut handshake: CableHandshakeInitiator,
        initiator_ephemeral: Option<P256SecretKey>,
        ctap_request: &[u8],
        timeout: Duration,
    ) -> Result<CtapExchangeResult, CableTunnelError> {
        let mut ws = self.connector.connect(endpoint, timeout)?;

        // 1. Noise KNpsk0 / NKpsk0 handshake.
        let initial_msg = match initiator_ephemeral {
            Some(eph) => handshake.build_initial_message_with_ephemeral(eph),
            None => handshake.build_initial_message(),
        }
        .ok_or(CableTunnelError::NoiseHandshakeFailed)?;
        ws.send_binary(&initial_msg)?;

        let resp_msg = ws.recv_binary()?;
        let CableHandshakeResult {
            mut crypter,
            handshake_hash,
        } = handshake
            .process_response(&resp_msg)
            .ok_or(CableTunnelError::NoiseHandshakeFailed)?;

        // 2. Post-handshake capabilities message from phone.
        let ph_encrypted = ws.recv_binary()?;
        let ph_plaintext = crypter
            .decrypt(&ph_encrypted)
            .ok_or(CableTunnelError::DecryptionFailed)?;
        let post_handshake = parse_post_handshake_message(&ph_plaintext)
            .ok_or(CableTunnelError::InvalidPostHandshake)?;
        if !post_handshake.supports_ctap {
            ws.close();
            return Err(CableTunnelError::CtapUnsupported);
        }

        // 3. Encrypt and send framed CTAP2 request.
        let framed_req = encode_tunnel_ctap_frame(post_handshake.protocol_revision, ctap_request);
        let encrypted_req = crypter
            .encrypt(&framed_req)
            .ok_or(CableTunnelError::EncryptionFailed)?;
        ws.send_binary(&encrypted_req)?;

        // 4. Receive decrypted frames until we get the CTAP2 reply (ignoring Update frames).
        let mut update_payloads = Vec::new();
        let ctap_reply = loop {
            let encrypted_reply = ws.recv_binary()?;
            let plaintext = crypter
                .decrypt(&encrypted_reply)
                .ok_or(CableTunnelError::DecryptionFailed)?;
            match decode_tunnel_frame(post_handshake.protocol_revision, &plaintext)? {
                CableTunnelFrame::Update(payload) => {
                    update_payloads.push(payload);
                }
                CableTunnelFrame::Ctap(payload) => break payload,
                CableTunnelFrame::Shutdown => {
                    ws.close();
                    return Err(CableTunnelError::TunnelShutdown);
                }
                CableTunnelFrame::Json(_) => {
                    ws.close();
                    return Err(CableTunnelError::UnexpectedMessageType(
                        CableMessageType::Json as u8,
                    ));
                }
            }
        };

        ws.close();
        Ok(CtapExchangeResult {
            ctap_reply,
            handshake_hash,
            post_handshake,
            update_payloads,
        })
    }
}

struct CtapExchangeResult {
    ctap_reply: Vec<u8>,
    handshake_hash: [u8; 32],
    post_handshake: CablePostHandshakeMessage,
    update_payloads: Vec<Vec<u8>>,
}

// ============================================================================
// Authenticator Trait & Deterministic In-Process Fake Phone / Tunnel
// ============================================================================

/// Decoded CTAP2 `authenticatorMakeCredential` request received by a [`CablePhoneAuthenticator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCtapMakeCredentialRequest {
    /// 32-byte `SHA-256(clientDataJSON)`.
    pub client_data_hash: [u8; 32],
    /// Relying party ID.
    pub rp_id: String,
    /// Relying party display name.
    pub rp_name: String,
    /// User handle bytes.
    pub user_id: Vec<u8>,
    /// User account name.
    pub user_name: String,
    /// User display name.
    pub user_display_name: String,
    /// Requested COSE algorithm identifiers.
    pub algorithms: Vec<i32>,
}

/// Decoded CTAP2 `authenticatorGetAssertion` request received by a [`CablePhoneAuthenticator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCtapGetAssertionRequest {
    /// Relying party ID.
    pub rp_id: String,
    /// 32-byte `SHA-256(clientDataJSON)`.
    pub client_data_hash: [u8; 32],
    /// Optional allow-list credential IDs.
    pub allow_credential_ids: Vec<Vec<u8>>,
}

/// Parses a raw CTAP2 `authenticatorMakeCredential` (`0x01 || CBOR`) command.
#[must_use]
pub fn parse_ctap_make_credential_request(raw: &[u8]) -> Option<ParsedCtapMakeCredentialRequest> {
    if raw.first().copied() != Some(0x01) {
        return None;
    }
    let map = cbor_decode(&raw[1..])?;
    let hash_slice = map.find_int(1)?.as_bytes()?;
    let client_data_hash: [u8; 32] = hash_slice.try_into().ok()?;
    let rp = map.find_int(2)?;
    let rp_id = rp.find_text("id")?.as_text()?.to_owned();
    let rp_name = rp
        .find_text("name")
        .and_then(CborValue::as_text)
        .unwrap_or_default()
        .to_owned();
    let user = map.find_int(3)?;
    let user_id = user.find_text("id")?.as_bytes()?.to_vec();
    let user_name = user
        .find_text("name")
        .and_then(CborValue::as_text)
        .unwrap_or_default()
        .to_owned();
    let user_display_name = user
        .find_text("displayName")
        .and_then(CborValue::as_text)
        .unwrap_or_default()
        .to_owned();
    let mut algorithms = Vec::new();
    if let Some(params) = map.find_int(4).and_then(CborValue::as_array) {
        for item in params {
            if let Some(alg) = item.find_text("alg").and_then(CborValue::as_int)
                && let Ok(alg_i32) = i32::try_from(alg)
            {
                algorithms.push(alg_i32);
            }
        }
    }
    Some(ParsedCtapMakeCredentialRequest {
        client_data_hash,
        rp_id,
        rp_name,
        user_id,
        user_name,
        user_display_name,
        algorithms,
    })
}

/// Parses a raw CTAP2 `authenticatorGetAssertion` (`0x02 || CBOR`) command.
#[must_use]
pub fn parse_ctap_get_assertion_request(raw: &[u8]) -> Option<ParsedCtapGetAssertionRequest> {
    if raw.first().copied() != Some(0x02) {
        return None;
    }
    let map = cbor_decode(&raw[1..])?;
    let rp_id = map.find_int(1)?.as_text()?.to_owned();
    let hash_slice = map.find_int(2)?.as_bytes()?;
    let client_data_hash: [u8; 32] = hash_slice.try_into().ok()?;
    let mut allow_credential_ids = Vec::new();
    if let Some(allow) = map.find_int(3).and_then(CborValue::as_array) {
        for entry in allow {
            if let Some(id) = entry.find_text("id").and_then(CborValue::as_bytes) {
                allow_credential_ids.push(id.to_vec());
            }
        }
    }
    Some(ParsedCtapGetAssertionRequest {
        rp_id,
        client_data_hash,
        allow_credential_ids,
    })
}

/// Phone / authenticator abstraction behind the caBLEv2 tunnel for deterministic tests.
pub trait CablePhoneAuthenticator: Send + Sync {
    /// Raw CTAP2 `getInfo` CBOR bytes sent inside the post-handshake message.
    fn get_info_cbor(&self) -> Vec<u8> {
        vec![0xA1, 0x01, 0x80]
    }

    /// Protocol revision (`1` = unpadded CBOR + message-type framing; `0` = legacy padded).
    fn protocol_revision(&self) -> u8 {
        1
    }

    /// Whether the phone advertises `"ctap"` support in its post-handshake message.
    fn supports_ctap(&self) -> bool {
        true
    }

    /// Optional `CableMessageType::Update` payloads emitted before the CTAP2 reply (rev >= 1).
    fn pre_reply_updates(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }

    /// If `true`, emits a `CableMessageType::Shutdown` instead of a CTAP2 reply.
    fn should_shutdown_instead_of_reply(&self) -> bool {
        false
    }

    /// Handles a decrypted CTAP2 `authenticatorMakeCredential` request, returning either a
    /// [`CtapMakeCredentialResponse`] or a non-zero CTAP2 error status byte.
    fn handle_make_credential(
        &self,
        req: &ParsedCtapMakeCredentialRequest,
    ) -> Result<CtapMakeCredentialResponse, u8>;

    /// Handles a decrypted CTAP2 `authenticatorGetAssertion` request, returning either a
    /// [`CtapGetAssertionResponse`] or a non-zero CTAP2 error status byte.
    fn handle_get_assertion(
        &self,
        req: &ParsedCtapGetAssertionRequest,
    ) -> Result<CtapGetAssertionResponse, u8>;
}

#[derive(Debug)]
struct FakeCablePhoneState {
    credential_id: Vec<u8>,
    user_handle: Vec<u8>,
    omit_credential_id_on_single_allow: bool,
    protocol_revision: u8,
    supports_ctap: bool,
    pre_reply_updates: Vec<Vec<u8>>,
    shutdown_before_reply: bool,
    ctap_error: Option<u8>,
    recorded_make_credentials: Vec<ParsedCtapMakeCredentialRequest>,
    recorded_get_assertions: Vec<ParsedCtapGetAssertionRequest>,
}

/// Deterministic in-process fake phone implementing [`CablePhoneAuthenticator`].
#[derive(Debug, Clone)]
pub struct FakeCablePhone {
    inner: Arc<Mutex<FakeCablePhoneState>>,
}

impl Default for FakeCablePhone {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCablePhone {
    /// Creates a new [`FakeCablePhone`] with deterministic synthetic credentials.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeCablePhoneState {
                credential_id: b"cable-fake-phone-cred-01".to_vec(),
                user_handle: b"2:1000".to_vec(),
                omit_credential_id_on_single_allow: false,
                protocol_revision: 1,
                supports_ctap: true,
                pre_reply_updates: Vec::new(),
                shutdown_before_reply: false,
                ctap_error: None,
                recorded_make_credentials: Vec::new(),
                recorded_get_assertions: Vec::new(),
            })),
        }
    }

    /// Sets the credential ID returned by `MakeCredential` and `GetAssertion`.
    pub fn set_credential_id(&self, id: impl Into<Vec<u8>>) {
        if let Ok(mut g) = self.inner.lock() {
            g.credential_id = id.into();
        }
    }

    /// Sets the user handle returned by `GetAssertion`.
    pub fn set_user_handle(&self, handle: impl Into<Vec<u8>>) {
        if let Ok(mut g) = self.inner.lock() {
            g.user_handle = handle.into();
        }
    }

    /// Configures whether to omit map key `1` (`credential`) when `allowList.len() == 1`.
    pub fn set_omit_credential_id_on_single_allow(&self, omit: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.omit_credential_id_on_single_allow = omit;
        }
    }

    /// Sets the negotiated caBLE protocol revision (`1` default, `0` legacy padded).
    pub fn set_protocol_revision(&self, rev: u8) {
        if let Ok(mut g) = self.inner.lock() {
            g.protocol_revision = rev;
        }
    }

    /// Sets whether the phone advertises CTAP support in its post-handshake message.
    pub fn set_supports_ctap(&self, supported: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.supports_ctap = supported;
        }
    }

    /// Configures `CableMessageType::Update` payloads sent before the CTAP2 reply.
    pub fn set_pre_reply_updates(&self, updates: Vec<Vec<u8>>) {
        if let Ok(mut g) = self.inner.lock() {
            g.pre_reply_updates = updates;
        }
    }

    /// Configures the phone to send `CableMessageType::Shutdown` instead of a CTAP2 reply.
    pub fn set_shutdown_before_reply(&self, shutdown: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.shutdown_before_reply = shutdown;
        }
    }

    /// Injects a non-zero CTAP2 status error (e.g. [`CTAP_ERR_OPERATION_DENIED`]).
    pub fn set_ctap_error(&self, error: Option<u8>) {
        if let Ok(mut g) = self.inner.lock() {
            g.ctap_error = error;
        }
    }

    /// Returns all decrypted `MakeCredential` requests received by the fake phone.
    #[must_use]
    pub fn recorded_make_credentials(&self) -> Vec<ParsedCtapMakeCredentialRequest> {
        self.inner
            .lock()
            .map(|g| g.recorded_make_credentials.clone())
            .unwrap_or_default()
    }

    /// Returns all decrypted `GetAssertion` requests received by the fake phone.
    #[must_use]
    pub fn recorded_get_assertions(&self) -> Vec<ParsedCtapGetAssertionRequest> {
        self.inner
            .lock()
            .map(|g| g.recorded_get_assertions.clone())
            .unwrap_or_default()
    }
}

impl CablePhoneAuthenticator for FakeCablePhone {
    fn protocol_revision(&self) -> u8 {
        self.inner.lock().map(|g| g.protocol_revision).unwrap_or(1)
    }

    fn supports_ctap(&self) -> bool {
        self.inner.lock().map(|g| g.supports_ctap).unwrap_or(true)
    }

    fn pre_reply_updates(&self) -> Vec<Vec<u8>> {
        self.inner
            .lock()
            .map(|g| g.pre_reply_updates.clone())
            .unwrap_or_default()
    }

    fn should_shutdown_instead_of_reply(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.shutdown_before_reply)
            .unwrap_or(false)
    }

    fn handle_make_credential(
        &self,
        req: &ParsedCtapMakeCredentialRequest,
    ) -> Result<CtapMakeCredentialResponse, u8> {
        let mut g = self.inner.lock().map_err(|_| CTAP_ERR_OPERATION_DENIED)?;
        g.recorded_make_credentials.push(req.clone());
        if let Some(code) = g.ctap_error {
            return Err(code);
        }
        let cred_id = g.credential_id.clone();
        let auth_data = build_synthetic_auth_data(&req.rp_id, &cred_id, 0x45);
        Ok(CtapMakeCredentialResponse {
            auth_data,
            credential_id: cred_id,
        })
    }

    fn handle_get_assertion(
        &self,
        req: &ParsedCtapGetAssertionRequest,
    ) -> Result<CtapGetAssertionResponse, u8> {
        let mut g = self.inner.lock().map_err(|_| CTAP_ERR_OPERATION_DENIED)?;
        g.recorded_get_assertions.push(req.clone());
        if let Some(code) = g.ctap_error {
            return Err(code);
        }
        let credential_id =
            if g.omit_credential_id_on_single_allow && req.allow_credential_ids.len() == 1 {
                Vec::new()
            } else {
                req.allow_credential_ids
                    .first()
                    .cloned()
                    .unwrap_or_else(|| g.credential_id.clone())
            };
        let auth_data = build_synthetic_auth_data(&req.rp_id, &[], 0x05);
        let mut sig_input = auth_data.clone();
        sig_input.extend_from_slice(&req.client_data_hash);
        let signature = sha256_digest(&sig_input).to_vec();
        Ok(CtapGetAssertionResponse {
            credential_id,
            auth_data,
            signature,
            user_handle: g.user_handle.clone(),
        })
    }
}

/// Encodes a CTAP2 `authenticatorMakeCredential` success response (`0x00 || CBOR`).
#[must_use]
pub fn encode_ctap_make_credential_response(resp: &CtapMakeCredentialResponse) -> Vec<u8> {
    let mut out = vec![CTAP_SUCCESS];
    out.extend_from_slice(&cbor_encode(&CborValue::Map(vec![
        (CborValue::Int(1), CborValue::Text("none".to_owned())),
        (CborValue::Int(2), CborValue::Bytes(resp.auth_data.clone())),
        (CborValue::Int(3), CborValue::Map(Vec::new())),
    ])));
    out
}

/// Encodes a CTAP2 `authenticatorGetAssertion` success response (`0x00 || CBOR`).
#[must_use]
pub fn encode_ctap_get_assertion_response(resp: &CtapGetAssertionResponse) -> Vec<u8> {
    let mut entries = Vec::new();
    if !resp.credential_id.is_empty() {
        entries.push((
            CborValue::Int(1),
            CborValue::Map(vec![
                (
                    CborValue::Text("id".to_owned()),
                    CborValue::Bytes(resp.credential_id.clone()),
                ),
                (
                    CborValue::Text("type".to_owned()),
                    CborValue::Text("public-key".to_owned()),
                ),
            ]),
        ));
    }
    entries.push((CborValue::Int(2), CborValue::Bytes(resp.auth_data.clone())));
    entries.push((CborValue::Int(3), CborValue::Bytes(resp.signature.clone())));
    if !resp.user_handle.is_empty() {
        entries.push((
            CborValue::Int(4),
            CborValue::Map(vec![(
                CborValue::Text("id".to_owned()),
                CborValue::Bytes(resp.user_handle.clone()),
            )]),
        ));
    }
    let mut out = vec![CTAP_SUCCESS];
    out.extend_from_slice(&cbor_encode(&CborValue::Map(entries)));
    out
}

/// Encodes a caBLEv2 post-handshake capabilities CBOR payload (rev 1 unpadded or rev 0 padded).
#[must_use]
pub fn encode_post_handshake_message(
    get_info: &[u8],
    protocol_revision: u8,
    supports_ctap: bool,
) -> Vec<u8> {
    let features = if supports_ctap {
        vec![CborValue::Text("ctap".to_owned())]
    } else {
        vec![CborValue::Text("json".to_owned())]
    };
    let mut bytes = cbor_encode(&CborValue::Map(vec![
        (CborValue::Int(1), CborValue::Bytes(get_info.to_vec())),
        (CborValue::Int(3), CborValue::Array(features)),
    ]));
    if protocol_revision == 0 {
        // Legacy rev-0 1-byte trailing zero-padding trailer (`pad = 3`, 3 zero bytes + 0x03).
        bytes.extend_from_slice(&[0u8, 0u8, 0u8, 3u8]);
    }
    bytes
}

#[derive(Clone)]
enum FakeResponderConfig {
    KNpsk0 {
        psk: [u8; CABLE_PSK_LEN],
        initiator_identity_x962: [u8; P256_X962_LEN],
    },
    NKpsk0 {
        psk: [u8; CABLE_PSK_LEN],
        responder_identity: P256SecretKey,
    },
}

struct FakeTunnelState {
    responder_cfg: Option<FakeResponderConfig>,
    responder_ephemeral: Option<P256SecretKey>,
    expected_path: Option<String>,
    connect_error: Option<CableTunnelError>,
    corrupt_handshake_response: bool,
    connected_endpoints: Vec<CableTunnelEndpoint>,
    last_handshake_hash: Option<[u8; 32]>,
}

/// In-process deterministic [`CableTunnelConnector`] that routes WebSocket binary frames directly
/// into a [`CableHandshakeResponder`] and [`CablePhoneAuthenticator`] without any network I/O.
#[derive(Clone)]
pub struct FakeCableTunnelConnector {
    phone: Arc<dyn CablePhoneAuthenticator>,
    state: Arc<Mutex<FakeTunnelState>>,
}

impl FakeCableTunnelConnector {
    /// Creates a fake tunnel connector configured for a `KNpsk0` QR session given the shared
    /// `qr_secret`, decrypted `eid`, and desktop initiator's 65-byte uncompressed identity key.
    #[must_use]
    pub fn for_knpsk0_qr(
        qr_secret: &[u8; CABLE_QR_SECRET_LEN],
        decrypted_eid: &[u8; 16],
        initiator_identity_x962: [u8; P256_X962_LEN],
        phone: Arc<dyn CablePhoneAuthenticator>,
    ) -> Self {
        let psk =
            cable_derive::<CABLE_PSK_LEN>(qr_secret, decrypted_eid, CableDerivedValueType::Psk);
        let tunnel_id =
            cable_derive::<CABLE_TUNNEL_ID_LEN>(qr_secret, &[], CableDerivedValueType::TunnelId);
        let mut routing_id = [0u8; 3];
        routing_id.copy_from_slice(&decrypted_eid[11..14]);
        let expected_path = cable_tunnel_connect_path(&routing_id, &tunnel_id);
        Self {
            phone,
            state: Arc::new(Mutex::new(FakeTunnelState {
                responder_cfg: Some(FakeResponderConfig::KNpsk0 {
                    psk,
                    initiator_identity_x962,
                }),
                responder_ephemeral: None,
                expected_path: Some(expected_path),
                connect_error: None,
                corrupt_handshake_response: false,
                connected_endpoints: Vec::new(),
                last_handshake_hash: None,
            })),
        }
    }

    /// Creates a fake tunnel connector configured for an `NKpsk0` paired-device session given `psk`
    /// and the phone's static P-256 identity private key.
    #[must_use]
    pub fn for_nkpsk0(
        psk: [u8; CABLE_PSK_LEN],
        responder_identity: P256SecretKey,
        phone: Arc<dyn CablePhoneAuthenticator>,
    ) -> Self {
        Self {
            phone,
            state: Arc::new(Mutex::new(FakeTunnelState {
                responder_cfg: Some(FakeResponderConfig::NKpsk0 {
                    psk,
                    responder_identity,
                }),
                responder_ephemeral: None,
                expected_path: None,
                connect_error: None,
                corrupt_handshake_response: false,
                connected_endpoints: Vec::new(),
                last_handshake_hash: None,
            })),
        }
    }

    /// Creates a fake tunnel connector without pre-bound QR keys (useful when keys are configured
    /// after `start_hybrid_qr_registration` generates a fresh session, or for error injection).
    #[must_use]
    pub fn unbound(phone: Arc<dyn CablePhoneAuthenticator>) -> Self {
        Self {
            phone,
            state: Arc::new(Mutex::new(FakeTunnelState {
                responder_cfg: None,
                responder_ephemeral: None,
                expected_path: None,
                connect_error: None,
                corrupt_handshake_response: false,
                connected_endpoints: Vec::new(),
                last_handshake_hash: None,
            })),
        }
    }

    /// Binds a `KNpsk0` QR session's keys onto an existing [`FakeCableTunnelConnector`].
    pub fn bind_knpsk0_qr(
        &self,
        qr_secret: &[u8; CABLE_QR_SECRET_LEN],
        decrypted_eid: &[u8; 16],
        initiator_identity_x962: [u8; P256_X962_LEN],
    ) {
        let psk =
            cable_derive::<CABLE_PSK_LEN>(qr_secret, decrypted_eid, CableDerivedValueType::Psk);
        let tunnel_id =
            cable_derive::<CABLE_TUNNEL_ID_LEN>(qr_secret, &[], CableDerivedValueType::TunnelId);
        let mut routing_id = [0u8; 3];
        routing_id.copy_from_slice(&decrypted_eid[11..14]);
        let expected_path = cable_tunnel_connect_path(&routing_id, &tunnel_id);
        if let Ok(mut g) = self.state.lock() {
            g.responder_cfg = Some(FakeResponderConfig::KNpsk0 {
                psk,
                initiator_identity_x962,
            });
            g.expected_path = Some(expected_path);
        }
    }

    /// Sets a fixed P-256 ephemeral secret key for the fake phone's Noise response.
    pub fn set_responder_ephemeral(&self, ephemeral: P256SecretKey) {
        if let Ok(mut g) = self.state.lock() {
            g.responder_ephemeral = Some(ephemeral);
        }
    }

    /// Sets a fixed P-256 ephemeral secret key from a 32-byte scalar (for callers without a direct
    /// `p256` dependency). Returns `true` if the scalar is valid.
    pub fn set_responder_ephemeral_bytes(&self, scalar: &[u8; 32]) -> bool {
        let Some(key) = p256_secret_from_bytes(scalar) else {
            return false;
        };
        self.set_responder_ephemeral(key);
        true
    }

    /// Injects a connection error returned by [`CableTunnelConnector::connect`].
    pub fn set_connect_error(&self, err: Option<CableTunnelError>) {
        if let Ok(mut g) = self.state.lock() {
            g.connect_error = err;
        }
    }

    /// Corrupts the AES-GCM tag of the responder's Noise handshake reply to test authentication errors.
    pub fn set_corrupt_handshake_response(&self, corrupt: bool) {
        if let Ok(mut g) = self.state.lock() {
            g.corrupt_handshake_response = corrupt;
        }
    }

    /// Returns every [`CableTunnelEndpoint`] passed to [`CableTunnelConnector::connect`].
    #[must_use]
    pub fn connected_endpoints(&self) -> Vec<CableTunnelEndpoint> {
        self.state
            .lock()
            .map(|g| g.connected_endpoints.clone())
            .unwrap_or_default()
    }

    /// Returns the last Noise transcript hash observed by the fake phone responder.
    #[must_use]
    pub fn last_handshake_hash(&self) -> Option<[u8; 32]> {
        self.state.lock().ok().and_then(|g| g.last_handshake_hash)
    }
}

impl CableTunnelConnector for FakeCableTunnelConnector {
    fn connect(
        &self,
        endpoint: &CableTunnelEndpoint,
        _timeout: Duration,
    ) -> Result<Box<dyn CableWebSocket>, CableTunnelError> {
        endpoint.validate()?;
        if endpoint.subprotocol != CABLE_WS_SUBPROTOCOL {
            return Err(CableTunnelError::WebSocketHandshake(format!(
                "expected subprotocol '{CABLE_WS_SUBPROTOCOL}', got '{}'",
                endpoint.subprotocol
            )));
        }

        let (responder_cfg, responder_ephemeral, corrupt) = {
            let mut g = self
                .state
                .lock()
                .map_err(|_| CableTunnelError::Transport("fake tunnel lock poisoned".into()))?;
            g.connected_endpoints.push(endpoint.clone());
            if let Some(ref err) = g.connect_error {
                return Err(err.clone());
            }
            if let Some(ref expected_path) = g.expected_path
                && &endpoint.path != expected_path
            {
                return Err(CableTunnelError::WebSocketHandshake(format!(
                    "tunnel path mismatch: expected '{expected_path}', got '{}'",
                    endpoint.path
                )));
            }
            let cfg = g.responder_cfg.clone().ok_or_else(|| {
                CableTunnelError::Transport(
                    "fake tunnel has no bound responder credentials".to_owned(),
                )
            })?;
            (
                cfg,
                g.responder_ephemeral.clone(),
                g.corrupt_handshake_response,
            )
        };

        let responder = match responder_cfg {
            FakeResponderConfig::KNpsk0 {
                psk,
                initiator_identity_x962,
            } => CableHandshakeResponder::new_knpsk0(psk, initiator_identity_x962),
            FakeResponderConfig::NKpsk0 {
                psk,
                responder_identity,
            } => CableHandshakeResponder::new_nkpsk0(psk, responder_identity),
        };

        Ok(Box::new(FakePhoneWebSocket {
            phone: Arc::clone(&self.phone),
            tunnel_state: Arc::clone(&self.state),
            phase: FakePhonePhase::AwaitingHandshake {
                responder: Box::new(responder),
                responder_ephemeral,
                corrupt_response: corrupt,
            },
            outbound_queue: VecDeque::new(),
            closed: false,
        }))
    }
}

enum FakePhonePhase {
    AwaitingHandshake {
        responder: Box<CableHandshakeResponder>,
        responder_ephemeral: Option<P256SecretKey>,
        corrupt_response: bool,
    },
    Established {
        crypter: CableCrypter,
        protocol_revision: u8,
    },
    Done,
}

struct FakePhoneWebSocket {
    phone: Arc<dyn CablePhoneAuthenticator>,
    tunnel_state: Arc<Mutex<FakeTunnelState>>,
    phase: FakePhonePhase,
    outbound_queue: VecDeque<Vec<u8>>,
    closed: bool,
}

impl CableWebSocket for FakePhoneWebSocket {
    fn send_binary(&mut self, data: &[u8]) -> Result<(), CableTunnelError> {
        if self.closed {
            return Err(CableTunnelError::Closed);
        }
        let current = std::mem::replace(&mut self.phase, FakePhonePhase::Done);
        match current {
            FakePhonePhase::AwaitingHandshake {
                mut responder,
                responder_ephemeral,
                corrupt_response,
            } => {
                let res = match responder_ephemeral {
                    Some(eph) => responder.process_initial_message_with_ephemeral(data, eph),
                    None => responder.process_initial_message(data),
                };
                let Some((
                    mut resp_msg,
                    CableHandshakeResult {
                        mut crypter,
                        handshake_hash,
                    },
                )) = res
                else {
                    return Err(CableTunnelError::NoiseHandshakeFailed);
                };
                if let Ok(mut g) = self.tunnel_state.lock() {
                    g.last_handshake_hash = Some(handshake_hash);
                }
                if corrupt_response && let Some(last) = resp_msg.last_mut() {
                    *last ^= 0xFF;
                }
                self.outbound_queue.push_back(resp_msg);

                let protocol_revision = self.phone.protocol_revision();
                let ph_plain = encode_post_handshake_message(
                    &self.phone.get_info_cbor(),
                    protocol_revision,
                    self.phone.supports_ctap(),
                );
                let ph_enc = crypter
                    .encrypt(&ph_plain)
                    .ok_or(CableTunnelError::EncryptionFailed)?;
                self.outbound_queue.push_back(ph_enc);

                self.phase = FakePhonePhase::Established {
                    crypter,
                    protocol_revision,
                };
                Ok(())
            }
            FakePhonePhase::Established {
                mut crypter,
                protocol_revision,
            } => {
                let plaintext = crypter
                    .decrypt(data)
                    .ok_or(CableTunnelError::DecryptionFailed)?;
                let ctap_bytes = match decode_tunnel_frame(protocol_revision, &plaintext)? {
                    CableTunnelFrame::Ctap(bytes) => bytes,
                    _ => {
                        return Err(CableTunnelError::MalformedCtapResponse(
                            "expected CTAP frame from initiator".to_owned(),
                        ));
                    }
                };

                if protocol_revision >= 1 {
                    for update in self.phone.pre_reply_updates() {
                        let mut frame = Vec::with_capacity(1 + update.len());
                        frame.push(CableMessageType::Update as u8);
                        frame.extend_from_slice(&update);
                        let enc = crypter
                            .encrypt(&frame)
                            .ok_or(CableTunnelError::EncryptionFailed)?;
                        self.outbound_queue.push_back(enc);
                    }
                }

                if self.phone.should_shutdown_instead_of_reply() && protocol_revision >= 1 {
                    let enc = crypter
                        .encrypt(&[CableMessageType::Shutdown as u8])
                        .ok_or(CableTunnelError::EncryptionFailed)?;
                    self.outbound_queue.push_back(enc);
                    self.phase = FakePhonePhase::Done;
                    return Ok(());
                }

                let reply_ctap = match ctap_bytes.first().copied() {
                    Some(0x01) => {
                        let req =
                            parse_ctap_make_credential_request(&ctap_bytes).ok_or_else(|| {
                                CableTunnelError::MalformedCtapResponse(
                                    "fake phone failed to parse MakeCredential CBOR".to_owned(),
                                )
                            })?;
                        match self.phone.handle_make_credential(&req) {
                            Ok(resp) => encode_ctap_make_credential_response(&resp),
                            Err(code) => vec![code],
                        }
                    }
                    Some(0x02) => {
                        let req =
                            parse_ctap_get_assertion_request(&ctap_bytes).ok_or_else(|| {
                                CableTunnelError::MalformedCtapResponse(
                                    "fake phone failed to parse GetAssertion CBOR".to_owned(),
                                )
                            })?;
                        match self.phone.handle_get_assertion(&req) {
                            Ok(resp) => encode_ctap_get_assertion_response(&resp),
                            Err(code) => vec![code],
                        }
                    }
                    other => {
                        return Err(CableTunnelError::MalformedCtapResponse(format!(
                            "unknown CTAP2 command byte: {other:?}"
                        )));
                    }
                };

                let framed_reply = encode_tunnel_ctap_frame(protocol_revision, &reply_ctap);
                let enc_reply = crypter
                    .encrypt(&framed_reply)
                    .ok_or(CableTunnelError::EncryptionFailed)?;
                self.outbound_queue.push_back(enc_reply);
                self.phase = FakePhonePhase::Established {
                    crypter,
                    protocol_revision,
                };
                Ok(())
            }
            FakePhonePhase::Done => Err(CableTunnelError::Closed),
        }
    }

    fn recv_binary(&mut self) -> Result<Vec<u8>, CableTunnelError> {
        if self.closed {
            return Err(CableTunnelError::Closed);
        }
        self.outbound_queue
            .pop_front()
            .ok_or(CableTunnelError::Closed)
    }

    fn close(&mut self) {
        self.closed = true;
        self.phase = FakePhonePhase::Done;
    }
}

/// Validates and converts an uncompressed P-256 public key into `[u8; 65]`.
#[must_use]
pub fn p256_public_x962(secret: &P256SecretKey) -> [u8; P256_X962_LEN] {
    let sec1 = secret.public_key().to_sec1_bytes();
    let mut out = [0u8; P256_X962_LEN];
    out.copy_from_slice(&sec1);
    out
}

/// Parses a 32-byte big-endian scalar into a P-256 secret key (useful for deterministic test vectors).
#[must_use]
pub fn p256_secret_from_bytes(bytes: &[u8; 32]) -> Option<P256SecretKey> {
    P256SecretKey::from_bytes(bytes.into()).ok()
}

/// Validates that `bytes` is a valid uncompressed SEC1 P-256 point.
#[must_use]
pub fn is_valid_p256_x962(bytes: &[u8]) -> bool {
    bytes.len() == P256_X962_LEN && P256PublicKey::from_sec1_bytes(bytes).is_ok()
}

// ============================================================================
// Unit Tests: Spec KATs, Full Fake-Phone Ceremonies, Error Paths & RFC 6455
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webauthn::{
        AllowCredential, CABLE_EID_KEY_LEN, CABLE_EID_LEN, COSE_ES256, CableCeremonyStep,
        CableQrSession, CredentialParameter, DEFAULT_TIMEOUT_MS, PasskeyUser, RelyingParty,
        UserVerification, aes_gcm_seal, cable_encrypt_advert, client_data_hash, hkdf_sha256,
        parse_attestation_object, serialize_client_data_create, serialize_client_data_get,
    };

    fn fixed_key(fill: u8) -> P256SecretKey {
        let mut bytes = [0u8; 32];
        bytes[31] = fill;
        bytes[0] = 0x11;
        p256_secret_from_bytes(&bytes).expect("valid fixed P-256 test scalar")
    }

    /// Independent spec-algorithm computation for CTAP 2.2 / Noise `KNpsk0` and `NKpsk0`
    /// transcripts (SHA-256 + RFC 5869 HKDF-SHA256 + P-256 ECDH + AES-256-GCM).
    fn spec_mix_hash(h: &[u8; 32], input: &[u8]) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 + input.len());
        buf.extend_from_slice(h);
        buf.extend_from_slice(input);
        sha256_digest(&buf)
    }

    fn spec_mix_key(ck: &mut [u8; 32], ikm: &[u8]) -> [u8; 32] {
        let out = hkdf_sha256(ikm, ck, &[], 64);
        ck.copy_from_slice(&out[..32]);
        let mut k = [0u8; 32];
        k.copy_from_slice(&out[32..64]);
        k
    }

    fn spec_mix_key_and_hash(ck: &mut [u8; 32], h: &mut [u8; 32], ikm: &[u8]) -> [u8; 32] {
        let out = hkdf_sha256(ikm, ck, &[], 96);
        ck.copy_from_slice(&out[..32]);
        *h = spec_mix_hash(h, &out[32..64]);
        let mut k = [0u8; 32];
        k.copy_from_slice(&out[64..96]);
        k
    }

    #[test]
    fn kat_key_derivation_and_noise_knpsk0_nkpsk0_transcripts() {
        // Fixed test inputs (never real secrets).
        let qr_secret: [u8; CABLE_QR_SECRET_LEN] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10,
        ];
        let mut eid = [0u8; CABLE_EID_LEN];
        eid[0] = 0x00;
        eid[1..11].copy_from_slice(&[0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA]);
        eid[11..14].copy_from_slice(&[0x12, 0x34, 0x56]);
        eid[14..16].copy_from_slice(&0u16.to_le_bytes()); // cable.ua5v.com

        // 1. Key derivation KAT: EidKey (purpose=1), TunnelId (purpose=2), Psk (purpose=3).
        let expected_eid_key = hkdf_sha256(&qr_secret, &[], &1u32.to_le_bytes(), CABLE_EID_KEY_LEN);
        let expected_tunnel_id =
            hkdf_sha256(&qr_secret, &[], &2u32.to_le_bytes(), CABLE_TUNNEL_ID_LEN);
        let expected_psk = hkdf_sha256(&qr_secret, &eid, &3u32.to_le_bytes(), CABLE_PSK_LEN);

        let got_eid_key =
            cable_derive::<CABLE_EID_KEY_LEN>(&qr_secret, &[], CableDerivedValueType::EidKey);
        let got_tunnel_id =
            cable_derive::<CABLE_TUNNEL_ID_LEN>(&qr_secret, &[], CableDerivedValueType::TunnelId);
        let got_psk = cable_derive::<CABLE_PSK_LEN>(&qr_secret, &eid, CableDerivedValueType::Psk);

        assert_eq!(&got_eid_key[..], expected_eid_key.as_slice());
        assert_eq!(&got_tunnel_id[..], expected_tunnel_id.as_slice());
        assert_eq!(&got_psk[..], expected_psk.as_slice());

        // Verify the derived tunnel path matches wss://cable.ua5v.com/cable/connect/<routing>/<tunnel>.
        let expected_path = format!(
            "/cable/connect/123456/{}",
            crate::webauthn::cable_hex_lower(&got_tunnel_id)
        );
        assert_eq!(
            cable_tunnel_connect_path(&[0x12, 0x34, 0x56], &got_tunnel_id),
            expected_path
        );

        // Fixed P-256 test keys.
        let init_static = fixed_key(0x21);
        let init_eph = fixed_key(0x22);
        let resp_static = fixed_key(0x31);
        let resp_eph = fixed_key(0x32);
        let init_static_pub = p256_public_x962(&init_static);
        let init_eph_pub = p256_public_x962(&init_eph);
        let resp_static_pub = p256_public_x962(&resp_static);
        let resp_eph_pub = p256_public_x962(&resp_eph);

        // 2. Noise KNpsk0 transcript KAT (QR-initiated flow).
        let mut ck = [0u8; 32];
        ck[..31].copy_from_slice(b"Noise_KNpsk0_P256_AESGCM_SHA256");
        let mut h = ck;
        h = spec_mix_hash(&h, &[1u8]);
        h = spec_mix_hash(&h, &init_static_pub);
        let _k0 = spec_mix_key_and_hash(&mut ck, &mut h, &got_psk);
        h = spec_mix_hash(&h, &init_eph_pub);
        let k1 = spec_mix_key(&mut ck, &init_eph_pub);
        let expected_ct1 = aes_gcm_seal(&k1, &[0u8; 12], &[], &h).expect("ct1");
        h = spec_mix_hash(&h, &expected_ct1);

        let mut expected_msg1 = Vec::with_capacity(81);
        expected_msg1.extend_from_slice(&init_eph_pub);
        expected_msg1.extend_from_slice(&expected_ct1);

        let mut kn_init = CableHandshakeInitiator::new(got_psk, init_static.clone());
        let got_msg1 = kn_init
            .build_initial_message_with_ephemeral(init_eph.clone())
            .expect("KNpsk0 msg1");
        assert_eq!(got_msg1, expected_msg1);

        // Spec message 2 for KNpsk0 (<- e, ee, se).
        h = spec_mix_hash(&h, &resp_eph_pub);
        let _k2 = spec_mix_key(&mut ck, &resp_eph_pub);
        let ee = p256_ecdh_shared_x(&init_eph, &resp_eph_pub).expect("ee");
        let _k3 = spec_mix_key(&mut ck, &ee);
        let se = p256_ecdh_shared_x(&init_static, &resp_eph_pub).expect("se");
        let k4 = spec_mix_key(&mut ck, &se);
        let expected_ct2 = aes_gcm_seal(&k4, &[0u8; 12], &[], &h).expect("ct2");
        h = spec_mix_hash(&h, &expected_ct2);
        let expected_kn_hash = h;
        let expected_kn_keys = hkdf_sha256(&[], &ck, &[], 64);

        let mut kn_resp = CableHandshakeResponder::new_knpsk0(got_psk, init_static_pub);
        let (got_msg2, mut resp_res) = kn_resp
            .process_initial_message_with_ephemeral(&got_msg1, resp_eph.clone())
            .expect("KNpsk0 msg2");
        assert_eq!(&got_msg2[..65], &resp_eph_pub[..]);
        assert_eq!(&got_msg2[65..], expected_ct2.as_slice());

        let mut init_res = kn_init
            .process_response(&got_msg2)
            .expect("KNpsk0 initiator finish");
        assert_eq!(init_res.handshake_hash, expected_kn_hash);
        assert_eq!(resp_res.handshake_hash, expected_kn_hash);

        // Verify derived traffic keys match independent spec HKDF output by cross-encrypting.
        let mut spec_init_crypter = CableCrypter::new(
            expected_kn_keys[32..64].try_into().unwrap(),
            expected_kn_keys[..32].try_into().unwrap(),
        );
        let ct_a = init_res.crypter.encrypt(b"knpsk0-kat").expect("enc");
        assert_eq!(resp_res.crypter.decrypt(&ct_a).unwrap(), b"knpsk0-kat");
        let ct_b = spec_init_crypter.encrypt(b"knpsk0-spec").expect("enc2");
        let mut resp_crypter_check = CableCrypter::new(
            expected_kn_keys[..32].try_into().unwrap(),
            expected_kn_keys[32..64].try_into().unwrap(),
        );
        assert_eq!(resp_crypter_check.decrypt(&ct_b).unwrap(), b"knpsk0-spec");

        // 3. Noise NKpsk0 transcript KAT (paired/linked authenticator flow).
        let mut ck = [0u8; 32];
        ck[..31].copy_from_slice(b"Noise_NKpsk0_P256_AESGCM_SHA256");
        let mut h = ck;
        h = spec_mix_hash(&h, &[0u8]);
        h = spec_mix_hash(&h, &resp_static_pub);
        let _k0 = spec_mix_key_and_hash(&mut ck, &mut h, &got_psk);
        h = spec_mix_hash(&h, &init_eph_pub);
        let _k1 = spec_mix_key(&mut ck, &init_eph_pub);
        let es = p256_ecdh_shared_x(&init_eph, &resp_static_pub).expect("es");
        let k_es = spec_mix_key(&mut ck, &es);
        let expected_nk_ct1 = aes_gcm_seal(&k_es, &[0u8; 12], &[], &h).expect("nk ct1");
        h = spec_mix_hash(&h, &expected_nk_ct1);

        let mut nk_init = CableHandshakeInitiator::new_nkpsk0(got_psk, resp_static_pub);
        assert_eq!(nk_init.handshake_type(), CableHandshakeType::NKpsk0);
        let got_nk_msg1 = nk_init
            .build_initial_message_with_ephemeral(init_eph.clone())
            .expect("NKpsk0 msg1");
        assert_eq!(&got_nk_msg1[..65], &init_eph_pub[..]);
        assert_eq!(&got_nk_msg1[65..], expected_nk_ct1.as_slice());

        // Spec message 2 for NKpsk0 (<- e, ee).
        h = spec_mix_hash(&h, &resp_eph_pub);
        let _k2 = spec_mix_key(&mut ck, &resp_eph_pub);
        let ee_nk = p256_ecdh_shared_x(&init_eph, &resp_eph_pub).expect("ee nk");
        let k_ee = spec_mix_key(&mut ck, &ee_nk);
        let expected_nk_ct2 = aes_gcm_seal(&k_ee, &[0u8; 12], &[], &h).expect("nk ct2");
        h = spec_mix_hash(&h, &expected_nk_ct2);
        let expected_nk_hash = h;

        let mut nk_resp = CableHandshakeResponder::new_nkpsk0(got_psk, resp_static);
        let (got_nk_msg2, mut nk_resp_res) = nk_resp
            .process_initial_message_with_ephemeral(&got_nk_msg1, resp_eph)
            .expect("NKpsk0 msg2");
        assert_eq!(&got_nk_msg2[65..], expected_nk_ct2.as_slice());

        let mut nk_init_res = nk_init
            .process_response(&got_nk_msg2)
            .expect("NKpsk0 initiator finish");
        assert_eq!(nk_init_res.handshake_hash, expected_nk_hash);
        assert_eq!(nk_resp_res.handshake_hash, expected_nk_hash);

        let sealed = nk_init_res.crypter.encrypt(b"nkpsk0-kat").expect("enc");
        assert_eq!(nk_resp_res.crypter.decrypt(&sealed).unwrap(), b"nkpsk0-kat");
    }

    #[test]
    fn full_knpsk0_make_credential_and_get_assertion_over_fake_phone_tunnel() {
        let init_identity = fixed_key(0x41);
        let init_ephemeral = fixed_key(0x42);
        let resp_ephemeral = fixed_key(0x43);
        let qr_secret = [0x5Au8; CABLE_QR_SECRET_LEN];

        let mut qr_session =
            CableQrSession::from_parts(init_identity.clone(), qr_secret, true, 1_740_000_000)
                .expect("qr session");

        let mut eid = [0u8; CABLE_EID_LEN];
        eid[0] = 0;
        eid[1..11].copy_from_slice(&[0x33u8; 10]);
        eid[11..14].copy_from_slice(&[0xAB, 0xCD, 0xEF]);
        eid[14..16].copy_from_slice(&0u16.to_le_bytes()); // cable.ua5v.com
        let advert = cable_encrypt_advert(&eid, qr_session.eid_key());

        let (domain, path, initiator) = qr_session
            .accept_ble_advert(&advert)
            .expect("advert should match");
        assert_eq!(qr_session.step(), CableCeremonyStep::Connecting);
        let endpoint = CableTunnelEndpoint::new(domain, path);
        assert!(
            endpoint
                .wss_url()
                .starts_with("wss://cable.ua5v.com/cable/connect/abcdef/")
        );

        let phone = Arc::new(FakeCablePhone::new());
        phone.set_credential_id(b"phone-created-cred-77");
        phone.set_pre_reply_updates(vec![b"linking-metadata-cbor".to_vec()]);
        let connector = Arc::new(FakeCableTunnelConnector::for_knpsk0_qr(
            &qr_secret,
            &eid,
            qr_session.identity_public_x962(),
            phone.clone(),
        ));
        connector.set_responder_ephemeral(resp_ephemeral.clone());

        let reg_data = RegisterData {
            rp: RelyingParty {
                id: "telegram.org".to_owned(),
                name: "Telegram".to_owned(),
            },
            user: PasskeyUser {
                id: b"2:1000".to_vec(),
                name: "demo_user".to_owned(),
                display_name: "Demo User".to_owned(),
            },
            challenge: b"reg-challenge-bytes".to_vec(),
            pub_key_cred_params: vec![CredentialParameter {
                cred_type: "public-key".to_owned(),
                alg: COSE_ES256,
            }],
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };
        let client_json = serialize_client_data_create(&reg_data.challenge);
        let cd_hash = client_data_hash(client_json.as_bytes());

        let client = CableTunnelClient::new(connector.clone());
        let outcome = client
            .run_register_with_ephemeral(
                &endpoint,
                initiator,
                init_ephemeral.clone(),
                client_json.clone(),
                cd_hash,
                &reg_data,
                Duration::from_secs(5),
            )
            .expect("MakeCredential over fake tunnel");

        assert_eq!(outcome.result.credential_id, b"phone-created-cred-77");
        assert_eq!(outcome.result.client_data_json, client_json.as_bytes());
        assert_eq!(
            outcome.handshake_hash,
            connector.last_handshake_hash().unwrap()
        );
        assert_eq!(
            outcome.update_payloads,
            vec![b"linking-metadata-cbor".to_vec()]
        );
        let parsed_att =
            parse_attestation_object(&outcome.result.attestation_object).expect("attestation");
        assert_eq!(
            parsed_att.parsed_auth_data.credential_id.as_deref(),
            Some(&b"phone-created-cred-77"[..])
        );
        assert_eq!(phone.recorded_make_credentials().len(), 1);
        assert_eq!(phone.recorded_make_credentials()[0].rp_id, "telegram.org");

        // Now run NKpsk0 GetAssertion (paired device) with protocol revision 0 (legacy padded)
        // and single-credential allowList fallback!
        let resp_static = fixed_key(0x55);
        let psk = [0x77u8; CABLE_PSK_LEN];
        let phone_ga = Arc::new(FakeCablePhone::new());
        phone_ga.set_protocol_revision(0);
        phone_ga.set_user_handle(b"4:555000");
        phone_ga.set_omit_credential_id_on_single_allow(true);

        let connector_nk = Arc::new(FakeCableTunnelConnector::for_nkpsk0(
            psk,
            resp_static.clone(),
            phone_ga.clone(),
        ));
        connector_nk.set_responder_ephemeral(resp_ephemeral);

        let login_data = LoginData {
            challenge: b"login-challenge-bytes".to_vec(),
            rp_id: "telegram.org".to_owned(),
            allow_credentials: vec![AllowCredential {
                id: b"single-allow-cred".to_vec(),
                cred_type: "public-key".to_owned(),
            }],
            user_verification: UserVerification::Preferred,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };
        let login_json = serialize_client_data_get(&login_data.challenge);
        let login_hash = client_data_hash(login_json.as_bytes());
        let nk_init = CableHandshakeInitiator::new_nkpsk0(psk, p256_public_x962(&resp_static));

        let login_outcome = CableTunnelClient::new(connector_nk.clone())
            .run_login_with_ephemeral(
                &CableTunnelEndpoint::new("cable.auth.com", "/cable/connect/010203/aabb"),
                nk_init,
                init_ephemeral,
                login_json.clone(),
                login_hash,
                &login_data,
                Duration::from_secs(5),
            )
            .expect("NKpsk0 GetAssertion");

        assert_eq!(login_outcome.post_handshake.protocol_revision, 0);
        assert_eq!(login_outcome.result.credential_id, b"single-allow-cred");
        assert_eq!(login_outcome.result.user_handle, b"4:555000");
        assert_eq!(login_outcome.result.client_data_json, login_json.as_bytes());
        assert_eq!(
            login_outcome.handshake_hash,
            connector_nk.last_handshake_hash().unwrap()
        );
        assert_eq!(phone_ga.recorded_get_assertions().len(), 1);
    }

    #[test]
    fn tunnel_client_maps_ctap_and_transport_errors_cleanly() {
        let psk = [0x99u8; CABLE_PSK_LEN];
        let resp_static = fixed_key(0x61);
        let login_data = LoginData {
            challenge: b"err-challenge".to_vec(),
            rp_id: "telegram.org".to_owned(),
            allow_credentials: Vec::new(),
            user_verification: UserVerification::Preferred,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };
        let login_json = serialize_client_data_get(&login_data.challenge);
        let login_hash = client_data_hash(login_json.as_bytes());
        let ep = CableTunnelEndpoint::new("cable.ua5v.com", "/cable/connect/010203/0405");

        // 1. CTAP_ERR_OPERATION_DENIED -> WebAuthnErrorKind::Cancelled
        let phone = Arc::new(FakeCablePhone::new());
        phone.set_ctap_error(Some(CTAP_ERR_OPERATION_DENIED));
        let conn = Arc::new(FakeCableTunnelConnector::for_nkpsk0(
            psk,
            resp_static.clone(),
            phone.clone(),
        ));
        let err = CableTunnelClient::new(conn.clone())
            .run_login(
                &ep,
                CableHandshakeInitiator::new_nkpsk0(psk, p256_public_x962(&resp_static)),
                login_json.clone(),
                login_hash,
                &login_data,
                Duration::from_secs(2),
            )
            .unwrap_err();
        assert_eq!(err.to_webauthn_error(), WebAuthnErrorKind::Cancelled);

        // 2. CTAP_ERR_NO_CREDENTIALS -> WebAuthnErrorKind::NoCredentials
        phone.set_ctap_error(Some(CTAP_ERR_NO_CREDENTIALS));
        let err = CableTunnelClient::new(conn.clone())
            .run_login(
                &ep,
                CableHandshakeInitiator::new_nkpsk0(psk, p256_public_x962(&resp_static)),
                login_json.clone(),
                login_hash,
                &login_data,
                Duration::from_secs(2),
            )
            .unwrap_err();
        assert_eq!(err.to_webauthn_error(), WebAuthnErrorKind::NoCredentials);

        // 3. Shutdown control message -> TunnelShutdown
        phone.set_ctap_error(None);
        phone.set_shutdown_before_reply(true);
        let err = CableTunnelClient::new(conn.clone())
            .run_login(
                &ep,
                CableHandshakeInitiator::new_nkpsk0(psk, p256_public_x962(&resp_static)),
                login_json.clone(),
                login_hash,
                &login_data,
                Duration::from_secs(2),
            )
            .unwrap_err();
        assert_eq!(err, CableTunnelError::TunnelShutdown);

        // 4. Unsupported CTAP -> CtapUnsupported
        phone.set_shutdown_before_reply(false);
        phone.set_supports_ctap(false);
        let err = CableTunnelClient::new(conn.clone())
            .run_login(
                &ep,
                CableHandshakeInitiator::new_nkpsk0(psk, p256_public_x962(&resp_static)),
                login_json.clone(),
                login_hash,
                &login_data,
                Duration::from_secs(2),
            )
            .unwrap_err();
        assert_eq!(err, CableTunnelError::CtapUnsupported);

        // 5. Corrupted Noise handshake response -> NoiseHandshakeFailed
        phone.set_supports_ctap(true);
        conn.set_corrupt_handshake_response(true);
        let err = CableTunnelClient::new(conn)
            .run_login(
                &ep,
                CableHandshakeInitiator::new_nkpsk0(psk, p256_public_x962(&resp_static)),
                login_json,
                login_hash,
                &login_data,
                Duration::from_secs(2),
            )
            .unwrap_err();
        assert_eq!(err, CableTunnelError::NoiseHandshakeFailed);
    }

    #[test]
    fn rfc6455_websocket_upgrade_binary_ping_pong_and_fragmentation() {
        // RFC 6455 Section 4.2.2 official known-answer vector for Sec-WebSocket-Accept.
        assert_eq!(
            compute_websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );

        struct MockByteStream {
            inbound: VecDeque<u8>,
            outbound: Vec<u8>,
        }
        impl Read for MockByteStream {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let mut n = 0;
                while n < buf.len() {
                    let Some(b) = self.inbound.pop_front() else {
                        break;
                    };
                    buf[n] = b;
                    n += 1;
                }
                Ok(n)
            }
        }
        impl Write for MockByteStream {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.outbound.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let key = [0x11u8; 16];
        let key_b64 = BASE64_STANDARD.encode(key);
        let accept = compute_websocket_accept(&key_b64);

        let mut inbound = Vec::new();
        inbound.extend_from_slice(
            format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {accept}\r\n\
                 Sec-WebSocket-Protocol: fido.cable\r\n\r\n"
            )
            .as_bytes(),
        );
        // Server sends Ping ("hi"), then fragmented binary message ("hello " + "world" * 30).
        inbound.extend_from_slice(&[0x80 | WS_OPCODE_PING, 2, b'h', b'i']);
        inbound.extend_from_slice(&[WS_OPCODE_BINARY, 6]); // fin = 0
        inbound.extend_from_slice(b"hello ");
        let tail = vec![b'x'; 150]; // 126-byte extended length frame
        inbound.extend_from_slice(&[0x80 | WS_OPCODE_CONTINUATION, 126]);
        inbound.extend_from_slice(&(tail.len() as u16).to_be_bytes());
        inbound.extend_from_slice(&tail);
        // Server then sends Close frame.
        inbound.extend_from_slice(&[0x80 | WS_OPCODE_CLOSE, 0]);

        let stream = MockByteStream {
            inbound: inbound.into(),
            outbound: Vec::new(),
        };
        let ep = CableTunnelEndpoint::new("cable.ua5v.com", "/cable/connect/010203/deadbeef");
        let mut ws = Rfc6455WebSocket::client_handshake_with_key(stream, &ep, &key)
            .expect("upgrade succeeds");

        // Send a binary payload from client and read the fragmented binary reply (after auto-Pong).
        ws.send_binary(b"client-init").expect("send binary");
        let received = ws.recv_binary().expect("recv fragmented binary");
        let mut expected = b"hello ".to_vec();
        expected.extend_from_slice(&tail);
        assert_eq!(received, expected);

        // Next read sees Close frame.
        assert_eq!(ws.recv_binary(), Err(CableTunnelError::Closed));

        // Verify upgrade request headers contained GET path and Sec-WebSocket-Protocol: fido.cable.
        let out_str = String::from_utf8_lossy(&ws.stream.outbound);
        assert!(out_str.starts_with("GET /cable/connect/010203/deadbeef HTTP/1.1\r\n"));
        assert!(out_str.contains("Sec-WebSocket-Protocol: fido.cable\r\n"));
    }
}
