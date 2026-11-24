//! Local custom-scheme resource server for Instant View WebViews.
//!
//! Port of the resource serving and HTTP `Range` request handling in
//! `lib_webview/webview/webview_interface.h` (`ParseRangeHeaderFor`) and
//! `lib_webview/webview/platform/win/webview_windows_edge_chromium.cpp`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::assets::{PAGE_CSS, PAGE_JS, RICH_MESSAGE_CSS, RICH_MESSAGE_JS};
use crate::cached_media::IvCachedMedia;
use crate::prepare::{PreparedPage, escape_html};

/// Default `Content-Security-Policy` applied to served Instant View HTML documents.
pub const DEFAULT_IV_CSP: &str = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; frame-src https:; object-src 'none'; base-uri 'none'; form-action 'none';";

/// HTTP-like response returned by [`IvServer::serve`] for custom-scheme WebView handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceResponse {
    /// HTTP status code (`200`, `206`, `403`, `404`, `416`).
    pub status: u16,
    /// `Content-Type` header value.
    pub content_type: String,
    /// Raw response body bytes.
    pub body: Vec<u8>,
    /// Additional HTTP headers (e.g. `Content-Security-Policy`, `Accept-Ranges`, `Content-Range`).
    pub headers: Vec<(String, String)>,
}

impl ResourceResponse {
    /// Creates a `200 OK` response.
    pub fn ok(content_type: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: content_type.into(),
            body: body.into(),
            headers: Vec::new(),
        }
    }

    /// Creates a `404 Not Found` plain-text response.
    pub fn not_found() -> Self {
        Self {
            status: 404,
            content_type: "text/plain; charset=utf-8".to_string(),
            body: b"Not Found".to_vec(),
            headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
        }
    }

    /// Creates a `403 Forbidden` plain-text response.
    pub fn forbidden() -> Self {
        Self {
            status: 403,
            content_type: "text/plain; charset=utf-8".to_string(),
            body: b"Forbidden".to_vec(),
            headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
        }
    }

    /// Appends a response header and returns `self`.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Binary media payload returned by a [`MediaProvider`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaBlob {
    /// MIME type (e.g. `"image/jpeg"`, `"video/mp4"`, `"audio/mpeg"`).
    pub content_type: String,
    /// Raw media bytes.
    pub bytes: Vec<u8>,
}

/// Source of downloaded Telegram photos, documents, and map tiles for [`IvServer`].
pub trait MediaProvider {
    /// Returns bytes for a photo ID, if cached/downloaded.
    fn photo(&self, id: i64) -> Option<MediaBlob>;

    /// Returns bytes for a document (video/audio/inline image/file) ID, if cached/downloaded.
    fn document(&self, id: i64) -> Option<MediaBlob>;

    /// Returns bytes for a map tile snapshot, if available.
    fn map_tile(
        &self,
        _lat: f64,
        _lon: f64,
        _zoom: i32,
        _width: i32,
        _height: i32,
    ) -> Option<MediaBlob> {
        None
    }
}

/// Simple in-memory [`MediaProvider`] useful for tests, previews, and small caches.
#[derive(Debug, Default, Clone)]
pub struct MemoryMediaStore {
    photos: HashMap<i64, MediaBlob>,
    documents: HashMap<i64, MediaBlob>,
}

impl MemoryMediaStore {
    /// Creates an empty [`MemoryMediaStore`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a photo blob.
    pub fn insert_photo(
        &mut self,
        id: i64,
        content_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) {
        self.photos.insert(
            id,
            MediaBlob {
                content_type: content_type.into(),
                bytes: bytes.into(),
            },
        );
    }

    /// Inserts a document (video/audio/file/inline-image) blob.
    pub fn insert_document(
        &mut self,
        id: i64,
        content_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) {
        self.documents.insert(
            id,
            MediaBlob {
                content_type: content_type.into(),
                bytes: bytes.into(),
            },
        );
    }
}

impl MediaProvider for MemoryMediaStore {
    fn photo(&self, id: i64) -> Option<MediaBlob> {
        self.photos.get(&id).cloned()
    }

    fn document(&self, id: i64) -> Option<MediaBlob> {
        self.documents.get(&id).cloned()
    }
}

/// Result of parsing an HTTP `Range` header against a resource size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeParse {
    /// Header is absent or not a `bytes=` range; serve the full resource (`200 OK`).
    None,
    /// Valid inclusive byte range `(start, end)` (`206 Partial Content`).
    Satisfied(usize, usize),
    /// Malformed or out-of-bounds byte range (`416 Range Not Satisfiable`).
    Unsatisfiable,
}

/// Parses an HTTP `Range: bytes=start-end` header against a resource of `total_size` bytes.
pub fn parse_range_header(header: &str, total_size: usize) -> RangeParse {
    let trimmed = header.trim();
    let Some(spec) = trimmed
        .strip_prefix("bytes=")
        .or_else(|| trimmed.strip_prefix("BYTES="))
    else {
        return RangeParse::None;
    };
    if total_size == 0 {
        return RangeParse::Unsatisfiable;
    }
    // Single-range requests only (matches WebView2 / tdesktop `ParseRangeHeaderFor`).
    let first_range = spec.split(',').next().unwrap_or("").trim();
    let Some((start_str, end_str)) = first_range.split_once('-') else {
        return RangeParse::Unsatisfiable;
    };
    if start_str.is_empty() {
        // Suffix range: bytes=-N
        let Ok(suffix) = end_str.trim().parse::<usize>() else {
            return RangeParse::Unsatisfiable;
        };
        if suffix == 0 {
            return RangeParse::Unsatisfiable;
        }
        let start = total_size.saturating_sub(suffix);
        return RangeParse::Satisfied(start, total_size - 1);
    }
    let Ok(start) = start_str.trim().parse::<usize>() else {
        return RangeParse::Unsatisfiable;
    };
    if start >= total_size {
        return RangeParse::Unsatisfiable;
    }
    let end = if end_str.trim().is_empty() {
        total_size - 1
    } else {
        let Ok(parsed_end) = end_str.trim().parse::<usize>() else {
            return RangeParse::Unsatisfiable;
        };
        if parsed_end < start {
            return RangeParse::Unsatisfiable;
        }
        parsed_end.min(total_size - 1)
    };
    RangeParse::Satisfied(start, end)
}

/// Empty [`MediaProvider`] used by [`IvServer::serve_cached`] when only an attached
/// [`IvCachedMedia`] store is queried.
struct EmptyMediaProvider;

impl MediaProvider for EmptyMediaProvider {
    fn photo(&self, _id: i64) -> Option<MediaBlob> {
        None
    }

    fn document(&self, _id: i64) -> Option<MediaBlob> {
        None
    }
}

/// Serves HTML, CSS, JS, media, and sandboxed embeds for a local custom-scheme WebView handler.
#[derive(Debug, Clone)]
pub struct IvServer {
    page: PreparedPage,
    csp: String,
    placeholder_svg_on_missing_photo: bool,
    cached_media: Option<Arc<IvCachedMedia>>,
}

impl IvServer {
    /// Creates a new [`IvServer`] serving the given [`PreparedPage`].
    pub fn new(page: PreparedPage) -> Self {
        Self {
            page,
            csp: DEFAULT_IV_CSP.to_string(),
            placeholder_svg_on_missing_photo: true,
            cached_media: None,
        }
    }

    /// Creates a new [`IvServer`] backed by an [`IvCachedMedia`] store.
    pub fn with_cached_media(page: PreparedPage, cached_media: Arc<IvCachedMedia>) -> Self {
        Self {
            page,
            csp: DEFAULT_IV_CSP.to_string(),
            placeholder_svg_on_missing_photo: true,
            cached_media: Some(cached_media),
        }
    }

    /// Attaches or detaches an [`IvCachedMedia`] store queried before any fallback
    /// [`MediaProvider`] or placeholder SVG response.
    pub fn set_cached_media(&mut self, cached_media: Option<Arc<IvCachedMedia>>) {
        self.cached_media = cached_media;
    }

    /// Returns the attached [`IvCachedMedia`] store, if any.
    #[must_use]
    pub fn cached_media(&self) -> Option<&Arc<IvCachedMedia>> {
        self.cached_media.as_ref()
    }

    /// Serves a request using only the attached [`IvCachedMedia`] store (if present).
    pub fn serve_cached(
        &self,
        request_url_or_path: &str,
        range_header: Option<&str>,
    ) -> ResourceResponse {
        self.serve(request_url_or_path, range_header, &EmptyMediaProvider)
    }

    /// Replaces the currently served [`PreparedPage`].
    pub fn set_page(&mut self, page: PreparedPage) {
        self.page = page;
    }

    /// Overrides the `Content-Security-Policy` header for served HTML.
    pub fn set_csp(&mut self, csp: impl Into<String>) {
        self.csp = csp.into();
    }

    /// Configures whether missing photos return a lightweight placeholder SVG (`200`) or `404`.
    pub fn set_placeholder_on_missing_photo(&mut self, enabled: bool) {
        self.placeholder_svg_on_missing_photo = enabled;
    }

    /// Resolves a custom-scheme URL or path (e.g. `tg-iv://localhost/css/page.css`,
    /// `http://desktop-app-resource/media/photo/42`, or `/index.html`) into a [`ResourceResponse`].
    pub fn serve<M: MediaProvider>(
        &self,
        request_url_or_path: &str,
        range_header: Option<&str>,
        media: &M,
    ) -> ResourceResponse {
        let Some(path) = normalize_request_path(request_url_or_path) else {
            return ResourceResponse::forbidden();
        };

        match path.as_str() {
            "" | "/" | "/index.html" | "/page.html" => {
                ResourceResponse::ok("text/html; charset=utf-8", self.page.html.as_bytes())
                    .with_header("Content-Security-Policy", self.csp.clone())
                    .with_header("X-Content-Type-Options", "nosniff")
                    .with_header("Cache-Control", "no-store")
            }
            "/css/page.css" => ResourceResponse::ok("text/css; charset=utf-8", PAGE_CSS.as_bytes())
                .with_header("X-Content-Type-Options", "nosniff"),
            "/css/rich_message.css" => {
                ResourceResponse::ok("text/css; charset=utf-8", RICH_MESSAGE_CSS.as_bytes())
                    .with_header("X-Content-Type-Options", "nosniff")
            }
            "/js/page.js" => {
                ResourceResponse::ok("application/javascript; charset=utf-8", PAGE_JS.as_bytes())
                    .with_header("X-Content-Type-Options", "nosniff")
            }
            "/js/rich_message.js" => ResourceResponse::ok(
                "application/javascript; charset=utf-8",
                RICH_MESSAGE_JS.as_bytes(),
            )
            .with_header("X-Content-Type-Options", "nosniff"),
            _ => self.serve_dynamic(&path, range_header, media),
        }
    }

    fn serve_dynamic<M: MediaProvider>(
        &self,
        path: &str,
        range_header: Option<&str>,
        media: &M,
    ) -> ResourceResponse {
        if let Some(id_str) = path.strip_prefix("/media/photo/") {
            let Ok(id) = id_str.parse::<i64>() else {
                return ResourceResponse::not_found();
            };
            if let Some(blob) = self
                .cached_media
                .as_deref()
                .and_then(|cache| cache.photo(id))
                .or_else(|| media.photo(id))
            {
                return serve_blob(blob, range_header);
            }
            if self.placeholder_svg_on_missing_photo {
                return placeholder_svg_response(&format!("Photo #{id}"));
            }
            return ResourceResponse::not_found();
        }

        if let Some(id_str) = path
            .strip_prefix("/media/doc/")
            .or_else(|| path.strip_prefix("/media/video/"))
            .or_else(|| path.strip_prefix("/media/audio/"))
        {
            let Ok(id) = id_str.parse::<i64>() else {
                return ResourceResponse::not_found();
            };
            return match self
                .cached_media
                .as_deref()
                .and_then(|cache| cache.document(id))
                .or_else(|| media.document(id))
            {
                Some(blob) => serve_blob(blob, range_header),
                None => ResourceResponse::not_found(),
            };
        }

        if let Some(params) = path.strip_prefix("/media/map/") {
            if let Some((lat, lon, zoom, w, h)) = parse_map_params(params) {
                if let Some(blob) = self
                    .cached_media
                    .as_deref()
                    .and_then(|cache| cache.map_tile(lat, lon, zoom, w, h))
                    .or_else(|| media.map_tile(lat, lon, zoom, w, h))
                {
                    return serve_blob(blob, range_header);
                }
                if self.placeholder_svg_on_missing_photo {
                    return placeholder_svg_response(&format!("Map {lat:.4}, {lon:.4} (z{zoom})"));
                }
            }
            return ResourceResponse::not_found();
        }

        if let Some(idx_str) = path.strip_prefix("/embed/") {
            let Ok(index) = idx_str.parse::<usize>() else {
                return ResourceResponse::not_found();
            };
            if let Some(embed) = self.page.embeds.get(index)
                && let Some(ref raw_html) = embed.html
            {
                return ResourceResponse::ok("text/html; charset=utf-8", raw_html.as_bytes())
                    .with_header(
                        "Content-Security-Policy",
                        "sandbox allow-scripts; default-src https: data:; object-src 'none';",
                    )
                    .with_header("Cache-Control", "no-store");
            }
            return ResourceResponse::not_found();
        }

        ResourceResponse::not_found()
    }
}

fn serve_blob(blob: MediaBlob, range_header: Option<&str>) -> ResourceResponse {
    let total = blob.bytes.len();
    if let Some(header) = range_header {
        match parse_range_header(header, total) {
            RangeParse::Satisfied(start, end) => {
                let slice = blob.bytes[start..=end].to_vec();
                return ResourceResponse {
                    status: 206,
                    content_type: blob.content_type,
                    body: slice,
                    headers: vec![
                        ("Accept-Ranges".to_string(), "bytes".to_string()),
                        (
                            "Content-Range".to_string(),
                            format!("bytes {start}-{end}/{total}"),
                        ),
                        ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
                    ],
                };
            }
            RangeParse::None => {}
            RangeParse::Unsatisfiable => {
                return ResourceResponse {
                    status: 416,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    body: b"Range Not Satisfiable".to_vec(),
                    headers: vec![("Content-Range".to_string(), format!("bytes */{total}"))],
                };
            }
        }
    }
    ResourceResponse::ok(blob.content_type, blob.bytes)
        .with_header("Accept-Ranges", "bytes")
        .with_header("X-Content-Type-Options", "nosniff")
}

fn placeholder_svg_response(label: &str) -> ResourceResponse {
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"400\" height=\"220\" viewBox=\"0 0 400 220\">\
         <rect width=\"400\" height=\"220\" fill=\"#e8edf0\" rx=\"8\"/>\
         <text x=\"200\" y=\"114\" text-anchor=\"middle\" font-family=\"sans-serif\" font-size=\"14\" fill=\"#70777b\">{}</text>\
         </svg>",
        escape_html(label)
    );
    ResourceResponse::ok("image/svg+xml; charset=utf-8", svg.into_bytes())
        .with_header("Cache-Control", "no-store")
        .with_header("X-Content-Type-Options", "nosniff")
}

fn normalize_request_path(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.bytes().any(|b| b <= 0x1F || b == 0x7F) {
        return None;
    }
    // Strip scheme + authority if a full URL was passed (e.g. `tg-iv://localhost/css/page.css`).
    let mut path = if let Some(idx) = trimmed.find("://") {
        let rest = &trimmed[idx + 3..];
        match rest.find('/') {
            Some(slash) => &rest[slash..],
            None => "/",
        }
    } else {
        trimmed
    };
    if let Some(q) = path.find(['?', '#']) {
        path = &path[..q];
    }
    if path.contains('\\') {
        return None;
    }
    // Reject path traversal segments (`..` or percent-encoded `%2e`).
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
        return None;
    }
    for segment in path.split('/') {
        if segment == ".." {
            return None;
        }
    }
    if path.is_empty() {
        Some("/".to_string())
    } else if path.starts_with('/') {
        Some(path.to_string())
    } else {
        Some(format!("/{path}"))
    }
}

fn parse_map_params(params: &str) -> Option<(f64, f64, i32, i32, i32)> {
    let parts: Vec<&str> = params.split(',').collect();
    if parts.len() != 4 {
        return None;
    }
    let lat: f64 = parts[0].parse().ok()?;
    let lon: f64 = parts[1].parse().ok()?;
    let zoom: i32 = parts[2].parse().ok()?;
    let (w_str, h_str) = parts[3].split_once('x')?;
    let w: i32 = w_str.parse().ok()?;
    let h: i32 = h_str.parse().ok()?;
    if !lat.is_finite()
        || !lon.is_finite()
        || !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
        || !(1..=22).contains(&zoom)
        || w <= 0
        || h <= 0
    {
        return None;
    }
    Some((lat, lon, zoom, w, h))
}
