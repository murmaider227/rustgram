# Hosting Instant View (`tg-iv`) in `tg-app` on Windows (WebView2)

This guide describes how `tg-app` should host Instant View pages rendered by `tg-iv` on Windows 10/11 x64 (`x86_64-pc-windows-msvc`), mirroring Telegram Desktop's WebView2 host in `Telegram/lib_webview/webview/platform/win/webview_windows_edge_chromium.cpp` and `Telegram/SourceFiles/iv/iv_controller.cpp`.

## 1. Crate Choice: `webview2-com` vs. `wry`

Both crates build cleanly with MSVC (`x86_64-pc-windows-msvc`) without CMake:

| Criterion | `webview2-com` (+ `windows`) | `wry` |
|---|---|---|
| Control over WebView2 APIs | Direct COM bindings (`ICoreWebView2`, `ICoreWebView2Controller`, `ICoreWebView2Environment`, `WebResourceRequested`, `ZoomFactorChanged`, `DownloadStarting`, `PermissionRequested`, `ProcessFailed`). | High-level cross-platform wrapper around WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux. |
| Integration with `eframe`/`egui` | Attaches directly to a dedicated Win32 child/top-level `HWND` (same pattern as tdesktop's `Ui::RpWindow` + `HWND`). | Attaches to any `raw-window-handle` (`HasWindowHandle`), including a child window or secondary `winit` window. |
| Custom-scheme + HTTP `Range` | Full control via `AddWebResourceRequestedFilter` + `CreateWebResourceResponse` + `IStream` (needed for seeking `<video>`/`<audio>`). | Built-in `with_custom_protocol("tg-iv", ...)` supporting `http::Request`/`http::Response` including `Range` headers and `206 Partial Content`. |
| Recommendation | **Best if `tg-app` already owns Win32 windows via `tg-platform` / `windows`** and wants exact tdesktop parity (zoom controller, process priority, transient permissions, crash recovery). | **Best for fastest bring-up** and future macOS/Linux portability with minimal COM boilerplate. |

Either crate plugs directly into `tg-iv` because `tg-iv` is completely window-agnostic:

1. `tg_iv::prepare_page(&page, &options)` produces `PreparedPage` (`html`, `anchors`, `photo_ids`, `document_ids`, `embeds`).
2. `tg_iv::IvServer` serves `/`, `/css/page.css`, `/js/page.js`, `/media/...`, and `/embed/{i}` via `serve(url, range_header, &media_provider)`.
3. `tg_iv::IvController` handles navigation policy, zoom state, history stack, and JSON bridge events via `handle_bridge_json(&raw)`.

## 2. Window Architecture

Match tdesktop's `Iv::Controller` (`iv_controller.cpp`):

- Open Instant View in a **separate top-level window** (not overlaid on the `egui` canvas, which avoids airspace/clipping glitches with native child `HWND`s on Windows).
- Reserve a thin top chrome bar (`egui` or native Win32 toolbar) for:
  - Back / Forward buttons wired to `IvController::can_go_back()` / `go_back()` / `go_forward()`
  - Page site name / URL subtitle (`PageData::name()` or `https_to_tonsite(url)` for TON sites)
  - Zoom controls (`-`, `100%`, `+`, Reset) wired to `IvController::zoom_in` / `zoom_out` / `reset_zoom`
  - Open in external browser button
- Place the WebView2 controller in the client rect below the chrome bar and resize its bounds (`ICoreWebView2Controller::put_Bounds`) on `WM_SIZE`.
- Forward keyboard shortcuts (`Esc`, `Ctrl+W`, `Ctrl+M`, `Ctrl+Q`, `Ctrl++`, `Ctrl+-`, `Ctrl+0`) into `IvController::handle_bridge_message(BridgeMessage::KeyShortcut { .. })`.

## 3. Custom-Scheme Resource Handler (`IvServer::serve`)

In `RenderOptions`, configure:

```rust
let options = tg_iv::RenderOptions {
    base_url: "http://tg-iv.localhost".to_string(), // or "tg-iv://localhost"
    inline_assets: false,
    include_bridge: true,
    embed_policy: tg_iv::EmbedPolicy::default(),
    dark_mode: Some(is_dark_theme),
    zoom_percent: controller.zoom().percent(),
    ..Default::default()
};
let prepared = tg_iv::prepare_page(&page, &options);
let server = tg_iv::IvServer::new(prepared);
```

### With `webview2-com`

1. Mirror tdesktop's `kDataUrlPrefix` (`http://desktop-app-resource/` or `http://tg-iv.localhost/`):
   - Call `webview.AddWebResourceRequestedFilter(w!("http://tg-iv.localhost/*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)`.
2. In the `WebResourceRequested` callback:
   - Read the request URI (`request.Uri()`) and optional `Range` header (`request.Headers().GetHeader(w!("Range"))`).
   - Call `let resp = server.serve(&uri, range.as_deref(), &media_store);`.
   - Wrap `resp.body` in an `IStream` (`SHCreateMemStream` or a custom `IStream` impl).
   - Format headers (`Content-Type: ...` plus each `(k, v)` in `resp.headers`).
   - Call `environment.CreateWebResourceResponse(&stream, resp.status as i32, reason, &headers)` and assign via `args.SetResponse(&response)`.
3. For media not yet downloaded from Telegram:
   - Either return WebView2's `GetDeferral()` while `tg-media` fetches the photo/document and complete the deferral once ready, **or** let `IvServer` immediately return its placeholder SVG (`200`) and push `HostCommand::UpdateMedia { kind, id, url }` once `tg-media` finishes downloading.

### With `wry`

```rust,ignore
let builder = WebViewBuilder::new()
    .with_custom_protocol("tg-iv".into(), move |_webview_id, request| {
        let range = request
            .headers()
            .get("range")
            .and_then(|v| v.to_str().ok());
        let resp = server.serve(request.uri().path(), range, &media_store);
        let mut http_resp = http::Response::builder()
            .status(resp.status)
            .header("Content-Type", resp.content_type);
        for (k, v) in resp.headers {
            http_resp = http_resp.header(k, v);
        }
        http_resp.body(std::borrow::Cow::Owned(resp.body)).unwrap()
    });
```

Note: On Windows, `wry` maps custom scheme `tg-iv://localhost/...` to `http://tg-iv.localhost/...` automatically. `IvServer::serve` accepts both full URLs and bare paths.

## 4. JS Bridge Wiring (`page.js` <-> `IvController`)

`assets/page.js` automatically detects WebView2's `window.chrome.webview.postMessage(json)` and sends JSON events for:

- `link_click` (`href`, `webpage_id`, `new_window`)
- `media_click` (`kind`, `id`)
- `channel_open` / `channel_join` (`channel_id`, `username`)
- `context_menu` (`x`, `y`, `selection`, `link_url`, `media_kind`, `media_id`)
- `scroll_position` (`y`, `max_y`, `progress`)
- `report` (`webpage_id`)
- `["resize_frame", { "height": ... }]` (from embedded widgets via `TelegramWebviewProxy.postEvent`)

### Inbound (WebView -> Rust)

- In `webview2-com`: register `add_WebMessageReceived`, read `args.TryGetWebMessageAsString()`, pass the string to `controller.handle_bridge_json(&raw)`.
- In `wry`: pass a closure to `.with_ipc_handler(move |req| { let actions = controller.handle_bridge_json(req.body()); ... })`.
- Dispatch each returned `ControllerAction`:
  - `ControllerAction::Eval(cmd)` -> call `webview.ExecuteScript(cmd.to_eval_js()?)`
  - `ControllerAction::OpenPage { url, webpage_id, fragment }` -> fetch via `tg-api` (`messages.getWebPage`) and call `controller.load_page(...)` + `server.set_page(...)` + reload
  - `ControllerAction::OpenTelegramLink { uri }` -> route through `tg-app`'s internal `tg://` link handler
  - `ControllerAction::OpenTonSite { https_gateway_url, .. }` -> open TON Site WebView navigated to `https_gateway_url`
  - `ControllerAction::OpenExternal { url }` -> `ShellExecuteW` / `tg-platform` open-url (only after `classify_link` verified the scheme)
  - `ControllerAction::OpenMedia { kind, id }` -> open `tg-app`'s native media viewer
  - `ControllerAction::OpenChannel` / `JoinChannel` -> open chat or invoke `channels.joinChannel` via `tg-api`, then send `HostCommand::SetChannelJoined`
  - `ControllerAction::ShowContextMenu` -> show `egui` / native Win32 popup menu (Copy, Copy Link, Open in Browser, Save Media)
  - `ControllerAction::Report { start_token, .. }` -> open `@previews` with `start_token`
  - `ControllerAction::ZoomChanged { percent }` -> persist to `tg-core` settings and sync `ICoreWebView2Controller::put_ZoomFactor(percent as f64 / 100.0)`

## 5. Security Hardening Checklist (Matching tdesktop)

Follow the exact protections implemented in `webview_windows_edge_chromium.cpp`:

1. **Isolated `userDataFolder`**: Store WebView2 profile data under `<workdir>/tdata/webview/iv` (never default to the executable directory). Use a separate profile directory for TON sites (`Storage::TonSiteStorageId()`).
2. **Navigation Interception (`NavigationStarting` & `NewWindowRequested`)**:
   - Call `controller.navigation_policy(&uri, is_new_window)`.
   - On `NavigationDecision::Cancel(target)`, set `args.SetCancel(true)` (and `args.SetHandled(true)` for `NewWindowRequested`) and handle `target` in Rust.
   - This prevents any link or script inside the page from navigating the top-level WebView away from the local IV origin.
3. **Strict CSP**: `IvServer` emits `DEFAULT_IV_CSP` (`object-src 'none'; base-uri 'none'; form-action 'none'; script-src 'self'`).
4. **Disable Downloads & Auth Prompts**:
   - `DownloadStarting`: set `Cancel = true`, `Handled = true`.
   - `BasicAuthenticationRequested` / `ClientCertificateRequested`: cancel automatically.
   - `PermissionRequested`: deny camera/microphone/geolocation by default (`COREWEBVIEW2_PERMISSION_STATE_DENY`, `SavesInProfile = false`).
5. **Disable Browser Chrome**:
   - `ICoreWebView2Settings`: `put_AreDefaultContextMenusEnabled(false)`, `put_AreDevToolsEnabled(cfg!(debug_assertions))`, `put_IsStatusBarEnabled(false)`, `put_AreDefaultScriptDialogsEnabled(false)`.
6. **Crash Recovery (`ProcessFailed`)**:
   - Listen to `ProcessFailed`; tear down the dead controller and display a fallback error card with an "Open in Browser" link, just as `iv_controller.cpp::showWebviewError` does.

## 6. Zoom, DPI, Dark Mode & Availability Check

- **Runtime detection**: Call `GetAvailableCoreWebView2BrowserVersionString` (exported by `WebView2Loader.dll` / `webview2-com`). If missing, show a clean error message with a link to Microsoft's Evergreen WebView2 Runtime installer and fall back to opening the URL in the system browser.
- **Zoom**: Use `tg_iv::default_iv_zoom(app_scale, screen_scale)` on first launch. Sync `ICoreWebView2Controller::put_ZoomFactor(controller.zoom().factor())` and subscribe to `ZoomFactorChanged` so Ctrl+Scroll updates `controller.set_zoom(...)`.
- **Dark mode**: Set `ICoreWebView2Profile2::put_PreferredColorScheme` (`DARK` / `LIGHT`) to match `tg-theme` and send `HostCommand::SetTheme { dark }` when the user toggles night mode live.
- **Opaque background**: Set `ICoreWebView2Controller2::put_DefaultBackgroundColor` to the active `tg-theme` window background color before showing the window to avoid a white flash in dark mode.
