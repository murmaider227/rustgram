//! Injectable 3-D Secure payment verification WebView host contract and `tg-webview` adapter.
//!
//! Port origin: `Telegram/SourceFiles/payments/payments_checkout_process.cpp`
//! (`panelWebviewNavigationAttempt`, `VerificationNeeded`) and
//! `Telegram/SourceFiles/payments/ui/payments_panel.cpp` (`showWebview`, `createWebview`).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tg_webview::{WebView, WebViewCallbacks, WebViewConfig, WindowMode};

static NEXT_WEBVIEW_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// UI-thread registry holding live COM/HWND `tg_webview::WebView` instances so
    /// [`TgWebViewVerificationHost`] stays `Send` without any `unsafe` code.
    static ACTIVE_VERIFICATION_WEBVIEWS: RefCell<HashMap<u64, WebView>> =
        RefCell::new(HashMap::new());
}

/// Lifecycle event emitted by a [`PaymentVerificationHost`] during 3-D Secure verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentVerificationEvent {
    /// The embedded webview navigated to an intermediate HTTPS URL inside the 3-DS flow.
    Navigating(String),
    /// The payment provider redirected back to a Telegram completion URI (`tg://` / `t.me/...`).
    Completed {
        /// Captured Telegram redirect URL.
        redirect_url: String,
    },
    /// Verification was cancelled via a cancel redirect or by closing the verification window.
    Cancelled,
    /// WebView creation, navigation, or renderer process failed.
    Failed(String),
}

/// Injectable host for opening a payment provider's 3-D Secure verification URL in `tg-webview`
/// and capturing completion or cancellation redirects.
///
/// Production code uses [`TgWebViewVerificationHost`]; tests inject [`FakePaymentVerificationHost`]
/// so no real WebView2 window or external network request is ever opened.
pub trait PaymentVerificationHost: Send {
    /// Opens `url` in the verification webview for `provider`.
    fn open_verification(&mut self, url: &str, provider: &str) -> Result<(), String>;

    /// Polls the next queued [`PaymentVerificationEvent`], if any.
    fn poll_event(&mut self) -> Option<PaymentVerificationEvent>;

    /// Closes the active verification webview, if open.
    fn close(&mut self);

    /// Returns `true` while a verification session is currently open.
    fn is_active(&self) -> bool;
}

/// Returns `true` when `uri` is a local Telegram redirect target (`tg://`, `tg:`, `t.me/`).
///
/// Mirrors `Core::TryConvertUrlToLocal(uri) != uri` in `payments_checkout_process.cpp:771`.
#[must_use]
pub fn is_telegram_verification_redirect(uri: &str) -> bool {
    let trimmed = uri.trim();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("tg://")
        || lower.starts_with("tg:")
        || lower.starts_with("https://t.me/")
        || lower.starts_with("http://t.me/")
        || lower.starts_with("t.me/")
        || lower.starts_with("https://telegram.me/")
        || lower.starts_with("http://telegram.me/")
}

/// Classifies a webview navigation URI during 3-D Secure verification.
///
/// - Returns `Some(PaymentVerificationEvent::Cancelled)` when a Telegram redirect signals cancel
///   or failure (e.g. `tg://cancel`, `tg://resolve?...&status=cancel`).
/// - Returns `Some(PaymentVerificationEvent::Completed { redirect_url })` for any other Telegram
///   redirect (`tg://resolve?...`, `https://t.me/...`).
/// - Returns `None` for normal third-party HTTPS pages (navigation is allowed to proceed).
#[must_use]
pub fn classify_verification_redirect(uri: &str) -> Option<PaymentVerificationEvent> {
    let trimmed = uri.trim();
    if !is_telegram_verification_redirect(trimmed) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let is_cancel = lower.starts_with("tg://cancel")
        || lower.starts_with("tg:cancel")
        || lower.contains("status=cancel")
        || lower.contains("status=failed")
        || lower.contains("result=cancel")
        || lower.contains("canceled=1")
        || lower.contains("cancelled=1")
        || lower.ends_with("/cancel");
    if is_cancel {
        Some(PaymentVerificationEvent::Cancelled)
    } else {
        Some(PaymentVerificationEvent::Completed {
            redirect_url: trimmed.to_owned(),
        })
    }
}

/// Live `tg-webview` (WebView2) implementation of [`PaymentVerificationHost`].
///
/// Opens the 3-D Secure challenge URL inside a dedicated `tg_webview::WebView` instance on the
/// UI thread and intercepts `tg://` / `t.me/` completion and cancel redirects through
/// [`WebViewCallbacks::on_navigation_policy`].
pub struct TgWebViewVerificationHost {
    active_id: Option<u64>,
    events: Arc<Mutex<VecDeque<PaymentVerificationEvent>>>,
}

impl Default for TgWebViewVerificationHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TgWebViewVerificationHost {
    /// Creates an idle [`TgWebViewVerificationHost`] ready to open a 3-D Secure challenge.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_id: None,
            events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl Drop for TgWebViewVerificationHost {
    fn drop(&mut self) {
        self.close();
    }
}

impl PaymentVerificationHost for TgWebViewVerificationHost {
    fn open_verification(&mut self, url: &str, _provider: &str) -> Result<(), String> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Err("Verification URL cannot be empty.".to_owned());
        }
        if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
            return Err("Verification URL must use http:// or https://.".to_owned());
        }
        self.close();
        if !tg_webview::is_supported() {
            return Err(
                "Embedded WebView2 runtime is unavailable on this system for 3-D Secure verification."
                    .to_owned(),
            );
        }

        let events_nav = Arc::clone(&self.events);
        let events_fail = Arc::clone(&self.events);
        let callbacks = WebViewCallbacks {
            on_navigation_policy: Some(Arc::new(move |uri: &str, new_window: bool| {
                if let Some(event) = classify_verification_redirect(uri) {
                    if let Ok(mut q) = events_nav.lock() {
                        q.push_back(event);
                    }
                    return false;
                }
                if !new_window && let Ok(mut q) = events_nav.lock() {
                    q.push_back(PaymentVerificationEvent::Navigating(uri.to_owned()));
                }
                !new_window
            })),
            on_process_failed: Some(Arc::new(move || {
                if let Ok(mut q) = events_fail.lock() {
                    q.push_back(PaymentVerificationEvent::Failed(
                        "Payment verification WebView process terminated unexpectedly.".to_owned(),
                    ));
                }
            })),
            ..WebViewCallbacks::default()
        };

        let config = WebViewConfig {
            mode: WindowMode::External,
            callbacks,
            ..WebViewConfig::default()
        };

        let view = WebView::create_from_raw(None, config).map_err(|e| e.to_string())?;
        view.navigate(trimmed).map_err(|e| e.to_string())?;
        let id = NEXT_WEBVIEW_ID.fetch_add(1, Ordering::Relaxed);
        ACTIVE_VERIFICATION_WEBVIEWS.with(|map| {
            map.borrow_mut().insert(id, view);
        });
        self.active_id = Some(id);
        Ok(())
    }

    fn poll_event(&mut self) -> Option<PaymentVerificationEvent> {
        let event = self.events.lock().ok()?.pop_front()?;
        if matches!(
            event,
            PaymentVerificationEvent::Completed { .. }
                | PaymentVerificationEvent::Cancelled
                | PaymentVerificationEvent::Failed(_)
        ) {
            self.close();
        }
        Some(event)
    }

    fn close(&mut self) {
        if let Some(id) = self.active_id.take() {
            ACTIVE_VERIFICATION_WEBVIEWS.with(|map| {
                if let Some(view) = map.borrow_mut().remove(&id) {
                    view.close();
                }
            });
        }
    }

    fn is_active(&self) -> bool {
        self.active_id.is_some()
    }
}

/// Shared test handle for inspecting and driving a [`FakePaymentVerificationHost`].
#[derive(Debug, Clone, Default)]
pub struct FakePaymentVerificationController {
    opened: Arc<Mutex<Vec<(String, String)>>>,
    events: Arc<Mutex<VecDeque<PaymentVerificationEvent>>>,
    active: Arc<AtomicBool>,
    close_count: Arc<AtomicUsize>,
    fail_on_open: Arc<Mutex<Option<String>>>,
}

impl FakePaymentVerificationController {
    /// Returns all `(url, provider)` pairs passed to [`PaymentVerificationHost::open_verification`].
    #[must_use]
    pub fn opened_requests(&self) -> Vec<(String, String)> {
        self.opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Returns `true` when the fake webview is currently open.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Returns how many times [`PaymentVerificationHost::close`] has been called.
    #[must_use]
    pub fn close_count(&self) -> usize {
        self.close_count.load(Ordering::SeqCst)
    }

    /// Configures the fake host to fail the next `open_verification` call with `message`.
    pub fn set_fail_on_open(&self, message: Option<String>) {
        *self.fail_on_open.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }

    /// Simulates a webview navigation to `uri` through the exact same
    /// [`classify_verification_redirect`] policy used by [`TgWebViewVerificationHost`].
    ///
    /// Returns `false` when the redirect was intercepted (completion or cancel), `true` when
    /// normal in-flow HTTPS navigation was allowed.
    pub fn simulate_redirect(&self, uri: &str) -> bool {
        let mut q = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(event) = classify_verification_redirect(uri) {
            q.push_back(event);
            false
        } else {
            q.push_back(PaymentVerificationEvent::Navigating(uri.to_owned()));
            true
        }
    }

    /// Enqueues an explicit [`PaymentVerificationEvent`] directly.
    pub fn push_event(&self, event: PaymentVerificationEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(event);
    }
}

/// Deterministic in-memory fake [`PaymentVerificationHost`] for unit and integration tests.
pub struct FakePaymentVerificationHost {
    controller: FakePaymentVerificationController,
}

impl Default for FakePaymentVerificationHost {
    fn default() -> Self {
        Self::new().0
    }
}

impl FakePaymentVerificationHost {
    /// Creates a `(host, controller)` pair so tests can inspect opened URLs and inject redirects
    /// after passing `host` into `CheckoutView` or `PaymentsScreenHost`.
    #[must_use]
    pub fn new() -> (Self, FakePaymentVerificationController) {
        let controller = FakePaymentVerificationController::default();
        (
            Self {
                controller: controller.clone(),
            },
            controller,
        )
    }
}

impl PaymentVerificationHost for FakePaymentVerificationHost {
    fn open_verification(&mut self, url: &str, provider: &str) -> Result<(), String> {
        if let Some(err) = self
            .controller
            .fail_on_open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Err(err);
        }
        self.controller
            .opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((url.to_owned(), provider.to_owned()));
        self.controller.active.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn poll_event(&mut self) -> Option<PaymentVerificationEvent> {
        let event = self
            .controller
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()?;
        if matches!(
            event,
            PaymentVerificationEvent::Completed { .. }
                | PaymentVerificationEvent::Cancelled
                | PaymentVerificationEvent::Failed(_)
        ) {
            self.close();
        }
        Some(event)
    }

    fn close(&mut self) {
        if self.controller.active.swap(false, Ordering::SeqCst) {
            self.controller.close_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn is_active(&self) -> bool {
        self.controller.is_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_telegram_redirects_and_allows_bank_urls() {
        assert_eq!(
            classify_verification_redirect("https://acs.bank.example/3ds/challenge"),
            None
        );
        assert_eq!(
            classify_verification_redirect("tg://resolve?domain=merchant_bot&start=paid_42"),
            Some(PaymentVerificationEvent::Completed {
                redirect_url: "tg://resolve?domain=merchant_bot&start=paid_42".to_owned(),
            })
        );
        assert_eq!(
            classify_verification_redirect("https://t.me/merchant_bot?start=paid_42"),
            Some(PaymentVerificationEvent::Completed {
                redirect_url: "https://t.me/merchant_bot?start=paid_42".to_owned(),
            })
        );
        assert_eq!(
            classify_verification_redirect("tg://cancel"),
            Some(PaymentVerificationEvent::Cancelled)
        );
        assert_eq!(
            classify_verification_redirect("tg://resolve?domain=merchant_bot&status=cancel"),
            Some(PaymentVerificationEvent::Cancelled)
        );
    }

    #[test]
    fn fake_host_records_open_and_delivers_redirect_events() {
        let (mut host, ctrl) = FakePaymentVerificationHost::new();
        host.open_verification("https://acs.example/verify?t=1", "Stripe")
            .expect("open succeeds");
        assert!(host.is_active());
        assert_eq!(
            ctrl.opened_requests(),
            vec![(
                "https://acs.example/verify?t=1".to_owned(),
                "Stripe".to_owned()
            )]
        );

        assert!(ctrl.simulate_redirect("https://acs.example/step2"));
        assert_eq!(
            host.poll_event(),
            Some(PaymentVerificationEvent::Navigating(
                "https://acs.example/step2".to_owned()
            ))
        );
        assert!(host.is_active());

        assert!(!ctrl.simulate_redirect("tg://resolve?domain=shop_bot&start=ok"));
        assert_eq!(
            host.poll_event(),
            Some(PaymentVerificationEvent::Completed {
                redirect_url: "tg://resolve?domain=shop_bot&start=ok".to_owned(),
            })
        );
        assert!(!host.is_active());
        assert_eq!(ctrl.close_count(), 1);
    }
}
