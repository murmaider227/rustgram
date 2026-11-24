//! Plain HTTP POST `/api` MTProto transport.
//!
//! Port of `mtproto/connection_http.h/.cpp`.

use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::connection::{
    FULL_CONNECTION_TIMEOUT, build_unencrypted_req_pq_multi, parse_unencrypted_res_pq,
};
use crate::error::{Error, TransportErrorCode};
use crate::framing::MAX_PACKET_SIZE;
use crate::proxy::{ProxyData, ProxyType};
use crate::proxy_dial::{
    PrefixedStream, dial_tcp, find_double_crlf, format_host_port, socks5_handshake,
};

/// Default HTTP transport port (`kForceHttpPort` in `mtproto/connection_http.cpp`).
pub const FORCE_HTTP_PORT: u16 = 80;

/// Maximum HTTP response header size accepted by [`HttpConnection`].
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;

/// Configuration for [`HttpConnection`].
#[derive(Debug, Clone)]
pub struct HttpConnectionConfig {
    /// Port to connect to (defaults to [`FORCE_HTTP_PORT`] = 80, matching `connection_http.cpp`).
    pub port: u16,
    /// Optional SOCKS5 or HTTP proxy (`ProxyData::none()` for direct).
    pub proxy: ProxyData,
    /// Timeout for establishing the connection and waiting for each HTTP response.
    pub timeout: Duration,
}

impl Default for HttpConnectionConfig {
    fn default() -> Self {
        Self {
            port: FORCE_HTTP_PORT,
            proxy: ProxyData::none(),
            timeout: FULL_CONNECTION_TIMEOUT,
        }
    }
}

/// Plain HTTP POST `/api` MTProto transport (`MTP::details::HttpConnection` port).
///
/// Each [`Self::send`] posts raw MTProto bytes to `http://<address>:<port>/api` with
/// `Content-Type: application/x-www-form-urlencoded` and stores the HTTP response body
/// to be returned by [`Self::recv`] (or use [`Self::post`] for single-call request/response).
pub struct HttpConnection {
    address: String,
    config: HttpConnectionConfig,
    stream: Option<PrefixedStream<TcpStream>>,
    read_buf: Vec<u8>,
    pending_responses: std::collections::VecDeque<Vec<u8>>,
    tag: String,
    ping_time: Option<Duration>,
}

impl HttpConnection {
    /// Create a new HTTP transport targeting `address` (IPv4, IPv6, or hostname).
    ///
    /// Does not perform network I/O until the first [`Self::post`], [`Self::send`],
    /// or [`Self::probe_pq`].
    #[must_use]
    pub fn new(address: impl Into<String>, config: HttpConnectionConfig) -> Self {
        let address = address.into();
        let tag = if address.contains(':') {
            "HTTP/IPv6".to_owned()
        } else {
            "HTTP/IPv4".to_owned()
        };
        Self {
            address,
            config,
            stream: None,
            read_buf: Vec::new(),
            pending_responses: std::collections::VecDeque::new(),
            tag,
            ping_time: None,
        }
    }

    /// Return the connection transport tag (`"HTTP/IPv4"` or `"HTTP/IPv6"`).
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Return the measured PQ probe round-trip time, if [`Self::probe_pq`] succeeded.
    #[must_use]
    pub const fn ping_time(&self) -> Option<Duration> {
        self.ping_time
    }

    /// Return the canonical endpoint URL (`http://<ip>:<port>/api`), matching `HttpConnection::url()`.
    #[must_use]
    pub fn url(&self) -> String {
        format!(
            "http://{}/api",
            format_host_port(self.address.trim_matches(['[', ']']), self.config.port)
        )
    }

    /// POST raw MTProto packet bytes and return the response body (if non-empty).
    pub async fn post(&mut self, payload: &[u8]) -> Result<Vec<u8>, Error> {
        timeout(self.config.timeout, self.post_inner(payload))
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// Send a packet via HTTP POST and queue its response for [`Self::recv`].
    pub async fn send(&mut self, payload: &[u8]) -> Result<(), Error> {
        let response = self.post(payload).await?;
        if !response.is_empty() {
            self.pending_responses.push_back(response);
        }
        Ok(())
    }

    /// Return the next queued HTTP response body.
    pub fn recv(&mut self) -> Option<Vec<u8>> {
        self.pending_responses.pop_front()
    }

    /// Send an unencrypted `req_pq_multi` probe over HTTP POST and verify `resPQ`.
    pub async fn probe_pq(&mut self) -> Result<Duration, Error> {
        let mut nonce = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce);
        let packet = build_unencrypted_req_pq_multi(nonce);
        let started = Instant::now();
        let reply = self.post(&packet).await?;
        let parsed = parse_unencrypted_res_pq(&reply)?;
        if parsed.nonce != nonce {
            return Err(Error::ProbeFailed("HTTP resPQ nonce mismatch".into()));
        }
        let rtt = started.elapsed();
        self.ping_time = Some(rtt);
        Ok(rtt)
    }

    async fn ensure_connected(&mut self) -> Result<(), Error> {
        if self.stream.is_some() {
            return Ok(());
        }
        let stream = match self.config.proxy.proxy_type {
            ProxyType::None => {
                let s =
                    dial_tcp(&self.address, self.config.port, self.config.timeout, true).await?;
                PrefixedStream::new(s, Vec::new())
            }
            ProxyType::Socks5 => {
                let mut s = dial_tcp(
                    &self.config.proxy.host,
                    self.config.proxy.port,
                    self.config.timeout,
                    true,
                )
                .await?;
                socks5_handshake(
                    &mut s,
                    &self.address,
                    self.config.port,
                    &self.config.proxy.user,
                    &self.config.proxy.password,
                )
                .await?;
                PrefixedStream::new(s, Vec::new())
            }
            ProxyType::Http => {
                // Plain HTTP over an HTTP proxy uses absolute-form request target directly on the proxy socket.
                let s = dial_tcp(
                    &self.config.proxy.host,
                    self.config.proxy.port,
                    self.config.timeout,
                    true,
                )
                .await?;
                PrefixedStream::new(s, Vec::new())
            }
            other => {
                return Err(Error::InvalidProxy(format!(
                    "HTTP transport cannot use proxy type {other:?}"
                )));
            }
        };
        self.stream = Some(stream);
        self.read_buf.clear();
        Ok(())
    }

    async fn post_inner(&mut self, payload: &[u8]) -> Result<Vec<u8>, Error> {
        let req_bytes = self.build_http_request(payload);

        // Retry once if a kept-alive connection was closed by the server before our write.
        for attempt in 0..2 {
            self.ensure_connected().await?;
            let write_res = {
                let stream = self.stream.as_mut().expect("just connected");
                async {
                    stream.write_all(&req_bytes).await?;
                    stream.flush().await
                }
                .await
            };
            if let Err(e) = write_res {
                self.stream = None;
                if attempt == 0 {
                    continue;
                }
                return Err(Error::Io(e));
            }
            match self.read_http_response().await {
                Ok(body) => return self.validate_response_body(body),
                Err(Error::Disconnected) if attempt == 0 => {
                    self.stream = None;
                    continue;
                }
                Err(e) => {
                    self.stream = None;
                    return Err(e);
                }
            }
        }
        Err(Error::Disconnected)
    }

    fn build_http_request(&self, payload: &[u8]) -> Vec<u8> {
        let host_header = format_host_port(self.address.trim_matches(['[', ']']), self.config.port);
        let target = if self.config.proxy.proxy_type == ProxyType::Http {
            self.url()
        } else {
            "/api".to_owned()
        };
        let mut head = format!(
            "POST {target} HTTP/1.1\r\n\
             Host: {host_header}\r\n\
             Content-Type: application/x-www-form-urlencoded\r\n\
             Content-Length: {}\r\n\
             Connection: keep-alive\r\n",
            payload.len()
        );
        if self.config.proxy.proxy_type == ProxyType::Http
            && (!self.config.proxy.user.is_empty() || !self.config.proxy.password.is_empty())
        {
            let auth = STANDARD.encode(format!(
                "{}:{}",
                self.config.proxy.user, self.config.proxy.password
            ));
            head.push_str(&format!("Proxy-Authorization: Basic {auth}\r\n"));
        }
        head.push_str("\r\n");

        let mut out = Vec::with_capacity(head.len() + payload.len());
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(payload);
        out
    }

    async fn read_http_response(&mut self) -> Result<Vec<u8>, Error> {
        let mut tmp = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = find_double_crlf(&self.read_buf) {
                break pos;
            }
            if self.read_buf.len() > MAX_HTTP_HEADER_BYTES {
                return Err(Error::Http("HTTP response header exceeded 16 KiB".into()));
            }
            let stream = self.stream.as_mut().ok_or(Error::Disconnected)?;
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                return Err(Error::Disconnected);
            }
            self.read_buf.extend_from_slice(&tmp[..n]);
        };

        let header_str = std::str::from_utf8(&self.read_buf[..header_end])
            .map_err(|_| Error::Http("non-UTF-8 HTTP response headers".into()))?
            .to_owned();
        self.read_buf.drain(..header_end + 4);

        let mut lines = header_str.lines();
        let status_line = lines.next().unwrap_or("");
        let mut status_parts = status_line.split_whitespace();
        let version = status_parts.next().unwrap_or("");
        let code_str = status_parts.next().unwrap_or("");
        if !version.starts_with("HTTP/") {
            return Err(Error::Http(format!(
                "malformed HTTP status line: {status_line}"
            )));
        }
        let status: u16 = code_str
            .parse()
            .map_err(|_| Error::Http(format!("invalid HTTP status code: {code_str}")))?;

        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        let mut connection_close = false;
        for line in lines {
            if let Some((name, val)) = line.split_once(':') {
                let name_lower = name.trim().to_ascii_lowercase();
                let val_trim = val.trim();
                match name_lower.as_str() {
                    "content-length" => {
                        content_length = Some(
                            val_trim
                                .parse::<usize>()
                                .map_err(|_| Error::Http("invalid Content-Length".into()))?,
                        );
                    }
                    "transfer-encoding" => {
                        if val_trim.to_ascii_lowercase().contains("chunked") {
                            chunked = true;
                        }
                    }
                    "connection" if val_trim.eq_ignore_ascii_case("close") => {
                        connection_close = true;
                    }
                    _ => {}
                }
            }
        }

        let body = if chunked {
            self.read_chunked_body().await?
        } else if let Some(len) = content_length {
            if len >= MAX_PACKET_SIZE {
                return Err(Error::InvalidPacketLength(len));
            }
            self.read_exact_buffered(len).await?
        } else {
            // Read until EOF.
            connection_close = true;
            let mut body = std::mem::take(&mut self.read_buf);
            let stream = self.stream.as_mut().ok_or(Error::Disconnected)?;
            loop {
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                if body.len() + n >= MAX_PACKET_SIZE {
                    return Err(Error::InvalidPacketLength(body.len() + n));
                }
                body.extend_from_slice(&tmp[..n]);
            }
            body
        };

        if connection_close {
            self.stream = None;
            self.read_buf.clear();
        }

        if status != 200 {
            // Port of HttpConnection::handleError: negative HTTP status code.
            return Err(Error::ServerError(TransportErrorCode::from_code(
                -(status as i32),
            )));
        }

        Ok(body)
    }

    async fn read_exact_buffered(&mut self, len: usize) -> Result<Vec<u8>, Error> {
        let mut tmp = [0u8; 8192];
        while self.read_buf.len() < len {
            let stream = self.stream.as_mut().ok_or(Error::Disconnected)?;
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                return Err(Error::Disconnected);
            }
            self.read_buf.extend_from_slice(&tmp[..n]);
        }
        let body: Vec<u8> = self.read_buf.drain(..len).collect();
        Ok(body)
    }

    async fn read_chunked_body(&mut self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        loop {
            let line = self.read_crlf_line().await?;
            let size_hex = line.split(';').next().unwrap_or("").trim();
            let chunk_size = usize::from_str_radix(size_hex, 16)
                .map_err(|_| Error::Http(format!("invalid chunk size: {size_hex}")))?;
            if chunk_size == 0 {
                // Consume trailing CRLF (or trailer lines ending in empty line).
                loop {
                    let trailer = self.read_crlf_line().await?;
                    if trailer.is_empty() {
                        break;
                    }
                }
                break;
            }
            if out.len().saturating_add(chunk_size) >= MAX_PACKET_SIZE {
                return Err(Error::InvalidPacketLength(out.len() + chunk_size));
            }
            let chunk = self.read_exact_buffered(chunk_size).await?;
            out.extend_from_slice(&chunk);
            let crlf = self.read_exact_buffered(2).await?;
            if crlf != b"\r\n" {
                return Err(Error::Http("missing CRLF after chunk body".into()));
            }
        }
        Ok(out)
    }

    async fn read_crlf_line(&mut self) -> Result<String, Error> {
        let mut tmp = [0u8; 1024];
        loop {
            if let Some(pos) = self.read_buf.windows(2).position(|w| w == b"\r\n") {
                let line = std::str::from_utf8(&self.read_buf[..pos])
                    .map_err(|_| Error::Http("non-UTF-8 chunk line".into()))?
                    .to_owned();
                self.read_buf.drain(..pos + 2);
                return Ok(line);
            }
            if self.read_buf.len() > MAX_HTTP_HEADER_BYTES {
                return Err(Error::Http("chunk header line too long".into()));
            }
            let stream = self.stream.as_mut().ok_or(Error::Disconnected)?;
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                return Err(Error::Disconnected);
            }
            self.read_buf.extend_from_slice(&tmp[..n]);
        }
    }

    fn validate_response_body(&self, body: Vec<u8>) -> Result<Vec<u8>, Error> {
        if body.is_empty() {
            return Ok(body);
        }
        if body.len() == 4 {
            let code = i32::from_le_bytes([body[0], body[1], body[2], body[3]]);
            if code < 0 {
                return Err(Error::ServerError(TransportErrorCode::from_code(code)));
            }
        }
        // Port of HttpConnection::handleResponse: response.size() & 0x03 || response.size() < 8 -> -500.
        if !body.len().is_multiple_of(4) || body.len() < 8 {
            return Err(Error::ServerError(TransportErrorCode::InternalServerError));
        }
        Ok(body)
    }
}
