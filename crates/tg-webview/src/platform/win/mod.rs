//! Windows WebView2 backend module.
//!
//! Port of `Telegram/lib_webview/webview/platform/win/webview_win.cpp`,
//! `webview_windows_data_stream.cpp`, and `webview_windows_edge_chromium.cpp`.

pub(crate) mod data_stream_com;
pub(crate) mod webview2;

pub(crate) use webview2::{EdgeChromiumWebView as PlatformWebView, availability};
