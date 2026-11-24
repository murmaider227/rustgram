//! Interactive demo opening a `winit` window with an embedded `tg-webview` child WebView2
//! serving a local HTML page over `rustgram-internal://` and exercising the Mini App bridge.
//!
//! Run with: `cargo run -p tg-webview --example demo`

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::json;
use tg_theme::{EmbeddedType, Palette, prepare_embedded_palette, web_view_params};
use tg_webview::{
    HostEvent, MiniAppAction, MiniAppController, MiniAppPanelConfig, SchemeRouter,
    ThemeChangedPayload, WebView, WebViewBounds, WebViewCallbacks, WebViewConfig, availability,
    resolve_user_data_dir,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

const DEMO_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Rustgram WebView Mini App Bridge Demo</title>
<link rel="stylesheet" href="rustgram-internal://app/demo.css">
</head>
<body>
  <header>
    <h1>Rustgram WebView2 Mini App Bridge Demo</h1>
    <p class="subtitle">Served via <code>rustgram-internal://app/index.html</code></p>
  </header>

  <section class="controls">
    <button onclick="send('web_app_ready')">web_app_ready</button>
    <button onclick="send('web_app_request_theme')">web_app_request_theme</button>
    <button onclick="send('web_app_request_viewport')">web_app_request_viewport</button>
    <button onclick="send('web_app_biometry_get_info')">web_app_biometry_get_info</button>
    <button onclick="send('web_app_check_home_screen')">web_app_check_home_screen</button>
    <button onclick="send('web_app_setup_main_button', {is_visible:true,is_active:true,text:'Pay 100 Stars',color:'#40a7e3',text_color:'#ffffff'})">setup_main_button</button>
    <button onclick="send('web_app_setup_back_button', {is_visible:true})">show_back_button</button>
    <button onclick="send('web_app_open_popup', {title:'Confirm',message:'Trigger host popup reply?',buttons:[{id:'yes',type:'ok'},{id:'no',type:'cancel'}]})">open_popup</button>
    <button onclick="send('web_app_invoke_custom_method', {req_id:'req-1',method:'demoPing',params:{echo:'hello'}})">custom_method</button>
    <button onclick="send('web_app_data_send', {data:'demo-payload-42'})">web_app_data_send</button>
    <button onclick="send('web_app_invoke_custom_method', {req_id:'theme-toggle',method:'toggleTheme',params:{}})">Toggle Day / Night</button>
    <button onclick="fetchAsset()">Test Range Fetch</button>
  </section>

  <section class="panel">
    <h2>Bridge Event Log</h2>
    <pre id="log"></pre>
  </section>

<script>
const logEl = document.getElementById('log');
function appendLog(dir, type, data) {
  const ts = new Date().toLocaleTimeString();
  const payload = data === undefined ? '' : ' ' + JSON.stringify(data);
  logEl.textContent = `[${ts}] ${dir} ${type}${payload}\n` + logEl.textContent;
}

function applyTheme(params) {
  if (!params) return;
  const root = document.documentElement.style;
  if (params.bg_color) root.setProperty('--bg', params.bg_color);
  if (params.secondary_bg_color) root.setProperty('--card', params.secondary_bg_color);
  if (params.text_color) root.setProperty('--fg', params.text_color);
  if (params.hint_color) root.setProperty('--hint', params.hint_color);
  if (params.button_color) root.setProperty('--btn', params.button_color);
  if (params.button_text_color) root.setProperty('--btn-fg', params.button_text_color);
}

function send(eventType, eventData) {
  appendLog('JS -> HOST', eventType, eventData);
  window.Telegram.WebView.postEvent(eventType, eventData);
}

async function fetchAsset() {
  try {
    const res = await fetch('rustgram-internal://app/sample.txt', {
      headers: { 'Range': 'bytes=0-25' }
    });
    const text = await res.text();
    appendLog('FETCH', `HTTP ${res.status}`, { text, contentRange: res.headers.get('Content-Range') });
  } catch (e) {
    appendLog('FETCH ERR', String(e));
  }
}

const events = [
  'theme_changed', 'viewport_changed', 'main_button_pressed', 'back_button_pressed',
  'popup_closed', 'biometry_info_received', 'home_screen_checked', 'custom_method_invoked',
  'fullscreen_changed', 'safe_area_changed', 'content_safe_area_changed'
];
for (const ev of events) {
  window.Telegram.WebView.onEvent(ev, (type, data) => {
    appendLog('HOST -> JS', type, data);
    if (type === 'theme_changed' && data && data.theme_params) {
      applyTheme(data.theme_params);
    }
  });
}

window.addEventListener('DOMContentLoaded', () => {
  send('web_app_ready');
  send('web_app_request_theme');
  send('web_app_request_viewport');
});
</script>
</body>
</html>
"#;

const DEMO_CSS: &str = r#"
:root {
  --bg: #ffffff;
  --card: #f1f1f1;
  --fg: #000000;
  --hint: #707579;
  --btn: #40a7e3;
  --btn-fg: #ffffff;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  padding: 20px;
  font-family: "Segoe UI", system-ui, sans-serif;
  background: var(--bg);
  color: var(--fg);
  transition: background 0.15s ease, color 0.15s ease;
}
h1 { margin: 0 0 4px 0; font-size: 20px; }
.subtitle { margin: 0 0 16px 0; color: var(--hint); font-size: 13px; }
.controls {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 16px;
}
button {
  background: var(--btn);
  color: var(--btn-fg);
  border: none;
  border-radius: 6px;
  padding: 8px 12px;
  font-size: 13px;
  cursor: pointer;
}
button:hover { filter: brightness(0.92); }
.panel {
  background: var(--card);
  border-radius: 8px;
  padding: 12px;
}
h2 { margin: 0 0 8px 0; font-size: 14px; color: var(--hint); }
pre {
  margin: 0;
  max-height: 360px;
  overflow-y: auto;
  font-family: Consolas, monospace;
  font-size: 12px;
  white-space: pre-wrap;
}
"#;

const SAMPLE_ASSET: &str = "Rustgram custom scheme range-request test payload (0123456789ABCDEF).";

enum DemoEvent {
    BridgeMessage(tg_webview::WebMessage),
    UserInteraction,
}

struct DemoApp {
    proxy: EventLoopProxy<DemoEvent>,
    window: Option<Window>,
    webview: Option<WebView>,
    controller: Arc<Mutex<MiniAppController>>,
    night_mode: bool,
    palette: Palette,
}

impl DemoApp {
    fn new(proxy: EventLoopProxy<DemoEvent>) -> Self {
        Self {
            proxy,
            window: None,
            webview: None,
            controller: Arc::new(Mutex::new(MiniAppController::new(MiniAppPanelConfig {
                initial_url: "rustgram-internal://app/index.html".to_owned(),
                same_origin: false,
                allow_clipboard_read: false,
                fullscreen: false,
            }))),
            night_mode: false,
            palette: Palette::default(),
        }
    }

    fn toggle_theme(&mut self) {
        self.night_mode = !self.night_mode;
        self.palette = if self.night_mode {
            prepare_embedded_palette(EmbeddedType::Night, None).unwrap_or_default()
        } else {
            Palette::default()
        };
        let params = web_view_params(&self.palette);
        if let Some(wv) = &self.webview {
            let _ = wv.update_theme(&params);
            let _ = wv.post_host_event(
                &HostEvent::ThemeChanged(ThemeChangedPayload::from_web_view_params(&params)),
                None,
            );
        }
    }

    fn handle_actions(&mut self, actions: Vec<MiniAppAction>) {
        for action in actions {
            match action {
                MiniAppAction::PostEvent(ev) => {
                    if let Some(wv) = &self.webview {
                        let _ = wv.post_host_event(&ev, None);
                    }
                }
                MiniAppAction::SendDynamicViewport => {
                    if let Some(wv) = &self.webview {
                        let _ = wv.post_dynamic_viewport(None);
                    }
                }
                MiniAppAction::ShowPopup(args) => {
                    tracing::info!(title = %args.title, text = %args.text, "Mini App requested popup");
                    let chosen = args.buttons.first().map(|b| b.id.clone());
                    if let Some(wv) = &self.webview {
                        let _ =
                            wv.post_host_event(&HostEvent::PopupClosed { button_id: chosen }, None);
                    }
                }
                MiniAppAction::InvokeCustomMethod(req) => {
                    if req.method == "toggleTheme" {
                        self.toggle_theme();
                        if let Some(wv) = &self.webview {
                            let _ = wv.post_host_event(
                                &HostEvent::CustomMethodInvoked {
                                    req_id: req.req_id,
                                    result: Some(json!({ "nightMode": self.night_mode })),
                                    error: None,
                                },
                                None,
                            );
                        }
                    } else if let Some(wv) = &self.webview {
                        let _ = wv.post_host_event(
                            &HostEvent::CustomMethodInvoked {
                                req_id: req.req_id,
                                result: Some(
                                    json!({ "ok": true, "method": req.method, "echo": req.params }),
                                ),
                                error: None,
                            },
                            None,
                        );
                    }
                }
                MiniAppAction::SendData(data) => {
                    tracing::info!(data = %data, "Mini App sent data");
                    if let Some(wv) = &self.webview {
                        let _ = wv.post_host_event(&HostEvent::MainButtonPressed, None);
                    }
                }
                other => {
                    tracing::info!(?other, "Mini App action");
                }
            }
        }
    }
}

impl ApplicationHandler<DemoEvent> for DemoApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Rustgram WebView2 Mini App Demo")
            .with_inner_size(LogicalSize::new(900.0, 680.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("Failed to create winit window: {e}");
                event_loop.exit();
                return;
            }
        };

        let mut router = SchemeRouter::default();
        router.add_html("index.html", DEMO_HTML);
        router.add_static("demo.css", "text/css; charset=utf-8", DEMO_CSS.as_bytes());
        router.add_static(
            "sample.txt",
            "text/plain; charset=utf-8",
            SAMPLE_ASSET.as_bytes(),
        );

        let proxy_msg = self.proxy.clone();
        let proxy_int = self.proxy.clone();
        let user_data_path =
            resolve_user_data_dir(&std::env::temp_dir().join("rustgram-demo"), "demo");
        let size = window.inner_size();

        let config = WebViewConfig {
            opaque_bg: self.palette.window_bg,
            user_data_path: Some(user_data_path),
            debug: true,
            initial_bounds: Some(WebViewBounds::new(
                0,
                0,
                size.width as i32,
                size.height as i32,
            )),
            scheme_router: Some(Arc::new(router)),
            callbacks: WebViewCallbacks {
                on_message: Some(Arc::new(move |msg| {
                    let _ = proxy_msg.send_event(DemoEvent::BridgeMessage(msg));
                })),
                on_interaction: Some(Arc::new(move || {
                    let _ = proxy_int.send_event(DemoEvent::UserInteraction);
                })),
                ..WebViewCallbacks::default()
            },
            ..WebViewConfig::default()
        };

        match WebView::create(&window, config) {
            Ok(wv) => {
                let _ = wv.install_mini_app_bridge();
                let _ = wv.update_theme(&web_view_params(&self.palette));
                let _ = wv.navigate_to_data("index.html");
                self.webview = Some(wv);
            }
            Err(e) => {
                eprintln!("Failed to create WebView2: {e}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: DemoEvent) {
        match event {
            DemoEvent::UserInteraction => {
                if let Ok(mut ctrl) = self.controller.lock() {
                    ctrl.record_user_interaction(Instant::now());
                }
            }
            DemoEvent::BridgeMessage(msg) => {
                let actions = match self.controller.lock() {
                    Ok(mut ctrl) => {
                        match ctrl.handle_web_message(&msg, &self.palette, Instant::now()) {
                            Ok(actions) => actions,
                            Err(err) => {
                                tracing::warn!(%err, "Ignored invalid bridge message");
                                Vec::new()
                            }
                        }
                    }
                    Err(_) => Vec::new(),
                };
                self.handle_actions(actions);
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(wv) = self.webview.take() {
                    wv.close();
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(wv) = &self.webview {
                    let _ = wv.set_bounds(WebViewBounds::new(
                        0,
                        0,
                        size.width as i32,
                        size.height as i32,
                    ));
                    let _ = wv.post_dynamic_viewport(None);
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let _ = tracing_subscriber::fmt::try_init();
    let avail = availability();
    println!(
        "WebView2 availability: supported={}, version={:?}, details={:?}",
        avail.is_supported(),
        avail.browser_version,
        avail.details
    );
    if !avail.is_supported() {
        if let Some(msg) = avail.user_error_text() {
            eprintln!("{msg}");
        }
        return;
    }

    let event_loop = EventLoop::<DemoEvent>::with_user_event()
        .build()
        .expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = DemoApp::new(event_loop.create_proxy());
    let _ = event_loop.run_app(&mut app);
}
