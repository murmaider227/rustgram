//! Host bridge policies for widget link opening, clipboard copying, and bot query tracking.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_bridge.h/.cpp` and `tgw_wallet.h/.cpp`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

/// Bot query timeout in milliseconds (`kRequestTimeout = 30000` in `tgw_bridge.cpp:41`).
pub const REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Maximum toast lines shown when a widget copies text (`kCopiedToastLines = 4` in `tgw_bridge.cpp:42`).
pub const COPIED_TOAST_LINES: usize = 4;

/// Failure classification returned to the widget SDK (`twidget::WidgetRequestFailure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetRequestFailure {
    /// Server returned `FLOOD_WAIT_*`.
    RateLimited,
    /// Server returned `WIDGET_RESPONSE_TIMEOUT`.
    Timeout,
    /// Any other transport, RPC, or local deadline failure.
    Failed,
}

/// Successful answer payload from `messages.getBotWidgetQueryAnswer`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetRequestAnswer {
    /// Optional UTF-8 result string returned by the bot.
    pub result: Option<String>,
}

/// Full outcome delivered back to the sandboxed widget (`twidget::WidgetRequestOutcome`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetRequestOutcome {
    /// Bot answer payload (when `failure` is `None`).
    pub answer: WidgetRequestAnswer,
    /// Failure classification, if the request failed.
    pub failure: Option<WidgetRequestFailure>,
}

impl WidgetRequestOutcome {
    /// Creates a successful outcome with an optional result string.
    #[must_use]
    pub fn ok(result: Option<String>) -> Self {
        Self {
            answer: WidgetRequestAnswer { result },
            failure: None,
        }
    }

    /// Creates a failed outcome with the given failure reason.
    #[must_use]
    pub const fn err(failure: WidgetRequestFailure) -> Self {
        Self {
            answer: WidgetRequestAnswer { result: None },
            failure: Some(failure),
        }
    }

    /// Returns `true` when no failure occurred.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.failure.is_none()
    }
}

/// Maps an MTProto RPC error type into a [`WidgetRequestOutcome`] (`ErrorOutcome` in `tgw_bridge.cpp:59-67`).
#[must_use]
pub fn map_rpc_error_to_outcome(error_name: &str, is_flood: bool) -> WidgetRequestOutcome {
    if is_flood
        || error_name.starts_with("FLOOD_WAIT")
        || error_name.starts_with("FLOOD_PREMIUM_WAIT")
    {
        WidgetRequestOutcome::err(WidgetRequestFailure::RateLimited)
    } else if error_name == "WIDGET_RESPONSE_TIMEOUT" {
        WidgetRequestOutcome::err(WidgetRequestFailure::Timeout)
    } else {
        WidgetRequestOutcome::err(WidgetRequestFailure::Failed)
    }
}

/// Pending bot query entry tracked by [`BotQueryTracker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBotQuery {
    /// Caller-assigned request ID.
    pub request_id: u64,
    /// Deadline timestamp in milliseconds.
    pub deadline_ms: u64,
    /// Whether `with_wallet` was requested.
    pub with_wallet: bool,
}

/// Tracks in-flight widget bot queries and enforces [`REQUEST_TIMEOUT_MS`] deadlines
/// (`Tgw::BotQueries` in `tgw_bridge.h:30-59`, `tgw_bridge.cpp:116-190`).
#[derive(Debug, Default)]
pub struct BotQueryTracker {
    pending: BTreeMap<u64, PendingBotQuery>,
}

impl BotQueryTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new in-flight request with deadline `now_ms + REQUEST_TIMEOUT_MS`.
    pub fn register(&mut self, request_id: u64, with_wallet: bool, now_ms: u64) {
        self.pending.insert(
            request_id,
            PendingBotQuery {
                request_id,
                deadline_ms: now_ms.saturating_add(REQUEST_TIMEOUT_MS),
                with_wallet,
            },
        );
    }

    /// Completes and removes `request_id`, returning `true` if it was still pending.
    pub fn complete(&mut self, request_id: u64) -> bool {
        self.pending.remove(&request_id).is_some()
    }

    /// Sweeps all expired requests at `now_ms`, returning their request IDs to fail with `Failed`.
    pub fn expire(&mut self, now_ms: u64) -> Vec<u64> {
        let expired: Vec<u64> = self
            .pending
            .iter()
            .filter_map(|(&id, entry)| (entry.deadline_ms <= now_ms).then_some(id))
            .collect();
        for id in &expired {
            self.pending.remove(id);
        }
        expired
    }

    /// Cancels all pending requests (e.g. when the widget is destroyed) and returns their IDs.
    pub fn cancel_all(&mut self) -> Vec<u64> {
        let ids: Vec<u64> = self.pending.keys().copied().collect();
        self.pending.clear();
        ids
    }

    /// Returns the earliest deadline among pending requests, if any.
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.pending.values().map(|p| p.deadline_ms).min()
    }

    /// Returns the number of currently pending queries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Returns `true` if there are no pending queries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Decision returned by [`evaluate_link_open`] (`Bridge::openLink` in `tgw_bridge.cpp:202-226`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPolicyDecision {
    /// Refused by security/window policy with diagnostic note.
    Refused(String),
    /// Dropped because another modal dialog is currently shown.
    Dropped(String),
    /// External URL requires explicit user confirmation dialog.
    ConfirmExternal {
        /// Validated URL string.
        url: String,
        /// Diagnostic note.
        note: String,
    },
    /// Telegram internal/t.me link delegated to the app's URL handler.
    OpenTelegram {
        /// Validated Telegram link.
        url: String,
        /// Diagnostic note.
        note: String,
    },
}

/// Returns `true` if `url` targets internal Telegram Passport or OAuth/Login flows that widgets
/// are forbidden from invoking (`Core::InternalPassportOrOAuthLink` in `tgw_bridge.cpp:210`).
#[must_use]
pub fn is_passport_or_oauth_link(url: &str) -> bool {
    let trimmed = url.trim().to_ascii_lowercase();
    if trimmed.starts_with("tg://passport")
        || trimmed.starts_with("tg://oauth")
        || trimmed.starts_with("tg://login")
        || trimmed.starts_with("tg://confirmphone")
    {
        return true;
    }
    if let Ok(parsed) = Url::parse(url.trim())
        && let Some(host) = parsed.host_str()
    {
        let h = host.to_ascii_lowercase();
        if h == "t.me" || h == "telegram.me" || h == "telegram.dog" {
            let path = parsed.path().trim_start_matches('/').to_ascii_lowercase();
            if path.starts_with("passport")
                || path.starts_with("login")
                || path.starts_with("oauth")
            {
                return true;
            }
        }
    }
    false
}

/// Evaluates whether a widget's `openLink` / `openTelegramLink` call is allowed
/// (`Bridge::openLink` in `tgw_bridge.cpp:202-226`).
#[must_use]
pub fn evaluate_link_open(
    url: &str,
    telegram: bool,
    has_window: bool,
    layer_shown: bool,
) -> LinkPolicyDecision {
    let method = if telegram {
        "openTelegramLink"
    } else {
        "openLink"
    };
    if !has_window {
        return LinkPolicyDecision::Refused(format!(
            "{method}: refused, no window of this account is open"
        ));
    }
    if layer_shown {
        return LinkPolicyDecision::Dropped(format!(
            "{method}: dropped, a dialog is already shown"
        ));
    }
    let trimmed = url.trim();
    let Ok(parsed) = Url::parse(trimmed) else {
        return LinkPolicyDecision::Refused(format!("{method}: refused, invalid URL"));
    };
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" && scheme != "tg" {
        return LinkPolicyDecision::Refused(format!(
            "{method}: refused, disallowed scheme '{scheme}'"
        ));
    }
    if is_passport_or_oauth_link(trimmed) {
        return LinkPolicyDecision::Refused(format!("{method}: refused, a passport or login link"));
    }
    if !telegram {
        return LinkPolicyDecision::ConfirmExternal {
            url: trimmed.to_owned(),
            note: "openLink: asked to confirm the link".to_owned(),
        };
    }
    LinkPolicyDecision::OpenTelegram {
        url: trimmed.to_owned(),
        note: "openTelegramLink: passed to the app's t.me handling".to_owned(),
    }
}

/// Decision returned by [`evaluate_copy_text`] (`Bridge::copyText` in `tgw_bridge.cpp:228-248`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyTextDecision {
    /// Refused because no window of this account currently displays the widget.
    Refused(String),
    /// Allowed: copy text to clipboard and show toast.
    Copy {
        /// Text to copy.
        text: String,
        /// UTF-16 code unit count (for diagnostics parity).
        utf16_units: usize,
        /// Diagnostic note.
        note: String,
    },
}

/// Evaluates whether a widget's `copyText` request is allowed.
#[must_use]
pub fn evaluate_copy_text(text: &str, has_window: bool) -> CopyTextDecision {
    if !has_window {
        return CopyTextDecision::Refused(
            "copyText: refused, no window of this account shows it".to_owned(),
        );
    }
    let utf16_units = text.encode_utf16().count();
    CopyTextDecision::Copy {
        text: text.to_owned(),
        utf16_units,
        note: format!("copyText: copied {utf16_units} UTF-16 units"),
    }
}
