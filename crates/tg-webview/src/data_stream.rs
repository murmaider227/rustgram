//! Custom-scheme resource streaming, HTTP `Range` parsing, and request routing.
//!
//! Port of `Telegram/lib_webview/webview/webview_data_stream.h`,
//! `Telegram/lib_webview/webview/webview_data_stream_memory.h/.cpp`,
//! `Telegram/lib_webview/webview/webview_embed.cpp` (`ParseRangeHeaderFor`), and the
//! `WebResourceRequested` routing logic in
//! `Telegram/lib_webview/webview/platform/win/webview_windows_edge_chromium.cpp`.

use std::collections::HashMap;
use std::io::{self, SeekFrom};
use std::sync::Arc;

/// Default custom URL scheme registered by Rustgram (analogous to tdesktop's internal scheme).
pub const DEFAULT_CUSTOM_SCHEME: &str = "rustgram-internal";

/// Default URL prefix used by [`crate::WebView::navigate_to_data`] on Rustgram.
pub const DEFAULT_DATA_URL_PREFIX: &str = "rustgram-internal://app/";

/// Legacy tdesktop Windows WebView2 local resource prefix (`webview_windows_edge_chromium.cpp`).
pub const LEGACY_DATA_URL_PREFIX: &str = "http://desktop-app-resource/";

/// Seekable, read-only byte stream returned to the WebView2 `IStream` bridge.
///
/// Mirrors `Webview::DataStream` from `webview_data_stream.h`.
pub trait DataStream: Send {
    /// Total byte length of this stream (or `< 0` if unknown).
    fn size(&self) -> i64;

    /// MIME type reported in the `Content-Type` response header (e.g. `"text/html; charset=utf-8"`).
    fn mime(&self) -> &str;

    /// Seeks to `pos` and returns the new offset from the start of the stream.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;

    /// Reads up to `buf.len()` bytes into `buf`, returning the number of bytes read (`0` at EOF).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

/// In-memory [`DataStream`] backed by a byte buffer.
///
/// Direct port of `Webview::DataStreamFromMemory` (`webview_data_stream_memory.cpp`).
#[derive(Debug, Clone)]
pub struct MemoryDataStream {
    data: Vec<u8>,
    mime: String,
    offset: usize,
}

impl MemoryDataStream {
    /// Creates a new in-memory stream with the given `data` and `mime` type.
    #[must_use]
    pub fn new(data: impl Into<Vec<u8>>, mime: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            mime: mime.into(),
            offset: 0,
        }
    }

    /// Creates a UTF-8 HTML stream (`text/html; charset=utf-8`).
    #[must_use]
    pub fn html(html: impl Into<String>) -> Self {
        Self::new(html.into().into_bytes(), "text/html; charset=utf-8")
    }

    /// Returns the underlying byte slice.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Returns the current read offset.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl DataStream for MemoryDataStream {
    fn size(&self) -> i64 {
        i64::try_from(self.data.len()).unwrap_or(i64::MAX)
    }

    fn mime(&self) -> &str {
        &self.mime
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let len = i64::try_from(self.data.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "stream too large"))?;
        let current = i64::try_from(self.offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        let target = match pos {
            SeekFrom::Start(p) => i64::try_from(p)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
            SeekFrom::Current(delta) => current
                .checked_add(delta)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
            SeekFrom::End(delta) => {
                if delta > 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "seek past end of memory stream",
                    ));
                }
                len.checked_add(delta)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek underflow"))?
            }
        };
        if target < 0 || target > len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position out of bounds",
            ));
        }
        self.offset = usize::try_from(target)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek position overflow"))?;
        Ok(self.offset as u64)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.data.len().saturating_sub(self.offset);
        let to_copy = remaining.min(buf.len());
        if to_copy > 0 {
            buf[..to_copy].copy_from_slice(&self.data[self.offset..self.offset + to_copy]);
            self.offset += to_copy;
        }
        Ok(to_copy)
    }
}

/// Resolved payload for a local custom-scheme data request.
///
/// Mirrors `Webview::DataResponse` (`webview_interface.h`).
pub struct DataResponse {
    /// Body stream to serve (`None` results in `404 Not Found`).
    pub stream: Option<Box<dyn DataStream>>,
    /// Starting byte offset for partial content (`206 Partial Content`).
    pub stream_offset: i64,
    /// Total full resource size (`0` means same as `stream.size()`).
    pub total_size: i64,
}

impl DataResponse {
    /// Creates a full `200 OK` response from raw bytes and MIME type.
    #[must_use]
    pub fn from_bytes(data: impl Into<Vec<u8>>, mime: impl Into<String>) -> Self {
        let bytes = data.into();
        let total_size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        Self {
            stream: Some(Box::new(MemoryDataStream::new(bytes, mime))),
            stream_offset: 0,
            total_size,
        }
    }

    /// Creates a full `200 OK` HTML response.
    #[must_use]
    pub fn from_html(html: impl Into<String>) -> Self {
        Self::from_bytes(html.into().into_bytes(), "text/html; charset=utf-8")
    }

    /// Creates a `404 Not Found` empty response.
    #[must_use]
    pub fn not_found() -> Self {
        Self {
            stream: None,
            stream_offset: 0,
            total_size: 0,
        }
    }

    /// Slices an in-memory resource according to a [`DataRequest`]'s `offset` and `limit`.
    ///
    /// Returns a `206 Partial Content` response when a valid byte range was requested,
    /// or a full `200 OK` response when `request.offset == 0 && request.limit == 0`.
    #[must_use]
    pub fn from_bytes_for_request(
        data: &[u8],
        mime: impl Into<String>,
        request: &DataRequest,
    ) -> Self {
        let total = data.len();
        if request.offset == 0 && request.limit == 0 {
            return Self::from_bytes(data.to_vec(), mime);
        }
        let Ok(start) = usize::try_from(request.offset) else {
            return Self::not_found();
        };
        if start >= total {
            return Self::not_found();
        }
        let end = if request.limit < 0 {
            total
        } else if let Ok(limit) = usize::try_from(request.limit) {
            start.saturating_add(limit).min(total)
        } else {
            return Self::not_found();
        };
        if end <= start {
            return Self::not_found();
        }
        Self {
            stream: Some(Box::new(MemoryDataStream::new(
                data[start..end].to_vec(),
                mime,
            ))),
            stream_offset: request.offset,
            total_size: i64::try_from(total).unwrap_or(i64::MAX),
        }
    }
}

/// Callback used to asynchronously complete a [`DataResult::Pending`] resource request.
pub type DataRequestDone = Box<dyn FnOnce(DataResponse) + Send>;

/// Incoming custom-scheme data request.
///
/// Mirrors `Webview::DataRequest` (`webview_interface.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRequest {
    /// Resource identifier after the configured scheme/prefix (e.g. `"index.html"` or `"iv/page"`).
    pub id: String,
    /// Byte offset parsed from HTTP `Range` header (default `0`).
    pub offset: i64,
    /// Byte count limit parsed from HTTP `Range` header (`0` = full file, `< 0` = open-ended `bytes=offset-`).
    pub limit: i64,
}

impl DataRequest {
    /// Creates a full-resource request for `id`.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            offset: 0,
            limit: 0,
        }
    }
}

/// Synchronous or deferred result of handling a [`DataRequest`].
///
/// Mirrors `Webview::DataResult` (`webview_interface.h`).
pub enum DataResult {
    /// Response is immediately available.
    Done(DataResponse),
    /// Response will be delivered asynchronously via the [`DataRequestDone`] callback.
    Pending,
    /// Request failed or resource does not exist (`404 Not Found`).
    Failed,
}

/// Handler function invoked for custom-scheme resource requests.
pub type DataRequestHandler = Arc<dyn Fn(DataRequest, DataRequestDone) -> DataResult + Send + Sync>;

/// Parses an HTTP `Range` header into `(offset, limit)` following tdesktop's `ParseRangeHeaderFor`.
///
/// Rules matching `webview_embed.cpp:553-599`:
/// - Must start with `"bytes="` and contain `'-'`.
/// - `"bytes=0-499"` -> `Some((0, 500))` (`limit` is inclusive byte count).
/// - `"bytes=100-"` -> `Some((100, -1))` (open-ended to EOF).
/// - `"bytes=-500"` -> `Some((0, 501))` (tdesktop parses finish=500 -> limit=501, offset=0).
/// - Invalid ranges or `end < start` return `None`.
#[must_use]
pub fn parse_range_header(header: &str) -> Option<(i64, i64)> {
    let range = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = range.split_once('-')?;

    let mut limit: i64 = if !end_str.is_empty() {
        let parsed: i64 = end_str.parse().ok()?;
        if parsed < 0 {
            return None;
        }
        parsed.checked_add(1)? // 0-499 means first 500 bytes.
    } else {
        -1
    };

    let offset: i64 = if !start_str.is_empty() {
        let parsed: i64 = start_str.parse().ok()?;
        if parsed < 0 {
            return None;
        }
        if limit > 0 {
            limit = limit.checked_sub(parsed)?;
            if limit <= 0 {
                return None;
            }
        }
        parsed
    } else {
        0
    };

    Some((offset, limit))
}

/// Applies an HTTP `Range` header to `request`, leaving it unchanged if the header is unsupported.
///
/// Direct port of `Webview::ParseRangeHeaderFor` (`webview_embed.cpp:553-599`).
pub fn parse_range_header_for(request: &mut DataRequest, header: &str) -> bool {
    if let Some((offset, limit)) = parse_range_header(header) {
        request.offset = offset;
        request.limit = limit;
        true
    } else {
        tracing::debug!(header = %header, "Unsupported range header");
        false
    }
}

/// Structured HTTP response built from a [`DataResponse`] for WebView2 or unit tests.
pub struct HttpResponse {
    /// HTTP status code (`200`, `206`, `403`, or `404`).
    pub status_code: u16,
    /// HTTP reason phrase (`"OK"`, `"Partial Content"`, `"Forbidden"`, `"Not Found"`).
    pub reason_phrase: &'static str,
    /// Formatted header lines (`"Name: Value"` joined by `'\n'`, as expected by WebView2).
    pub headers: String,
    /// Body stream (`None` for 403/404 responses).
    pub stream: Option<Box<dyn DataStream>>,
}

impl HttpResponse {
    /// Builds a `403 Forbidden` response (used when restricted-origin policy blocks a request).
    #[must_use]
    pub fn forbidden() -> Self {
        Self {
            status_code: 403,
            reason_phrase: "Forbidden",
            headers: "Cache-Control: no-store".to_owned(),
            stream: None,
        }
    }

    /// Builds a `404 Not Found` response.
    #[must_use]
    pub fn not_found() -> Self {
        Self {
            status_code: 404,
            reason_phrase: "Not Found",
            headers: String::new(),
            stream: None,
        }
    }

    /// Converts a [`DataResponse`] into a `200 OK`, `206 Partial Content`, or `404 Not Found`
    /// response with the exact headers from `webview_windows_edge_chromium.cpp:813-850`.
    #[must_use]
    pub fn from_data_response(response: DataResponse) -> Self {
        let Some(stream) = response.stream else {
            return Self::not_found();
        };
        let length = stream.size();
        let offset = response.stream_offset;
        let total = if response.total_size != 0 {
            response.total_size
        } else {
            length
        };
        let partial = offset > 0 || total != length;
        let mut headers = format!(
            "Content-Type: {}\nAccess-Control-Allow-Origin: *\nAccept-Ranges: bytes\nCache-Control: no-store\nContent-Length: {}",
            stream.mime(),
            length,
        );
        if partial && length > 0 {
            headers.push_str(&format!(
                "\nContent-Range: bytes {}-{}/{}",
                offset,
                offset + length - 1,
                total
            ));
        }
        Self {
            status_code: if partial { 206 } else { 200 },
            reason_phrase: if partial { "Partial Content" } else { "OK" },
            headers,
            stream: Some(stream),
        }
    }
}

/// Static asset entry registered in a [`SchemeRouter`].
#[derive(Debug, Clone)]
struct StaticAsset {
    bytes: Arc<[u8]>,
    mime: String,
}

/// Prefix handler callback for dynamic route segments in [`SchemeRouter`].
pub type PrefixRouteHandler = Arc<dyn Fn(&str, &DataRequest) -> Option<DataResponse> + Send + Sync>;

/// Platform-independent router for custom-scheme (`rustgram-internal://...`) resource requests.
///
/// Supports:
/// - Exact path static assets (with automatic `Range` slicing for `206 Partial Content`),
/// - Dynamic prefix handlers (e.g. `iv/`, `bot/`, `assets/`),
/// - Fallback [`DataRequestHandler`] (for async/deferred media streaming),
/// - Matching both `rustgram-internal://<host>/<path>` and optional legacy `http://desktop-app-resource/<path>`.
#[derive(Clone)]
pub struct SchemeRouter {
    scheme: String,
    data_url_prefix: String,
    accept_legacy_prefix: bool,
    static_routes: HashMap<String, StaticAsset>,
    prefix_routes: Vec<(String, PrefixRouteHandler)>,
    fallback: Option<DataRequestHandler>,
}

impl Default for SchemeRouter {
    fn default() -> Self {
        Self::new(DEFAULT_CUSTOM_SCHEME)
    }
}

impl std::fmt::Debug for SchemeRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemeRouter")
            .field("scheme", &self.scheme)
            .field("data_url_prefix", &self.data_url_prefix)
            .field("accept_legacy_prefix", &self.accept_legacy_prefix)
            .field("static_route_count", &self.static_routes.len())
            .field("prefix_route_count", &self.prefix_routes.len())
            .field("has_fallback", &self.fallback.is_some())
            .finish()
    }
}

impl SchemeRouter {
    /// Creates a new router for `scheme` (e.g. `"rustgram-internal"`).
    ///
    /// By default, `data_url_prefix` is `"{scheme}://app/"` and legacy
    /// `http://desktop-app-resource/` URLs are also accepted.
    #[must_use]
    pub fn new(scheme: impl Into<String>) -> Self {
        let scheme = scheme.into().to_ascii_lowercase();
        let data_url_prefix = format!("{scheme}://app/");
        Self {
            scheme,
            data_url_prefix,
            accept_legacy_prefix: true,
            static_routes: HashMap::new(),
            prefix_routes: Vec::new(),
            fallback: None,
        }
    }

    /// Returns the registered custom scheme name (lowercase, without `"://"`).
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Returns the base URL prefix used when building local data URLs.
    #[must_use]
    pub fn data_url_prefix(&self) -> &str {
        &self.data_url_prefix
    }

    /// Overrides the base data URL prefix (e.g. `"rustgram-internal://demo/"`).
    pub fn set_data_url_prefix(&mut self, prefix: impl Into<String>) {
        self.data_url_prefix = prefix.into();
    }

    /// Controls whether legacy `http://desktop-app-resource/` URLs are also matched.
    pub fn set_accept_legacy_prefix(&mut self, accept: bool) {
        self.accept_legacy_prefix = accept;
    }

    /// Builds a full URL for a resource `id` under this router's [`Self::data_url_prefix`].
    #[must_use]
    pub fn url_for(&self, id: &str) -> String {
        let trimmed = id.trim_start_matches('/');
        if self.data_url_prefix.ends_with('/') {
            format!("{}{}", self.data_url_prefix, trimmed)
        } else {
            format!("{}/{}", self.data_url_prefix, trimmed)
        }
    }

    /// Registers a static in-memory resource at `id` (e.g. `"index.html"` or `"demo/app.js"`).
    pub fn add_static(
        &mut self,
        id: impl Into<String>,
        mime: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) {
        let key = id.into().trim_start_matches('/').to_owned();
        self.static_routes.insert(
            key,
            StaticAsset {
                bytes: Arc::from(bytes.into()),
                mime: mime.into(),
            },
        );
    }

    /// Registers a UTF-8 HTML document at `id`.
    pub fn add_html(&mut self, id: impl Into<String>, html: impl Into<String>) {
        self.add_static(id, "text/html; charset=utf-8", html.into().into_bytes());
    }

    /// Registers a dynamic handler for resource IDs starting with `prefix` (e.g. `"iv/"`).
    pub fn add_prefix_handler<F>(&mut self, prefix: impl Into<String>, handler: F)
    where
        F: Fn(&str, &DataRequest) -> Option<DataResponse> + Send + Sync + 'static,
    {
        let key = prefix.into().trim_start_matches('/').to_owned();
        self.prefix_routes.push((key, Arc::new(handler)));
    }

    /// Sets a fallback handler invoked when no static or prefix route matches.
    pub fn set_fallback(&mut self, handler: DataRequestHandler) {
        self.fallback = Some(handler);
    }

    /// Extracts the normalized resource ID if `uri` belongs to this router's custom scheme
    /// (or legacy `http://desktop-app-resource/` when enabled).
    ///
    /// Strips query strings and fragments for route matching while preserving host/path when
    /// navigating to `{scheme}://<host>/<path>` outside `data_url_prefix`.
    #[must_use]
    pub fn extract_resource_id(&self, uri: &str) -> Option<String> {
        let without_fragment = uri.split('#').next().unwrap_or(uri);
        let without_query = without_fragment
            .split('?')
            .next()
            .unwrap_or(without_fragment);

        if let Some(rest) = strip_prefix_ascii_case(without_query, &self.data_url_prefix) {
            return Some(rest.trim_start_matches('/').to_owned());
        }
        let scheme_prefix = format!("{}://", self.scheme);
        if let Some(rest) = strip_prefix_ascii_case(without_query, &scheme_prefix) {
            return Some(rest.trim_start_matches('/').to_owned());
        }
        if self.accept_legacy_prefix
            && let Some(rest) = strip_prefix_ascii_case(without_query, LEGACY_DATA_URL_PREFIX)
        {
            return Some(rest.trim_start_matches('/').to_owned());
        }
        None
    }

    /// Returns the WebView2 `AddWebResourceRequestedFilter` URI patterns needed for this router.
    #[must_use]
    pub fn filter_patterns(&self) -> Vec<String> {
        let mut patterns = vec![format!("{}://*", self.scheme)];
        if self.accept_legacy_prefix {
            patterns.push(format!("{LEGACY_DATA_URL_PREFIX}*"));
        }
        patterns
    }

    /// Resolves `uri` synchronously against static and prefix routes.
    ///
    /// Returns `None` if `uri` does not match this router's custom scheme.
    #[must_use]
    pub fn route_sync(&self, uri: &str, range_header: Option<&str>) -> Option<HttpResponse> {
        let id = self.extract_resource_id(uri)?;
        let mut req = DataRequest::new(id);
        if let Some(range) = range_header {
            parse_range_header_for(&mut req, range);
        }
        match self.dispatch_request(req, Box::new(|_| {})) {
            DataResult::Done(resp) => Some(HttpResponse::from_data_response(resp)),
            DataResult::Pending | DataResult::Failed => Some(HttpResponse::not_found()),
        }
    }

    /// Dispatches a parsed [`DataRequest`] through static routes, prefix handlers, and fallback.
    pub fn dispatch_request(&self, request: DataRequest, done: DataRequestDone) -> DataResult {
        if let Some(asset) = self.static_routes.get(&request.id) {
            let response =
                DataResponse::from_bytes_for_request(&asset.bytes, asset.mime.clone(), &request);
            return DataResult::Done(response);
        }

        for (prefix, handler) in &self.prefix_routes {
            if let Some(rest) = request.id.strip_prefix(prefix.as_str())
                && let Some(response) = handler(rest, &request)
            {
                return DataResult::Done(response);
            }
        }

        if let Some(fallback) = &self.fallback {
            return fallback(request, done);
        }

        DataResult::Failed
    }
}

/// Guesses a sensible MIME type from a resource path extension.
#[must_use]
pub fn mime_from_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn strip_prefix_ascii_case<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    if input.len() < prefix.len() || !input.is_char_boundary(prefix.len()) {
        return None;
    }
    if input[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&input[prefix.len()..])
    } else {
        None
    }
}
