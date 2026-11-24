//! JavaScript dialog (`alert`/`confirm`/`prompt`) conversion and popup flood protection.
//!
//! Port of `Telegram/lib_webview/webview/webview_dialog.h` and
//! `Telegram/lib_webview/webview/webview_dialog.cpp`.

use std::time::{Duration, Instant};

use url::Url;

use crate::interface::{DialogArgs, DialogResult, DialogType};

/// Maximum number of rapid popups allowed before throttling (`kPopupsQuicklyLimit`).
pub const POPUPS_QUICKLY_LIMIT: u32 = 3;

/// Window during which consecutive popups count toward [`POPUPS_QUICKLY_LIMIT`] (`kPopupsQuicklyDelay`).
pub const POPUPS_QUICKLY_DELAY: Duration = Duration::from_secs(8);

/// Semantic kind of button shown in a webview popup dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupButtonType {
    /// Custom-labeled default button.
    Default,
    /// Localized "OK" button.
    Ok,
    /// Localized "Close" button.
    Close,
    /// Localized "Cancel" button.
    Cancel,
    /// Destructive/attention-styled custom button.
    Destructive,
}

/// Single button descriptor inside [`PopupArgs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupButton {
    /// Unique identifier echoed back in [`PopupResult::id`].
    pub id: String,
    /// Custom text label (used for `Default` and `Destructive`).
    pub text: String,
    /// Button visual/semantic type.
    pub button_type: PopupButtonType,
}

/// Parameters describing a modal popup shown for JS dialogs or Mini App `web_app_open_popup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupArgs {
    /// Optional title (host name for JS dialogs, or custom title for Mini App popups).
    pub title: String,
    /// Main body text.
    pub text: String,
    /// Initial input value when showing a `prompt()` dialog.
    pub value: Option<String>,
    /// Action buttons in display order.
    pub buttons: Vec<PopupButton>,
    /// Bypasses [`PopupFloodGuard`] (used for unsaved-changes close confirmation).
    pub ignore_flood_check: bool,
}

/// Result returned when the user dismisses or clicks a button in a popup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PopupResult {
    /// Clicked button ID, or `None` if dismissed without clicking a button.
    pub id: Option<String>,
    /// Entered text value for `prompt()` dialogs.
    pub value: Option<String>,
}

/// Converts WebView JS dialog arguments (`alert`/`confirm`/`prompt`) into [`PopupArgs`].
///
/// Port of `DialogPopupArgs` (`webview_dialog.cpp:53-75`).
#[must_use]
pub fn dialog_to_popup_args(args: &DialogArgs) -> PopupArgs {
    let mut buttons = vec![PopupButton {
        id: "ok".to_owned(),
        text: String::new(),
        button_type: PopupButtonType::Ok,
    }];
    if args.dialog_type != DialogType::Alert {
        buttons.push(PopupButton {
            id: "cancel".to_owned(),
            text: String::new(),
            button_type: PopupButtonType::Cancel,
        });
    }
    let title = Url::parse(&args.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();

    PopupArgs {
        title,
        text: args.text.clone(),
        value: (args.dialog_type == DialogType::Prompt).then(|| args.value.clone()),
        buttons,
        ignore_flood_check: false,
    }
}

/// Converts a [`PopupResult`] back into a JS [`DialogResult`].
///
/// Port of `DialogResultFromPopup` (`webview_dialog.cpp:77-85`).
#[must_use]
pub fn dialog_result_from_popup(result: PopupResult) -> DialogResult {
    if result.id.as_deref() == Some("cancel") {
        return DialogResult::default();
    }
    let accepted = result.id.as_deref() == Some("ok") || result.value.is_some();
    DialogResult {
        text: result.value.unwrap_or_default(),
        accepted,
    }
}

/// Rate limiter preventing malicious web pages from spamming blocking modal dialogs.
///
/// Port of `PopupsShownTooQuickly` and `InBlockingPopup` (`webview_dialog.cpp:36-94`).
#[derive(Debug, Default)]
pub struct PopupFloodGuard {
    in_blocking_popup: bool,
    popups_shown_quickly: u32,
    popup_last_shown: Option<Instant>,
}

impl PopupFloodGuard {
    /// Creates a new guard with no active popup.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if a modal popup is currently open.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.in_blocking_popup
    }

    /// Attempts to begin showing a popup at `now`.
    ///
    /// Returns `false` if another popup is already active or if the flood limit
    /// ([`POPUPS_QUICKLY_LIMIT`] within [`POPUPS_QUICKLY_DELAY`]) has been exceeded
    /// (unless `ignore_flood_check` is `true`).
    pub fn try_begin(&mut self, now: Instant, ignore_flood_check: bool) -> bool {
        if self.in_blocking_popup {
            return false;
        }
        if !ignore_flood_check && self.is_flooding(now) {
            return false;
        }
        self.in_blocking_popup = true;
        true
    }

    /// Marks the currently active popup as closed at `now`.
    pub fn finish(&mut self, now: Instant) {
        self.in_blocking_popup = false;
        self.popup_last_shown = Some(now);
    }

    fn is_flooding(&mut self, now: Instant) -> bool {
        match self.popup_last_shown {
            None => {
                self.popups_shown_quickly = 1;
                false
            }
            Some(last) if now.saturating_duration_since(last) >= POPUPS_QUICKLY_DELAY => {
                self.popups_shown_quickly = 1;
                false
            }
            Some(_) => {
                self.popups_shown_quickly = self.popups_shown_quickly.saturating_add(1);
                self.popups_shown_quickly > POPUPS_QUICKLY_LIMIT
            }
        }
    }
}
