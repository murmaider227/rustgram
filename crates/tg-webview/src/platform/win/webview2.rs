//! Microsoft Edge WebView2 COM backend for Windows.
//!
//! Port of `Telegram/lib_webview/webview/platform/win/webview_windows_edge_chromium.cpp` and
//! `Telegram/lib_webview/webview/platform/win/webview_win.cpp`.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use raw_window_handle::RawWindowHandle;
use tg_theme::Color;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_COLOR, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
    COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
    COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
    COREWEBVIEW2_PERMISSION_STATE_ALLOW, COREWEBVIEW2_PERMISSION_STATE_DENY,
    COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT, COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM,
    COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
    CreateCoreWebView2EnvironmentWithOptions, GetAvailableCoreWebView2BrowserVersionString,
    ICoreWebView2, ICoreWebView2_4, ICoreWebView2_5, ICoreWebView2_8, ICoreWebView2_10,
    ICoreWebView2Controller, ICoreWebView2Controller2, ICoreWebView2CustomSchemeRegistration,
    ICoreWebView2Environment, ICoreWebView2Environment10, ICoreWebView2EnvironmentOptions,
    ICoreWebView2PermissionRequestedEventArgs3, ICoreWebView2Settings8,
    ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2WebResourceResponse,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    BasicAuthenticationRequestedEventHandler, ClientCertificateRequestedEventHandler,
    CoTaskMemPWSTR, ContainsFullScreenElementChangedEventHandler, ContentLoadingEventHandler,
    CoreWebView2CustomSchemeRegistration, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, DownloadStartingEventHandler, ExecuteScriptCompletedHandler,
    NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, PermissionRequestedEventHandler, ProcessFailedEventHandler,
    ScriptDialogOpeningEventHandler, SourceChangedEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler, ZoomFactorChangedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{
    E_POINTER, HINSTANCE, HWND, LPARAM, LRESULT, RECT, RPC_E_CHANGED_MODE, S_FALSE, S_OK, WPARAM,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetClientRect, GetWindowLongPtrW, HWND_MESSAGE, MoveWindow, RegisterClassExW, SW_HIDE, SW_SHOW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_PARENTNOTIFY, WM_RBUTTONDOWN, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_OVERLAPPED, WS_VISIBLE,
};
use windows_core::{Interface, PCWSTR, PWSTR, w};

use crate::bridge::origin_from_url;
use crate::data_stream::{
    DataRequest, DataResponse, DataResult, HttpResponse, SchemeRouter, parse_range_header_for,
};
use crate::embed::{
    is_allowed_navigation_scheme, is_allowed_restricted_url, make_restricted_content,
};
use crate::error::{Availability, AvailabilityError, Result, WEBVIEW2_DOWNLOAD_URL, WebViewError};
use crate::interface::{
    DialogArgs, DialogType, InteractionHandler, NavigationHistoryState, PermissionType, WebMessage,
    WebViewBounds, WebViewConfig, WindowMode,
};
use crate::platform::win::data_stream_com::DataStreamCom;

const HOST_CLASS_NAME: PCWSTR = w!("RustgramWebView2Host");

/// Browser arguments matching `webview_windows_edge_chromium.cpp:1171-1177`.
const DEFAULT_BROWSER_ARGS: &str =
    "--disable-features=ElasticOverscroll,AutofillAiWalletPrivatePasses,FileChooserMinWindowSize";
const RESTRICTED_BROWSER_ARGS: &str = "--disable-features=ElasticOverscroll,AutofillAiWalletPrivatePasses,FileChooserMinWindowSize,msSmartScreenProtection --force-webrtc-ip-handling-policy=disable_non_proxied_udp --mute-audio";

/// Core bridge init script matching `webview_windows_edge_chromium.cpp:1141`.
const EXTERNAL_INVOKE_INIT_SCRIPT: &str =
    "if(window===window.top){window.external={invoke:s=>window.chrome.webview.postMessage(s)}}";

static HOST_CLASS_REGISTERED: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Queries Microsoft Edge WebView2 Runtime availability and version on Windows.
///
/// Port of `Webview::Availability()` (`webview_win.cpp:31-50`) and
/// `Webview::EdgeChromium::Supported()` (`webview_windows_edge_chromium.cpp:1464-1473`).
#[must_use]
pub(crate) fn availability() -> Availability {
    let mut version_ptr = PWSTR::null();
    // SAFETY: `GetAvailableCoreWebView2BrowserVersionString` allocates a CoTaskMem UTF-16 string
    // on `S_OK`, which `take_pwstr` immediately copies and frees via `CoTaskMemFree`.
    let hr =
        unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version_ptr) };
    if hr.is_ok() && !version_ptr.is_null() {
        let version = take_pwstr(version_ptr);
        if !version.is_empty() {
            return Availability {
                error: AvailabilityError::None,
                custom_scheme_requests: true,
                custom_range_requests: true,
                custom_referer: true,
                browser_version: Some(version),
                details: String::new(),
            };
        }
    }
    Availability {
        error: AvailabilityError::NoWebView2,
        custom_scheme_requests: false,
        custom_range_requests: false,
        custom_referer: false,
        browser_version: None,
        details: "Please install Microsoft Edge WebView2 Runtime.".to_owned(),
    }
}

/// Shared state accessed by WebView2 COM callbacks and the main [`EdgeChromiumWebView`] handle.
struct SharedState {
    history: Mutex<NavigationHistoryState>,
    allowed_permissions: Mutex<HashSet<(String, i32)>>,
    destroying: AtomicBool,
}

/// Embedded Microsoft Edge WebView2 instance owning a child host HWND and COM controller.
pub(crate) struct EdgeChromiumWebView {
    host_hwnd: HWND,
    interaction_box_ptr: Option<*mut InteractionHandler>,
    _environment: ICoreWebView2Environment,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    router: Arc<SchemeRouter>,
    shared: Arc<SharedState>,
    closed: AtomicBool,
    com_initialized: bool,
}

impl EdgeChromiumWebView {
    /// Creates and initializes a WebView2 instance embedded as a child of `parent`.
    pub(crate) fn create(parent: Option<RawWindowHandle>, config: WebViewConfig) -> Result<Self> {
        let avail = availability();
        if !avail.is_supported() {
            return Err(WebViewError::RuntimeNotInstalled {
                details: avail.details,
                download_url: WEBVIEW2_DOWNLOAD_URL,
            });
        }

        let restricted_content = match config.restricted_origin.as_deref() {
            Some(origin) => Some(
                make_restricted_content(origin)
                    .ok_or_else(|| WebViewError::InvalidRestrictedOrigin(origin.to_owned()))?,
            ),
            None => None,
        };

        let parent_hwnd = extract_parent_hwnd(parent, config.mode)?;
        let (host_hwnd, initial_rect, interaction_box_ptr) = create_host_window(
            parent_hwnd,
            config.mode,
            config.initial_bounds,
            config.callbacks.on_interaction.clone(),
        )?;

        // SAFETY: Initialize single-threaded COM apartment required by WebView2 on the UI thread.
        let com_hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let com_initialized = com_hr == S_OK || com_hr == S_FALSE;
        if !com_initialized && com_hr != RPC_E_CHANGED_MODE {
            destroy_host_window(host_hwnd, interaction_box_ptr);
            return Err(WebViewError::Backend(format!(
                "CoInitializeEx failed: {com_hr:?}"
            )));
        }

        match Self::init_webview2(
            host_hwnd,
            initial_rect,
            interaction_box_ptr,
            config,
            restricted_content,
            com_initialized,
        ) {
            Ok(instance) => Ok(instance),
            Err(err) => {
                destroy_host_window(host_hwnd, interaction_box_ptr);
                if com_initialized {
                    // SAFETY: Paired with `CoInitializeEx` returning `S_OK`/`S_FALSE` above.
                    unsafe { CoUninitialize() };
                }
                Err(err)
            }
        }
    }

    fn init_webview2(
        host_hwnd: HWND,
        initial_rect: RECT,
        interaction_box_ptr: Option<*mut InteractionHandler>,
        config: WebViewConfig,
        restricted_content: Option<crate::embed::RestrictedContent>,
        com_initialized: bool,
    ) -> Result<Self> {
        let router = config
            .scheme_router
            .clone()
            .unwrap_or_else(|| Arc::new(SchemeRouter::new(config.custom_scheme.clone())));

        if let Some(dir) = &config.user_data_path {
            std::fs::create_dir_all(dir).map_err(|source| WebViewError::UserDataDir {
                path: dir.display().to_string(),
                source,
            })?;
        }

        let environment = create_environment(
            config.user_data_path.as_deref(),
            router.scheme(),
            config.restricted_origin.is_some(),
        )?;

        let controller =
            create_controller(&environment, host_hwnd, config.restricted_origin.is_some())?;

        // SAFETY: `controller` is a valid COM interface created on this STA thread.
        let webview = unsafe { controller.CoreWebView2() }
            .map_err(|e| WebViewError::Backend(format!("CoreWebView2() failed: {e}")))?;

        let shared = Arc::new(SharedState {
            history: Mutex::new(NavigationHistoryState::default()),
            allowed_permissions: Mutex::new(HashSet::new()),
            destroying: AtomicBool::new(false),
        });

        configure_settings(
            &webview,
            &controller,
            config.debug,
            config.restricted_origin.is_some(),
            config.opaque_bg,
            initial_rect,
            config.mode == WindowMode::Hidden,
        )?;

        register_event_handlers(
            &environment,
            &controller,
            &webview,
            &router,
            &config,
            &shared,
        )?;

        let instance = Self {
            host_hwnd,
            interaction_box_ptr,
            _environment: environment,
            controller,
            webview,
            router,
            shared,
            closed: AtomicBool::new(false),
            com_initialized,
        };

        // Inject default bridge shim (`window.external.invoke`) and optional restricted lockdown.
        instance.init(EXTERNAL_INVOKE_INIT_SCRIPT)?;
        if let Some(restricted) = restricted_content {
            instance.init(&restricted.script)?;
        }

        Ok(instance)
    }

    pub(crate) fn navigate(&self, url: &str) -> Result<()> {
        self.ensure_open()?;
        let wide = CoTaskMemPWSTR::from(url);
        // SAFETY: `wide` stays alive across the COM call and holds a NUL-terminated UTF-16 string.
        unsafe { self.webview.Navigate(*wide.as_ref().as_pcwstr()) }
            .map_err(|e| WebViewError::Backend(format!("Navigate failed: {e}")))
    }

    pub(crate) fn navigate_to_data(&self, id: &str) -> Result<()> {
        let full = self.router.url_for(id);
        self.navigate(&full)
    }

    pub(crate) fn load_html(&self, html: &str) -> Result<()> {
        self.ensure_open()?;
        let wide = CoTaskMemPWSTR::from(html);
        // SAFETY: `wide` stays alive across the COM call and holds a NUL-terminated UTF-16 string.
        unsafe { self.webview.NavigateToString(*wide.as_ref().as_pcwstr()) }
            .map_err(|e| WebViewError::Backend(format!("NavigateToString failed: {e}")))
    }

    pub(crate) fn reload(&self) -> Result<()> {
        self.ensure_open()?;
        // SAFETY: Valid COM interface on STA thread.
        unsafe { self.webview.Reload() }
            .map_err(|e| WebViewError::Backend(format!("Reload failed: {e}")))
    }

    pub(crate) fn init(&self, js: &str) -> Result<()> {
        self.ensure_open()?;
        let webview = self.webview.clone();
        let script = js.to_owned();
        AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                let wide = CoTaskMemPWSTR::from(script.as_str());
                // SAFETY: `wide` stays alive across the COM call.
                unsafe {
                    webview
                        .AddScriptToExecuteOnDocumentCreated(*wide.as_ref().as_pcwstr(), &handler)
                        .map_err(webview2_com::Error::WindowsError)
                }
            }),
            Box::new(|error_code, _id| error_code),
        )
        .map_err(|e| {
            WebViewError::Backend(format!("AddScriptToExecuteOnDocumentCreated failed: {e}"))
        })
    }

    pub(crate) fn eval(&self, js: &str) -> Result<()> {
        self.ensure_open()?;
        let wide = CoTaskMemPWSTR::from(js);
        let handler = ExecuteScriptCompletedHandler::create(Box::new(|_res, _output| Ok(())));
        // SAFETY: `wide` stays alive across the COM call; async execution fires and forgets.
        unsafe {
            self.webview
                .ExecuteScript(*wide.as_ref().as_pcwstr(), &handler)
        }
        .map_err(|e| WebViewError::Backend(format!("ExecuteScript failed: {e}")))
    }

    pub(crate) fn set_opaque_bg(&self, color: Color) -> Result<()> {
        self.ensure_open()?;
        apply_default_background(&self.controller, color);
        Ok(())
    }

    pub(crate) fn set_bounds(&self, bounds: WebViewBounds) -> Result<()> {
        self.ensure_open()?;
        let width = bounds.width.max(0);
        let height = bounds.height.max(0);
        // SAFETY: `self.host_hwnd` is a valid HWND owned by this instance.
        unsafe {
            let _ = MoveWindow(self.host_hwnd, bounds.x, bounds.y, width, height, true);
            self.controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                })
                .map_err(|e| WebViewError::Backend(format!("SetBounds failed: {e}")))?;
        }
        Ok(())
    }

    pub(crate) fn set_visible(&self, visible: bool) -> Result<()> {
        self.ensure_open()?;
        // SAFETY: Valid COM controller and owned host HWND.
        unsafe {
            let _ = ShowWindow(self.host_hwnd, if visible { SW_SHOW } else { SW_HIDE });
            self.controller
                .SetIsVisible(visible)
                .map_err(|e| WebViewError::Backend(format!("SetIsVisible failed: {e}")))?;
        }
        Ok(())
    }

    pub(crate) fn set_devtools_enabled(&self, enabled: bool) -> Result<()> {
        self.ensure_open()?;
        // SAFETY: Valid COM interface.
        unsafe {
            let settings = self
                .webview
                .Settings()
                .map_err(|e| WebViewError::Backend(format!("Settings() failed: {e}")))?;
            settings
                .SetAreDevToolsEnabled(enabled)
                .map_err(|e| WebViewError::Backend(format!("SetAreDevToolsEnabled failed: {e}")))?;
            settings
                .SetAreDefaultContextMenusEnabled(enabled)
                .map_err(|e| {
                    WebViewError::Backend(format!("SetAreDefaultContextMenusEnabled failed: {e}"))
                })?;
        }
        Ok(())
    }

    pub(crate) fn open_devtools(&self) -> Result<()> {
        self.ensure_open()?;
        // SAFETY: Valid COM interface.
        unsafe { self.webview.OpenDevToolsWindow() }
            .map_err(|e| WebViewError::Backend(format!("OpenDevToolsWindow failed: {e}")))
    }

    pub(crate) fn set_zoom(&self, percent: u32) -> Result<()> {
        self.ensure_open()?;
        let clamped = percent.clamp(25, 500);
        // SAFETY: Valid COM controller.
        unsafe { self.controller.SetZoomFactor(f64::from(clamped) / 100.0) }
            .map_err(|e| WebViewError::Backend(format!("SetZoomFactor failed: {e}")))
    }

    pub(crate) fn zoom(&self) -> Result<u32> {
        self.ensure_open()?;
        let mut factor = 1.0_f64;
        // SAFETY: Valid COM controller and writable out parameter.
        unsafe { self.controller.ZoomFactor(&mut factor) }
            .map_err(|e| WebViewError::Backend(format!("ZoomFactor failed: {e}")))?;
        Ok((factor * 100.0).round().clamp(0.0, 1000.0) as u32)
    }

    pub(crate) fn focus(&self) -> Result<()> {
        self.ensure_open()?;
        // SAFETY: Valid owned HWND and COM controller.
        unsafe {
            let _ = SetForegroundWindow(self.host_hwnd);
            let _ = SetFocus(Some(self.host_hwnd));
            self.controller
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                .map_err(|e| WebViewError::Backend(format!("MoveFocus failed: {e}")))?;
        }
        Ok(())
    }

    pub(crate) fn navigation_history_state(&self) -> NavigationHistoryState {
        self.shared
            .history
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub(crate) fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.shared.destroying.store(true, Ordering::SeqCst);
            // SAFETY: Closing the WebView2 controller is safe once per instance.
            unsafe {
                let _ = self.controller.Close();
            }
        }
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(WebViewError::Closed)
        } else {
            Ok(())
        }
    }
}

impl Drop for EdgeChromiumWebView {
    fn drop(&mut self) {
        self.close();
        destroy_host_window(self.host_hwnd, self.interaction_box_ptr.take());
        if self.com_initialized {
            // SAFETY: Paired with `CoInitializeEx` returning `S_OK`/`S_FALSE` in `create`.
            unsafe { CoUninitialize() };
        }
    }
}

fn extract_parent_hwnd(parent: Option<RawWindowHandle>, mode: WindowMode) -> Result<Option<HWND>> {
    match (parent, mode) {
        (Some(RawWindowHandle::Win32(handle)), _) => {
            Ok(Some(HWND(handle.hwnd.get() as *mut c_void)))
        }
        (Some(other), _) => Err(WebViewError::InvalidWindowHandle(format!(
            "expected Win32 HWND, got {other:?}"
        ))),
        (None, WindowMode::Hidden) => Ok(None),
        (None, _) => Err(WebViewError::InvalidWindowHandle(
            "parent window handle is required for Embedded/External mode".to_owned(),
        )),
    }
}

fn ensure_host_class_registered() -> Result<()> {
    let result = HOST_CLASS_REGISTERED.get_or_init(|| {
        // SAFETY: Registering a process-global Win32 window class with a valid static WndProc.
        unsafe {
            let hinstance = GetModuleHandleW(None)
                .map(|m| HINSTANCE(m.0))
                .unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(host_wnd_proc),
                hInstance: hinstance,
                lpszClassName: HOST_CLASS_NAME,
                ..WNDCLASSEXW::default()
            };
            if RegisterClassExW(&wc) == 0 {
                return Err("RegisterClassExW failed".to_owned());
            }
            Ok(())
        }
    });
    result.clone().map_err(WebViewError::HostWindow)
}

fn create_host_window(
    parent: Option<HWND>,
    mode: WindowMode,
    initial_bounds: Option<WebViewBounds>,
    on_interaction: Option<InteractionHandler>,
) -> Result<(HWND, RECT, Option<*mut InteractionHandler>)> {
    ensure_host_class_registered()?;

    let (bounds, style, parent_arg) = match mode {
        WindowMode::Hidden => (
            WebViewBounds::new(0, 0, 1, 1),
            WS_OVERLAPPED,
            Some(HWND_MESSAGE),
        ),
        WindowMode::Embedded | WindowMode::External => {
            let parent_hwnd = parent.ok_or_else(|| {
                WebViewError::InvalidWindowHandle("missing parent HWND".to_owned())
            })?;
            let bounds = initial_bounds.unwrap_or_else(|| {
                let mut rc = RECT::default();
                // SAFETY: `parent_hwnd` was provided by the caller via `raw-window-handle`.
                let _ = unsafe { GetClientRect(parent_hwnd, &mut rc) };
                WebViewBounds::new(
                    0,
                    0,
                    (rc.right - rc.left).max(1),
                    (rc.bottom - rc.top).max(1),
                )
            });
            (
                bounds,
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                Some(parent_hwnd),
            )
        }
    };

    // SAFETY: `CreateWindowExW` creates a child host HWND owned by `EdgeChromiumWebView`.
    let hwnd = unsafe {
        let hinstance = GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or_default();
        CreateWindowExW(
            Default::default(),
            HOST_CLASS_NAME,
            w!(""),
            style,
            bounds.x,
            bounds.y,
            bounds.width.max(1),
            bounds.height.max(1),
            parent_arg,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| WebViewError::HostWindow(format!("CreateWindowExW failed: {e}")))?;

    let interaction_ptr = on_interaction.map(|cb| {
        let raw = Box::into_raw(Box::new(cb));
        // SAFETY: Stores a pointer to the heap-allocated `InteractionHandler` in `GWLP_USERDATA`;
        // cleared and reclaimed in `destroy_host_window`.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        }
        raw
    });

    let rect = RECT {
        left: 0,
        top: 0,
        right: bounds.width.max(1),
        bottom: bounds.height.max(1),
    };
    Ok((hwnd, rect, interaction_ptr))
}

fn destroy_host_window(hwnd: HWND, interaction_ptr: Option<*mut InteractionHandler>) {
    // SAFETY: Detaches `GWLP_USERDATA` before destroying the window and frees the boxed callback.
    unsafe {
        if !hwnd.is_invalid() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
        }
        if let Some(ptr) = interaction_ptr {
            drop(Box::from_raw(ptr));
        }
    }
}

/// Child host window procedure forwarding `WM_PARENTNOTIFY` mouse clicks to `on_interaction`
/// (exact port of `webview_windows_edge_chromium.cpp:1258-1275`).
unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_PARENTNOTIFY {
        let event = (wparam.0 & 0xFFFF) as u32;
        if event == WM_LBUTTONDOWN || event == WM_RBUTTONDOWN || event == WM_MBUTTONDOWN {
            // SAFETY: Pointer was stored by `create_host_window` and cleared before `DestroyWindow`.
            let ptr =
                unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const InteractionHandler;
            if !ptr.is_null() {
                // SAFETY: `ptr` points to a live `InteractionHandler` while `hwnd` is alive.
                let handler = unsafe { &*ptr };
                handler();
            }
        }
    }
    // SAFETY: Standard Win32 default message processing.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn create_environment(
    user_data_path: Option<&std::path::Path>,
    custom_scheme: &str,
    restricted: bool,
) -> Result<ICoreWebView2Environment> {
    let options = CoreWebView2EnvironmentOptions::default();
    let args = if restricted {
        RESTRICTED_BROWSER_ARGS
    } else {
        DEFAULT_BROWSER_ARGS
    };

    // SAFETY: We own `options` exclusively before handing its COM interface to WebView2.
    unsafe {
        options.set_additional_browser_arguments(args.to_owned());
        if !restricted && !custom_scheme.is_empty() {
            let registration = CoreWebView2CustomSchemeRegistration::new(custom_scheme.to_owned());
            registration.set_treat_as_secure(true);
            registration.set_has_authority_component(true);
            registration.set_allowed_origins(vec!["*".to_owned()]);
            let iface: ICoreWebView2CustomSchemeRegistration = registration.into();
            options.set_scheme_registrations(vec![Some(iface)]);
        }
    }

    let options_iface: ICoreWebView2EnvironmentOptions = options.into();
    let user_data_str = user_data_path.map(|p| p.to_string_lossy().into_owned());

    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            let user_data_pwstr = user_data_str.as_deref().map(CoTaskMemPWSTR::from);
            let user_data_pcwstr = user_data_pwstr
                .as_ref()
                .map(|p| *p.as_ref().as_pcwstr())
                .unwrap_or_else(PCWSTR::null);
            // SAFETY: `user_data_pwstr`, `options_iface`, and `handler` stay alive for the call.
            unsafe {
                CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    user_data_pcwstr,
                    &options_iface,
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)
            }
        }),
        Box::new(move |error_code, environment| {
            let res = error_code
                .and_then(|()| environment.ok_or_else(|| windows_core::Error::from(E_POINTER)));
            let _ = tx.send(res);
            Ok(())
        }),
    )
    .map_err(|e| {
        WebViewError::Backend(format!(
            "CreateCoreWebView2EnvironmentWithOptions failed: {e}"
        ))
    })?;

    rx.recv()
        .map_err(|_| WebViewError::Backend("WebView2 environment callback dropped".to_owned()))?
        .map_err(|e| WebViewError::Backend(format!("WebView2 environment creation error: {e}")))
}

fn create_controller(
    environment: &ICoreWebView2Environment,
    host_hwnd: HWND,
    restricted: bool,
) -> Result<ICoreWebView2Controller> {
    let (tx, rx) = mpsc::channel();
    let env_clone = environment.clone();
    CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            // SAFETY: Valid COM environment and owned HWND.
            unsafe {
                if restricted {
                    let env10: ICoreWebView2Environment10 = env_clone
                        .cast()
                        .map_err(webview2_com::Error::WindowsError)?;
                    let ctrl_opts = env10
                        .CreateCoreWebView2ControllerOptions()
                        .map_err(webview2_com::Error::WindowsError)?;
                    ctrl_opts
                        .SetIsInPrivateModeEnabled(true)
                        .map_err(webview2_com::Error::WindowsError)?;
                    env10
                        .CreateCoreWebView2ControllerWithOptions(host_hwnd, &ctrl_opts, &handler)
                        .map_err(webview2_com::Error::WindowsError)
                } else {
                    env_clone
                        .CreateCoreWebView2Controller(host_hwnd, &handler)
                        .map_err(webview2_com::Error::WindowsError)
                }
            }
        }),
        Box::new(move |error_code, controller| {
            let res = error_code
                .and_then(|()| controller.ok_or_else(|| windows_core::Error::from(E_POINTER)));
            let _ = tx.send(res);
            Ok(())
        }),
    )
    .map_err(|e| WebViewError::Backend(format!("CreateCoreWebView2Controller failed: {e}")))?;

    rx.recv()
        .map_err(|_| WebViewError::Backend("WebView2 controller callback dropped".to_owned()))?
        .map_err(|e| WebViewError::Backend(format!("WebView2 controller creation error: {e}")))
}

fn configure_settings(
    webview: &ICoreWebView2,
    controller: &ICoreWebView2Controller,
    debug: bool,
    restricted: bool,
    opaque_bg: Color,
    initial_rect: RECT,
    hidden: bool,
) -> Result<()> {
    // SAFETY: `webview` and `controller` are valid COM interfaces on the STA thread.
    unsafe {
        let settings = webview
            .Settings()
            .map_err(|e| WebViewError::Backend(format!("Settings() failed: {e}")))?;
        let allow_debug = debug && !restricted;
        let _ = settings.SetAreDefaultContextMenusEnabled(allow_debug);
        let _ = settings.SetAreDevToolsEnabled(allow_debug);
        let _ = settings.SetAreDefaultScriptDialogsEnabled(false);
        let _ = settings.SetIsStatusBarEnabled(false);

        if restricted {
            if let Ok(settings8) = settings.cast::<ICoreWebView2Settings8>() {
                let _ = settings8.SetIsReputationCheckingRequired(false);
            }
            if let Ok(webview8) = webview.cast::<ICoreWebView2_8>() {
                let _ = webview8.SetIsMuted(true);
            }
        }

        apply_default_background(controller, opaque_bg);
        controller
            .SetBounds(initial_rect)
            .map_err(|e| WebViewError::Backend(format!("SetBounds failed: {e}")))?;
        controller
            .SetIsVisible(!hidden)
            .map_err(|e| WebViewError::Backend(format!("SetIsVisible failed: {e}")))?;
    }
    Ok(())
}

fn apply_default_background(controller: &ICoreWebView2Controller, color: Color) {
    if let Ok(ctrl2) = controller.cast::<ICoreWebView2Controller2>() {
        // SAFETY: `SetDefaultBackgroundColor` accepts an RGBA byte struct (alpha 0 or 255 in WebView2).
        let alpha = if color.a == 0 { 0 } else { 255 };
        unsafe {
            let _ = ctrl2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                A: alpha,
                R: color.r,
                G: color.g,
                B: color.b,
            });
        }
    }
}

fn register_event_handlers(
    environment: &ICoreWebView2Environment,
    controller: &ICoreWebView2Controller,
    webview: &ICoreWebView2,
    router: &Arc<SchemeRouter>,
    config: &WebViewConfig,
    shared: &Arc<SharedState>,
) -> Result<()> {
    let mut token = 0_i64;
    let restricted_origin = config.restricted_origin.clone();
    let custom_scheme = if restricted_origin.is_some() {
        None
    } else {
        Some(router.scheme().to_owned())
    };

    // 1. WebMessageReceived (main-frame origin check + optional echo).
    {
        let on_message = config.callbacks.on_message.clone();
        let restricted = restricted_origin.is_some();
        let shared = Arc::clone(shared);
        let handler = WebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
            if shared.destroying.load(Ordering::SeqCst) {
                return Ok(());
            }
            let (Some(sender), Some(args)) = (sender, args) else {
                return Ok(());
            };
            // SAFETY: `args` and `sender` are valid COM event interfaces on the STA thread.
            let (source_url, main_url) = unsafe {
                let mut src = PWSTR::null();
                let mut main = PWSTR::null();
                let _ = args.Source(&mut src);
                let _ = sender.Source(&mut main);
                (
                    if src.is_null() {
                        String::new()
                    } else {
                        take_pwstr(src)
                    },
                    if main.is_null() {
                        String::new()
                    } else {
                        take_pwstr(main)
                    },
                )
            };
            // Security check matching `webview_windows_edge_chromium.cpp:567-571`:
            // ignore messages from child iframes / mismatched URLs.
            if source_url.is_empty() || source_url != main_url {
                return Ok(());
            }
            let mut msg_ptr = PWSTR::null();
            // SAFETY: Valid COM event args on the STA thread; `take_pwstr` frees the returned string.
            let hr = unsafe { args.TryGetWebMessageAsString(&mut msg_ptr) };
            if hr.is_ok() && !msg_ptr.is_null() {
                let text = take_pwstr(msg_ptr);
                if let Some(cb) = &on_message {
                    cb(WebMessage {
                        text: text.clone(),
                        source_url,
                    });
                }
                if !restricted && !shared.destroying.load(Ordering::SeqCst) {
                    let echo = CoTaskMemPWSTR::from(text.as_str());
                    // SAFETY: `echo` remains live across the COM call.
                    unsafe {
                        let _ = sender.PostWebMessageAsString(*echo.as_ref().as_pcwstr());
                    }
                }
            }
            Ok(())
        }));
        // SAFETY: Valid COM interface and handler.
        unsafe { webview.add_WebMessageReceived(&handler, &mut token) }
            .map_err(|e| WebViewError::Backend(format!("add_WebMessageReceived failed: {e}")))?;
    }

    // 2. WebResourceRequested (custom scheme routing + restricted-origin blocking).
    {
        let patterns: Vec<String> = if restricted_origin.is_some() {
            vec!["*".to_owned()]
        } else {
            router.filter_patterns()
        };
        for pattern in &patterns {
            let wide = CoTaskMemPWSTR::from(pattern.as_str());
            // SAFETY: Valid COM interface and NUL-terminated UTF-16 pattern.
            unsafe {
                webview
                    .AddWebResourceRequestedFilter(
                        *wide.as_ref().as_pcwstr(),
                        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                    )
                    .map_err(|e| {
                        WebViewError::Backend(format!("AddWebResourceRequestedFilter failed: {e}"))
                    })?;
            }
        }

        let env_clone = environment.clone();
        let router_clone = Arc::clone(router);
        let fallback_data = config.callbacks.on_data_request.clone();
        let restricted = restricted_origin.clone();
        let shared = Arc::clone(shared);

        let handler = WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
            if shared.destroying.load(Ordering::SeqCst) {
                return Ok(());
            }
            let Some(args) = args else {
                return Ok(());
            };
            handle_web_resource_requested(
                &env_clone,
                &args,
                &router_clone,
                fallback_data.as_ref(),
                restricted.as_deref(),
            );
            Ok(())
        }));
        // SAFETY: Valid COM interface and handler.
        unsafe { webview.add_WebResourceRequested(&handler, &mut token) }
            .map_err(|e| WebViewError::Backend(format!("add_WebResourceRequested failed: {e}")))?;
    }

    // 3. NavigationStarting (scheme filtering + user policy + history refresh).
    {
        let on_policy = config.callbacks.on_navigation_policy.clone();
        let on_start = config.callbacks.on_navigation_start.clone();
        let on_history = config.callbacks.on_history_changed.clone();
        let scheme = custom_scheme.clone();
        let restricted = restricted_origin.clone();
        let shared = Arc::clone(shared);
        let handler = NavigationStartingEventHandler::create(Box::new(move |sender, args| {
            if shared.destroying.load(Ordering::SeqCst) {
                return Ok(());
            }
            let Some(args) = args else {
                return Ok(());
            };
            let uri = unsafe {
                let mut ptr = PWSTR::null();
                let _ = args.Uri(&mut ptr);
                if ptr.is_null() {
                    String::new()
                } else {
                    take_pwstr(ptr)
                }
            };

            let mut allowed = if let Some(origin) = restricted.as_deref() {
                is_allowed_restricted_url(&uri, origin)
            } else {
                uri == "about:blank"
                    || uri.starts_with("data:")
                    || is_allowed_navigation_scheme(&uri, scheme.as_deref())
            };
            if allowed && let Some(cb) = &on_policy {
                allowed = cb(&uri, false);
            }
            if !allowed {
                unsafe {
                    let _ = args.SetCancel(true);
                }
                return Ok(());
            }
            if let Some(cb) = &on_start {
                cb();
            }
            if let Some(wv) = sender {
                refresh_history_state(&wv, &shared, on_history.as_ref());
            }
            Ok(())
        }));
        // SAFETY: Valid COM interface.
        unsafe { webview.add_NavigationStarting(&handler, &mut token) }
            .map_err(|e| WebViewError::Backend(format!("add_NavigationStarting failed: {e}")))?;
    }

    // 4. NavigationCompleted.
    {
        let on_done = config.callbacks.on_navigation_done.clone();
        let on_history = config.callbacks.on_history_changed.clone();
        let shared = Arc::clone(shared);
        let handler = NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
            if shared.destroying.load(Ordering::SeqCst) {
                return Ok(());
            }
            let is_success = args
                .map(|a| {
                    let mut ok = windows_core::BOOL(0);
                    unsafe { a.IsSuccess(&mut ok).is_ok() && ok.as_bool() }
                })
                .unwrap_or(false);
            if let Some(cb) = &on_done {
                cb(is_success);
            }
            if let Some(wv) = sender {
                refresh_history_state(&wv, &shared, on_history.as_ref());
            }
            Ok(())
        }));
        // SAFETY: Valid COM interface.
        unsafe { webview.add_NavigationCompleted(&handler, &mut token) }
            .map_err(|e| WebViewError::Backend(format!("add_NavigationCompleted failed: {e}")))?;
    }

    // 5. ContentLoading, DocumentTitleChanged, SourceChanged -> history state updates.
    {
        let on_history = config.callbacks.on_history_changed.clone();
        let shared_c = Arc::clone(shared);
        let handler = ContentLoadingEventHandler::create(Box::new(move |sender, _args| {
            if let Some(wv) = sender {
                refresh_history_state(&wv, &shared_c, on_history.as_ref());
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.add_ContentLoading(&handler, &mut token);
        }
    }
    {
        let on_history = config.callbacks.on_history_changed.clone();
        let shared_t = Arc::clone(shared);
        let handler = DocumentTitleChangedEventHandler::create(Box::new(move |sender, _args| {
            if let Some(wv) = sender {
                refresh_history_state(&wv, &shared_t, on_history.as_ref());
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.add_DocumentTitleChanged(&handler, &mut token);
        }
    }
    {
        let on_history = config.callbacks.on_history_changed.clone();
        let shared_s = Arc::clone(shared);
        let handler = SourceChangedEventHandler::create(Box::new(move |sender, _args| {
            if let Some(wv) = sender {
                refresh_history_state(&wv, &shared_s, on_history.as_ref());
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.add_SourceChanged(&handler, &mut token);
        }
    }

    // 6. NewWindowRequested (always handled; user-initiated popups pass policy -> on_external_link).
    {
        let on_policy = config.callbacks.on_navigation_policy.clone();
        let on_external = config.callbacks.on_external_link.clone();
        let restricted = restricted_origin.is_some();
        let scheme = custom_scheme.clone();
        let handler = NewWindowRequestedEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else {
                return Ok(());
            };
            unsafe {
                let _ = args.SetHandled(true);
            }
            if restricted {
                return Ok(());
            }
            let (uri, user_initiated) = unsafe {
                let mut ptr = PWSTR::null();
                let mut ui = windows_core::BOOL(0);
                let _ = args.Uri(&mut ptr);
                let _ = args.IsUserInitiated(&mut ui);
                (
                    if ptr.is_null() {
                        String::new()
                    } else {
                        take_pwstr(ptr)
                    },
                    ui.as_bool(),
                )
            };
            if user_initiated && is_allowed_navigation_scheme(&uri, scheme.as_deref()) {
                let allowed = on_policy.as_ref().is_none_or(|cb| cb(&uri, true));
                if allowed && let Some(open) = &on_external {
                    open(&uri);
                }
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.add_NewWindowRequested(&handler, &mut token);
        }
    }

    // 7. ScriptDialogOpening (`alert`/`confirm`/`prompt`).
    {
        let on_dialog = config.callbacks.on_dialog.clone();
        let shared = Arc::clone(shared);
        let handler = ScriptDialogOpeningEventHandler::create(Box::new(move |_sender, args| {
            if shared.destroying.load(Ordering::SeqCst) {
                return Ok(());
            }
            let (Some(args), Some(cb)) = (args, on_dialog.as_ref()) else {
                return Ok(());
            };
            let mut kind = COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT;
            let (uri, message, default_text) = unsafe {
                if args.Kind(&mut kind).is_err() {
                    return Ok(());
                }
                let mut u = PWSTR::null();
                let mut m = PWSTR::null();
                let mut d = PWSTR::null();
                let _ = args.Uri(&mut u);
                let _ = args.Message(&mut m);
                let _ = args.DefaultText(&mut d);
                (
                    if u.is_null() {
                        String::new()
                    } else {
                        take_pwstr(u)
                    },
                    if m.is_null() {
                        String::new()
                    } else {
                        take_pwstr(m)
                    },
                    if d.is_null() {
                        String::new()
                    } else {
                        take_pwstr(d)
                    },
                )
            };
            let dialog_type = match kind {
                COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM => DialogType::Confirm,
                COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT => DialogType::Prompt,
                _ => DialogType::Alert,
            };
            let result = cb(DialogArgs {
                dialog_type,
                value: default_text,
                text: message,
                url: uri,
            });
            if !shared.destroying.load(Ordering::SeqCst) && result.accepted {
                unsafe {
                    let _ = args.Accept();
                    if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT {
                        let wide = CoTaskMemPWSTR::from(result.text.as_str());
                        let _ = args.SetResultText(*wide.as_ref().as_pcwstr());
                    }
                }
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.add_ScriptDialogOpening(&handler, &mut token);
        }
    }

    // 8. PermissionRequested (always deny CLIPBOARD_READ; session-scoped camera/mic prompt).
    {
        let on_permission = config.callbacks.on_permission.clone();
        let restricted = restricted_origin.is_some();
        let shared = Arc::clone(shared);
        let handler = PermissionRequestedEventHandler::create(Box::new(move |_sender, args| {
            if shared.destroying.load(Ordering::SeqCst) {
                return Ok(());
            }
            let Some(args) = args else {
                return Ok(());
            };
            let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
            if unsafe { args.PermissionKind(&mut kind) }.is_err() {
                return Ok(());
            }
            if restricted || kind == COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ {
                // Never allow pages to silently read the clipboard via navigator.clipboard.readText().
                unsafe {
                    let _ = args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY);
                }
                return Ok(());
            }
            if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                || kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA
            {
                let Ok(transient) = args.cast::<ICoreWebView2PermissionRequestedEventArgs3>()
                else {
                    return Ok(());
                };
                unsafe {
                    if transient.SetSavesInProfile(false).is_err()
                        || transient
                            .SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)
                            .is_err()
                    {
                        return Ok(());
                    }
                }
                let Some(cb) = on_permission.as_ref() else {
                    return Ok(());
                };
                let uri = unsafe {
                    let mut p = PWSTR::null();
                    let _ = transient.Uri(&mut p);
                    if p.is_null() {
                        String::new()
                    } else {
                        take_pwstr(p)
                    }
                };
                let origin = origin_from_url(&uri);
                if origin.is_empty() {
                    return Ok(());
                }
                let key = (origin, kind.0);
                if shared
                    .allowed_permissions
                    .lock()
                    .is_ok_and(|set| set.contains(&key))
                {
                    unsafe {
                        let _ = transient.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW);
                    }
                    return Ok(());
                }
                let Ok(deferral) = (unsafe { transient.GetDeferral() }) else {
                    return Ok(());
                };
                let perm_type = if kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA {
                    PermissionType::Camera
                } else {
                    PermissionType::Microphone
                };
                let shared_done = Arc::clone(&shared);
                // NOTE: `ICoreWebView2PermissionRequestedEventArgs3` and `ICoreWebView2Deferral`
                // are STA COM pointers; wrap raw pointers via usize only for same-thread completion.
                let transient_raw = transient.into_raw() as usize;
                let deferral_raw = deferral.into_raw() as usize;
                cb(
                    perm_type,
                    Box::new(move |allowed| {
                        // SAFETY: Reclaims the COM references created above on completion.
                        unsafe {
                            let transient = ICoreWebView2PermissionRequestedEventArgs3::from_raw(
                                transient_raw as *mut c_void,
                            );
                            let deferral =
                                webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Deferral::from_raw(
                                    deferral_raw as *mut c_void,
                                );
                            if !shared_done.destroying.load(Ordering::SeqCst) {
                                if allowed
                                    && let Ok(mut set) = shared_done.allowed_permissions.lock()
                                {
                                    set.insert(key);
                                }
                                let _ = transient.SetState(if allowed {
                                    COREWEBVIEW2_PERMISSION_STATE_ALLOW
                                } else {
                                    COREWEBVIEW2_PERMISSION_STATE_DENY
                                });
                                let _ = deferral.Complete();
                            }
                        }
                    }),
                );
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.add_PermissionRequested(&handler, &mut token);
        }
    }

    // 9. Block browser-initiated downloads (matching `webview_windows_edge_chromium.cpp:903-909`).
    if let Ok(wv4) = webview.cast::<ICoreWebView2_4>() {
        let handler = DownloadStartingEventHandler::create(Box::new(|_sender, args| {
            if let Some(args) = args {
                unsafe {
                    let _ = args.SetCancel(true);
                    let _ = args.SetHandled(true);
                }
            }
            Ok(())
        }));
        unsafe {
            let _ = wv4.add_DownloadStarting(&handler, &mut token);
        }
    }

    // 10. Restricted-origin auth/cert cancellation & ProcessFailed reporting.
    if restricted_origin.is_some() {
        if let Ok(wv10) = webview.cast::<ICoreWebView2_10>() {
            let handler =
                BasicAuthenticationRequestedEventHandler::create(Box::new(|_sender, args| {
                    if let Some(args) = args {
                        unsafe {
                            let _ = args.SetCancel(true);
                        }
                    }
                    Ok(())
                }));
            unsafe {
                let _ = wv10.add_BasicAuthenticationRequested(&handler, &mut token);
            }
        }
        if let Ok(wv5) = webview.cast::<ICoreWebView2_5>() {
            let handler =
                ClientCertificateRequestedEventHandler::create(Box::new(|_sender, args| {
                    if let Some(args) = args {
                        unsafe {
                            let _ = args.SetCancel(true);
                            let _ = args.SetHandled(true);
                        }
                    }
                    Ok(())
                }));
            unsafe {
                let _ = wv5.add_ClientCertificateRequested(&handler, &mut token);
            }
        }
    }
    {
        let on_failed = config.callbacks.on_process_failed.clone();
        let on_done = config.callbacks.on_navigation_done.clone();
        let restricted = restricted_origin.is_some();
        let handler = ProcessFailedEventHandler::create(Box::new(move |_sender, _args| {
            if let Some(cb) = &on_failed {
                cb();
            }
            if restricted && let Some(cb) = &on_done {
                cb(false);
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.add_ProcessFailed(&handler, &mut token);
        }
    }

    // 11. ZoomFactorChanged.
    {
        let on_zoom = config.callbacks.on_zoom_changed.clone();
        let handler = ZoomFactorChangedEventHandler::create(Box::new(move |sender, _args| {
            if let (Some(ctrl), Some(cb)) = (sender, on_zoom.as_ref()) {
                let mut factor = 1.0_f64;
                if unsafe { ctrl.ZoomFactor(&mut factor) }.is_ok() {
                    cb((factor * 100.0).round().clamp(0.0, 1000.0) as u32);
                }
            }
            Ok(())
        }));
        unsafe {
            let _ = controller.add_ZoomFactorChanged(&handler, &mut token);
        }
    }

    // 12. ContainsFullScreenElementChanged.
    {
        let on_fs = config.callbacks.on_fullscreen_changed.clone();
        let handler =
            ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _args| {
                if let (Some(wv), Some(cb)) = (sender, on_fs.as_ref()) {
                    let mut fs = windows_core::BOOL(0);
                    if unsafe { wv.ContainsFullScreenElement(&mut fs) }.is_ok() {
                        cb(fs.as_bool());
                    }
                }
                Ok(())
            }));
        unsafe {
            let _ = webview.add_ContainsFullScreenElementChanged(&handler, &mut token);
        }
    }

    // 13. AcceleratorKeyPressed -> interaction notification.
    if let Some(on_interaction) = config.callbacks.on_interaction.clone() {
        let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_sender, args| {
            if let Some(args) = args {
                let mut kind = Default::default();
                if unsafe { args.KeyEventKind(&mut kind) }.is_ok()
                    && (kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN)
                {
                    on_interaction();
                }
            }
            Ok(())
        }));
        unsafe {
            let _ = controller.add_AcceleratorKeyPressed(&handler, &mut token);
        }
    }

    Ok(())
}

fn handle_web_resource_requested(
    environment: &ICoreWebView2Environment,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    router: &SchemeRouter,
    fallback: Option<&crate::data_stream::DataRequestHandler>,
    restricted_origin: Option<&str>,
) {
    // SAFETY: All COM interfaces (`environment`, `args`, `request`, `headers`, `iter`) are valid
    // on this STA thread for the duration of the `WebResourceRequested` callback.
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let uri = unsafe {
        let mut ptr = PWSTR::null();
        if request.Uri(&mut ptr).is_err() || ptr.is_null() {
            return;
        }
        take_pwstr(ptr)
    };

    if let Some(origin) = restricted_origin {
        if !is_allowed_restricted_url(&uri, origin) {
            apply_http_response(environment, args, HttpResponse::forbidden());
            return;
        }
        // SAFETY: Valid COM request object on STA thread.
        if let Ok(headers) = unsafe { request.Headers() } {
            let cookie = CoTaskMemPWSTR::from("Cookie");
            // SAFETY: `cookie` remains live across the COM call.
            unsafe {
                let _ = headers.RemoveHeader(*cookie.as_ref().as_pcwstr());
            }
        }
    }

    let Some(resource_id) = router.extract_resource_id(&uri) else {
        return;
    };

    let mut data_req = DataRequest::new(resource_id);
    // SAFETY: Iterates HTTP request headers on a live WebView2 request on the STA thread.
    unsafe {
        if let Ok(headers) = request.Headers()
            && let Ok(iter) = headers.GetIterator()
        {
            loop {
                let mut has_current = windows_core::BOOL(0);
                if iter.HasCurrentHeader(&mut has_current).is_err() || !has_current.as_bool() {
                    break;
                }
                let mut name_ptr = PWSTR::null();
                let mut val_ptr = PWSTR::null();
                if iter.GetCurrentHeader(&mut name_ptr, &mut val_ptr).is_ok()
                    && !name_ptr.is_null()
                    && !val_ptr.is_null()
                {
                    let name = take_pwstr(name_ptr);
                    let value = take_pwstr(val_ptr);
                    if name.eq_ignore_ascii_case("Range") {
                        parse_range_header_for(&mut data_req, &value);
                    }
                }
                let mut has_next = windows_core::BOOL(0);
                if iter.MoveNext(&mut has_next).is_err() || !has_next.as_bool() {
                    break;
                }
            }
        }
    }

    enum DeferralState {
        Waiting,
        ResolvedEarly(DataResponse),
        Armed {
            env_raw: usize,
            args_raw: usize,
            def_raw: usize,
        },
        Completed,
    }

    let deferral_state = Arc::new(Mutex::new(DeferralState::Waiting));
    let make_done = |state: Arc<Mutex<DeferralState>>| -> crate::data_stream::DataRequestDone {
        Box::new(move |response: DataResponse| {
            let Ok(mut guard) = state.lock() else {
                return;
            };
            match std::mem::replace(&mut *guard, DeferralState::Completed) {
                DeferralState::Waiting => {
                    *guard = DeferralState::ResolvedEarly(response);
                }
                DeferralState::Armed {
                    env_raw,
                    args_raw,
                    def_raw,
                } => {
                    drop(guard);
                    // SAFETY: Reclaims the STA COM interfaces stored when `GetDeferral` armed this slot.
                    unsafe {
                        let env = ICoreWebView2Environment::from_raw(env_raw as *mut c_void);
                        let args = ICoreWebView2WebResourceRequestedEventArgs::from_raw(
                            args_raw as *mut c_void,
                        );
                        let def =
                            webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Deferral::from_raw(
                                def_raw as *mut c_void,
                            );
                        apply_http_response(
                            &env,
                            &args,
                            HttpResponse::from_data_response(response),
                        );
                        let _ = def.Complete();
                    }
                }
                DeferralState::ResolvedEarly(_) | DeferralState::Completed => {}
            }
        })
    };

    let result =
        match router.dispatch_request(data_req.clone(), make_done(Arc::clone(&deferral_state))) {
            DataResult::Done(resp) => DataResult::Done(resp),
            DataResult::Pending => DataResult::Pending,
            DataResult::Failed => {
                if let Some(fb) = fallback {
                    fb(data_req, make_done(Arc::clone(&deferral_state)))
                } else {
                    DataResult::Failed
                }
            }
        };

    match result {
        DataResult::Done(resp) => {
            apply_http_response(environment, args, HttpResponse::from_data_response(resp));
        }
        DataResult::Pending => {
            let Ok(mut guard) = deferral_state.lock() else {
                apply_http_response(environment, args, HttpResponse::not_found());
                return;
            };
            match std::mem::replace(&mut *guard, DeferralState::Completed) {
                DeferralState::ResolvedEarly(resp) => {
                    drop(guard);
                    apply_http_response(environment, args, HttpResponse::from_data_response(resp));
                }
                DeferralState::Waiting => {
                    // SAFETY: Valid COM event args on STA thread.
                    if let Ok(deferral) = unsafe { args.GetDeferral() } {
                        *guard = DeferralState::Armed {
                            env_raw: environment.clone().into_raw() as usize,
                            args_raw: args.clone().into_raw() as usize,
                            def_raw: deferral.into_raw() as usize,
                        };
                    } else {
                        drop(guard);
                        apply_http_response(environment, args, HttpResponse::not_found());
                    }
                }
                DeferralState::Armed { .. } | DeferralState::Completed => {}
            }
        }
        DataResult::Failed => {
            apply_http_response(environment, args, HttpResponse::not_found());
        }
    }
}

fn apply_http_response(
    environment: &ICoreWebView2Environment,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    http: HttpResponse,
) {
    let istream = http.stream.map(DataStreamCom::into_istream);
    let reason = CoTaskMemPWSTR::from(http.reason_phrase);
    let headers = CoTaskMemPWSTR::from(http.headers.as_str());
    // SAFETY: `reason` and `headers` stay alive across `CreateWebResourceResponse`.
    let response: windows_core::Result<ICoreWebView2WebResourceResponse> = unsafe {
        environment.CreateWebResourceResponse(
            istream.as_ref(),
            i32::from(http.status_code),
            *reason.as_ref().as_pcwstr(),
            *headers.as_ref().as_pcwstr(),
        )
    };
    if let Ok(resp) = response {
        // SAFETY: Valid COM response object.
        unsafe {
            let _ = args.SetResponse(&resp);
        }
    }
}

fn refresh_history_state(
    webview: &ICoreWebView2,
    shared: &SharedState,
    on_history: Option<&crate::interface::HistoryChangedHandler>,
) {
    // SAFETY: `webview` is a valid COM interface invoked on the STA thread; `take_pwstr` frees
    // the CoTaskMem UTF-16 strings returned by `Source` and `DocumentTitle`.
    let state = unsafe {
        let mut can_back = windows_core::BOOL(0);
        let mut can_forward = windows_core::BOOL(0);
        let mut url_ptr = PWSTR::null();
        let mut title_ptr = PWSTR::null();
        let _ = webview.CanGoBack(&mut can_back);
        let _ = webview.CanGoForward(&mut can_forward);
        let _ = webview.Source(&mut url_ptr);
        let _ = webview.DocumentTitle(&mut title_ptr);
        NavigationHistoryState {
            url: if url_ptr.is_null() {
                String::new()
            } else {
                take_pwstr(url_ptr)
            },
            title: if title_ptr.is_null() {
                String::new()
            } else {
                take_pwstr(title_ptr)
            },
            can_go_back: can_back.as_bool(),
            can_go_forward: can_forward.as_bool(),
        }
    };
    if let Ok(mut guard) = shared.history.lock() {
        *guard = state.clone();
    }
    if let Some(cb) = on_history {
        cb(state);
    }
}
