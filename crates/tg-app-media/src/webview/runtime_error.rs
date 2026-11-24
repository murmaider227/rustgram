//! Missing WebView2 runtime and renderer crash dialog presentation using `tg-ui` boxes.
//!
//! Port origin: `Telegram/SourceFiles/iv/iv_controller.cpp` (`showWebviewError`) and
//! `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` (`showWebviewError`).

use egui::{Align, Layout, Rect, RichText, Ui};
use tg_lang::tr;
use tg_theme::Palette as TgPalette;
use tg_ui::layers::{BoxButton, BoxModal, LayerStack};
use tg_ui::stand_in::{BoxStyle, Colors, LinkButtonStyle};
use tg_ui::widgets::buttons::LinkButton;
use tg_webview::{Availability, AvailabilityError, WEBVIEW2_DOWNLOAD_URL, WebViewError};

use crate::webview::strings;
use crate::webview::theme::TgThemeColors;

/// Modal ID used by [`push_runtime_error_box`] on a `tg-ui` [`LayerStack`].
pub const RUNTIME_ERROR_BOX_ID: &str = "webview_runtime_error";

/// Action button IDs emitted by [`RUNTIME_ERROR_BOX_ID`].
pub const ACTION_DOWNLOAD_WEBVIEW2: &str = "download_webview2";
/// Opens the target URL in the user's external browser as a fallback.
pub const ACTION_OPEN_IN_BROWSER: &str = "open_in_browser";
/// Closes the error dialog / window.
pub const ACTION_CLOSE_ERROR: &str = "close";

/// Categorized WebView runtime failure shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeErrorKind {
    /// Microsoft Edge WebView2 Runtime is not installed on Windows.
    MissingWebView2 {
        /// Diagnostic detail text.
        details: String,
        /// Official download URL (`WEBVIEW2_DOWNLOAD_URL`).
        download_url: String,
    },
    /// Generic WebView initialization failure (e.g. unsupported OS or COM error).
    InitFailed {
        /// Error message.
        details: String,
    },
    /// WebView2 renderer process crashed while open.
    ProcessCrashed,
}

impl RuntimeErrorKind {
    /// Converts a `tg-webview` [`Availability`] check into an error if unsupported.
    #[must_use]
    pub fn from_availability(avail: &Availability) -> Option<Self> {
        if avail.is_supported() {
            return None;
        }
        match avail.error {
            AvailabilityError::NoWebView2 => Some(Self::MissingWebView2 {
                details: avail.details.clone(),
                download_url: WEBVIEW2_DOWNLOAD_URL.to_owned(),
            }),
            _ => Some(Self::InitFailed {
                details: avail
                    .user_error_text()
                    .unwrap_or_else(|| avail.details.clone()),
            }),
        }
    }

    /// Converts a [`WebViewError`] returned by `WebView::create` into a user-facing error.
    #[must_use]
    pub fn from_webview_error(err: &WebViewError) -> Self {
        match err {
            WebViewError::RuntimeNotInstalled {
                details,
                download_url,
            } => Self::MissingWebView2 {
                details: details.clone(),
                download_url: (*download_url).to_owned(),
            },
            other => Self::InitFailed {
                details: other.to_string(),
            },
        }
    }

    /// Returns the dialog title for this error.
    #[must_use]
    pub fn title(&self) -> &'static str {
        match self {
            Self::MissingWebView2 { .. } => strings::WEBVIEW2_REQUIRED_TITLE,
            Self::InitFailed { .. } => strings::WEBVIEW_UNAVAILABLE_TITLE,
            Self::ProcessCrashed => strings::WEBVIEW_CRASHED_TITLE,
        }
    }

    /// Returns the download link if this error is caused by a missing WebView2 runtime.
    #[must_use]
    pub fn download_url(&self) -> Option<&str> {
        match self {
            Self::MissingWebView2 { download_url, .. } => Some(download_url.as_str()),
            _ => None,
        }
    }

    /// Returns a clear user-facing explanation message.
    #[must_use]
    pub fn body_text(&self) -> String {
        match self {
            Self::MissingWebView2 { details, .. } => {
                if details.is_empty() {
                    strings::WEBVIEW2_MISSING_BODY.to_owned()
                } else {
                    format!("{}\n\n{details}", strings::WEBVIEW2_MISSING_BODY_PREFIX)
                }
            }
            Self::InitFailed { details } => {
                format!("{}\n\n{details}", tr::lng_bot_webview_failed())
            }
            Self::ProcessCrashed => tr::lng_bot_webview_crashed(),
        }
    }
}

/// Pushes a `tg-ui` [`BoxModal`] describing `error` onto `layers`.
pub fn push_runtime_error_box(
    ctx: &egui::Context,
    layers: &mut LayerStack,
    palette: &TgPalette,
    error: &RuntimeErrorKind,
    allow_open_in_browser: bool,
) {
    let colors = TgThemeColors(palette);
    let style = BoxStyle::default_box(&colors);
    let mut modal = BoxModal::new(RUNTIME_ERROR_BOX_ID, error.title())
        .width(420.0)
        .add_button(BoxButton::default_btn(ACTION_CLOSE_ERROR, tr::lng_close()));

    if error.download_url().is_some() {
        modal = modal.add_button(BoxButton::default_btn(
            ACTION_DOWNLOAD_WEBVIEW2,
            strings::DOWNLOAD_WEBVIEW2,
        ));
    }
    if allow_open_in_browser {
        modal = modal.with_left_button(BoxButton::default_btn(
            ACTION_OPEN_IN_BROWSER,
            tr::lng_iv_open_in_browser(),
        ));
    }
    layers.push(ctx, modal, &style);
}

/// Renders the interior body of [`RUNTIME_ERROR_BOX_ID`] inside a `tg-ui` modal box.
///
/// Returns `true` if the user clicked the inline download URL link directly.
pub fn render_runtime_error_body(
    ui: &mut Ui,
    palette: &TgPalette,
    error: &RuntimeErrorKind,
    fallback_url: Option<&str>,
) -> bool {
    let colors = TgThemeColors(palette);
    let mut clicked_download = false;

    ui.label(
        RichText::new(error.body_text())
            .size(14.0)
            .color(colors.color("boxTextFg")),
    );

    if let Some(url) = error.download_url() {
        ui.add_space(10.0);
        ui.label(
            RichText::new(strings::OFFICIAL_DOWNLOAD_LINK)
                .size(12.5)
                .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(2.0);
        let link_st = LinkButtonStyle::default_link(&colors);
        if LinkButton::new(url, link_st).show(ui).clicked() {
            clicked_download = true;
        }
    }

    if let Some(url) = fallback_url
        && !url.is_empty()
    {
        ui.add_space(8.0);
        ui.label(
            RichText::new(format!("{}{url}", strings::PAGE_URL_PREFIX))
                .size(12.0)
                .color(colors.color("windowSubTextFg")),
        );
    }

    clicked_download
}

/// Renders a clean centered fallback panel inside `bounds` when the WebView is unavailable
/// (used alongside or behind the modal error box).
pub fn render_unavailable_backdrop(
    ui: &mut Ui,
    bounds: Rect,
    palette: &TgPalette,
    error: &RuntimeErrorKind,
) {
    let colors = TgThemeColors(palette);
    ui.painter()
        .rect_filled(bounds, egui::CornerRadius::ZERO, colors.color("windowBg"));

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bounds.shrink(32.0))
            .layout(Layout::top_down(Align::Center)),
    );
    child.add_space((bounds.height() * 0.25).max(16.0));
    child.label(
        RichText::new(error.title())
            .size(18.0)
            .strong()
            .color(colors.color("windowBoldFg")),
    );
    child.add_space(8.0);
    child.label(
        RichText::new(error.body_text())
            .size(13.5)
            .color(colors.color("windowSubTextFg")),
    );
    if let Some(url) = error.download_url() {
        child.add_space(8.0);
        child.label(
            RichText::new(url)
                .size(12.5)
                .color(colors.color("windowActiveTextFg")),
        );
    }
}
