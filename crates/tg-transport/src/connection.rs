//! Async MTProto TCP connection combining framing, obfuscated2, proxies, and fake-TLS.
//!
//! Port of `mtproto/connection_abstract.h/.cpp` and `mtproto/connection_tcp.h/.cpp`.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand::RngCore;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::error::Error;
use crate::framing::{AnyTransport, FrameKind, PaddedIntermediate, Transport, TransportKind};
use crate::obfuscated::{OBFUSCATED_HEADER_SIZE, Obfuscated2};
use crate::proxy::{MtprotoSecret, ProxyData, ProxyType};
use crate::proxy_dial::{PrefixedStream, dial_through_proxy};
use crate::tls::TlsStream;

/// Default full connection timeout (`kFullConnectionTimeout` in `mtproto/connection_tcp.cpp`).
pub const FULL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(8);

/// Test-mode DC ID offset (`kTestModeDcIdShift` in `mtproto/connection_abstract.h`).
pub const TEST_MODE_DC_ID_SHIFT: i16 = 10000;

/// Initial read buffer size (`kSmallBufferSize` in `mtproto/connection_tcp.cpp`).
const SMALL_BUFFER_SIZE: usize = 64 * 1024;

/// TL constructor ID for `req_pq#60469778`.
pub const CONSTRUCTOR_REQ_PQ: u32 = 0x6046_9778;

/// TL constructor ID for `req_pq_multi#be7e8ef1`.
pub const CONSTRUCTOR_REQ_PQ_MULTI: u32 = 0xBE7E_8EF1;

/// TL constructor ID for `resPQ#05162463`.
pub const CONSTRUCTOR_RES_PQ: u32 = 0x0516_2463;

/// Compute the signed 16-bit protocol DC ID written into bytes `60..62` of the obfuscated2 header.
///
/// Port of `SessionPrivate::getProtocolDcId` (`mtproto/session_private.cpp`).
#[must_use]
pub const fn protocol_dc_id(dc_id: i16, is_media: bool, is_test_mode: bool) -> i16 {
    let base = if is_test_mode {
        TEST_MODE_DC_ID_SHIFT + dc_id
    } else {
        dc_id
    };
    if is_media { -base } else { base }
}

/// Configuration for establishing an MTProto TCP [`Connection`].
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// Framing protocol to use (overridden automatically when using an MTProxy secret).
    pub transport: TransportKind,
    /// Enable obfuscated2 encryption (always enabled when a proxy/DC secret is active).
    pub obfuscated: bool,
    /// Signed protocol DC ID written to bytes `60..62` of the obfuscated2 header.
    pub dc_id: i16,
    /// Optional DC-level protocol secret (from `dcOption.secret`).
    pub protocol_secret: Vec<u8>,
    /// Proxy configuration (`ProxyData::none()` for direct connections).
    pub proxy: ProxyData,
    /// Overall timeout for dialing + proxy/TLS handshakes (`FULL_CONNECTION_TIMEOUT` by default).
    pub connect_timeout: Duration,
    /// Set `TCP_NODELAY` on the underlying TCP socket (default `true`).
    pub tcp_nodelay: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            transport: TransportKind::Intermediate,
            obfuscated: true,
            dc_id: 2,
            protocol_secret: Vec::new(),
            proxy: ProxyData::none(),
            connect_timeout: FULL_CONNECTION_TIMEOUT,
            tcp_nodelay: true,
        }
    }
}

/// Frame returned by [`Connection::recv_frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceivedFrame {
    /// Complete MTProto packet payload.
    Payload(Vec<u8>),
    /// Server quick-ack token (31-bit value with MSB cleared).
    QuickAck(u32),
}

enum RawStream<S> {
    Plain(S),
    FakeTls(TlsStream<S>),
}

impl<S: AsyncRead + AsyncWrite + Unpin> RawStream<S> {
    async fn write_packet(&mut self, prefix: &[u8], payload: &[u8]) -> Result<(), Error> {
        match self {
            Self::Plain(s) => {
                if !prefix.is_empty() {
                    let mut combined = Vec::with_capacity(prefix.len() + payload.len());
                    combined.extend_from_slice(prefix);
                    combined.extend_from_slice(payload);
                    s.write_all(&combined).await?;
                } else {
                    s.write_all(payload).await?;
                }
                s.flush().await?;
                Ok(())
            }
            Self::FakeTls(tls) => tls.write_framed(prefix, payload).await,
        }
    }

    async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        match self {
            Self::Plain(s) => Ok(s.read(buf).await?),
            Self::FakeTls(tls) => tls.read_payload(buf).await,
        }
    }
}

/// Async MTProto transport connection (`Connection` contract from `docs/ARCHITECTURE.md`).
///
/// Defaults to `PrefixedStream<TcpStream>` for network connections; generic parameter `S`
/// allows in-memory streams (such as `tokio::io::DuplexStream`) for unit testing.
pub struct Connection<S = PrefixedStream<TcpStream>> {
    stream: RawStream<S>,
    transport: AnyTransport,
    obfuscated: Option<Box<Obfuscated2>>,
    pending_init: Vec<u8>,
    read_buf: Vec<u8>,
    read_pos: usize,
    tag: String,
    ping_time: Option<Duration>,
    bytes_received: u64,
}

impl Connection<PrefixedStream<TcpStream>> {
    /// Connect asynchronously to `address:port` (IPv4, IPv6, or hostname) using `config`.
    ///
    /// Supports direct TCP, SOCKS5 (with optional username/password), HTTP CONNECT
    /// (with optional Basic auth), and MTProxy (plain, `dd`, and `ee` fake-TLS).
    pub async fn connect(
        address: &str,
        port: u16,
        config: ConnectionConfig,
    ) -> Result<Self, Error> {
        let is_ipv6 = address.contains(':');
        let tag = if is_ipv6 {
            "TCP/IPv6".to_owned()
        } else {
            "TCP/IPv4".to_owned()
        };

        tracing::debug!(
            address,
            port,
            dc_id = config.dc_id,
            proxy = ?config.proxy.proxy_type,
            transport = ?config.transport,
            "connecting MTProto TCP transport"
        );

        let raw_tcp = dial_through_proxy(
            address,
            port,
            &config.proxy,
            config.connect_timeout,
            config.tcp_nodelay,
        )
        .await?;

        timeout(
            config.connect_timeout,
            Self::from_stream_with_tag(raw_tcp, config, tag),
        )
        .await
        .map_err(|_| Error::Timeout)?
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    /// Wrap an already-connected async byte stream with the configured transport, obfuscation,
    /// and (if an `0xEE` secret is present) fake-TLS handshake.
    pub async fn from_stream(stream: S, config: ConnectionConfig) -> Result<Self, Error> {
        Self::from_stream_with_tag(stream, config, "TCP/Stream".to_owned()).await
    }

    async fn from_stream_with_tag(
        stream: S,
        config: ConnectionConfig,
        tag: String,
    ) -> Result<Self, Error> {
        // Determine effective MTProto secret (from MTProxy password or DC protocol_secret).
        let raw_secret = if config.proxy.proxy_type == ProxyType::Mtproto {
            config.proxy.secret_from_mtproto_password()
        } else {
            config.protocol_secret
        };

        let parsed_secret = if raw_secret.is_empty() {
            None
        } else {
            Some(MtprotoSecret::from_bytes(&raw_secret)?)
        };

        // Select framing protocol and socket layer matching TcpConnection::Protocol::Create
        // and AbstractSocket::Create.
        let (transport, secret_key, raw_stream, force_obfuscated) = match parsed_secret {
            Some(MtprotoSecret::FakeTls { key, domain }) => {
                let tls = TlsStream::connect(stream, &domain, &key).await?;
                (
                    TransportKind::PaddedIntermediate.build(),
                    Some(key),
                    RawStream::FakeTls(tls),
                    true,
                )
            }
            Some(MtprotoSecret::Padded(key)) => (
                TransportKind::PaddedIntermediate.build(),
                Some(key),
                RawStream::Plain(stream),
                true,
            ),
            Some(MtprotoSecret::Plain(key)) => (
                TransportKind::Abridged.build(),
                Some(key),
                RawStream::Plain(stream),
                true,
            ),
            None => (
                config.transport.build(),
                None,
                RawStream::Plain(stream),
                false,
            ),
        };

        let use_obfuscation = config.obfuscated || force_obfuscated;
        let (obfuscated, pending_init) = if use_obfuscation {
            let proto_tag = transport
                .protocol_tag()
                .ok_or(Error::UnsupportedObfuscation)?;
            let (obf, header): (Obfuscated2, [u8; OBFUSCATED_HEADER_SIZE]) =
                Obfuscated2::generate(proto_tag, config.dc_id, secret_key.as_ref());
            (Some(Box::new(obf)), header.to_vec())
        } else {
            let init = transport
                .plain_init_bytes()
                .map(|b| b.to_vec())
                .unwrap_or_default();
            (None, init)
        };

        Ok(Self {
            stream: raw_stream,
            transport,
            obfuscated,
            pending_init,
            read_buf: Vec::with_capacity(SMALL_BUFFER_SIZE),
            read_pos: 0,
            tag,
            ping_time: None,
            bytes_received: 0,
        })
    }

    /// Return the connection transport tag (e.g. `"TCP/IPv4"` or `"TCP/IPv6"`).
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Return the active [`TransportKind`].
    #[must_use]
    pub const fn transport_kind(&self) -> TransportKind {
        self.transport.kind()
    }

    /// Return the measured PQ probe round-trip time, if [`Self::probe_pq`] was run.
    #[must_use]
    pub const fn ping_time(&self) -> Option<Duration> {
        self.ping_time
    }

    /// Total bytes read from the socket so far, including parts of packets that have not
    /// fully arrived yet. The session's receive watchdog treats any growth as "received
    /// some" (`AbstractConnection::receivedSome` fires on every socket read in tdesktop), so a
    /// large reply arriving slowly does not count as a silent connection.
    #[must_use]
    pub const fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    /// Send a single MTProto packet payload over this connection.
    pub async fn send(&mut self, payload: &[u8]) -> Result<(), Error> {
        self.send_inner(payload, false).await
    }

    /// Send a single MTProto packet payload requesting a quick-ack bit if the transport supports it.
    pub async fn send_with_quick_ack(&mut self, payload: &[u8]) -> Result<(), Error> {
        self.send_inner(payload, true).await
    }

    async fn send_inner(&mut self, payload: &[u8], quick_ack: bool) -> Result<(), Error> {
        self.transport.set_quick_ack(quick_ack);
        let mut framed = Vec::with_capacity(payload.len() + 32);
        self.transport.pack(payload, &mut framed);
        if let Some(obf) = &mut self.obfuscated {
            obf.encrypt_outgoing(&mut framed);
        }
        let prefix = std::mem::take(&mut self.pending_init);
        self.stream.write_packet(&prefix, &framed).await
    }

    /// Receive the next complete MTProto packet payload, transparently skipping NOP frames
    /// and quick-ack notifications.
    pub async fn recv(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            match self.recv_frame().await? {
                ReceivedFrame::Payload(data) => return Ok(data),
                ReceivedFrame::QuickAck(_) => continue,
            }
        }
    }

    /// Receive the next MTProto frame (either a payload or a quick-ack token),
    /// transparently skipping NOP packets.
    pub async fn recv_frame(&mut self) -> Result<ReceivedFrame, Error> {
        let mut scratch = [0u8; 16 * 1024];
        loop {
            if self.read_pos < self.read_buf.len() {
                match self.transport.unpack(&self.read_buf[self.read_pos..]) {
                    Ok(unpacked) => {
                        let result = match unpacked.kind {
                            FrameKind::Payload => {
                                let slice = unpacked.slice(&self.read_buf[self.read_pos..]);
                                let data =
                                    if self.transport.kind() == TransportKind::PaddedIntermediate {
                                        PaddedIntermediate::strip_mtproto_padding(slice).to_vec()
                                    } else {
                                        slice.to_vec()
                                    };
                                Some(ReceivedFrame::Payload(data))
                            }
                            FrameKind::QuickAck(token) => Some(ReceivedFrame::QuickAck(token)),
                            FrameKind::Nop => None,
                        };
                        self.read_pos += unpacked.next_offset;
                        self.compact_read_buf();
                        if let Some(frame) = result {
                            return Ok(frame);
                        }
                        continue;
                    }
                    Err(Error::MissingBytes) => {}
                    Err(e) => return Err(e),
                }
            }

            let n = self.stream.read_chunk(&mut scratch).await?;
            if n == 0 {
                return Err(Error::Disconnected);
            }
            self.bytes_received = self.bytes_received.saturating_add(n as u64);
            if let Some(obf) = &mut self.obfuscated {
                obf.decrypt_incoming(&mut scratch[..n]);
            }
            self.read_buf.extend_from_slice(&scratch[..n]);
        }
    }

    /// Send an unencrypted `req_pq_multi` connectivity probe and verify the `resPQ` reply
    /// echoes the same 16-byte nonce, returning the measured round-trip time.
    ///
    /// Port of `TcpConnection::socketConnected` / `preparePQFake` / `readPQFakeReply`.
    pub async fn probe_pq(&mut self, probe_timeout: Duration) -> Result<Duration, Error> {
        let mut nonce = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce);
        let packet = build_unencrypted_req_pq_multi(nonce);
        let started = Instant::now();
        timeout(probe_timeout, async {
            self.send(&packet).await?;
            let reply = self.recv().await?;
            let res = parse_unencrypted_res_pq(&reply)?;
            if res.nonce != nonce {
                return Err(Error::ProbeFailed("resPQ nonce mismatch".into()));
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout)??;

        let rtt = started.elapsed();
        self.ping_time = Some(rtt);
        Ok(rtt)
    }

    fn compact_read_buf(&mut self) {
        if self.read_pos == self.read_buf.len() {
            self.read_buf.clear();
            self.read_pos = 0;
        } else if self.read_pos >= SMALL_BUFFER_SIZE {
            self.read_buf.drain(..self.read_pos);
            self.read_pos = 0;
        }
    }
}

/// Minimal parsed fields from an unencrypted `resPQ#05162463` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResPqProbe {
    /// Server message ID (`msg_id`).
    pub msg_id: i64,
    /// Echoed client 128-bit nonce.
    pub nonce: [u8; 16],
    /// Server 128-bit `server_nonce`.
    pub server_nonce: [u8; 16],
}

/// Build an unencrypted MTProto `req_pq_multi#be7e8ef1` packet (40 bytes):
/// `auth_key_id(0) || msg_id || length(20) || 0xbe7e8ef1 || nonce(16)`.
#[must_use]
pub fn build_unencrypted_req_pq_multi(nonce: [u8; 16]) -> Vec<u8> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let msg_id = (((now.as_secs() as i64) << 32)
        | (((now.subsec_nanos() as i64) << 2) & 0xFFFF_FFFC))
        & !3i64;
    build_unencrypted_req_pq_multi_with_msg_id(nonce, msg_id)
}

/// Build an unencrypted `req_pq_multi#be7e8ef1` packet with an explicit `msg_id`.
#[must_use]
pub fn build_unencrypted_req_pq_multi_with_msg_id(nonce: [u8; 16], msg_id: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&0u64.to_le_bytes()); // auth_key_id = 0
    out.extend_from_slice(&msg_id.to_le_bytes()); // msg_id
    out.extend_from_slice(&20u32.to_le_bytes()); // body length = 4 + 16
    out.extend_from_slice(&CONSTRUCTOR_REQ_PQ_MULTI.to_le_bytes()); // req_pq_multi
    out.extend_from_slice(&nonce);
    out
}

/// Parse and validate an unencrypted MTProto `resPQ#05162463` response packet.
///
/// Port of `AbstractConnection::parseNotSecureResponse` + `readPQFakeReply`.
pub fn parse_unencrypted_res_pq(packet: &[u8]) -> Result<ResPqProbe, Error> {
    if packet.len() < 56 {
        return Err(Error::ProbeFailed(format!(
            "unencrypted response too short: {} bytes",
            packet.len()
        )));
    }
    let auth_key_id = u64::from_le_bytes([
        packet[0], packet[1], packet[2], packet[3], packet[4], packet[5], packet[6], packet[7],
    ]);
    if auth_key_id != 0 {
        return Err(Error::ProbeFailed(format!(
            "expected auth_key_id 0, got 0x{auth_key_id:016x}"
        )));
    }
    let msg_id = i64::from_le_bytes([
        packet[8], packet[9], packet[10], packet[11], packet[12], packet[13], packet[14],
        packet[15],
    ]);
    if (msg_id & 0x03) != 1 {
        return Err(Error::ProbeFailed(format!(
            "bad unencrypted server msg_id low bits: 0x{msg_id:016x}"
        )));
    }
    let answer_len = u32::from_le_bytes([packet[16], packet[17], packet[18], packet[19]]) as usize;
    if answer_len < 36 || !answer_len.is_multiple_of(4) || answer_len > packet.len() - 20 {
        return Err(Error::ProbeFailed(format!(
            "invalid unencrypted message length {answer_len} for {} total bytes",
            packet.len()
        )));
    }
    let constructor = u32::from_le_bytes([packet[20], packet[21], packet[22], packet[23]]);
    if constructor != CONSTRUCTOR_RES_PQ {
        return Err(Error::ProbeFailed(format!(
            "expected resPQ (0x{CONSTRUCTOR_RES_PQ:08x}), got 0x{constructor:08x}"
        )));
    }
    let mut nonce = [0u8; 16];
    nonce.copy_from_slice(&packet[24..40]);
    let mut server_nonce = [0u8; 16];
    server_nonce.copy_from_slice(&packet[40..56]);

    Ok(ResPqProbe {
        msg_id,
        nonce,
        server_nonce,
    })
}
