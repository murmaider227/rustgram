//! High-level embedding helpers: restricted-origin CSP/lockdown, navigation policy,
//! scrollbar CSS theming, and user-data storage paths.
//!
//! Port of `Telegram/lib_webview/webview/webview_embed.cpp`,
//! `Telegram/lib_webview/webview/webview_common.h`, and
//! `Telegram/lib_webview/webview/platform/win/webview_win.cpp` (`GenerateStorageToken`).

use std::path::{Path, PathBuf};

use rand::RngCore;
use tg_theme::Color;
use url::Url;

/// Legacy storage token sentinel (`Webview::LegacyStorageIdToken` in `webview_common.h`).
pub const LEGACY_STORAGE_ID_TOKEN: &[u8] = b"<legacy>";

/// Default user-data subdirectory name under the Rustgram working directory.
pub const WEBVIEW_STORAGE_DIR_NAME: &str = "webview";

/// Size in bytes of a freshly generated storage token (`GenerateStorageToken` in `webview_win.cpp`).
pub const STORAGE_TOKEN_LEN: usize = 16;

/// Content-Security-Policy header value and lockdown init script for restricted origins.
///
/// Port of `RestrictedContent` / `MakeRestrictedContent` (`webview_embed.cpp:45-117`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictedContent {
    /// Strict Content-Security-Policy allowing only `connect-src` to the restricted origin.
    pub policy: String,
    /// JavaScript init script that injects `<meta>` CSP / DNS-prefetch-off headers and locks down
    /// browser storage, WebRTC, workers, dialogs, clipboard, media devices, and cookies.
    pub script: String,
}

/// Builds the strict CSP policy and lockdown init script for a restricted `https://host` origin.
///
/// Exact port of `MakeRestrictedContent` (`webview_embed.cpp:50-117`).
/// Returns `None` if `origin` is not a valid strict `https://host` origin with no port,
/// path, query, fragment, or userinfo.
#[must_use]
pub fn make_restricted_content(origin: &str) -> Option<RestrictedContent> {
    if !is_strict_https_origin(origin) {
        return None;
    }
    let parsed = Url::parse(origin).ok()?;
    let host = parsed.host_str()?;
    let ws_origin = format!("wss://{host}");

    let policy = format!(
        "default-src 'none'; \
         base-uri 'none'; \
         child-src 'none'; \
         connect-src {origin} {ws_origin}; \
         font-src 'none'; \
         form-action 'none'; \
         frame-src 'none'; \
         img-src 'none'; \
         manifest-src 'none'; \
         media-src 'none'; \
         object-src 'none'; \
         script-src 'unsafe-inline'; \
         style-src 'none'; \
         worker-src 'none'"
    );

    let literal = serde_json::to_string(&policy).ok()?;
    let script = format!(
        r#"(()=>{{const policy={literal};
const applyPolicy=()=>{{
 const head=document.head;if(!head)return false;
 const dns=document.createElement('meta');
 dns.httpEquiv='x-dns-prefetch-control';
 dns.content='off';
 head.insertBefore(dns,head.firstChild);
 const meta=document.createElement('meta');
 meta.httpEquiv='Content-Security-Policy';
 meta.content=policy;
 head.insertBefore(meta,head.firstChild);
 return true;
}};
if(!applyPolicy()){{
 const observer=new MutationObserver(()=>{{if(applyPolicy())observer.disconnect()}});
 observer.observe(document,{{childList:true,subtree:true}});
}}
const lock=(object,name,value)=>{{try{{Object.defineProperty(object,name,{{value,configurable:false,writable:false}})}}catch(error){{}}}};
for(const name of ['localStorage','sessionStorage','indexedDB','caches','Worker','SharedWorker','BroadcastChannel','Audio','AudioContext','webkitAudioContext','OfflineAudioContext','webkitOfflineAudioContext','speechSynthesis','SpeechSynthesisUtterance','RTCPeerConnection','webkitRTCPeerConnection','mozRTCPeerConnection','RTCDataChannel','RTCIceGatherer','RTCSessionDescription','RTCIceCandidate','WebTransport','WebAssembly','Notification','PresentationRequest','PaymentRequest','MediaRecorder','ImageCapture','SpeechRecognition','webkitSpeechRecognition','EyeDropper','showOpenFilePicker','showSaveFilePicker','showDirectoryPicker'])lock(globalThis,name,undefined);
for(const name of ['serviceWorker','clipboard','geolocation','mediaDevices','usb','serial','hid','bluetooth','credentials','getUserMedia','webkitGetUserMedia','wakeLock','share','presentation','xr','getGamepads','storage','locks','sendBeacon','permissions'])lock(navigator,name,undefined);
lock(globalThis,'open',()=>null);
lock(globalThis,'print',()=>{{}});
lock(globalThis,'alert',()=>{{}});
lock(globalThis,'confirm',()=>false);
lock(globalThis,'prompt',()=>null);
try{{Object.defineProperty(document,'cookie',{{get:()=>'',set:()=>true,configurable:false}})}}catch(error){{}}
}})()"#
    );

    Some(RestrictedContent { policy, script })
}

/// Checks whether `origin` is a strict `https://<host>` origin (no explicit port, path, query,
/// fragment, or userinfo), matching `MakeRestrictedContent` in `webview_embed.cpp:50-61`.
#[must_use]
fn is_strict_https_origin(origin: &str) -> bool {
    if !origin.starts_with("https://") || origin.ends_with('/') {
        return false;
    }
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && (url.path().is_empty() || url.path() == "/")
        && url.query().is_none()
        && url.fragment().is_none()
        // Reject explicit ":443" in raw text as QUrl::port(-1) != -1 does.
        && !origin["https://".len()..].contains(':')
}

/// Validates whether a resource request `value` is allowed under `restricted_origin`.
///
/// Exact port of `AllowedRestrictedUrl` (`webview_windows_edge_chromium.cpp:98-116`).
#[must_use]
pub fn is_allowed_restricted_url(value: &str, restricted_origin: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let Ok(expected) = Url::parse(restricted_origin) else {
        return false;
    };
    expected.scheme() == "https"
        && expected.username().is_empty()
        && expected.password().is_none()
        && expected.port_or_known_default() == Some(443)
        && (url.scheme() == "https" || url.scheme() == "wss")
        && url.host_str().is_some()
        && url.host_str() == expected.host_str()
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

/// Default navigation scheme policy matching `Window::navigationPolicyHandler`
/// (`webview_embed.cpp:443-462`), extended to allow the configured custom internal scheme.
#[must_use]
pub fn is_allowed_navigation_scheme(url: &str, custom_scheme: Option<&str>) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("tonsite://")
        || lower.starts_with("ton://")
    {
        return true;
    }
    if let Some(scheme) = custom_scheme {
        let prefix = format!("{}://", scheme.to_ascii_lowercase());
        if lower.starts_with(&prefix) {
            return true;
        }
    }
    false
}

/// Generates the scrollbar CSS injection script used by `Window::updateTheme`
/// (`webview_embed.cpp:205-256`).
///
/// Returns `(init_script, eval_script)` — `init_script` hooks `DOMContentLoaded` for future
/// navigations, while `eval_script` applies the style immediately to the active document.
#[must_use]
pub fn scrollbar_theme_scripts(
    scroll_bg: Color,
    scroll_bg_over: Color,
    scroll_bar_bg: Color,
    scroll_bar_bg_over: Color,
) -> (String, String) {
    let rgba = |c: Color| {
        format!(
            "rgba({}, {}, {}, {:.4})",
            c.r,
            c.g,
            c.b,
            f64::from(c.a) / 255.0
        )
    };
    let function = format!(
        "function() {{\n\
\tconst style = document.createElement('style');\n\
\tstyle.textContent = ' \\\n\
::-webkit-scrollbar {{ \\\n\
\tborder-radius: 5px !important; \\\n\
\tborder: 3px solid transparent !important; \\\n\
\tbackground-color: {} !important; \\\n\
\tbackground-clip: content-box !important; \\\n\
\twidth: 10px !important; \\\n\
}} \\\n\
::-webkit-scrollbar:hover {{ \\\n\
\tbackground-color: {} !important; \\\n\
}} \\\n\
::-webkit-scrollbar-thumb {{ \\\n\
\tborder-radius: 5px !important; \\\n\
\tborder: 3px solid transparent !important; \\\n\
\tbackground-color: {} !important; \\\n\
\tbackground-clip: content-box !important; \\\n\
}} \\\n\
::-webkit-scrollbar-thumb:hover {{ \\\n\
\tbackground-color: {} !important; \\\n\
}} \\\n\
';\n\
  if (document.head) {{ document.head.append(style); }}\n\
}}",
        rgba(scroll_bg),
        rgba(scroll_bg_over),
        rgba(scroll_bar_bg),
        rgba(scroll_bar_bg_over),
    );
    let init_script = format!("document.addEventListener('DOMContentLoaded', {function}, false);");
    let eval_script = format!("({function}());");
    (init_script, eval_script)
}

/// Generates a random 16-byte storage token (`Webview::GenerateStorageToken` in `webview_win.cpp`).
#[must_use]
pub fn generate_storage_token() -> Vec<u8> {
    let mut bytes = vec![0u8; STORAGE_TOKEN_LEN];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

/// Resolves a WebView2 user-data folder inside the Rustgram working directory.
///
/// Produces `<workdir>/webview/<profile>` (or `<workdir>/webview/default` if `profile` is empty),
/// sanitizing path separators to prevent escaping the working directory.
#[must_use]
pub fn resolve_user_data_dir(workdir: &Path, profile: &str) -> PathBuf {
    let sanitized: String = profile
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let folder = if sanitized.is_empty() {
        "default"
    } else {
        sanitized.as_str()
    };
    workdir.join(WEBVIEW_STORAGE_DIR_NAME).join(folder)
}
