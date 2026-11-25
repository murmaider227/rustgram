//! Instant View top-level window hosting `tg_iv::IvServer` + `tg_iv::IvController` in `tg_webview::WebView`.
//!
//! Port origin: `Telegram/SourceFiles/iv/iv_controller.cpp` and `Telegram/SourceFiles/iv/iv_instance.cpp`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Layout, Modifiers, Panel,
    Pos2, Rect, RichText, Role, Sense, Stroke, TextEdit, Ui, Vec2, ViewportBuilder,
    ViewportCommand, ViewportId, WidgetInfo,
};
use tg_iv::{
    ControllerAction, DEFAULT_CACHE_MAX_BYTES, DEFAULT_CACHE_MAX_ENTRIES, EmbedPolicy, HostCommand,
    IvCachedMedia, IvController, IvSearchController, IvServer, LinkTarget, MediaProvider,
    MemoryMediaStore, NavigationDecision, Page, PreparedPage, RenderOptions, SearchMatch,
    ShortcutAction, ZOOM_DEFAULT, ZoomStep, extract_search_sources_from_page, https_to_tonsite,
    prepare_page,
};
use tg_lang::tr;
use tg_theme::Palette as TgPalette;
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::LayerStack;
use tg_ui::stand_in::{BoxStyle, PopupMenuStyle, ToggleStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::IconKind;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};
use tg_webview::{
    DEFAULT_CUSTOM_SCHEME, DataRequest, DataResponse, DataResult, MemoryDataStream, SchemeRouter,
    WebMessage, WebView, WebViewBounds, WebViewCallbacks, WebViewConfig, availability,
    resolve_user_data_dir,
};

use crate::webview::backend::{AppWebViewEvent, ClipboardAccess, ExternalOpener, WebViewBackend};
use crate::webview::platform_win::{
    client_size_for_hwnd, find_thread_window_by_title, win32_raw_handle,
};
use crate::webview::runtime_error::{
    ACTION_CLOSE_ERROR, ACTION_DOWNLOAD_WEBVIEW2, ACTION_OPEN_IN_BROWSER, RUNTIME_ERROR_BOX_ID,
    RuntimeErrorKind, push_runtime_error_box, render_runtime_error_body,
    render_unavailable_backdrop,
};
use crate::webview::strings;
use crate::webview::theme::{
    TgThemeColors, build_webview_theme_params, color_to_egui, egui_visuals_from_tg_palette,
};

/// Custom-scheme origin used for Instant View documents served through [`tg_iv::IvServer`].
pub const IV_ORIGIN: &str = "rustgram-internal://iv";

/// Custom-scheme data URL prefix registered with [`SchemeRouter`].
pub const IV_DATA_URL_PREFIX: &str = "rustgram-internal://iv/";

/// Height of the Instant View title bar in logical pixels (`st::ivSubtitleHeight`).
pub const IV_TITLE_BAR_HEIGHT: f32 = st::iv_subtitle_height.0 as f32;

/// Height of the Instant View in-page search bar in logical pixels (`st::ivSearchBarHeight`).
pub const IV_SEARCH_BAR_HEIGHT: f32 = st::iv_search_bar_height.0 as f32;

/// Slide reveal duration in seconds for pointer-triggered find bar toggle (`st::slideWrapDuration`).
const IV_SEARCH_BAR_ANIM_SECS: f32 = st::slide_wrap_duration as f32 / 1000.0;

/// Default Instant View window width in logical pixels (`st::ivWidthDefault`).
pub const IV_DEFAULT_WIDTH: f32 = st::iv_width_default.0 as f32;

/// Default Instant View window height in logical pixels (`st::ivHeightDefault`).
pub const IV_DEFAULT_HEIGHT: f32 = st::iv_height_default.0 as f32;

/// Minimum Instant View window width in logical pixels (`st::ivWidthMin`).
pub const IV_MIN_WIDTH: f32 = st::iv_width_min.0 as f32;

/// Minimum Instant View window height in logical pixels (`st::ivHeightMin`).
pub const IV_MIN_HEIGHT: f32 = st::iv_height_min.0 as f32;

const IV_VIEWPORT_ID_STR: &str = "rustgram_instant_view_window";
const TOAST_DURATION: Duration = Duration::from_secs(3);

/// Events posted asynchronously from WebView2 COM callbacks to [`InstantViewWindow`].
#[derive(Debug, Clone)]
pub enum IvBridgeEvent {
    /// Raw WebView2 message (payload + verified `source_url`) posted by `tg-iv`'s `assets/page.js`.
    BridgeMessage(WebMessage),
    /// Top-level navigation intercepted by `NavigationStarting` / `NewWindowRequested`.
    InterceptedTarget(LinkTarget),
    /// Native WebView2 zoom changed (e.g. via Ctrl+Scroll).
    ZoomFactorChanged(u32),
    /// Navigation finished (`success`).
    NavigationDone(bool),
    /// WebView2 browser/renderer process crashed.
    ProcessFailed,
}

/// Thread-safe bridge connecting `tg_webview::SchemeRouter` (`rustgram-internal://iv/*`) to
/// `tg_iv::IvServer` backed by [`IvCachedMedia`].
#[derive(Clone)]
pub struct IvResourceBridge {
    server: Arc<RwLock<IvServer>>,
    media: Arc<RwLock<Arc<dyn MediaProvider + Send + Sync>>>,
    cached_media: Arc<IvCachedMedia>,
    router: Arc<SchemeRouter>,
}

impl IvResourceBridge {
    /// Creates a resource bridge serving `prepared` and `media` on `rustgram-internal://iv/`,
    /// backed by a default in-memory [`IvCachedMedia`] LRU cache.
    #[must_use]
    pub fn new(prepared: PreparedPage, media: Arc<dyn MediaProvider + Send + Sync>) -> Self {
        Self::with_cache(prepared, media, Arc::new(IvCachedMedia::with_defaults()))
    }

    /// Creates a resource bridge backed by an explicit [`IvCachedMedia`] store.
    #[must_use]
    pub fn with_cache(
        prepared: PreparedPage,
        media: Arc<dyn MediaProvider + Send + Sync>,
        cached_media: Arc<IvCachedMedia>,
    ) -> Self {
        let server = Arc::new(RwLock::new(IvServer::with_cached_media(
            prepared,
            Arc::clone(&cached_media),
        )));
        let media_lock = Arc::new(RwLock::new(media));

        let mut router = SchemeRouter::new(DEFAULT_CUSTOM_SCHEME);
        router.set_data_url_prefix(IV_DATA_URL_PREFIX);

        let server_cb = Arc::clone(&server);
        let media_cb = Arc::clone(&media_lock);
        let cache_cb = Arc::clone(&cached_media);
        router.set_fallback(Arc::new(move |req: DataRequest, _done| {
            let response = serve_iv_data_request(&server_cb, &media_cb, &cache_cb, &req);
            if response.stream.is_some() {
                DataResult::Done(response)
            } else {
                DataResult::Failed
            }
        }));

        Self {
            server,
            media: media_lock,
            cached_media,
            router: Arc::new(router),
        }
    }

    /// Creates a resource bridge backed by a disk-persisted [`IvCachedMedia`] store in `cache_dir`.
    pub fn with_disk_cache(
        prepared: PreparedPage,
        media: Arc<dyn MediaProvider + Send + Sync>,
        cache_dir: impl Into<PathBuf>,
    ) -> std::io::Result<Self> {
        let cache = IvCachedMedia::with_disk_dir(
            cache_dir,
            DEFAULT_CACHE_MAX_BYTES,
            DEFAULT_CACHE_MAX_ENTRIES,
        )?;
        Ok(Self::with_cache(prepared, media, Arc::new(cache)))
    }

    /// Returns the attached [`IvCachedMedia`] store queried before fallback media providers.
    #[must_use]
    pub fn cached_media(&self) -> &Arc<IvCachedMedia> {
        &self.cached_media
    }

    /// Replaces the active [`PreparedPage`] and [`MediaProvider`] served by `IvServer`.
    pub fn set_page(
        &self,
        prepared: PreparedPage,
        media: Option<Arc<dyn MediaProvider + Send + Sync>>,
    ) {
        if let Ok(mut guard) = self.server.write() {
            guard.set_page(prepared);
        }
        if let Some(m) = media
            && let Ok(mut guard) = self.media.write()
        {
            *guard = m;
        }
    }

    /// Returns the underlying [`SchemeRouter`] to pass to [`WebViewConfig::scheme_router`].
    #[must_use]
    pub fn router(&self) -> Arc<SchemeRouter> {
        Arc::clone(&self.router)
    }

    /// Resolves a `rustgram-internal://iv/...` URL synchronously (used by unit tests).
    #[must_use]
    pub fn serve_sync(
        &self,
        url: &str,
        range_header: Option<&str>,
    ) -> Option<tg_webview::HttpResponse> {
        self.router.route_sync(url, range_header)
    }
}

/// Converts a `tg_webview::DataRequest` into an `IvServer::serve` call and wraps the result
/// as a `tg_webview::DataResponse` (preserving HTTP `206 Partial Content` byte ranges).
///
/// Any photo, document, or map tile fetched from the fallback [`MediaProvider`] is written
/// through into [`IvCachedMedia`] so subsequent requests are served directly from cache.
fn serve_iv_data_request(
    server_lock: &RwLock<IvServer>,
    media_lock: &RwLock<Arc<dyn MediaProvider + Send + Sync>>,
    cached_media: &IvCachedMedia,
    req: &DataRequest,
) -> DataResponse {
    let Ok(server) = server_lock.read() else {
        return DataResponse::not_found();
    };
    let Ok(media_guard) = media_lock.read() else {
        return DataResponse::not_found();
    };
    let path = if req.id.starts_with('/') {
        req.id.clone()
    } else {
        format!("/{}", req.id)
    };
    let range_header = format_range_header_from_request(req);
    let resp = server.serve(
        &path,
        range_header.as_deref(),
        &CachingMediaProvider {
            inner: media_guard.as_ref(),
            cache: cached_media,
        },
    );

    match resp.status {
        200 => DataResponse::from_bytes(resp.body, resp.content_type),
        206 => {
            let total = resp
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("Content-Range"))
                .and_then(|(_, v)| v.rsplit('/').next())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(resp.body.len() as i64);
            DataResponse {
                stream: Some(Box::new(MemoryDataStream::new(
                    resp.body,
                    resp.content_type,
                ))),
                stream_offset: req.offset,
                total_size: total,
            }
        }
        _ => DataResponse::not_found(),
    }
}

fn format_range_header_from_request(req: &DataRequest) -> Option<String> {
    if req.offset == 0 && req.limit == 0 {
        return None;
    }
    if req.limit < 0 {
        return Some(format!("bytes={}-", req.offset.max(0)));
    }
    let start = req.offset.max(0);
    let end = start.saturating_add(req.limit).saturating_sub(1);
    Some(format!("bytes={start}-{end}"))
}

/// Write-through adapter that populates [`IvCachedMedia`] whenever a fallback [`MediaProvider`]
/// supplies a photo, document, or geo-map tile.
struct CachingMediaProvider<'a> {
    inner: &'a dyn MediaProvider,
    cache: &'a IvCachedMedia,
}

impl MediaProvider for CachingMediaProvider<'_> {
    fn photo(&self, id: i64) -> Option<tg_iv::MediaBlob> {
        let blob = self.inner.photo(id)?;
        let ct = (!blob.content_type.is_empty()).then_some(blob.content_type.as_str());
        self.cache.insert_photo(id, ct, blob.bytes.clone());
        Some(blob)
    }

    fn document(&self, id: i64) -> Option<tg_iv::MediaBlob> {
        let blob = self.inner.document(id)?;
        let ct = (!blob.content_type.is_empty()).then_some(blob.content_type.as_str());
        self.cache.insert_document(id, ct, blob.bytes.clone());
        Some(blob)
    }

    fn map_tile(
        &self,
        lat: f64,
        lon: f64,
        zoom: i32,
        width: i32,
        height: i32,
    ) -> Option<tg_iv::MediaBlob> {
        let blob = self.inner.map_tile(lat, lon, zoom, width, height)?;
        let ct = (!blob.content_type.is_empty()).then_some(blob.content_type.as_str());
        self.cache
            .insert_map_tile(lat, lon, zoom, width, height, ct, blob.bytes.clone());
        Some(blob)
    }
}

/// Top-level Instant View window controller and renderer.
pub struct InstantViewWindow {
    controller: IvController,
    resource_bridge: IvResourceBridge,
    search: IvSearchController,
    search_query_input: String,
    search_focus_requested: bool,
    search_bar_anim: f32,
    search_anim_instant: bool,
    current_page: Option<Page>,
    current_site_name: String,
    current_url_shared: Arc<Mutex<String>>,
    webview: Option<WebView>,
    attached_hwnd: Option<isize>,
    last_bounds: Option<WebViewBounds>,
    webview_hidden_for_overlay: bool,
    webview_init_attempted: bool,
    tx_events: mpsc::Sender<IvBridgeEvent>,
    rx_events: mpsc::Receiver<IvBridgeEvent>,
    popup_menu: PopupMenuState,
    layer_stack: LayerStack,
    runtime_error: Option<RuntimeErrorKind>,
    runtime_error_box_pushed: bool,
    pending_scroll_restore: Option<f64>,
    pending_fragment: Option<String>,
    toast: Option<(String, Instant)>,
    dark_mode: bool,
    zoom_synced: i32,
    user_data_root: PathBuf,
    open: bool,
    viewport_frames: u32,
}

impl InstantViewWindow {
    /// Creates an Instant View window ready to display `page` with `media`.
    #[must_use]
    pub fn new(
        page: Page,
        media: Arc<dyn MediaProvider + Send + Sync>,
        dark_mode: bool,
        initial_fragment: Option<&str>,
    ) -> Self {
        Self::with_media_cache(
            page,
            media,
            Arc::new(IvCachedMedia::with_defaults()),
            dark_mode,
            initial_fragment,
        )
    }

    /// Creates an Instant View window backed by an explicit [`IvCachedMedia`] store.
    #[must_use]
    pub fn with_media_cache(
        page: Page,
        media: Arc<dyn MediaProvider + Send + Sync>,
        cached_media: Arc<IvCachedMedia>,
        dark_mode: bool,
        initial_fragment: Option<&str>,
    ) -> Self {
        let mut controller = IvController::new(IV_ORIGIN, ZOOM_DEFAULT);
        let options = Self::render_options(dark_mode, controller.zoom().percent());
        let prepared = prepare_page(&page, &options);
        let site_name = prepared.site_name.clone();
        let initial_actions = controller.load_page(&page.url, page.id, &prepared, initial_fragment);
        let pending_fragment = initial_actions.into_iter().find_map(|a| match a {
            ControllerAction::Eval(HostCommand::ScrollToAnchor { fragment }) => Some(fragment),
            _ => None,
        });

        let current_url_shared = Arc::new(Mutex::new(page.url.clone()));
        let resource_bridge = IvResourceBridge::with_cache(prepared, media, cached_media);
        let search = IvSearchController::from_page(&page);
        let (tx_events, rx_events) = mpsc::channel();
        let user_data_root = crate::webview::mini_app::default_webview_data_root();

        Self {
            controller,
            resource_bridge,
            search,
            search_query_input: String::new(),
            search_focus_requested: false,
            search_bar_anim: 0.0,
            search_anim_instant: true,
            current_page: Some(page),
            current_site_name: site_name,
            current_url_shared,
            webview: None,
            attached_hwnd: None,
            last_bounds: None,
            webview_hidden_for_overlay: false,
            webview_init_attempted: false,
            tx_events,
            rx_events,
            popup_menu: PopupMenuState::new(),
            layer_stack: LayerStack::new(),
            runtime_error: None,
            runtime_error_box_pushed: false,
            pending_scroll_restore: None,
            pending_fragment,
            toast: None,
            dark_mode,
            zoom_synced: ZOOM_DEFAULT,
            user_data_root,
            open: true,
            viewport_frames: 0,
        }
    }

    /// Creates an Instant View window backed by a disk-persisted [`IvCachedMedia`] directory.
    pub fn with_disk_cache(
        page: Page,
        media: Arc<dyn MediaProvider + Send + Sync>,
        cache_dir: impl Into<PathBuf>,
        dark_mode: bool,
        initial_fragment: Option<&str>,
    ) -> std::io::Result<Self> {
        let cache_dir = cache_dir.into();
        let profile_root = cache_dir
            .parent()
            .map(|p| p.join("webview_profiles"))
            .unwrap_or_else(|| cache_dir.join("webview_profiles"));
        let cache = IvCachedMedia::with_disk_dir(
            cache_dir,
            DEFAULT_CACHE_MAX_BYTES,
            DEFAULT_CACHE_MAX_ENTRIES,
        )?;
        let mut win =
            Self::with_media_cache(page, media, Arc::new(cache), dark_mode, initial_fragment);
        win.user_data_root = profile_root;
        Ok(win)
    }

    /// Overrides the WebView2 user-data root folder used by this Instant View window.
    pub fn set_user_data_root(&mut self, root: PathBuf) {
        self.user_data_root = root;
    }

    /// Returns the resolved WebView2 user-data folder for this Instant View window.
    #[must_use]
    pub fn webview_user_data_dir(&self) -> PathBuf {
        resolve_user_data_dir(&self.user_data_root, "iv")
    }

    /// Creates an Instant View window displaying an explicit runtime error (e.g. missing WebView2).
    #[must_use]
    pub fn with_runtime_error(url: &str, error: RuntimeErrorKind, dark_mode: bool) -> Self {
        let empty_page = Page {
            id: 0,
            url: url.to_owned(),
            site_name: Some(tr::lng_iv_window_title()),
            ..Page::default()
        };
        let mut win = Self::new(
            empty_page,
            Arc::new(MemoryMediaStore::new()),
            dark_mode,
            None,
        );
        win.runtime_error = Some(error);
        win.webview_init_attempted = true;
        win
    }

    fn render_options(dark_mode: bool, zoom_percent: i32) -> RenderOptions {
        RenderOptions {
            base_url: IV_ORIGIN.to_owned(),
            inline_assets: false,
            include_bridge: true,
            embed_policy: EmbedPolicy::default(),
            dark_mode: Some(dark_mode),
            zoom_percent,
            ..RenderOptions::default()
        }
    }

    /// Returns whether this Instant View window is still open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Returns the active [`IvController`].
    #[must_use]
    pub const fn controller(&self) -> &IvController {
        &self.controller
    }

    /// Returns the local custom-scheme [`IvResourceBridge`].
    #[must_use]
    pub const fn resource_bridge(&self) -> &IvResourceBridge {
        &self.resource_bridge
    }

    /// Returns the [`IvCachedMedia`] store attached to this Instant View window.
    #[must_use]
    pub fn cached_media(&self) -> &Arc<IvCachedMedia> {
        self.resource_bridge.cached_media()
    }

    /// Returns the in-page [`IvSearchController`].
    #[must_use]
    pub const fn search_controller(&self) -> &IvSearchController {
        &self.search
    }

    /// Returns mutable access to the in-page [`IvSearchController`].
    pub fn search_controller_mut(&mut self) -> &mut IvSearchController {
        &mut self.search
    }

    /// Returns `true` when the Instant View in-page search bar is visible.
    #[must_use]
    pub const fn is_search_open(&self) -> bool {
        self.search.is_shown()
    }

    /// Opens the in-page search bar and requests keyboard focus for the query field.
    pub fn open_search(&mut self) {
        self.search_anim_instant = true;
        self.search.show();
        self.search_bar_anim = 1.0;
        self.search_focus_requested = true;
    }

    /// Hides the in-page search bar and clears active search highlights.
    pub fn close_search(&mut self) {
        self.search_anim_instant = true;
        self.search.hide();
        self.search_bar_anim = 0.0;
        self.search_focus_requested = false;
    }

    /// Toggles the in-page search bar (instant for keyboard `Ctrl+F`, animated for pointer clicks).
    pub fn toggle_search(&mut self) {
        self.toggle_search_with_animation(false);
    }

    fn toggle_search_with_animation(&mut self, animate: bool) {
        self.search_anim_instant = !animate;
        if self.search.is_shown() {
            self.search.hide();
            self.search_focus_requested = false;
            if !animate {
                self.search_bar_anim = 0.0;
            }
        } else {
            self.search.show();
            self.search_focus_requested = true;
            if !animate {
                self.search_bar_anim = 1.0;
            }
        }
    }

    /// Updates the in-page search query and returns the total number of matches.
    pub fn set_search_query(&mut self, query: &str) -> usize {
        self.search_query_input = query.to_owned();
        if !self.search.is_shown() {
            self.open_search();
        }
        let count = self.search.set_query(query);
        self.sync_search_highlight_to_webview();
        count
    }

    /// Advances to the next in-page search match with wrap-around.
    pub fn search_next(&mut self) -> Option<&SearchMatch> {
        let _ = self.search.next_match();
        self.sync_search_highlight_to_webview();
        self.search.current_match()
    }

    /// Moves to the previous in-page search match with wrap-around.
    pub fn search_prev(&mut self) -> Option<&SearchMatch> {
        let _ = self.search.prev_match();
        self.sync_search_highlight_to_webview();
        self.search.current_match()
    }

    fn sync_search_highlight_to_webview(&self) {
        let Some(wv) = &self.webview else {
            return;
        };
        let query = self.search.query();
        if query.is_empty() || !self.search.is_shown() {
            let _ =
                wv.eval("if (window.getSelection) { window.getSelection().removeAllRanges(); }");
            return;
        }
        if let Ok(encoded) = serde_json::to_string(query) {
            let js =
                format!("if (typeof window.find === 'function') {{ window.find({encoded}); }}");
            let _ = wv.eval(&js);
        }
    }

    /// Returns the current runtime error, if WebView2 is missing or crashed.
    #[must_use]
    pub const fn runtime_error(&self) -> Option<&RuntimeErrorKind> {
        self.runtime_error.as_ref()
    }

    /// Returns the attached native Win32 `HWND`, if any.
    #[must_use]
    pub const fn attached_hwnd(&self) -> Option<isize> {
        self.attached_hwnd
    }

    /// Returns the OS window title matching `iv_controller.cpp` (`"Instant View — <site>"`).
    #[must_use]
    pub fn window_title(&self) -> String {
        let base = tr::lng_iv_window_title();
        let subtitle = self.subtitle_text();
        if subtitle.is_empty() {
            base
        } else {
            format!("{base} — {subtitle}")
        }
    }

    /// Returns the title-bar subtitle (site name, TON site display URL, or page URL).
    #[must_use]
    pub fn subtitle_text(&self) -> String {
        let url = self.controller.current_url();
        if url.to_ascii_lowercase().contains(".magic.org") {
            return https_to_tonsite(url);
        }
        if !self.current_site_name.is_empty() {
            return self.current_site_name.clone();
        }
        url.to_owned()
    }

    /// Closes the window, tears down the embedded WebView2 controller, and releases in-memory
    /// media cache payloads while preserving any disk-backed cache files.
    pub fn close(&mut self) {
        self.open = false;
        self.resource_bridge.cached_media().release_heavy_data();
        if let Some(wv) = self.webview.take() {
            wv.close();
        }
    }

    /// Syncs Day/Night theme from `tg-theme` to both `IvServer` and the live WebView2 page.
    pub fn apply_theme(&mut self, palette: &TgPalette, dark_mode: bool) {
        self.dark_mode = dark_mode;
        if let Some(page) = &self.current_page {
            let options = Self::render_options(dark_mode, self.controller.zoom().percent());
            let prepared = prepare_page(page, &options);
            self.resource_bridge.set_page(prepared, None);
        }
        if let Some(wv) = &self.webview {
            let params = build_webview_theme_params(palette);
            let _ = wv.update_theme(&params);
            let cmd = HostCommand::SetTheme { dark: dark_mode };
            if let Ok(js) = cmd.to_eval_js() {
                let _ = wv.eval(&js);
            }
        }
    }

    /// Loads a new Instant View page into this window, pushing the previous page onto history.
    pub fn load_page(
        &mut self,
        page: Page,
        media: Arc<dyn MediaProvider + Send + Sync>,
        fragment: Option<&str>,
    ) {
        let options = Self::render_options(self.dark_mode, self.controller.zoom().percent());
        let prepared = prepare_page(&page, &options);
        self.current_site_name = prepared.site_name.clone();
        let actions = self
            .controller
            .load_page(&page.url, page.id, &prepared, fragment);
        if let Ok(mut guard) = self.current_url_shared.lock() {
            *guard = page.url.clone();
        }
        self.resource_bridge.set_page(prepared, Some(media));
        self.search
            .set_sources(extract_search_sources_from_page(&page));
        self.current_page = Some(page);
        self.pending_scroll_restore = None;
        self.pending_fragment = actions.into_iter().find_map(|a| match a {
            ControllerAction::Eval(HostCommand::ScrollToAnchor { fragment }) => Some(fragment),
            _ => None,
        });
        self.reload_webview_page();
    }

    /// Fetches and opens an Instant View page by URL, falling back to external browser if unavailable.
    pub fn open_url(
        &mut self,
        url: &str,
        webpage_id: Option<i64>,
        fragment: Option<&str>,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
    ) -> Vec<AppWebViewEvent> {
        match backend.fetch_iv_page(url, webpage_id) {
            Ok(Some(bundle)) => {
                self.load_page(bundle.page, bundle.media, fragment);
                Vec::new()
            }
            Ok(None) | Err(_) => {
                let _ = external.open_url(url);
                vec![AppWebViewEvent::OpenExternalUrl {
                    url: url.to_owned(),
                }]
            }
        }
    }

    /// Navigates back in Instant View history and restores the saved scroll offset.
    pub fn go_back(
        &mut self,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
    ) -> Vec<AppWebViewEvent> {
        let Some(entry) = self.controller.go_back() else {
            return Vec::new();
        };
        self.restore_history_entry(&entry, backend, external)
    }

    /// Navigates forward in Instant View history and restores the saved scroll offset.
    pub fn go_forward(
        &mut self,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
    ) -> Vec<AppWebViewEvent> {
        let Some(entry) = self.controller.go_forward() else {
            return Vec::new();
        };
        self.restore_history_entry(&entry, backend, external)
    }

    fn restore_history_entry(
        &mut self,
        entry: &tg_iv::HistoryEntry,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
    ) -> Vec<AppWebViewEvent> {
        let wp_id = i64::try_from(entry.page_id).ok().filter(|&id| id != 0);
        match backend.fetch_iv_page(&entry.url, wp_id) {
            Ok(Some(bundle)) => {
                let options =
                    Self::render_options(self.dark_mode, self.controller.zoom().percent());
                let prepared = prepare_page(&bundle.page, &options);
                self.current_site_name = prepared.site_name.clone();
                if let Ok(mut guard) = self.current_url_shared.lock() {
                    *guard = bundle.page.url.clone();
                }
                self.resource_bridge.set_page(prepared, Some(bundle.media));
                self.search
                    .set_sources(extract_search_sources_from_page(&bundle.page));
                self.current_page = Some(bundle.page);
                self.pending_fragment = None;
                self.pending_scroll_restore = (entry.scroll.y > 0.0).then_some(entry.scroll.y);
                self.reload_webview_page();
                Vec::new()
            }
            Ok(None) | Err(_) => {
                let _ = external.open_url(&entry.url);
                vec![AppWebViewEvent::OpenExternalUrl {
                    url: entry.url.clone(),
                }]
            }
        }
    }

    /// Evaluates a WebView2 [`WebMessage`] (checking `source_url` against [`IV_ORIGIN`]) and
    /// dispatches resulting actions.
    pub fn handle_web_message(
        &mut self,
        msg: &WebMessage,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        match self
            .controller
            .handle_bridge_json_from_source(&msg.text, &msg.source_url)
        {
            Ok(actions) => self.handle_controller_actions(actions, backend, external, clipboard),
            Err(err) => {
                tracing::debug!(%err, source = %msg.source_url, "Ignored unauthorized or invalid Instant View bridge message");
                Vec::new()
            }
        }
    }

    /// Evaluates a raw JSON bridge message from `page.js` (assuming the trusted [`IV_ORIGIN`]
    /// sender; use [`Self::handle_web_message`] for untrusted WebView2 callbacks) and dispatches
    /// resulting actions.
    pub fn handle_bridge_json(
        &mut self,
        raw_json: &str,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        let msg = WebMessage {
            text: raw_json.to_owned(),
            source_url: format!("{IV_ORIGIN}/index.html"),
        };
        self.handle_web_message(&msg, backend, external, clipboard)
    }

    /// Routes a [`LinkTarget`] returned by `tg_iv::classify_link` or `navigation_policy`.
    pub fn route_link_target(
        &mut self,
        target: LinkTarget,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
    ) -> Vec<AppWebViewEvent> {
        match target {
            LinkTarget::Anchor { fragment } => {
                self.eval_host_command(&HostCommand::ScrollToAnchor { fragment });
                Vec::new()
            }
            LinkTarget::InstantViewPage {
                url,
                webpage_id,
                fragment,
            } => self.open_url(&url, webpage_id, fragment.as_deref(), backend, external),
            LinkTarget::TelegramLink { uri } => vec![AppWebViewEvent::OpenTelegramLink { uri }],
            LinkTarget::TonSite {
                uri,
                https_gateway_url,
            } => vec![AppWebViewEvent::OpenTonSite {
                uri,
                https_gateway_url,
            }],
            LinkTarget::Email { address } => {
                let _ = external.open_mailto(&address);
                vec![AppWebViewEvent::OpenExternalUrl {
                    url: format!("mailto:{address}"),
                }]
            }
            LinkTarget::Phone { number } => {
                let url = format!("tel:{number}");
                let _ = external.open_url(&url);
                vec![AppWebViewEvent::OpenExternalUrl { url }]
            }
            LinkTarget::ExternalUrl { url } => {
                let _ = external.open_url(&url);
                vec![AppWebViewEvent::OpenExternalUrl { url }]
            }
            LinkTarget::Rejected { .. } => Vec::new(),
        }
    }

    /// Executes a batch of [`ControllerAction`]s produced by [`IvController`].
    pub fn handle_controller_actions(
        &mut self,
        actions: Vec<ControllerAction>,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        let mut events = Vec::new();
        for action in actions {
            match action {
                ControllerAction::Eval(cmd) => {
                    self.eval_host_command(&cmd);
                }
                ControllerAction::OpenPage {
                    url,
                    webpage_id,
                    fragment,
                } => {
                    events.extend(self.open_url(
                        &url,
                        webpage_id,
                        fragment.as_deref(),
                        backend,
                        external,
                    ));
                }
                ControllerAction::OpenTelegramLink { uri } => {
                    events.push(AppWebViewEvent::OpenTelegramLink { uri });
                }
                ControllerAction::OpenTonSite {
                    uri,
                    https_gateway_url,
                } => {
                    events.push(AppWebViewEvent::OpenTonSite {
                        uri,
                        https_gateway_url,
                    });
                }
                ControllerAction::OpenExternal { url } => {
                    let _ = external.open_url(&url);
                    events.push(AppWebViewEvent::OpenExternalUrl { url });
                }
                ControllerAction::OpenMedia { kind, id } => {
                    self.show_toast(format!(
                        "{}{kind:?} #{id}{}",
                        strings::OPENING_MEDIA_PREFIX,
                        strings::OPENING_MEDIA_SUFFIX
                    ));
                    events.push(AppWebViewEvent::OpenMediaViewer { kind, id });
                }
                ControllerAction::OpenChannel {
                    channel_id,
                    username,
                } => {
                    events.push(AppWebViewEvent::OpenChannel {
                        channel_id,
                        username,
                    });
                }
                ControllerAction::JoinChannel {
                    channel_id,
                    username: _,
                } => {
                    if backend.join_channel(channel_id).is_ok() {
                        self.eval_host_command(&HostCommand::SetChannelJoined {
                            channel_id,
                            joined: true,
                        });
                        self.show_toast(strings::JOINED_CHANNEL.to_owned());
                        events.push(AppWebViewEvent::Toast {
                            message: strings::JOINED_CHANNEL.to_owned(),
                        });
                    }
                }
                ControllerAction::ShowContextMenu {
                    selection,
                    link_url,
                    ..
                } => {
                    if let Some(link) = link_url {
                        if clipboard.write_text(&link) {
                            self.show_toast(tr::lng_channel_public_link_copied());
                            events.push(AppWebViewEvent::CopyToClipboard { text: link });
                        }
                    } else if !selection.is_empty() && clipboard.write_text(&selection) {
                        self.show_toast(strings::SELECTION_COPIED.to_owned());
                        events.push(AppWebViewEvent::CopyToClipboard { text: selection });
                    }
                }
                ControllerAction::Report { start_token, .. } => {
                    events.push(AppWebViewEvent::OpenTelegramLink {
                        uri: format!("tg://resolve?domain=previews&start={start_token}"),
                    });
                }
                ControllerAction::ResizeEmbed { .. } => {}
                ControllerAction::CloseWindow => {
                    self.close();
                }
                ControllerAction::MinimizeWindow => {}
                ControllerAction::QuitApp => {
                    events.push(AppWebViewEvent::QuitApp);
                }
                ControllerAction::ZoomChanged { percent } => {
                    self.sync_zoom_to_webview(percent);
                }
            }
        }
        events
    }

    /// Triggers an IV title-bar menu action by ID (`"open_browser"`, `"copy_link"`, `"share"`,
    /// `"zoom_in"`, `"zoom_out"`, `"zoom_reset"`).
    pub fn handle_menu_action(
        &mut self,
        action_id: &str,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        match action_id {
            "open_browser" => {
                let url = self.controller.current_url().to_owned();
                if !url.is_empty() {
                    let _ = external.open_url(&url);
                    self.show_toast(strings::OPENED_IN_BROWSER.to_owned());
                    return vec![AppWebViewEvent::OpenExternalUrl { url }];
                }
            }
            "copy_link" => {
                let url = self.controller.current_url().to_owned();
                if !url.is_empty() {
                    let _ = clipboard.write_text(&url);
                    let copied = tr::lng_channel_public_link_copied();
                    self.show_toast(copied.clone());
                    return vec![
                        AppWebViewEvent::CopyToClipboard { text: url },
                        AppWebViewEvent::Toast { message: copied },
                    ];
                }
            }
            "share" => {
                let url = self.controller.current_url().to_owned();
                let title = self.controller.current_title().to_owned();
                if !url.is_empty() {
                    self.show_toast(strings::SHARE_LINK_READY.to_owned());
                    return vec![AppWebViewEvent::ShareLink { url, title }];
                }
            }
            "find" => {
                self.toggle_search_with_animation(true);
            }
            "zoom_in" => {
                let actions = self.controller.zoom_in(ZoomStep::Normal);
                return self.handle_controller_actions(actions, backend, external, clipboard);
            }
            "zoom_out" => {
                let actions = self.controller.zoom_out(ZoomStep::Normal);
                return self.handle_controller_actions(actions, backend, external, clipboard);
            }
            "zoom_reset" => {
                let actions = self.controller.reset_zoom();
                return self.handle_controller_actions(actions, backend, external, clipboard);
            }
            _ => {}
        }
        Vec::new()
    }

    fn eval_host_command(&self, cmd: &HostCommand) {
        if let Some(wv) = &self.webview
            && let Ok(js) = cmd.to_eval_js()
        {
            let _ = wv.eval(&js);
        }
    }

    fn sync_zoom_to_webview(&mut self, percent: i32) {
        self.zoom_synced = percent;
        if let Some(wv) = &self.webview
            && let Ok(pct) = u32::try_from(percent)
        {
            let _ = wv.set_zoom(pct);
        }
    }

    fn reload_webview_page(&self) {
        if let Some(wv) = &self.webview {
            let _ = wv.navigate_to_data("index.html");
        }
    }

    fn show_toast(&mut self, message: String) {
        self.toast = Some((message, Instant::now()));
    }

    /// Renders this Instant View window as a separate top-level `eframe` viewport.
    pub fn show_viewport(
        &mut self,
        ctx: &egui::Context,
        palette: &TgPalette,
        is_night: bool,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        if !self.open {
            return Vec::new();
        }
        let mut events = self.drain_bridge_events(backend, external, clipboard);
        let title = self.window_title();
        let viewport_id = ViewportId::from_hash_of(IV_VIEWPORT_ID_STR);
        let builder = ViewportBuilder::default()
            .with_title(title.clone())
            .with_inner_size(Vec2::new(IV_DEFAULT_WIDTH, IV_DEFAULT_HEIGHT))
            .with_min_inner_size(Vec2::new(IV_MIN_WIDTH, IV_MIN_HEIGHT));

        ctx.show_viewport_immediate(viewport_id, builder, |vp_ui, _class| {
            let vp_ctx = vp_ui.ctx().clone();
            vp_ctx.set_visuals(egui_visuals_from_tg_palette(palette, is_night));
            if vp_ctx.input(|i| i.viewport().close_requested()) {
                self.close();
                return;
            }
            self.viewport_frames = self.viewport_frames.saturating_add(1);
            events.extend(
                self.render_window_contents(vp_ui, palette, &title, backend, external, clipboard),
            );
        });

        events
    }

    /// Renders the Instant View chrome + attaches/resizes WebView2 inside an active `egui::Ui`
    /// (callable from either [`Self::show_viewport`] or a standalone screenshot window).
    pub fn render_window_contents(
        &mut self,
        ui: &mut Ui,
        palette: &TgPalette,
        window_title: &str,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        let ctx = ui.ctx().clone();
        ensure_tdesktop_fonts(&ctx);
        let mut events = self.handle_keyboard_shortcuts(&ctx, backend, external, clipboard);
        let colors = TgThemeColors(palette);

        // 1. Top Instant View title bar (`iv_controller.cpp`: Back, Forward, Site/URL, Find, Zoom, Menu).
        Panel::top("tg_iv_title_bar")
            .exact_size(IV_TITLE_BAR_HEIGHT)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(color_to_egui(palette.window_bg)))
            .show(ui, |ui| {
                events.extend(self.render_title_bar(ui, palette, backend, external, clipboard));
            });

        // 1b. In-page Find Bar (`iv_search_bar.cpp` + `iv_search_controller.cpp`).
        let search_height = self.step_search_bar_animation(&ctx);
        if search_height > 0.5 {
            Panel::top("tg_iv_search_bar")
                .exact_size(search_height)
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(color_to_egui(palette.window_bg)))
                .show(ui, |ui| {
                    self.render_search_bar(ui, palette);
                });
        }

        // 2. Central webview content area (native child HWND or native RichPage + formula renderer).
        let mut content_rect = Rect::NOTHING;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(color_to_egui(palette.window_bg)))
            .show(ui, |ui| {
                content_rect = ui.max_rect();
                if let Some(err) = &self.runtime_error {
                    render_unavailable_backdrop(ui, content_rect, palette, err);
                } else if self.webview.is_none()
                    && let Some(page) = self.current_page.clone()
                {
                    // Native egui Instant View reader (paints PageBlocks + TeX formulas via formula_widget)
                    // while WebView2 is absent or headless.
                    let view_style = if self.dark_mode {
                        crate::webview::rich_page_view::RichPageViewStyle::night()
                    } else {
                        crate::webview::rich_page_view::RichPageViewStyle::day()
                    };
                    egui::ScrollArea::vertical()
                        .id_salt("tg_iv_native_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let max_w = (content_rect.width() - 32.0).clamp(240.0, 720.0);
                            egui::Frame::NONE
                                .inner_margin(egui::Margin::symmetric(16, 12))
                                .show(ui, |ui| {
                                    if self.search.is_shown()
                                        && let Some(snippet) = self.search.current_snippet()
                                    {
                                        self.render_native_search_match_banner(
                                            ui,
                                            palette,
                                            &view_style,
                                            &snippet,
                                            max_w,
                                        );
                                        ui.add_space(10.0);
                                    }
                                    let evs = crate::webview::formula_widget::with_math_engine(
                                        |engine| {
                                            crate::webview::rich_page_view::paint_iv_model_page(
                                                ui,
                                                "tg_iv_native_page",
                                                engine,
                                                &page,
                                                max_w,
                                                &view_style,
                                            )
                                        },
                                    );
                                    for ev in evs {
                                        let crate::webview::rich_page_view::RichPageViewEvent::OpenLink(
                                            url,
                                        ) = ev;
                                        let target = tg_iv::classify_link(
                                            &url,
                                            Some(self.controller.current_url()),
                                            None,
                                        );
                                        events.extend(self.route_link_target(
                                            target, backend, external,
                                        ));
                                    }
                                });
                        });
                }
            });

        // 3. Title bar dropdown menu (`tg-ui` PopupMenu).
        let menu_items = self.build_menu_items();
        let menu_style = PopupMenuStyle::default_popup(&colors);
        let toggle_style = ToggleStyle::default_toggle(&colors);
        if let Some(action_id) = self.popup_menu.show(
            &ctx,
            Id::unique("tg_iv_menu"),
            &menu_items,
            &menu_style,
            &toggle_style,
        ) {
            events.extend(self.handle_menu_action(&action_id, backend, external, clipboard));
        }

        // 4. Push missing-runtime / crash modal box once if needed.
        if let Some(err) = self.runtime_error.clone()
            && !self.runtime_error_box_pushed
        {
            push_runtime_error_box(&ctx, &mut self.layer_stack, palette, &err, true);
            self.runtime_error_box_pushed = true;
        }

        // 5. Render `tg-ui` LayerStack boxes (e.g. missing WebView2 runtime box).
        let box_style = BoxStyle::default_box(&colors);
        let fallback_url = self.controller.current_url().to_owned();
        let mut inline_download_clicked = false;
        let layer_event = self.layer_stack.show(
            &ctx,
            ctx.viewport_rect(),
            &box_style,
            &colors,
            |box_id, ui| {
                if box_id == RUNTIME_ERROR_BOX_ID
                    && let Some(err) = &self.runtime_error
                {
                    inline_download_clicked =
                        render_runtime_error_body(ui, palette, err, Some(&fallback_url));
                }
            },
        );
        if inline_download_clicked
            && let Some(url) = self
                .runtime_error
                .as_ref()
                .and_then(RuntimeErrorKind::download_url)
        {
            let _ = external.open_url(url);
            events.push(AppWebViewEvent::OpenExternalUrl {
                url: url.to_owned(),
            });
        }
        if let Some(ev) = layer_event
            && ev.box_id == RUNTIME_ERROR_BOX_ID
        {
            match ev.action_id.as_str() {
                ACTION_DOWNLOAD_WEBVIEW2 => {
                    if let Some(url) = self
                        .runtime_error
                        .as_ref()
                        .and_then(RuntimeErrorKind::download_url)
                    {
                        let _ = external.open_url(url);
                        events.push(AppWebViewEvent::OpenExternalUrl {
                            url: url.to_owned(),
                        });
                    }
                }
                ACTION_OPEN_IN_BROWSER => {
                    if !fallback_url.is_empty() {
                        let _ = external.open_url(&fallback_url);
                        events.push(AppWebViewEvent::OpenExternalUrl { url: fallback_url });
                    }
                }
                ACTION_CLOSE_ERROR | "__close__" => {
                    self.layer_stack.clear(&ctx, &box_style);
                    self.close();
                }
                _ => {}
            }
        }

        // 6. Transient status toast overlay.
        self.render_toast(&ctx, palette);

        // 7. Attach / synchronize native WebView2 child HWND.
        let overlay_active = self.popup_menu.is_open() || !self.layer_stack.is_empty();
        self.sync_native_webview(&ctx, window_title, content_rect, palette, overlay_active);

        events
    }

    fn render_title_bar(
        &mut self,
        ui: &mut Ui,
        palette: &TgPalette,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        let mut events = Vec::new();
        let colors = TgThemeColors(palette);
        let bar_rect = ui.max_rect();

        ui.horizontal_centered(|ui| {
            ui.add_space(6.0);

            // Back button (`ivBack`).
            let can_back = self.controller.can_go_back();
            let back_resp = nav_arrow_button(ui, "tg_iv_back", false, can_back, palette);
            if back_resp.clicked() && can_back {
                events.extend(self.go_back(backend, external));
            }

            // Forward button (`ivForward`).
            let can_fwd = self.controller.can_go_forward();
            let fwd_resp = nav_arrow_button(ui, "tg_iv_forward", true, can_fwd, palette);
            if fwd_resp.clicked() && can_fwd {
                events.extend(self.go_forward(backend, external));
            }

            ui.add_space(6.0);

            // Reserve exact right-cluster width (menu 34 + zoom 48 + search 32 + gaps) so the
            // title/subtitle truncates cleanly with ellipsis on narrow viewports (e.g. 390px).
            let right_cluster_w = 136.0;
            let title_w = (ui.available_width() - right_cluster_w).max(80.0);
            ui.allocate_ui_with_layout(
                Vec2::new(title_w, IV_TITLE_BAR_HEIGHT),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ui.set_max_width(title_w);
                    ui.add_space(5.0);
                    let title_text = if self.controller.current_title().is_empty() {
                        self.subtitle_text()
                    } else {
                        self.controller.current_title().to_owned()
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(title_text)
                                .font(font_id_from_style(st::iv_subtitle_font))
                                .strong()
                                .color(color_to_egui(palette.window_bold_fg)),
                        )
                        .truncate(),
                    );
                    let sub = format!(
                        "{} · {}",
                        self.subtitle_text(),
                        self.controller.current_url()
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(sub)
                                .size(11.5)
                                .color(color_to_egui(palette.window_sub_text_fg)),
                        )
                        .truncate(),
                    );
                },
            );

            // Right-aligned search button, zoom pill & overflow menu button.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);

                let (menu_rect, menu_resp) =
                    ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                menu_resp.widget_info(|| {
                    WidgetInfo::labeled(Role::Button, true, strings::INSTANT_VIEW_MENU_TOOLTIP)
                });
                if menu_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter().circle_filled(
                        menu_rect.center(),
                        16.0,
                        color_to_egui(palette.window_bg_over),
                    );
                }
                let menu_icon = if menu_resp.hovered() {
                    st::iv_menu_toggle.icon_over
                } else {
                    st::iv_menu_toggle.icon
                };
                paint_style_icon(ui.painter(), menu_rect, menu_icon, &colors, None);
                if menu_resp
                    .on_hover_text(strings::INSTANT_VIEW_MENU_TOOLTIP)
                    .clicked()
                {
                    if self.popup_menu.is_open() {
                        self.popup_menu.close();
                    } else {
                        let anchor = Pos2::new(menu_rect.right() - 210.0, menu_rect.bottom() + 4.0);
                        let menu_style = PopupMenuStyle::default_popup(&colors);
                        self.popup_menu.open_at(ui.ctx(), anchor, &menu_style);
                    }
                }

                // Quick zoom readout pill (clicks reset zoom).
                let zoom_pct = self.controller.zoom().percent();
                let zoom_label = format!("{zoom_pct}%");
                let (pill_rect, pill_resp) =
                    ui.allocate_exact_size(Vec2::new(46.0, 24.0), Sense::click());
                pill_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &zoom_label));
                if pill_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let pill_bg = if pill_resp.hovered() {
                    color_to_egui(palette.window_bg_over)
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter()
                    .rect_filled(pill_rect, CornerRadius::same(12), pill_bg);
                ui.painter().text(
                    pill_rect.center(),
                    Align2::CENTER_CENTER,
                    zoom_label,
                    FontId::proportional(12.0),
                    color_to_egui(palette.window_sub_text_fg),
                );
                if pill_resp
                    .on_hover_text(format!(
                        "{}\n{}",
                        strings::RESET_ZOOM_TOOLTIP,
                        tr::lng_iv_zoom_tooltip_ctrl()
                    ))
                    .clicked()
                {
                    events.extend(self.handle_menu_action(
                        "zoom_reset",
                        backend,
                        external,
                        clipboard,
                    ));
                }

                // In-page Search toggle button (`Ctrl+F`).
                let search_open = self.search.is_shown();
                let (search_rect, search_resp) =
                    ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                search_resp
                    .widget_info(|| WidgetInfo::labeled(Role::Button, true, tr::lng_dlg_filter()));
                if search_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if search_open || search_resp.hovered() {
                    ui.painter().circle_filled(
                        search_rect.center(),
                        15.0,
                        color_to_egui(palette.window_bg_over),
                    );
                }
                let icon_col = if search_open {
                    color_to_egui(palette.window_active_text_fg)
                } else if search_resp.hovered() {
                    color_to_egui(palette.window_fg)
                } else {
                    color_to_egui(palette.menu_icon_fg)
                };
                paint_search_magnifier_icon(ui.painter(), search_rect.center(), icon_col);
                if search_resp
                    .on_hover_text(strings::FIND_IN_PAGE_TOOLTIP)
                    .clicked()
                {
                    self.toggle_search_with_animation(true);
                }
            });
        });

        // Bottom 1px divider line.
        ui.painter().line_segment(
            [
                Pos2::new(bar_rect.min.x, bar_rect.max.y - 0.5),
                Pos2::new(bar_rect.max.x, bar_rect.max.y - 0.5),
            ],
            Stroke::new(1.0, color_to_egui(palette.menu_separator_fg)),
        );

        events
    }

    fn step_search_bar_animation(&mut self, ctx: &egui::Context) -> f32 {
        let target = if self.search.is_shown() { 1.0 } else { 0.0 };
        let reduced_motion = ctx.global_style().animation_time <= 0.0;
        if self.search_anim_instant || reduced_motion {
            self.search_bar_anim = target;
            self.search_anim_instant = false;
            return IV_SEARCH_BAR_HEIGHT * target;
        }
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.05);
        if (self.search_bar_anim - target).abs() > 0.001 {
            let step = dt / IV_SEARCH_BAR_ANIM_SECS.max(0.05);
            if self.search_bar_anim < target {
                self.search_bar_anim = (self.search_bar_anim + step).min(1.0);
            } else {
                self.search_bar_anim = (self.search_bar_anim - step).max(0.0);
            }
            ctx.request_repaint();
        } else {
            self.search_bar_anim = target;
        }
        let inv = 1.0 - self.search_bar_anim;
        let eased = 1.0 - inv * inv * inv; // cubic ease-out (`st::slideWrapDuration`)
        IV_SEARCH_BAR_HEIGHT * eased
    }

    fn render_search_bar(&mut self, ui: &mut Ui, palette: &TgPalette) {
        let colors = TgThemeColors(palette);
        let bar_rect = ui.max_rect();
        let (cur, total) = self.search.display_position();
        let has_matches = total > 0;
        let counter_skip = st::iv_search_bar_counter_skip.0 as f32;

        let counter_text = if total > 0 {
            Some((format!("{cur} / {total}"), color_to_egui(palette.window_fg)))
        } else if !self.search_query_input.is_empty() {
            Some((
                tr::lng_search_messages_none(),
                color_to_egui(palette.attention_button_fg),
            ))
        } else {
            None
        };

        let mut step_delta: isize = 0;
        let mut close_clicked = false;
        let mut query_changed = false;

        ui.horizontal_centered(|ui| {
            ui.add_space(10.0);

            let counter_w = if counter_text.is_some() {
                72.0 + counter_skip
            } else {
                0.0
            };
            // Reserve space for: counter + prev(32) + next(32) + close(32) + right padding(12).
            let right_w = counter_w + 114.0;
            let input_w = (ui.available_width() - right_w).max(110.0);

            let resp = ui.add_sized(
                Vec2::new(input_w, 30.0),
                TextEdit::singleline(&mut self.search_query_input)
                    .id(Id::unique("tg_iv_search_input"))
                    .hint_text(tr::lng_dlg_filter())
                    .margin(egui::Margin::symmetric(8, 5)),
            );
            resp.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, strings::FIND_IN_PAGE));
            if self.search_focus_requested {
                resp.request_focus();
                self.search_focus_requested = false;
            }
            if resp.changed() {
                query_changed = true;
            }
            let enter_pressed =
                (resp.has_focus() || resp.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
            if enter_pressed {
                let shift = ui.input(|i| i.modifiers.shift);
                step_delta = if shift { -1 } else { 1 };
                resp.request_focus();
            }

            // Right-aligned controls matching `iv_search_bar.cpp`: counter, Prev (▲), Next (▼), Close (×).
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);

                // Close search button.
                let (close_rect, close_resp) =
                    ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                close_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Close search"));
                if close_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter().circle_filled(
                        close_rect.center(),
                        14.0,
                        color_to_egui(palette.window_bg_over),
                    );
                }
                let close_col = if close_resp.hovered() {
                    color_to_egui(palette.window_fg)
                } else {
                    color_to_egui(palette.menu_icon_fg)
                };
                paint_close_cross(ui.painter(), close_rect.center(), 5.0, close_col);
                if close_resp
                    .on_hover_text(strings::CLOSE_SEARCH_TOOLTIP)
                    .clicked()
                {
                    close_clicked = true;
                }

                // Next match button (`calendarNext` / `calendarNextDisabled`).
                let next_resp = search_nav_button(
                    ui,
                    "Next match",
                    strings::NEXT_MATCH_TOOLTIP,
                    true,
                    has_matches,
                    palette,
                    &colors,
                );
                if next_resp.clicked() && has_matches {
                    step_delta = 1;
                }

                // Previous match button (`calendarPrevious` / `calendarPreviousDisabled`).
                let prev_resp = search_nav_button(
                    ui,
                    "Previous match",
                    strings::PREV_MATCH_TOOLTIP,
                    false,
                    has_matches,
                    palette,
                    &colors,
                );
                if prev_resp.clicked() && has_matches {
                    step_delta = -1;
                }

                if let Some((ref txt, col)) = counter_text {
                    ui.add_space(counter_skip * 0.5);
                    ui.label(RichText::new(txt).size(12.5).color(col));
                    ui.add_space(counter_skip * 0.5);
                }
            });
        });

        // Bottom 1px divider line (`Ui::PlainShadow` at `st::lineWidth`).
        ui.painter().line_segment(
            [
                Pos2::new(bar_rect.min.x, bar_rect.max.y - 0.5),
                Pos2::new(bar_rect.max.x, bar_rect.max.y - 0.5),
            ],
            Stroke::new(1.0, color_to_egui(palette.menu_separator_fg)),
        );

        if query_changed {
            let q = self.search_query_input.clone();
            let _ = self.search.set_query(q);
            self.sync_search_highlight_to_webview();
            ui.ctx().request_repaint();
        }
        if step_delta > 0 {
            let _ = self.search_next();
            ui.ctx().request_repaint();
        } else if step_delta < 0 {
            let _ = self.search_prev();
            ui.ctx().request_repaint();
        }
        if close_clicked {
            self.close_search();
            ui.ctx().request_repaint();
        }
    }

    fn render_native_search_match_banner(
        &self,
        ui: &mut Ui,
        palette: &TgPalette,
        view_style: &crate::webview::rich_page_view::RichPageViewStyle,
        snippet: &tg_iv::SearchSnippet,
        max_width: f32,
    ) {
        let (cur, total) = self.search.display_position();
        let accent = color_to_egui(palette.window_active_text_fg);
        let resp = egui::Frame::NONE
            .fill(color_to_egui(palette.window_bg_over))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin {
                left: 12,
                right: 10,
                top: 6,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.set_max_width((max_width - 22.0).max(160.0));
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = (max_width - 24.0).max(160.0);
                let normal_fmt = egui::TextFormat {
                    font_id: FontId::proportional(12.5),
                    color: view_style.text_fg,
                    ..egui::TextFormat::default()
                };
                let badge_fmt = egui::TextFormat {
                    font_id: FontId::proportional(11.5),
                    color: accent,
                    ..egui::TextFormat::default()
                };
                let hit_fmt = egui::TextFormat {
                    font_id: FontId::proportional(12.5),
                    color: view_style.text_fg,
                    background: view_style.mark_bg,
                    ..egui::TextFormat::default()
                };
                job.append(&format!("Match {cur}/{total}: "), 0.0, badge_fmt);
                job.append(&snippet.prefix, 0.0, normal_fmt.clone());
                job.append(&snippet.matched, 0.0, hit_fmt);
                job.append(&snippet.suffix, 0.0, normal_fmt);
                ui.label(job);
            })
            .response;
        let stripe = Rect::from_min_size(resp.rect.min, Vec2::new(3.0, resp.rect.height()));
        ui.painter()
            .rect_filled(stripe, CornerRadius::same(2), accent);
    }

    fn build_menu_items(&self) -> Vec<MenuItem> {
        let zoom = self.controller.zoom().percent();
        vec![
            MenuItem::action("find", strings::FIND_IN_PAGE)
                .with_icon(IconKind::Search)
                .with_shortcut("Ctrl+F"),
            MenuItem::Separator,
            MenuItem::action("open_browser", tr::lng_iv_open_in_browser())
                .with_icon(IconKind::Info),
            MenuItem::action("copy_link", tr::lng_context_copy_link()).with_icon(IconKind::Edit),
            MenuItem::action("share", tr::lng_iv_share()).with_icon(IconKind::Send),
            MenuItem::Separator,
            MenuItem::action("zoom_in", strings::ZOOM_IN)
                .with_icon(IconKind::Plus)
                .with_shortcut("Ctrl++"),
            MenuItem::action("zoom_out", strings::ZOOM_OUT).with_shortcut("Ctrl+-"),
            MenuItem::action(
                "zoom_reset",
                format!("{} ({zoom}%)", strings::RESET_ZOOM_PREFIX),
            )
            .with_shortcut("Ctrl+0"),
        ]
    }

    fn handle_keyboard_shortcuts(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        let mut shortcut = None;
        let mut toggle_search = false;
        let mut close_search = false;
        let mut step_search: isize = 0;

        ctx.input(|i| {
            let cmd = i.modifiers.matches_logically(Modifiers::COMMAND)
                || (i.modifiers.command && !i.modifiers.alt);
            if cmd && i.key_pressed(Key::F) {
                toggle_search = true;
            } else if i.key_pressed(Key::Escape) {
                if self.search.is_shown() {
                    close_search = true;
                } else {
                    shortcut = Some(ShortcutAction::Close);
                }
            } else if self.search.is_shown()
                && (i.key_pressed(Key::F3) || (cmd && i.key_pressed(Key::G)))
            {
                step_search = if i.modifiers.shift { -1 } else { 1 };
            } else if i.modifiers.matches_logically(Modifiers::COMMAND) {
                if i.key_pressed(Key::W) {
                    shortcut = Some(ShortcutAction::Close);
                } else if i.key_pressed(Key::M) {
                    shortcut = Some(ShortcutAction::Minimize);
                } else if i.key_pressed(Key::Q) {
                    shortcut = Some(ShortcutAction::Quit);
                } else if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                    shortcut = Some(ShortcutAction::ZoomIn);
                } else if i.key_pressed(Key::Minus) {
                    shortcut = Some(ShortcutAction::ZoomOut);
                } else if i.key_pressed(Key::Num0) {
                    shortcut = Some(ShortcutAction::ZoomReset);
                }
            }
        });

        if toggle_search {
            self.toggle_search();
            return Vec::new();
        }
        if close_search {
            self.close_search();
            return Vec::new();
        }
        if step_search > 0 {
            let _ = self.search_next();
            return Vec::new();
        }
        if step_search < 0 {
            let _ = self.search_prev();
            return Vec::new();
        }

        let Some(sc) = shortcut else {
            return Vec::new();
        };
        if sc == ShortcutAction::Minimize {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        let actions = self
            .controller
            .handle_bridge_message(tg_iv::BridgeMessage::KeyShortcut { shortcut: sc });
        self.handle_controller_actions(actions, backend, external, clipboard)
    }

    fn drain_bridge_events(
        &mut self,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.rx_events.try_recv() {
            match ev {
                IvBridgeEvent::BridgeMessage(msg) => {
                    events.extend(self.handle_web_message(&msg, backend, external, clipboard));
                }
                IvBridgeEvent::InterceptedTarget(target) => {
                    events.extend(self.route_link_target(target, backend, external));
                }
                IvBridgeEvent::ZoomFactorChanged(pct) => {
                    let clamped = (pct as i32).clamp(tg_iv::ZOOM_MIN, tg_iv::ZOOM_MAX);
                    if clamped != self.controller.zoom().percent() {
                        let _ = self.controller.set_zoom(clamped);
                        self.zoom_synced = clamped;
                    }
                }
                IvBridgeEvent::NavigationDone(true) => {
                    if let Some(frag) = self.pending_fragment.take() {
                        self.eval_host_command(&HostCommand::ScrollToAnchor { fragment: frag });
                    } else if let Some(y) = self.pending_scroll_restore.take() {
                        self.eval_host_command(&HostCommand::ScrollToY { y });
                    }
                }
                IvBridgeEvent::NavigationDone(false) => {}
                IvBridgeEvent::ProcessFailed => {
                    if let Some(wv) = self.webview.take() {
                        wv.close();
                    }
                    self.runtime_error = Some(RuntimeErrorKind::ProcessCrashed);
                    self.runtime_error_box_pushed = false;
                }
            }
        }
        events
    }

    fn sync_native_webview(
        &mut self,
        ctx: &egui::Context,
        window_title: &str,
        content_rect: Rect,
        palette: &TgPalette,
        overlay_active: bool,
    ) {
        if self.runtime_error.is_some() {
            return;
        }

        // Wait one frame after viewport creation so the Win32 HWND is fully mapped.
        if !self.webview_init_attempted && self.viewport_frames >= 2 {
            self.try_attach_webview(ctx, window_title, content_rect, palette);
        }

        let Some(wv) = &self.webview else {
            return;
        };

        // Hide native child HWND while a `tg-ui` dropdown menu or modal box is open to avoid
        // Windows Win32 airspace occlusion.
        if overlay_active != self.webview_hidden_for_overlay {
            let _ = wv.set_visible(!overlay_active);
            self.webview_hidden_for_overlay = overlay_active;
        }

        if let Some(bounds) = self.compute_physical_bounds(ctx, content_rect)
            && self.last_bounds != Some(bounds)
        {
            let _ = wv.set_bounds(bounds);
            self.last_bounds = Some(bounds);
        }
    }

    /// Attempts to locate the parent Win32 `HWND` and embed a WebView2 child controller.
    pub fn try_attach_webview(
        &mut self,
        ctx: &egui::Context,
        window_title: &str,
        content_rect: Rect,
        palette: &TgPalette,
    ) {
        self.webview_init_attempted = true;

        let avail = availability();
        if let Some(err) = RuntimeErrorKind::from_availability(&avail) {
            self.runtime_error = Some(err);
            return;
        }

        let Some(hwnd) = find_thread_window_by_title(window_title) else {
            // Retry on next frame if the viewport HWND is not yet visible.
            self.webview_init_attempted = false;
            return;
        };
        self.attached_hwnd = Some(hwnd);

        let Some(raw_handle) = win32_raw_handle(hwnd) else {
            self.runtime_error = Some(RuntimeErrorKind::InitFailed {
                details: strings::PARENT_HANDLE_UNAVAILABLE.to_owned(),
            });
            return;
        };

        let bounds = self
            .compute_physical_bounds(ctx, content_rect)
            .unwrap_or_else(|| WebViewBounds::new(0, 46, 800, 680));

        let tx_msg = self.tx_events.clone();
        let tx_pol = self.tx_events.clone();
        let tx_done = self.tx_events.clone();
        let tx_zoom = self.tx_events.clone();
        let tx_fail = self.tx_events.clone();
        let ctx_msg = ctx.clone();
        let ctx_pol = ctx.clone();
        let ctx_done = ctx.clone();
        let ctx_zoom = ctx.clone();
        let ctx_fail = ctx.clone();
        let url_shared = Arc::clone(&self.current_url_shared);

        let user_data_path = self.webview_user_data_dir();
        let config = WebViewConfig {
            opaque_bg: palette.window_bg,
            user_data_path: Some(user_data_path),
            debug: cfg!(debug_assertions),
            safe: true,
            initial_bounds: Some(bounds),
            custom_scheme: DEFAULT_CUSTOM_SCHEME.to_owned(),
            scheme_router: Some(self.resource_bridge.router()),
            callbacks: WebViewCallbacks {
                on_message: Some(Arc::new(move |msg| {
                    let _ = tx_msg.send(IvBridgeEvent::BridgeMessage(msg));
                    ctx_msg.request_repaint();
                })),
                on_navigation_policy: Some(Arc::new(move |uri, new_window| {
                    let cur_url = url_shared.lock().map(|g| g.clone()).unwrap_or_default();
                    let policy_ctrl = IvController::new(IV_ORIGIN, ZOOM_DEFAULT);
                    match evaluate_iv_navigation_policy(&policy_ctrl, uri, &cur_url, new_window) {
                        NavigationDecision::Allow => true,
                        NavigationDecision::Cancel(target) => {
                            if let Some(t) = target {
                                let _ = tx_pol.send(IvBridgeEvent::InterceptedTarget(t));
                                ctx_pol.request_repaint();
                            }
                            false
                        }
                    }
                })),
                on_navigation_done: Some(Arc::new(move |ok| {
                    let _ = tx_done.send(IvBridgeEvent::NavigationDone(ok));
                    ctx_done.request_repaint();
                })),
                on_zoom_changed: Some(Arc::new(move |pct| {
                    let _ = tx_zoom.send(IvBridgeEvent::ZoomFactorChanged(pct));
                    ctx_zoom.request_repaint();
                })),
                on_process_failed: Some(Arc::new(move || {
                    let _ = tx_fail.send(IvBridgeEvent::ProcessFailed);
                    ctx_fail.request_repaint();
                })),
                ..WebViewCallbacks::default()
            },
            ..WebViewConfig::default()
        };

        match WebView::create_from_raw(Some(raw_handle), config) {
            Ok(wv) => {
                let params = build_webview_theme_params(palette);
                let _ = wv.update_theme(&params);
                if let Ok(pct) = u32::try_from(self.controller.zoom().percent()) {
                    let _ = wv.set_zoom(pct);
                }
                let _ = wv.navigate_to_data("index.html");
                self.last_bounds = Some(bounds);
                self.webview = Some(wv);
            }
            Err(err) => {
                self.runtime_error = Some(RuntimeErrorKind::from_webview_error(&err));
            }
        }
    }

    fn compute_physical_bounds(
        &self,
        ctx: &egui::Context,
        content_rect: Rect,
    ) -> Option<WebViewBounds> {
        let ppp = ctx.pixels_per_point();
        let top_px = (content_rect.min.y * ppp).round() as i32;
        let left_px = (content_rect.min.x * ppp).round() as i32;
        let (w_px, h_px) = if let Some(hwnd) = self.attached_hwnd
            && let Some((cw, ch)) = client_size_for_hwnd(hwnd)
        {
            ((cw - left_px).max(1), (ch - top_px).max(1))
        } else {
            (
                (content_rect.width() * ppp).round().max(1.0) as i32,
                (content_rect.height() * ppp).round().max(1.0) as i32,
            )
        };
        Some(WebViewBounds::new(left_px, top_px, w_px, h_px))
    }

    fn render_toast(&mut self, ctx: &egui::Context, palette: &TgPalette) {
        let Some((msg, shown_at)) = &self.toast else {
            return;
        };
        if shown_at.elapsed() >= TOAST_DURATION {
            self.toast = None;
            return;
        }
        let vp = ctx.viewport_rect();
        egui::Area::new(Id::unique("tg_iv_toast"))
            .order(egui::Order::Tooltip)
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -18.0))
            .show(ctx, |ui| {
                let bg = color_to_egui(palette.toast_bg);
                let fg = color_to_egui(palette.toast_fg);
                egui::Frame::NONE
                    .fill(bg)
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.set_max_width((vp.width() - 48.0).max(180.0));
                        ui.label(RichText::new(msg).size(12.5).color(fg));
                    });
            });
    }
}

/// Evaluates Instant View navigation policy for `uri` against `current_page_url`.
///
/// Allows `about:blank`, `rustgram-internal://iv/*`, and `.magic.org` TON sites; cancels everything
/// else and classifies the target via [`tg_iv::classify_link`].
#[must_use]
pub fn evaluate_iv_navigation_policy(
    controller: &IvController,
    uri: &str,
    current_page_url: &str,
    new_window: bool,
) -> NavigationDecision {
    match controller.navigation_policy(uri, new_window) {
        NavigationDecision::Allow => NavigationDecision::Allow,
        NavigationDecision::Cancel(_) => {
            let target = tg_iv::classify_link(uri, Some(current_page_url), None);
            NavigationDecision::Cancel(Some(target))
        }
    }
}

fn nav_arrow_button(
    ui: &mut Ui,
    id: &str,
    forward: bool,
    enabled: bool,
    palette: &TgPalette,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(34.0, 34.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(rect.center(), 16.0, color_to_egui(palette.window_bg_over));
    }
    let colors = TgThemeColors(palette);
    let icon = match (forward, enabled) {
        (true, true) => st::iv_forward_icon,
        (true, false) => st::iv_forward_icon_disabled,
        (false, true) => st::iv_back_icon,
        (false, false) => st::iv_back_icon_disabled,
    };
    let tint = if !enabled {
        Some(color_to_egui(palette.menu_fg_disabled))
    } else if resp.hovered() {
        Some(color_to_egui(palette.window_fg))
    } else {
        None
    };
    paint_style_icon(ui.painter(), rect, icon, &colors, tint);
    let tip = if forward {
        strings::FORWARD_TOOLTIP
    } else {
        strings::BACK_TOOLTIP
    };
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, tip));
    let _ = id;
    resp.on_hover_text(tip)
}

fn search_nav_button(
    ui: &mut Ui,
    accessible_label: &str,
    tooltip: &str,
    next: bool,
    enabled: bool,
    palette: &TgPalette,
    colors: &TgThemeColors<'_>,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::splat(32.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, accessible_label));
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(rect.center(), 14.0, color_to_egui(palette.window_bg_over));
    }
    let icon = match (next, enabled) {
        (true, true) => st::calendar_next.icon,
        (true, false) => st::calendar_next_disabled,
        (false, true) => st::calendar_previous.icon,
        (false, false) => st::calendar_previous_disabled,
    };
    let tint = if !enabled {
        Some(color_to_egui(palette.menu_fg_disabled))
    } else if resp.hovered() {
        Some(color_to_egui(palette.window_fg))
    } else {
        Some(color_to_egui(palette.menu_icon_fg))
    };
    paint_style_icon(ui.painter(), rect, icon, colors, tint);
    resp.on_hover_text(tooltip)
}

fn paint_search_magnifier_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let lens_center = Pos2::new(center.x - 1.5, center.y - 1.5);
    painter.circle_stroke(lens_center, 5.2, Stroke::new(1.7, color));
    painter.line_segment(
        [
            Pos2::new(lens_center.x + 3.7, lens_center.y + 3.7),
            Pos2::new(center.x + 6.2, center.y + 6.2),
        ],
        Stroke::new(1.8, color),
    );
}

fn paint_close_cross(painter: &egui::Painter, center: Pos2, half: f32, color: Color32) {
    let stroke = Stroke::new(1.7, color);
    painter.line_segment(
        [
            Pos2::new(center.x - half, center.y - half),
            Pos2::new(center.x + half, center.y + half),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x + half, center.y - half),
            Pos2::new(center.x - half, center.y + half),
        ],
        stroke,
    );
}
