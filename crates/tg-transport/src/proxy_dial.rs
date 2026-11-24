//! Async TCP dialing through direct connections, SOCKS5 proxies, and HTTP CONNECT proxies,
//! including multi-IP hostname resolution fallback.
//!
//! Port of proxy dialing from `mtproto/details/mtproto_abstract_socket.cpp`,
//! `mtproto/details/mtproto_tcp_socket.cpp`, and `mtproto/connection_resolving.cpp`.

use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpStream, lookup_host};
use tokio::time::timeout;

use crate::error::{Error, Socks5Reply};
use crate::proxy::{ProxyData, ProxyType};

/// Timeout per resolved IP attempt in `mtproto/connection_resolving.cpp` (`kOneConnectionTimeout`).
pub const ONE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(4);

/// Maximum HTTP CONNECT proxy response header size we buffer before rejecting.
const MAX_HTTP_CONNECT_HEADER_BYTES: usize = 8192;

/// Stream adapter that replays any extra bytes read past an HTTP CONNECT response header
/// before delegating reads directly to the underlying [`TcpStream`].
#[derive(Debug)]
pub struct PrefixedStream<S> {
    inner: S,
    prefix: Vec<u8>,
    prefix_pos: usize,
}

impl<S> PrefixedStream<S> {
    /// Wrap `inner` with an optional buffered `prefix` to be read first.
    #[must_use]
    pub fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            prefix,
            prefix_pos: 0,
        }
    }

    /// Borrow the underlying stream.
    #[must_use]
    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.prefix_pos < self.prefix.len() {
            let remaining = &self.prefix[self.prefix_pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.prefix_pos += n;
            if self.prefix_pos == self.prefix.len() {
                self.prefix.clear();
                self.prefix_pos = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Format `host:port` properly bracketing IPv6 literals.
#[must_use]
pub fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Connect directly over TCP to `host:port`, trying resolved IPs sequentially if `host` is
/// a hostname (with per-IP timeout when multiple IPs are returned, matching `ResolvingConnection`).
pub async fn dial_tcp(
    host: &str,
    port: u16,
    total_timeout: Duration,
    tcp_nodelay: bool,
) -> Result<TcpStream, Error> {
    let stream = if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        let addr = SocketAddr::new(ip, port);
        timeout(total_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| Error::Timeout)??
    } else {
        let addrs: Vec<SocketAddr> = timeout(total_timeout, lookup_host((host, port)))
            .await
            .map_err(|_| Error::Timeout)??
            .collect();
        if addrs.is_empty() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                format!("DNS lookup returned no addresses for {host}"),
            )));
        }
        let per_addr = if addrs.len() > 1 {
            ONE_CONNECTION_TIMEOUT.min(total_timeout)
        } else {
            total_timeout
        };
        let mut last_err: Option<Error> = None;
        let mut connected = None;
        for addr in addrs {
            match timeout(per_addr, TcpStream::connect(addr)).await {
                Ok(Ok(s)) => {
                    connected = Some(s);
                    break;
                }
                Ok(Err(e)) => last_err = Some(Error::Io(e)),
                Err(_) => last_err = Some(Error::Timeout),
            }
        }
        connected.ok_or_else(|| last_err.unwrap_or(Error::Timeout))?
    };

    if tcp_nodelay {
        let _ = stream.set_nodelay(true);
    }
    Ok(stream)
}

/// Dial a TCP tunnel to `target_host:target_port`, optionally via a SOCKS5 or HTTP CONNECT proxy.
///
/// For `ProxyType::Mtproto`, this dials the MTProxy server directly (`proxy.host:proxy.port`).
pub async fn dial_through_proxy(
    target_host: &str,
    target_port: u16,
    proxy: &ProxyData,
    connect_timeout: Duration,
    tcp_nodelay: bool,
) -> Result<PrefixedStream<TcpStream>, Error> {
    match proxy.proxy_type {
        ProxyType::None => {
            let stream = dial_tcp(target_host, target_port, connect_timeout, tcp_nodelay).await?;
            Ok(PrefixedStream::new(stream, Vec::new()))
        }
        ProxyType::Mtproto => {
            if !proxy.valid() {
                return Err(Error::InvalidProxy(format!(
                    "invalid MTProto proxy (status {:?})",
                    proxy.status()
                )));
            }
            let stream = dial_proxy_host(proxy, connect_timeout, tcp_nodelay).await?;
            Ok(PrefixedStream::new(stream, Vec::new()))
        }
        ProxyType::Socks5 => {
            if !proxy.valid() {
                return Err(Error::InvalidProxy("invalid SOCKS5 proxy".into()));
            }
            timeout(connect_timeout, async {
                let mut stream = dial_proxy_host(proxy, connect_timeout, tcp_nodelay).await?;
                socks5_handshake(
                    &mut stream,
                    target_host,
                    target_port,
                    &proxy.user,
                    &proxy.password,
                )
                .await?;
                Ok(PrefixedStream::new(stream, Vec::new()))
            })
            .await
            .map_err(|_| Error::Timeout)?
        }
        ProxyType::Http => {
            if !proxy.valid() {
                return Err(Error::InvalidProxy("invalid HTTP proxy".into()));
            }
            timeout(connect_timeout, async {
                let mut stream = dial_proxy_host(proxy, connect_timeout, tcp_nodelay).await?;
                let leftover = http_connect_handshake(
                    &mut stream,
                    target_host,
                    target_port,
                    &proxy.user,
                    &proxy.password,
                )
                .await?;
                Ok(PrefixedStream::new(stream, leftover))
            })
            .await
            .map_err(|_| Error::Timeout)?
        }
        ProxyType::Web => Err(Error::InvalidProxy(
            "Web proxy requires a WebView bridge transport".into(),
        )),
    }
}

/// Dial `proxy.host:proxy.port`, using `proxy.resolved_ips` if present (port of
/// `ResolvingConnection` IP iteration in `mtproto/connection_resolving.cpp`).
async fn dial_proxy_host(
    proxy: &ProxyData,
    connect_timeout: Duration,
    tcp_nodelay: bool,
) -> Result<TcpStream, Error> {
    if !proxy.resolved_ips.is_empty() {
        let mut last_err: Option<Error> = None;
        for ip in &proxy.resolved_ips {
            let per_ip = ONE_CONNECTION_TIMEOUT.min(connect_timeout);
            match dial_tcp(ip, proxy.port, per_ip, tcp_nodelay).await {
                Ok(stream) => return Ok(stream),
                Err(e) => last_err = Some(e),
            }
        }
        return Err(last_err.unwrap_or(Error::Timeout));
    }
    dial_tcp(&proxy.host, proxy.port, connect_timeout, tcp_nodelay).await
}

/// Perform a SOCKS5 CONNECT handshake (RFC 1928 + RFC 1929 username/password auth) on `stream`.
pub async fn socks5_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    target_host: &str,
    target_port: u16,
    user: &str,
    password: &str,
) -> Result<(), Error> {
    let has_auth = !user.is_empty() || !password.is_empty();
    if has_auth {
        if user.len() > 255 || password.len() > 255 {
            return Err(Error::InvalidProxy(
                "SOCKS5 username/password exceeds 255 bytes".into(),
            ));
        }
        stream.write_all(&[0x05, 0x02, 0x00, 0x02]).await?;
    } else {
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
    }
    stream.flush().await?;

    let mut method_reply = [0u8; 2];
    read_exact_mapped(stream, &mut method_reply).await?;
    if method_reply[0] != 0x05 {
        return Err(Error::Socks5(format!(
            "unexpected SOCKS version 0x{:02x}",
            method_reply[0]
        )));
    }
    match method_reply[1] {
        0x00 => {}
        0x02 => {
            if !has_auth {
                return Err(Error::ProxyAuthFailed);
            }
            let mut auth_req = Vec::with_capacity(3 + user.len() + password.len());
            auth_req.push(0x01); // subnegotiation version 1
            auth_req.push(user.len() as u8);
            auth_req.extend_from_slice(user.as_bytes());
            auth_req.push(password.len() as u8);
            auth_req.extend_from_slice(password.as_bytes());
            stream.write_all(&auth_req).await?;
            stream.flush().await?;

            let mut auth_reply = [0u8; 2];
            read_exact_mapped(stream, &mut auth_reply).await?;
            if auth_reply[0] != 0x01 || auth_reply[1] != 0x00 {
                return Err(Error::ProxyAuthFailed);
            }
        }
        0xFF => return Err(Error::ProxyAuthFailed),
        other => {
            return Err(Error::Socks5(format!(
                "unsupported SOCKS5 auth method 0x{other:02x}"
            )));
        }
    }

    // Build CONNECT request: VER=5, CMD=1 (CONNECT), RSV=0, ATYP, DST.ADDR, DST.PORT.
    let mut req = Vec::with_capacity(6 + target_host.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00]);
    let clean_host = target_host.trim_matches(['[', ']']);
    match clean_host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            req.push(0x01);
            req.extend_from_slice(&v4.octets());
        }
        Ok(IpAddr::V6(v6)) => {
            req.push(0x04);
            req.extend_from_slice(&v6.octets());
        }
        Err(_) => {
            if clean_host.is_empty() || clean_host.len() > 255 {
                return Err(Error::Socks5("invalid target domain length".into()));
            }
            req.push(0x03);
            req.push(clean_host.len() as u8);
            req.extend_from_slice(clean_host.as_bytes());
        }
    }
    req.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&req).await?;
    stream.flush().await?;

    let mut reply_head = [0u8; 4];
    read_exact_mapped(stream, &mut reply_head).await?;
    if reply_head[0] != 0x05 {
        return Err(Error::Socks5(format!(
            "unexpected SOCKS5 reply version 0x{:02x}",
            reply_head[0]
        )));
    }
    if reply_head[1] != 0x00 {
        return Err(Error::Socks5Reply(Socks5Reply::from_byte(reply_head[1])));
    }

    // Drain BND.ADDR + BND.PORT according to ATYP.
    match reply_head[3] {
        0x01 => {
            let mut skip = [0u8; 6];
            read_exact_mapped(stream, &mut skip).await?;
        }
        0x04 => {
            let mut skip = [0u8; 18];
            read_exact_mapped(stream, &mut skip).await?;
        }
        0x03 => {
            let mut len_buf = [0u8; 1];
            read_exact_mapped(stream, &mut len_buf).await?;
            let mut skip = vec![0u8; len_buf[0] as usize + 2];
            read_exact_mapped(stream, &mut skip).await?;
        }
        atyp => {
            return Err(Error::Socks5(format!(
                "unknown SOCKS5 bound address type 0x{atyp:02x}"
            )));
        }
    }

    Ok(())
}

/// Perform an HTTP CONNECT tunnel handshake on `stream`, returning any extra bytes read
/// past the terminating `\r\n\r\n` header delimiter.
pub async fn http_connect_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    target_host: &str,
    target_port: u16,
    user: &str,
    password: &str,
) -> Result<Vec<u8>, Error> {
    let authority = format_host_port(target_host.trim_matches(['[', ']']), target_port);
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if !user.is_empty() || !password.is_empty() {
        let token = STANDARD.encode(format!("{user}:{password}"));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");

    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut buf = Vec::with_capacity(256);
    let mut tmp = [0u8; 256];
    let header_end = loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(Error::Disconnected);
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_double_crlf(&buf) {
            break pos;
        }
        if buf.len() > MAX_HTTP_CONNECT_HEADER_BYTES {
            return Err(Error::HttpProxy {
                status: 0,
                reason: "HTTP CONNECT response header exceeded 8 KiB".into(),
            });
        }
    };

    let header_bytes = &buf[..header_end];
    let header_str = std::str::from_utf8(header_bytes).map_err(|_| Error::HttpProxy {
        status: 0,
        reason: "non-UTF-8 HTTP CONNECT response".into(),
    })?;
    let status_line = header_str.lines().next().unwrap_or("");
    let mut parts = status_line.split_whitespace();
    let http_ver = parts.next().unwrap_or("");
    let status_str = parts.next().unwrap_or("");
    let reason = parts.collect::<Vec<_>>().join(" ");
    if !http_ver.starts_with("HTTP/") {
        return Err(Error::HttpProxy {
            status: 0,
            reason: format!("malformed HTTP status line: {status_line}"),
        });
    }
    let status: u16 = status_str.parse().map_err(|_| Error::HttpProxy {
        status: 0,
        reason: format!("invalid HTTP status code: {status_str}"),
    })?;

    if status == 407 {
        return Err(Error::ProxyAuthFailed);
    }
    if !(200..=299).contains(&status) {
        return Err(Error::HttpProxy { status, reason });
    }

    let leftover = buf[header_end + 4..].to_vec();
    Ok(leftover)
}

pub(crate) fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

async fn read_exact_mapped<S: AsyncRead + Unpin>(
    stream: &mut S,
    buf: &mut [u8],
) -> Result<(), Error> {
    stream.read_exact(buf).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Disconnected
        } else {
            Error::Io(e)
        }
    })?;
    Ok(())
}
