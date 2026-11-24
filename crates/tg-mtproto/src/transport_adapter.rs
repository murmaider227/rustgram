//! Thin adapter bridging `tg-transport` (`Connection`, `HttpConnection`, `ProxyData`,
//! `race_connections`) and in-memory test transports into `tg-mtproto`'s session runner.
//!
//! Port of connection candidate building from `mtproto/session_private.cpp` (`connectToServer`
//! and `appendTestConnection`) and pooled HTTP long-poll management from `mtproto/connection_http.cpp`.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tg_transport::PrefixedStream;
use tg_transport::{
    Connection, ConnectionConfig, Error as TransportError, FORCE_HTTP_PORT, HttpConnection,
    HttpConnectionConfig, ProxyData, ProxyType, RaceCandidate, RacedTransport, TransportKind,
    race_connections,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

use crate::config::ClientConfig;
use crate::dc_options::{DcId, DcOptions, DcType, protocol_dc_id};

/// HTTP POST timeout allowing `http_wait(100, 30, 25000)` long-poll responses to complete cleanly.
pub const HTTP_WAIT_TRANSPORT_TIMEOUT: Duration = Duration::from_secs(35);

/// Boxed future helper for object-safe async transport traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Async packet-oriented MTProto connection used by [`crate::SessionHandle`] and auth key exchange.
pub trait PacketConnection: Send + Sync {
    /// Sends one complete MTProto packet payload.
    fn send<'a>(&'a mut self, payload: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>>;

    /// Receives the next complete MTProto packet payload.
    ///
    /// HTTP long-poll transports may return `Ok(Vec::new())` when an empty 200 OK body arrives
    /// after an `http_wait` timeout, signaling that the caller should re-arm `http_wait` if idle.
    fn recv(&mut self) -> BoxFuture<'_, Result<Vec<u8>, TransportError>>;

    /// Closes the underlying connection and aborts any in-flight HTTP requests.
    fn close(&mut self) -> BoxFuture<'_, ()>;

    /// Returns `true` if this transport uses MTProto `http_wait` long-polling (`usingHttpWait`).
    fn using_http_wait(&self) -> bool {
        false
    }

    /// Returns `true` if this HTTP transport currently has no in-flight request (`needHttpWait`).
    fn need_http_wait(&self) -> bool {
        false
    }

    /// Human-readable transport tag (e.g. `"TCP/IPv4"`, `"HTTP/IPv4"`).
    fn transport_tag(&self) -> &str {
        "TCP"
    }
}

/// Factory that opens or races [`PacketConnection`] instances for a DC.
pub trait TransportFactory: Send + Sync {
    /// Connects directly to a single `ip:port` endpoint.
    fn connect<'a>(
        &'a self,
        ip: &'a str,
        port: u16,
        protocol_dc_id: i16,
        secret: &'a [u8],
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>>;

    /// Opens the best available connection to `bare_dc` using `dc_options` and `config`.
    ///
    /// Default implementation picks the first available endpoint and calls [`Self::connect`].
    /// [`TgTransportFactory`] overrides this to perform tdesktop-style concurrent racing via
    /// [`tg_transport::race_connections`].
    fn connect_dc<'a>(
        &'a self,
        bare_dc: DcId,
        dc_type: DcType,
        dc_options: &'a DcOptions,
        config: &'a ClientConfig,
        proxy: &'a ProxyData,
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            let through_proxy = proxy.proxy_type != ProxyType::None;
            let variants = dc_options.lookup(bare_dc, dc_type, through_proxy);
            // Match `SessionPrivate::connectToServer` (`mtproto/session_private.cpp`):
            // never fall back to IPv6 when `use_ipv6` is disabled, and respect `prefer_ipv6`.
            let endpoint = if config.use_ipv6 {
                if config.prefer_ipv6 {
                    variants
                        .ipv6_tcp
                        .first()
                        .or_else(|| config.use_ipv4.then(|| variants.ipv4_tcp.first()).flatten())
                } else {
                    config
                        .use_ipv4
                        .then(|| variants.ipv4_tcp.first())
                        .flatten()
                        .or_else(|| variants.ipv6_tcp.first())
                }
            } else if config.use_ipv4 {
                variants.ipv4_tcp.first()
            } else {
                None
            }
            .cloned()
            .ok_or_else(|| {
                TransportError::AllCandidatesFailed(format!("no endpoints for DC {bare_dc}"))
            })?;
            let proto_dc = protocol_dc_id(bare_dc, dc_type, dc_options.is_test_mode());
            self.connect(&endpoint.ip, endpoint.port, proto_dc, &endpoint.secret)
                .await
        })
    }
}

/// Wraps a real [`tg_transport::Connection`] (TCP + framing + Obfuscated2 + proxy/fake-TLS).
pub struct TgTcpConnection<S = PrefixedStream<TcpStream>> {
    inner: Connection<S>,
}

impl<S> TgTcpConnection<S> {
    /// Wraps an established [`tg_transport::Connection`].
    pub fn new(inner: Connection<S>) -> Self {
        Self { inner }
    }
}

impl<S> PacketConnection for TgTcpConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync,
{
    fn send<'a>(&'a mut self, payload: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move { self.inner.send(payload).await })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Vec<u8>, TransportError>> {
        Box::pin(async move { self.inner.recv().await })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {})
    }

    fn transport_tag(&self) -> &str {
        self.inner.tag()
    }
}

/// Pooled non-blocking adapter over [`tg_transport::HttpConnection`] supporting concurrent
/// HTTP POST requests (`http_wait` long-poll alongside immediate outgoing RPCs, matching
/// `QNetworkAccessManager`'s `_requests` set in `mtproto/connection_http.cpp`).
pub struct TgHttpConnection {
    address: String,
    config: HttpConnectionConfig,
    idle: Vec<HttpConnection>,
    in_flight: JoinSet<(Result<Vec<u8>, TransportError>, HttpConnection)>,
    buffered: VecDeque<Vec<u8>>,
    tag: String,
    closed: bool,
}

impl TgHttpConnection {
    /// Wraps an already-probed [`HttpConnection`] together with its endpoint parameters.
    pub fn new(address: String, config: HttpConnectionConfig, initial: HttpConnection) -> Self {
        let tag = initial.tag().to_owned();
        Self {
            address,
            config,
            idle: vec![initial],
            in_flight: JoinSet::new(),
            buffered: VecDeque::new(),
            tag,
            closed: false,
        }
    }
}

impl PacketConnection for TgHttpConnection {
    fn send<'a>(&'a mut self, payload: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            if self.closed {
                return Err(TransportError::Disconnected);
            }
            let mut conn = self
                .idle
                .pop()
                .unwrap_or_else(|| HttpConnection::new(self.address.clone(), self.config.clone()));
            let body = payload.to_vec();
            self.in_flight.spawn(async move {
                let res = conn.post(&body).await;
                (res, conn)
            });
            Ok(())
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
            if let Some(pkt) = self.buffered.pop_front() {
                return Ok(pkt);
            }
            if self.closed {
                return Err(TransportError::Disconnected);
            }
            match self.in_flight.join_next().await {
                Some(Ok((Ok(body), conn))) => {
                    if self.idle.len() < 4 {
                        self.idle.push(conn);
                    }
                    Ok(body)
                }
                Some(Ok((Err(err), _conn))) => Err(err),
                Some(Err(_join_err)) => Err(TransportError::Disconnected),
                None => {
                    // No HTTP requests in flight; wait until cancelled by select! or return empty
                    // so the session loop can issue an http_wait.
                    Ok(Vec::new())
                }
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.closed = true;
            self.in_flight.abort_all();
            self.idle.clear();
            self.buffered.clear();
        })
    }

    fn using_http_wait(&self) -> bool {
        true
    }

    fn need_http_wait(&self) -> bool {
        !self.closed && self.in_flight.is_empty() && self.buffered.is_empty()
    }

    fn transport_tag(&self) -> &str {
        &self.tag
    }
}

/// Builds the full list of [`RaceCandidate`]s for connecting to `bare_dc`, faithfully mirroring
/// `SessionPrivate::connectToServer` and `SessionPrivate::appendTestConnection`
/// (`mtproto/session_private.cpp`).
pub fn build_race_candidates(
    bare_dc: DcId,
    dc_type: DcType,
    dc_options: &DcOptions,
    config: &ClientConfig,
    proxy: &ProxyData,
) -> Vec<RaceCandidate> {
    let proto_dc = protocol_dc_id(bare_dc, dc_type, dc_options.is_test_mode());
    let mut candidates = Vec::new();

    // MTProxy / Web proxy routes directly through the proxy host:port with proxy secret.
    if proxy.proxy_type == ProxyType::Mtproto {
        let targets: Vec<ProxyData> = if !proxy.resolved_ips.is_empty() {
            (0..proxy.resolved_ips.len())
                .map(|i| proxy.to_direct_ip(i))
                .collect()
        } else {
            vec![proxy.clone()]
        };
        for target_proxy in targets {
            let conn_cfg = ConnectionConfig {
                transport: config.transport,
                obfuscated: true,
                dc_id: proto_dc,
                protocol_secret: Vec::new(),
                proxy: target_proxy,
                connect_timeout: config.race_options.total_timeout,
                tcp_nodelay: true,
            };
            candidates.push(RaceCandidate::tcp(
                proxy.host.clone(),
                proxy.port,
                conn_cfg,
                config.prefer_ipv6,
                config.probe_pq_on_connect,
            ));
        }
        return candidates;
    }

    let through_proxy = proxy.proxy_type != ProxyType::None;
    let variants = dc_options.lookup(bare_dc, dc_type, through_proxy);

    let special = dc_type == DcType::Temporary;
    let use_ipv4 = if special { true } else { config.use_ipv4 };
    let use_ipv6 = if special { false } else { config.use_ipv6 };
    let use_tcp = if special { true } else { config.use_tcp };
    let use_http = if special { false } else { config.use_http };

    // Deduplicated list of TCP transports to race (primary first, then any configured fallbacks).
    let mut tcp_transports: Vec<TransportKind> =
        Vec::with_capacity(1 + config.fallback_transports.len());
    tcp_transports.push(config.transport);
    for &extra in &config.fallback_transports {
        if !tcp_transports.contains(&extra) {
            tcp_transports.push(extra);
        }
    }

    let add_tcp_endpoints =
        |out: &mut Vec<RaceCandidate>, endpoints: &[crate::dc_options::DcEndpoint]| {
            for ep in endpoints {
                for (t_idx, &transport_kind) in tcp_transports.iter().enumerate() {
                    // Full transport does not support obfuscated2; only allow it when obfuscated=false
                    // and no endpoint secret is set.
                    let obfuscated = if transport_kind == TransportKind::Full {
                        if !ep.secret.is_empty() {
                            continue;
                        }
                        false
                    } else {
                        config.obfuscated
                    };
                    let conn_cfg = ConnectionConfig {
                        transport: transport_kind,
                        obfuscated,
                        dc_id: proto_dc,
                        protocol_secret: ep.secret.clone(),
                        proxy: proxy.clone(),
                        connect_timeout: config.race_options.total_timeout,
                        tcp_nodelay: true,
                    };
                    let mut cand = RaceCandidate::tcp(
                        ep.ip.clone(),
                        ep.port,
                        conn_cfg,
                        config.prefer_ipv6,
                        config.probe_pq_on_connect,
                    );
                    // Keep tdesktop's base priority scale and give the primary transport a small bonus
                    // when racing multiple transports so the preferred framing wins within
                    // WAIT_FOR_BETTER_TIMEOUT.
                    if t_idx == 0 && tcp_transports.len() > 1 {
                        cand.priority += 1;
                    }
                    out.push(cand);
                }
            }
        };

    let add_http_endpoints =
        |out: &mut Vec<RaceCandidate>, endpoints: &[crate::dc_options::DcEndpoint]| {
            for ep in endpoints {
                let http_cfg = HttpConnectionConfig {
                    port: FORCE_HTTP_PORT,
                    proxy: proxy.clone(),
                    timeout: HTTP_WAIT_TRANSPORT_TIMEOUT,
                };
                out.push(RaceCandidate::http(
                    ep.ip.clone(),
                    http_cfg,
                    config.prefer_ipv6,
                    config.probe_pq_on_connect,
                ));
            }
        };

    if use_ipv4 {
        if use_tcp {
            add_tcp_endpoints(&mut candidates, &variants.ipv4_tcp);
        }
        if use_http {
            add_http_endpoints(&mut candidates, &variants.ipv4_http);
        }
    }
    if use_ipv6 {
        if use_tcp {
            add_tcp_endpoints(&mut candidates, &variants.ipv6_tcp);
        }
        if use_http {
            add_http_endpoints(&mut candidates, &variants.ipv6_http);
        }
    }

    candidates
}

/// Default [`TransportFactory`] backed by `tg-transport` (`Connection`, `HttpConnection`,
/// Obfuscated2, SOCKS5/HTTP/MTProxy, and `race_connections`).
#[derive(Debug, Clone)]
pub struct TgTransportFactory {
    transport: TransportKind,
    obfuscated: bool,
    proxy: ProxyData,
}

impl Default for TgTransportFactory {
    fn default() -> Self {
        Self {
            transport: TransportKind::Intermediate,
            obfuscated: true,
            proxy: ProxyData::none(),
        }
    }
}

impl TgTransportFactory {
    /// Creates a factory configured from a [`ClientConfig`].
    pub fn from_config(config: &ClientConfig) -> Self {
        Self {
            transport: config.transport,
            obfuscated: config.obfuscated,
            proxy: config.proxy.clone(),
        }
    }
}

impl TransportFactory for TgTransportFactory {
    fn connect<'a>(
        &'a self,
        ip: &'a str,
        port: u16,
        protocol_dc_id: i16,
        secret: &'a [u8],
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            if self.proxy.proxy_type == ProxyType::Web {
                return crate::web_proxy::connect_via_active_web_proxy(
                    &self.proxy,
                    protocol_dc_id,
                    self.transport,
                )
                .await;
            }
            let cfg = ConnectionConfig {
                transport: self.transport,
                obfuscated: self.obfuscated,
                dc_id: protocol_dc_id,
                protocol_secret: secret.to_vec(),
                proxy: self.proxy.clone(),
                ..ConnectionConfig::default()
            };
            let conn = Connection::connect(ip, port, cfg).await?;
            Ok(Box::new(TgTcpConnection::new(conn)) as Box<dyn PacketConnection>)
        })
    }

    fn connect_dc<'a>(
        &'a self,
        bare_dc: DcId,
        dc_type: DcType,
        dc_options: &'a DcOptions,
        config: &'a ClientConfig,
        proxy: &'a ProxyData,
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            if proxy.proxy_type == ProxyType::Web {
                let proto_dc = protocol_dc_id(bare_dc, dc_type, dc_options.is_test_mode());
                return crate::web_proxy::connect_via_active_web_proxy(
                    proxy,
                    proto_dc,
                    config.transport,
                )
                .await;
            }
            let candidates = build_race_candidates(bare_dc, dc_type, dc_options, config, proxy);
            if candidates.is_empty() {
                return Err(TransportError::AllCandidatesFailed(format!(
                    "no connection candidates for DC {bare_dc} ({dc_type:?})"
                )));
            }
            // Capture endpoint metadata for HTTP winners before handing candidates to race_connections.
            let http_meta: Vec<Option<(String, HttpConnectionConfig)>> = candidates
                .iter()
                .map(|c| match &c.endpoint {
                    tg_transport::RaceEndpoint::Http { address, config } => {
                        Some((address.clone(), config.clone()))
                    }
                    tg_transport::RaceEndpoint::Tcp { .. } => None,
                })
                .collect();

            let winner = race_connections(candidates, config.race_options).await?;
            match winner.transport {
                RacedTransport::Tcp(conn) => {
                    Ok(Box::new(TgTcpConnection::new(conn)) as Box<dyn PacketConnection>)
                }
                RacedTransport::Http(http_conn) => {
                    let (addr, http_cfg) = http_meta
                        .get(winner.candidate_index)
                        .and_then(|m| m.clone())
                        .unwrap_or_else(|| {
                            (
                                String::new(),
                                HttpConnectionConfig {
                                    port: FORCE_HTTP_PORT,
                                    proxy: proxy.clone(),
                                    timeout: HTTP_WAIT_TRANSPORT_TIMEOUT,
                                },
                            )
                        });
                    Ok(Box::new(TgHttpConnection::new(addr, http_cfg, http_conn))
                        as Box<dyn PacketConnection>)
                }
            }
        })
    }
}

// ============================================================================
// In-memory mock transport for deterministic unit & integration tests
// ============================================================================

/// Server-side handle for an in-memory [`FakeConnection`].
pub struct FakeServerPeer {
    /// Receives packets sent by the client.
    pub rx: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Sends packets (or simulated transport errors) back to the client.
    pub tx: mpsc::UnboundedSender<Result<Vec<u8>, TransportError>>,
}

/// In-memory [`PacketConnection`] backed by unbounded tokio channels.
pub struct FakeConnection {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    rx: mpsc::UnboundedReceiver<Result<Vec<u8>, TransportError>>,
    closed: bool,
    http_wait: bool,
    in_flight_http: usize,
}

impl FakeConnection {
    /// Creates a connected `(client_conn, server_peer)` pair simulating a TCP transport.
    pub fn pair() -> (Self, FakeServerPeer) {
        Self::pair_with_http_wait(false)
    }

    /// Creates a connected `(client_conn, server_peer)` pair simulating an HTTP `http_wait`
    /// long-poll transport.
    pub fn pair_http() -> (Self, FakeServerPeer) {
        Self::pair_with_http_wait(true)
    }

    fn pair_with_http_wait(http_wait: bool) -> (Self, FakeServerPeer) {
        let (c2s_tx, c2s_rx) = mpsc::unbounded_channel();
        let (s2c_tx, s2c_rx) = mpsc::unbounded_channel();
        (
            Self {
                tx: c2s_tx,
                rx: s2c_rx,
                closed: false,
                http_wait,
                in_flight_http: 0,
            },
            FakeServerPeer {
                rx: c2s_rx,
                tx: s2c_tx,
            },
        )
    }
}

impl PacketConnection for FakeConnection {
    fn send<'a>(&'a mut self, payload: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            if self.closed {
                return Err(TransportError::Disconnected);
            }
            if self.http_wait {
                self.in_flight_http = self.in_flight_http.saturating_add(1);
            }
            self.tx
                .send(payload.to_vec())
                .map_err(|_| TransportError::Disconnected)
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
            if self.closed {
                return Err(TransportError::Disconnected);
            }
            match self.rx.recv().await {
                Some(res) => {
                    if self.http_wait {
                        self.in_flight_http = self.in_flight_http.saturating_sub(1);
                    }
                    res
                }
                None => Err(TransportError::Disconnected),
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.closed = true;
        })
    }

    fn using_http_wait(&self) -> bool {
        self.http_wait
    }

    fn need_http_wait(&self) -> bool {
        self.http_wait && !self.closed && self.in_flight_http == 0
    }

    fn transport_tag(&self) -> &str {
        if self.http_wait {
            "HTTP/Fake"
        } else {
            "TCP/Fake"
        }
    }
}

type QueuedConnection = Result<Box<dyn PacketConnection>, TransportError>;

/// Scriptable in-memory [`TransportFactory`] that yields pre-queued [`FakeConnection`]s.
#[derive(Clone, Default)]
pub struct FakeTransportFactory {
    queue: Arc<Mutex<VecDeque<QueuedConnection>>>,
    connect_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl FakeTransportFactory {
    /// Creates a new empty factory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of `connect()` attempts observed by this factory.
    pub fn connect_calls(&self) -> usize {
        self.connect_calls
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Enqueues a new in-memory TCP connection and returns its server-side peer handle.
    pub async fn push_connection(&self) -> FakeServerPeer {
        let (conn, peer) = FakeConnection::pair();
        self.queue
            .lock()
            .await
            .push_back(Ok(Box::new(conn) as Box<dyn PacketConnection>));
        peer
    }

    /// Enqueues a new in-memory HTTP (`http_wait`) connection and returns its server-side peer.
    pub async fn push_http_connection(&self) -> FakeServerPeer {
        let (conn, peer) = FakeConnection::pair_http();
        self.queue
            .lock()
            .await
            .push_back(Ok(Box::new(conn) as Box<dyn PacketConnection>));
        peer
    }

    /// Enqueues a connection error for the next `connect()` attempt.
    pub async fn push_error(&self, err: TransportError) {
        self.queue.lock().await.push_back(Err(err));
    }
}

impl TransportFactory for FakeTransportFactory {
    fn connect<'a>(
        &'a self,
        _ip: &'a str,
        _port: u16,
        _protocol_dc_id: i16,
        _secret: &'a [u8],
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        self.connect_calls
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Box::pin(async move {
            let mut guard = self.queue.lock().await;
            match guard.pop_front() {
                Some(res) => res,
                None => Err(TransportError::Disconnected),
            }
        })
    }
}
