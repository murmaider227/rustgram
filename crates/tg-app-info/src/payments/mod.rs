//! Payments, Telegram Stars, Star Gifts, Telegram Premium, Boosts/Giveaways, and Telegram
//! Passport UI screens rendered on top of `tg-payments` and `tg-passport`.
//!
//! Port origin:
//! - `payments/ui/payments_form_summary.cpp`, `payments_edit_information.cpp`,
//!   `payments_edit_card.cpp`, `payments_panel.cpp` ([`checkout`])
//! - `settings/sections/settings_credits.cpp`, `settings/settings_credits_graphics.cpp`,
//!   `payments/ui/payments_reaction_box.cpp` ([`credits`])
//! - `boxes/star_gift_box.cpp`, `boxes/transfer_gift_box.cpp` ([`star_gifts`])
//! - `settings/sections/settings_premium.cpp`, `ui/boxes/boost_box.cpp` ([`premium`])
//! - `passport/passport_panel*.cpp` ([`passport`])
//! - Decoupled [`backend::PaymentsBackend`] & [`backend::PassportBackend`] traits with
//!   offline fakes in [`mock`]

pub use tg_app_core::payments::backend;
pub mod checkout;
pub mod credits;
pub mod gift_box;
pub mod gift_card;
pub mod giveaways_ui;
pub use tg_app_core::payments::marketplace_backend;
pub mod mock;
pub mod mock_marketplace;
pub mod passport;
pub mod premium;
pub mod premium_section;
pub mod star_gifts;
pub mod star_gifts_marketplace;
pub mod starref_ui;
pub mod ui_helpers;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Margin, Modifiers,
    Order, Pos2, Rect, ScrollArea, Sense, Stroke, Ui, Vec2,
};

use tg_data::UserId as DataUserId;
use tg_passport::types::FormRequest;

use crate::payments::backend::{PassportBackend, PaymentsBackend};
use crate::payments::checkout::{
    CheckoutSubView, CheckoutView, PaymentVerificationHost, TgWebViewVerificationHost,
};
use crate::payments::credits::StarsView;
use crate::payments::mock::{MockPassportBackend, MockPaymentsBackend};
use crate::payments::passport::PassportView;
use crate::payments::premium::{BoostsGiveawaysView, PremiumView};
use crate::payments::star_gifts::StarGiftsView;
use crate::payments::starref_ui::StarRefView;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Environment variable that re-enables the developer Payments overlay (screen tabs and the
/// `Ctrl+Shift+P` toggle). The normal UI never shows it: Telegram Desktop has no such panel.
pub const DEV_OVERLAY_ENV: &str = "TGFORK_PAYMENTS_DEV_OVERLAY";

/// Returns `true` when the developer Payments overlay is enabled via [`DEV_OVERLAY_ENV`]`=1`.
#[must_use]
pub fn dev_overlay_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(DEV_OVERLAY_ENV).is_ok_and(|v| v.trim() == "1"))
}

/// Parsed parameters from a `tg://passport?...` deep link (`passport_form_controller.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassportDeepLink {
    /// Requesting bot user ID (`bot_id`), if supplied.
    pub bot_id: Option<i64>,
    /// Optional requesting bot username (`bot` / `domain`), without `@`.
    pub bot_username: String,
    /// Requested Passport scope JSON or legacy scope string (`scope`).
    pub scope: String,
    /// Bot's RSA public key PEM (`public_key`).
    pub public_key: String,
    /// Nonce or payload string (`nonce` or `payload`).
    pub nonce: String,
    /// Optional callback redirect URL (`callback_url`).
    pub callback_url: String,
}

/// Parses an invoice slug from `tg://invoice?slug=...`, `https://t.me/$<slug>`,
/// `t.me/$<slug>`, or `https://t.me/invoice/<slug>`.
#[must_use]
pub fn parse_invoice_deep_link(uri: &str) -> Option<String> {
    let trimmed = uri.trim();
    if let Some(rest) = trimmed.strip_prefix("tg://invoice") {
        let query = rest.strip_prefix('?')?;
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=')
                && k == "slug"
            {
                let decoded = tg_text::url::url_decode(v);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
        return None;
    }
    let tme = trimmed
        .strip_prefix("https://t.me/")
        .or_else(|| trimmed.strip_prefix("http://t.me/"))
        .or_else(|| trimmed.strip_prefix("t.me/"))?;
    let path = tme.split(['?', '#']).next().unwrap_or("");
    if let Some(slug) = path.strip_prefix('$')
        && !slug.is_empty()
    {
        return Some(slug.to_owned());
    }
    if let Some(slug) = path.strip_prefix("invoice/")
        && !slug.is_empty()
    {
        return Some(slug.to_owned());
    }
    None
}

/// Parses a `tg://passport?...` deep link into a [`PassportDeepLink`].
#[must_use]
pub fn parse_passport_deep_link(uri: &str) -> Option<PassportDeepLink> {
    let trimmed = uri.trim();
    let rest = trimmed
        .strip_prefix("tg://passport")
        .or_else(|| trimmed.strip_prefix("tg:passport"))?;
    let query = rest.strip_prefix('?')?;

    let mut bot_id = None;
    let mut bot_username = String::new();
    let mut scope = String::new();
    let mut public_key = String::new();
    let mut nonce = String::new();
    let mut payload = String::new();
    let mut callback_url = String::new();

    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let val = tg_text::url::url_decode(v);
        match k {
            "bot_id" => bot_id = val.parse::<i64>().ok(),
            "bot" | "domain" => bot_username = val.trim_start_matches('@').to_owned(),
            "scope" => scope = val,
            "public_key" => public_key = val,
            "nonce" => nonce = val,
            "payload" => payload = val,
            "callback_url" => callback_url = val,
            _ => {}
        }
    }

    let effective_nonce = if !nonce.is_empty() { nonce } else { payload };
    if scope.is_empty() || (bot_id.is_none() && bot_username.is_empty()) {
        return None;
    }
    Some(PassportDeepLink {
        bot_id,
        bot_username,
        scope,
        public_key,
        nonce: effective_nonce,
        callback_url,
    })
}

pub use tg_app_core::core_types::window_types::PaymentsScreen;

/// Factory producing a [`PaymentVerificationHost`] whenever a [`CheckoutView`] is created.
pub type VerificationHostFactory = Box<dyn FnMut() -> Box<dyn PaymentVerificationHost> + Send>;

/// Multi-screen Payments & Passport controller with lazy loading and toast feedback.
pub struct PaymentsScreenHost {
    payments_backend: Box<dyn PaymentsBackend>,
    passport_backend: Box<dyn PassportBackend>,
    verification_host_factory: Option<VerificationHostFactory>,
    started_at: Instant,
    active_screen: PaymentsScreen,
    invoice_target: Option<String>,
    passport_target: Option<String>,
    passport_start_unlocked: bool,
    checkout_view: Option<CheckoutView>,
    stars_view: Option<StarsView>,
    gifts_view: Option<StarGiftsView>,
    premium_view: Option<PremiumView>,
    boosts_view: Option<BoostsGiveawaysView>,
    starref_view: Option<StarRefView>,
    passport_view: Option<PassportView>,
    load_error: Option<String>,
    toast_message: Option<(String, i64)>,
    dev_tabs: bool,
}

impl PaymentsScreenHost {
    /// Creates a new [`PaymentsScreenHost`] backed by the provided trait implementations.
    ///
    /// Installs [`TgWebViewVerificationHost`] as the default 3-D Secure verification host; tests
    /// may override it via [`Self::set_verification_host_factory`] to inject a fake host.
    ///
    /// Does not load any hard-coded sample invoice or Passport bot; open real invoices via
    /// [`Self::open_invoice`], [`Self::open_invoice_message`], or [`Self::open_invoice_deep_link`],
    /// and open Telegram Passport via [`Self::open_passport`] or [`Self::open_passport_deep_link`].
    #[must_use]
    pub fn new(
        payments_backend: Box<dyn PaymentsBackend>,
        passport_backend: Box<dyn PassportBackend>,
    ) -> Self {
        let mut host = Self {
            payments_backend,
            passport_backend,
            verification_host_factory: Some(Box::new(
                || Box::new(TgWebViewVerificationHost::new()),
            )),
            started_at: Instant::now(),
            active_screen: PaymentsScreen::Stars,
            invoice_target: None,
            passport_target: None,
            passport_start_unlocked: false,
            checkout_view: None,
            stars_view: None,
            gifts_view: None,
            premium_view: None,
            boosts_view: None,
            starref_view: None,
            passport_view: None,
            load_error: None,
            toast_message: None,
            dev_tabs: dev_overlay_enabled(),
        };
        host.ensure_loaded(PaymentsScreen::Stars);
        host
    }

    /// Overrides the factory used to construct [`PaymentVerificationHost`] instances for checkout
    /// 3-D Secure verification (tests inject [`crate::payments::checkout::FakePaymentVerificationHost`]).
    pub fn set_verification_host_factory(&mut self, factory: Option<VerificationHostFactory>) {
        self.verification_host_factory = factory;
        if let Some(ref mut cv) = self.checkout_view
            && let Some(ref mut f) = self.verification_host_factory
        {
            cv.set_verification_host(f());
        }
    }

    fn build_checkout_view(
        &mut self,
        bundle: crate::payments::backend::CheckoutBundle,
    ) -> CheckoutView {
        let mut view = CheckoutView::new(bundle);
        if let Some(ref mut factory) = self.verification_host_factory {
            view.set_verification_host(factory());
        }
        view
    }

    /// Creates a host pre-wired to [`MockPaymentsBackend`] and [`MockPassportBackend`]
    /// for offline demo overlays and UI screenshot tests.
    #[must_use]
    pub fn with_mock() -> Self {
        let mut host = Self::new(
            Box::new(MockPaymentsBackend::new()),
            Box::new(MockPassportBackend::new()),
        );
        host.verification_host_factory = None;
        host.invoice_target = Some(crate::payments::mock::SAMPLE_INVOICE_ID.to_owned());
        host.passport_target = Some(crate::payments::mock::SAMPLE_PASSPORT_BOT.to_owned());
        host.passport_start_unlocked = true;
        host.select_screen(PaymentsScreen::Checkout);
        host
    }

    /// Opens a checkout form from an explicit invoice identifier (`"msg:<peer>:<msg>"`,
    /// `"receipt:<peer>:<msg>"`, or invoice slug).
    pub fn open_invoice(&mut self, invoice_id: &str) -> Result<(), String> {
        let trimmed = invoice_id.trim();
        if trimmed.is_empty() {
            return Err("Invoice identifier cannot be empty.".to_owned());
        }
        self.invoice_target = Some(trimmed.to_owned());
        self.checkout_view = None;
        self.active_screen = PaymentsScreen::Checkout;
        self.load_error = None;
        match self.payments_backend.load_checkout_form(trimmed) {
            Ok(bundle) => {
                self.checkout_view = Some(self.build_checkout_view(bundle));
                Ok(())
            }
            Err(e) => {
                self.load_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Opens checkout for a real invoice message `(peer_id, msg_id)`.
    pub fn open_invoice_message(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        if peer_id.is_zero() || msg_id.0 <= 0 {
            return Err("Invalid invoice message reference.".to_owned());
        }
        self.open_invoice(&format!("msg:{}:{}", peer_id.0, msg_id.0))
    }

    /// Opens checkout from a `tg://invoice?slug=...` or `t.me/$<slug>` deep link.
    pub fn open_invoice_deep_link(&mut self, uri: &str) -> Result<(), String> {
        let slug = parse_invoice_deep_link(uri)
            .ok_or_else(|| format!("Not a recognized Telegram invoice deep link: '{uri}'"))?;
        self.open_invoice(&slug)
    }

    /// Opens a Telegram Passport authorization form for `bot_username`.
    pub fn open_passport(&mut self, bot_username: &str) -> Result<(), String> {
        let cleaned = bot_username.trim().trim_start_matches('@');
        if cleaned.is_empty() {
            return Err("Passport bot username cannot be empty.".to_owned());
        }
        self.passport_target = Some(cleaned.to_owned());
        self.passport_view = None;
        self.active_screen = PaymentsScreen::Passport;
        self.load_error = None;
        match self.passport_backend.load_authorization_form(cleaned) {
            Ok(bundle) => {
                self.passport_view = Some(PassportView::new(bundle, self.passport_start_unlocked));
                Ok(())
            }
            Err(e) => {
                self.load_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Opens Telegram Passport from a `tg://passport?...` deep link, configuring the
    /// underlying [`PassportBackend`] with the parsed [`FormRequest`].
    pub fn open_passport_deep_link(&mut self, uri: &str) -> Result<PassportDeepLink, String> {
        let parsed = parse_passport_deep_link(uri)
            .ok_or_else(|| format!("Invalid tg://passport deep link: '{uri}'"))?;
        if let Some(bot_id) = parsed.bot_id {
            self.passport_backend.configure_form_request(FormRequest {
                bot_id: DataUserId::from_tl(bot_id),
                scope: parsed.scope.clone(),
                callback_url: parsed.callback_url.clone(),
                public_key: parsed.public_key.clone(),
                nonce: parsed.nonce.clone(),
            });
        }
        let target = if !parsed.bot_username.is_empty() {
            parsed.bot_username.clone()
        } else {
            format!("bot_{}", parsed.bot_id.unwrap_or_default())
        };
        self.passport_target = Some(target.clone());
        self.passport_view = None;
        self.active_screen = PaymentsScreen::Passport;
        self.load_error = None;
        match self.passport_backend.load_authorization_form(&target) {
            Ok(bundle) => {
                self.passport_view = Some(PassportView::new(bundle, self.passport_start_unlocked));
                Ok(parsed)
            }
            Err(e) => {
                self.load_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Returns the currently selected screen.
    #[must_use]
    pub const fn active_screen(&self) -> PaymentsScreen {
        self.active_screen
    }

    /// Returns the currently targeted invoice identifier, if an invoice has been opened.
    #[must_use]
    pub fn active_invoice_target(&self) -> Option<&str> {
        self.invoice_target.as_deref()
    }

    /// Returns the currently targeted Passport bot identifier, if a request has been opened.
    #[must_use]
    pub fn active_passport_target(&self) -> Option<&str> {
        self.passport_target.as_deref()
    }

    /// Switches to `screen`, lazily loading its data from the backend.
    pub fn select_screen(&mut self, screen: PaymentsScreen) {
        self.active_screen = screen;
        self.ensure_loaded(screen);
    }

    /// Shows or hides the developer screen tabs (off unless [`dev_overlay_enabled`]).
    ///
    /// Without them, Premium renders as tdesktop's Premium section and Star Gifts as the
    /// "Send a Gift" box.
    pub fn set_dev_tabs(&mut self, enabled: bool) {
        self.dev_tabs = enabled;
    }

    /// Returns whether the developer screen tabs are shown.
    #[must_use]
    pub const fn dev_tabs(&self) -> bool {
        self.dev_tabs
    }

    /// Panel width the active screen wants when shown as a layer: `boxWideWidth` for the gift
    /// box and `infoDesiredWidth` for the Premium section; `None` for the generic panel.
    #[must_use]
    pub const fn panel_width_hint(&self) -> Option<f32> {
        if self.dev_tabs {
            return None;
        }
        match self.active_screen {
            PaymentsScreen::StarGifts => Some(crate::payments::gift_card::BOX_WIDE_WIDTH),
            PaymentsScreen::Premium => Some(crate::payments::premium_section::PREMIUM_LAYER_WIDTH),
            _ => None,
        }
    }

    fn uses_parity_screen(&self) -> bool {
        !self.dev_tabs
            && self.load_error.is_none()
            && match self.active_screen {
                PaymentsScreen::StarGifts => self.gifts_view.is_some(),
                PaymentsScreen::Premium => self.premium_view.is_some(),
                _ => false,
            }
    }

    /// Renders the Premium section or the "Send a Gift" box filling `ui`; returns close.
    fn show_parity_screen(&mut self, ui: &mut Ui, palette: &Palette, now_ms: i64) -> bool {
        let rect = ui.max_rect();
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let mut closed = false;
        let mut pending_toast = None;
        match self.active_screen {
            PaymentsScreen::StarGifts => {
                if let Some(ref mut view) = self.gifts_view {
                    let (toast, back) =
                        view.show_gift_box(ui, self.payments_backend.as_mut(), palette, now_unix);
                    pending_toast = toast;
                    closed = back;
                }
            }
            PaymentsScreen::Premium => {
                if let Some(ref mut view) = self.premium_view {
                    let outcome = view.show_section(ui, self.payments_backend.as_ref(), palette);
                    pending_toast = outcome.toast;
                    closed = outcome.close;
                    // Subscribe opens the selected option's invoice (`botUrl`) in the payment
                    // form, like `CreateSubscribeButton` -> `UrlClickHandler::Open`.
                    if let Some(url) = outcome.open_invoice
                        && let Err(err) = self.open_invoice_deep_link(&url)
                    {
                        pending_toast = Some(err);
                    }
                }
            }
            _ => {}
        }
        if let Some(msg) = pending_toast {
            self.show_toast(msg, now_ms);
        }
        self.paint_toast(ui, rect, now_ms);
        closed
    }

    fn paint_toast(&mut self, ui: &Ui, rect: Rect, now_ms: i64) {
        if let Some((ref text, expires_at)) = self.toast_message {
            if now_ms >= expires_at {
                self.toast_message = None;
            } else {
                let toast_w = (rect.width() - 40.0).min(460.0);
                let toast_rect = Rect::from_min_size(
                    Pos2::new(rect.center().x - toast_w / 2.0, rect.bottom() - 48.0),
                    Vec2::new(toast_w, 34.0),
                );
                ui.painter().rect_filled(
                    toast_rect,
                    CornerRadius::same(8),
                    Color32::from_rgba_unmultiplied(0x1E, 0x24, 0x2B, 235),
                );
                ui.painter().text(
                    toast_rect.center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(11.5),
                    Color32::WHITE,
                );
                ui.ctx().request_repaint();
            }
        }
    }

    /// Returns the Stars Affiliate view, if it was opened.
    #[must_use]
    pub const fn starref_view(&self) -> Option<&StarRefView> {
        self.starref_view.as_ref()
    }

    /// Returns mutable access to the loaded [`CheckoutView`], if any.
    #[must_use]
    pub fn checkout_view_mut(&mut self) -> Option<&mut CheckoutView> {
        self.checkout_view.as_mut()
    }

    /// Returns mutable access to the loaded [`StarsView`], if any.
    #[must_use]
    pub fn stars_view_mut(&mut self) -> Option<&mut StarsView> {
        self.stars_view.as_mut()
    }

    /// Returns mutable access to the loaded [`StarGiftsView`], if any.
    #[must_use]
    pub fn gifts_view_mut(&mut self) -> Option<&mut StarGiftsView> {
        self.gifts_view.as_mut()
    }

    /// Returns mutable access to the loaded [`PremiumView`], if any.
    #[must_use]
    pub fn premium_view_mut(&mut self) -> Option<&mut PremiumView> {
        self.premium_view.as_mut()
    }

    /// Returns mutable access to the loaded [`BoostsGiveawaysView`], if any.
    #[must_use]
    pub fn boosts_view_mut(&mut self) -> Option<&mut BoostsGiveawaysView> {
        self.boosts_view.as_mut()
    }

    /// Returns mutable access to the loaded [`PassportView`], if any.
    #[must_use]
    pub fn passport_view_mut(&mut self) -> Option<&mut PassportView> {
        self.passport_view.as_mut()
    }

    /// Opens the native card entry form inside the Checkout screen.
    pub fn open_card_form(&mut self) {
        self.select_screen(PaymentsScreen::Checkout);
        if let Some(ref mut v) = self.checkout_view {
            v.sub_view = CheckoutSubView::EditCard;
        }
    }

    /// Looks up a unique collectible Star Gift by `slug` (`payments.getUniqueStarGift`) and
    /// opens its detail modal inside the Star Gifts screen.
    pub fn open_unique_gift_slug(&mut self, slug: &str) -> Result<(), String> {
        let cleaned = slug
            .trim()
            .trim_start_matches("https://t.me/nft/")
            .trim_start_matches("http://t.me/nft/")
            .trim_start_matches("t.me/nft/")
            .trim_start_matches("tg://nft?slug=");
        if cleaned.is_empty() {
            return Err("Unique gift slug cannot be empty.".to_owned());
        }
        self.select_screen(PaymentsScreen::StarGifts);
        // A `t.me/nft/<slug>` link must open even when the gift catalog cannot load.
        let gv = self.gifts_view.get_or_insert_with(|| {
            StarGiftsView::new(crate::payments::backend::StarGiftsBundle {
                balance: tg_payments::credits::CreditsAmount::ZERO_STARS,
                default_recipient: String::new(),
                recipient_peer_id: 0,
                recipient_is_self: true,
                catalog: Vec::new(),
                received: Vec::new(),
            })
        });
        gv.slug_lookup_input = cleaned.to_owned();
        let result = if let Some(mp) = self.payments_backend.as_marketplace_backend() {
            mp.get_unique_star_gift(cleaned)
        } else {
            Err("Star Gifts marketplace lookup is unavailable in this backend.".to_owned())
        };
        match result {
            Ok(detail) => {
                self.load_error = None;
                if let Some(ref mut gv) = self.gifts_view {
                    gv.modal =
                        Some(crate::payments::star_gifts::StarGiftModal::UniqueSlugDetail(detail));
                }
                Ok(())
            }
            Err(err) => {
                self.load_error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Opens the Telegram Stars screen with the Paid Media unlock confirmation box pre-populated
    /// for `(peer_id, msg_id)`.
    pub fn open_paid_media_unlock(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        stars_amount: i64,
        title: Option<&str>,
    ) {
        self.select_screen(PaymentsScreen::Stars);
        if let Some(ref mut sv) = self.stars_view {
            let synthetic_id = ((peer_id.0 as u64).wrapping_mul(31))
                .wrapping_add(msg_id.0.max(0) as u64)
                .max(1);
            sv.sample_paid_media.media_id = synthetic_id;
            sv.sample_paid_media.cost.stars_amount = stars_amount.max(1);
            if let Some(t) = title
                && !t.trim().is_empty()
            {
                sv.sample_paid_media.caption = t.trim().to_owned();
            }
            sv.open_paid_media_box();
        }
    }

    /// Dispatches a `DeepLinkOverlayRequest::OpenPayments { screen, slug }` payload into the
    /// appropriate screen and deep-link handler.
    pub fn open_deep_link_request(
        &mut self,
        screen: PaymentsScreen,
        slug: Option<&str>,
    ) -> Result<(), String> {
        let Some(raw) = slug.map(str::trim).filter(|s| !s.is_empty()) else {
            self.select_screen(screen);
            return Ok(());
        };
        match screen {
            PaymentsScreen::Checkout => {
                if raw.starts_with("tg://")
                    || raw.starts_with("https://")
                    || raw.starts_with("http://")
                    || raw.starts_with("t.me/")
                {
                    self.open_invoice_deep_link(raw)
                } else if raw.starts_with("msg:") || raw.starts_with("receipt:") {
                    self.open_invoice(raw)
                } else {
                    self.open_invoice_deep_link(&format!("tg://invoice?slug={raw}"))
                }
            }
            PaymentsScreen::Passport => {
                if raw.starts_with("tg://") || raw.starts_with("tg:") {
                    self.open_passport_deep_link(raw).map(|_| ())
                } else if raw.contains('=') {
                    self.open_passport_deep_link(&format!("tg://passport?{raw}"))
                        .map(|_| ())
                } else {
                    self.open_passport(raw)
                }
            }
            PaymentsScreen::StarGifts => self.open_unique_gift_slug(raw),
            PaymentsScreen::Affiliate => {
                self.starref_view = Some(StarRefView::for_peer(raw));
                self.select_screen(PaymentsScreen::Affiliate);
                Ok(())
            }
            PaymentsScreen::Stars => {
                if let Some(rest) = raw.strip_prefix("paid_media:") {
                    let mut parts = rest.splitn(4, ':');
                    let peer_raw = parts
                        .next()
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0);
                    let msg_raw = parts
                        .next()
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(0);
                    let stars = parts
                        .next()
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(1);
                    let title = parts.next();
                    self.open_paid_media_unlock(PeerId(peer_raw), MsgId(msg_raw), stars, title);
                } else {
                    self.select_screen(PaymentsScreen::Stars);
                }
                Ok(())
            }
            other => {
                self.select_screen(other);
                Ok(())
            }
        }
    }

    fn ensure_loaded(&mut self, screen: PaymentsScreen) {
        self.load_error = None;
        match screen {
            PaymentsScreen::Checkout => {
                if self.checkout_view.is_none()
                    && let Some(invoice_id) = self.invoice_target.clone()
                {
                    match self.payments_backend.load_checkout_form(&invoice_id) {
                        Ok(bundle) => self.checkout_view = Some(self.build_checkout_view(bundle)),
                        Err(e) => self.load_error = Some(e),
                    }
                }
            }
            PaymentsScreen::Stars => {
                if self.stars_view.is_none() {
                    match self.payments_backend.load_credits_overview() {
                        Ok(bundle) => self.stars_view = Some(StarsView::new(bundle)),
                        Err(e) => self.load_error = Some(e),
                    }
                }
            }
            PaymentsScreen::StarGifts => {
                if self.gifts_view.is_none() {
                    match self.payments_backend.load_star_gifts() {
                        Ok(bundle) => self.gifts_view = Some(StarGiftsView::new(bundle)),
                        Err(e) => self.load_error = Some(e),
                    }
                }
            }
            PaymentsScreen::Premium => {
                if self.premium_view.is_none() {
                    match self.payments_backend.load_premium_promo() {
                        Ok(bundle) => self.premium_view = Some(PremiumView::new(bundle)),
                        Err(e) => self.load_error = Some(e),
                    }
                }
            }
            PaymentsScreen::BoostsGiveaways => {
                if self.boosts_view.is_none() {
                    match self.payments_backend.load_boosts_giveaway() {
                        Ok(bundle) => self.boosts_view = Some(BoostsGiveawaysView::new(bundle)),
                        Err(e) => self.load_error = Some(e),
                    }
                }
            }
            PaymentsScreen::Affiliate => {
                if self.starref_view.is_none() {
                    let mut view = StarRefView::default();
                    if let Some(mp) = self.payments_backend.as_marketplace_backend()
                        && let Err(e) = view.reload(mp)
                    {
                        self.load_error = Some(e);
                    }
                    self.starref_view = Some(view);
                }
            }
            PaymentsScreen::Passport => {
                if self.passport_view.is_none()
                    && let Some(ref bot) = self.passport_target
                {
                    match self.passport_backend.load_authorization_form(bot) {
                        Ok(bundle) => {
                            self.passport_view =
                                Some(PassportView::new(bundle, self.passport_start_unlocked));
                        }
                        Err(e) => self.load_error = Some(e),
                    }
                }
            }
        }
    }

    /// Displays a transient toast notification for 3 seconds.
    pub fn show_toast(&mut self, msg: String, now_ms: i64) {
        let Some(msg) = tg_app_core::user_error::visible_toast_text(msg) else {
            return;
        };
        self.toast_message = Some((msg, now_ms + 3_000));
    }

    /// Renders the Payments & Passport panel using wall-clock elapsed time.
    pub fn show(&mut self, ui: &mut Ui, palette: &Palette, show_close: bool) -> bool {
        let now_ms = self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
        self.show_at(ui, palette, show_close, now_ms)
    }

    /// Renders the panel at an explicit timestamp `now_ms` (deterministic for tests & screenshots).
    pub fn show_at(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        show_close: bool,
        now_ms: i64,
    ) -> bool {
        let mut closed = false;
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, palette.window_bg);
        if self.uses_parity_screen() {
            return self.show_parity_screen(ui, palette, now_ms);
        }

        // Top navigation bar (48px)
        let top_h = 48.0;
        let top_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), top_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(top_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.label(
                            egui::RichText::new(self.active_screen.title())
                                .size(15.5)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        if show_close {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("X")
                                                    .size(14.0)
                                                    .strong()
                                                    .color(palette.window_sub_text_fg),
                                            )
                                            .frame(false),
                                        )
                                        .clicked()
                                    {
                                        closed = true;
                                    }
                                },
                            );
                        }
                    });
                });
            ui.painter().line_segment(
                [
                    Pos2::new(top_rect.left(), top_rect.bottom()),
                    Pos2::new(top_rect.right(), top_rect.bottom()),
                ],
                Stroke::new(1.0, palette.divider),
            );
        });

        // Developer section tab strip (36px, only with `TGFORK_PAYMENTS_DEV_OVERLAY=1`).
        let tab_h = if self.dev_tabs { 36.0 } else { 0.0 };
        let tab_rect = Rect::from_min_size(
            Pos2::new(rect.left(), top_rect.bottom()),
            Vec2::new(rect.width(), tab_h),
        );
        if self.dev_tabs {
            ui.scope_builder(egui::UiBuilder::new().max_rect(tab_rect), |ui| {
                let narrow = rect.width() < 440.0;
                Frame::NONE
                    .inner_margin(Margin::symmetric(if narrow { 8 } else { 12 }, 0))
                    .show(ui, |ui| {
                        ScrollArea::horizontal()
                            .id_salt("tg_payments_tabs_scroll")
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show(ui, |ui| {
                                ui.horizontal_centered(|ui| {
                                    ui.spacing_mut().item_spacing.x =
                                        if narrow { 6.0 } else { 12.0 };
                                    let tabs = [
                                        (PaymentsScreen::Checkout, "Checkout"),
                                        (PaymentsScreen::Stars, "Stars"),
                                        (PaymentsScreen::StarGifts, "Gifts"),
                                        (PaymentsScreen::Premium, "Premium"),
                                        (PaymentsScreen::BoostsGiveaways, "Boosts"),
                                        (PaymentsScreen::Affiliate, "Affiliate"),
                                        (PaymentsScreen::Passport, "Passport"),
                                    ];
                                    for (screen, label) in tabs {
                                        let selected = self.active_screen == screen;
                                        let col = if selected {
                                            palette.window_active_text_fg
                                        } else {
                                            palette.window_sub_text_fg
                                        };
                                        let font_size = if narrow { 11.5 } else { 12.5 };
                                        let galley = ui.fonts_mut(|f| {
                                            f.layout_no_wrap(
                                                label.to_owned(),
                                                FontId::proportional(font_size),
                                                col,
                                            )
                                        });
                                        let (r, resp) = ui.allocate_exact_size(
                                            Vec2::new(galley.size().x + 6.0, tab_h),
                                            Sense::click(),
                                        );
                                        resp.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::Role::Tab,
                                                true,
                                                label.to_owned(),
                                            )
                                        });
                                        if resp.hovered() {
                                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                        }
                                        if resp.clicked() {
                                            self.select_screen(screen);
                                        }
                                        ui.painter().galley(
                                            Pos2::new(
                                                r.left() + 3.0,
                                                r.center().y - galley.size().y / 2.0,
                                            ),
                                            galley,
                                            col,
                                        );
                                        if selected {
                                            ui.painter().line_segment(
                                                [
                                                    Pos2::new(r.left(), r.bottom() - 2.0),
                                                    Pos2::new(r.right(), r.bottom() - 2.0),
                                                ],
                                                Stroke::new(2.5, palette.window_active_text_fg),
                                            );
                                        }
                                    }
                                });
                            });
                    });
                ui.painter().line_segment(
                    [
                        Pos2::new(tab_rect.left(), tab_rect.bottom()),
                        Pos2::new(tab_rect.right(), tab_rect.bottom()),
                    ],
                    Stroke::new(1.0, palette.divider),
                );
            });
        }

        // Scrollable screen body
        let body_rect = Rect::from_min_max(Pos2::new(rect.left(), tab_rect.bottom()), rect.max);
        let mut pending_toast: Option<String> = None;

        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            if let Some(ref err) = self.load_error {
                let text = tg_app_core::user_error::load_payment_screen(err).into_text().unwrap_or_default();
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new(text)
                            .size(13.0)
                            .color(palette.attention_fg),
                    );
                });
                return;
            }

            ScrollArea::vertical()
                .id_salt(("tg_payments_scroll", format!("{:?}", self.active_screen)))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    match self.active_screen {
                        PaymentsScreen::Checkout => {
                            if let Some(ref mut view) = self.checkout_view {
                                pending_toast =
                                    view.show(ui, self.payments_backend.as_mut(), palette);
                            } else {
                                Frame::NONE.inner_margin(Margin::same(24)).show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("No active invoice selected.")
                                            .size(14.5)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    );
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new(
                                            "Open an invoice message in chat or a tg://invoice?slug=... / t.me/$... link to begin checkout.",
                                        )
                                        .size(12.5)
                                        .color(palette.window_sub_text_fg),
                                    );
                                });
                            }
                        }
                        PaymentsScreen::Stars => {
                            if let Some(ref mut view) = self.stars_view {
                                pending_toast =
                                    view.show(ui, self.payments_backend.as_mut(), palette);
                            }
                        }
                        PaymentsScreen::StarGifts => {
                            if let Some(ref mut view) = self.gifts_view {
                                pending_toast =
                                    view.show(ui, self.payments_backend.as_mut(), palette);
                            }
                        }
                        PaymentsScreen::Premium => {
                            if let Some(ref mut view) = self.premium_view {
                                pending_toast = view.show_with_backend(
                                    ui,
                                    self.payments_backend.as_ref(),
                                    palette,
                                );
                            }
                        }
                        PaymentsScreen::BoostsGiveaways => {
                            if let Some(ref mut view) = self.boosts_view {
                                pending_toast = view.show_with_payments_backend(
                                    ui,
                                    self.payments_backend.as_mut(),
                                    1_763_760_000,
                                    palette,
                                );
                            }
                        }
                        PaymentsScreen::Affiliate => {
                            if let Some(ref mut view) = self.starref_view
                                && let Some(mp) = self.payments_backend.as_marketplace_backend()
                            {
                                pending_toast = view.show(ui, mp, palette);
                            }
                        }
                        PaymentsScreen::Passport => {
                            if let Some(ref mut view) = self.passport_view {
                                pending_toast =
                                    view.show(ui, self.passport_backend.as_mut(), palette);
                            } else {
                                Frame::NONE.inner_margin(Margin::same(24)).show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(
                                            "No Telegram Passport authorization request open.",
                                        )
                                        .size(14.5)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                    );
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new(
                                            "Open a tg://passport?... deep link from a requesting service to authorize Telegram Passport.",
                                        )
                                        .size(12.5)
                                        .color(palette.window_sub_text_fg),
                                    );
                                });
                            }
                        }
                    }
                    ui.add_space(16.0);
                });
        });

        if let Some(msg) = pending_toast {
            self.show_toast(msg, now_ms);
        }
        self.paint_toast(ui, rect, now_ms);

        closed
    }
}

/// Self-contained overlay host for `app.rs` (single-line hook in `TgforkApp::ui`).
///
/// Press `Ctrl+Shift+P` in the running desktop app to open or close the Payments, Stars,
/// Star Gifts, Premium, Boosts, and Telegram Passport panel over the workspace.
pub struct AppPaymentsHost {
    open: bool,
    screen_host: PaymentsScreenHost,
}

impl AppPaymentsHost {
    fn get_or_init_state(ctx: &Context) -> Arc<Mutex<Self>> {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with(Id::unique("tg_app_payments_host"), || {
                Arc::new(Mutex::new(Self {
                    open: false,
                    screen_host: PaymentsScreenHost::with_mock(),
                }))
            })
            .clone()
        })
    }

    /// Opens the floating Payments overlay directly on `screen` (e.g. `PaymentsScreen::Premium`).
    pub fn open_screen(ctx: &Context, screen: PaymentsScreen) {
        let state_arc = Self::get_or_init_state(ctx);
        if let Ok(mut guard) = state_arc.lock() {
            guard.screen_host.select_screen(screen);
            guard.open = true;
        }
    }

    /// Opens the floating Payments overlay and routes `screen` / optional `slug` through
    /// [`PaymentsScreenHost::open_deep_link_request`].
    pub fn dispatch_overlay_request(
        ctx: &Context,
        screen: PaymentsScreen,
        slug: Option<&str>,
    ) -> Result<(), String> {
        let state_arc = Self::get_or_init_state(ctx);
        let Ok(mut guard) = state_arc.lock() else {
            return Err("Payments host lock unavailable.".to_owned());
        };
        guard.open = true;
        guard.screen_host.open_deep_link_request(screen, slug)
    }

    /// Returns `true` when the floating demo Payments overlay is currently open.
    #[must_use]
    pub fn is_open(ctx: &Context) -> bool {
        let state_arc = Self::get_or_init_state(ctx);
        state_arc.lock().is_ok_and(|g| g.open)
    }

    /// Closes the floating demo Payments overlay if open.
    pub fn close(ctx: &Context) {
        let state_arc = Self::get_or_init_state(ctx);
        if let Ok(mut guard) = state_arc.lock() {
            guard.open = false;
        }
    }

    /// Returns the currently selected [`PaymentsScreen`] in the demo Payments overlay.
    #[must_use]
    pub fn active_screen(ctx: &Context) -> Option<PaymentsScreen> {
        let state_arc = Self::get_or_init_state(ctx);
        state_arc.lock().ok().map(|g| g.screen_host.active_screen())
    }

    /// Returns the currently targeted invoice identifier in the demo Payments overlay, if any.
    #[must_use]
    pub fn active_invoice_target(ctx: &Context) -> Option<String> {
        let state_arc = Self::get_or_init_state(ctx);
        state_arc
            .lock()
            .ok()
            .and_then(|g| g.screen_host.active_invoice_target().map(str::to_owned))
    }

    /// Returns `true` when the Paid Media unlock confirmation box is currently open.
    #[must_use]
    pub fn has_paid_media_modal_open(ctx: &Context) -> bool {
        let state_arc = Self::get_or_init_state(ctx);
        state_arc.lock().is_ok_and(|g| {
            g.screen_host.stars_view.as_ref().is_some_and(|sv| {
                matches!(
                    sv.modal,
                    Some(crate::payments::credits::StarsModal::PaidMediaConfirm)
                )
            })
        })
    }

    /// Returns `true` when a Unique Star Gift detail modal is currently open.
    #[must_use]
    pub fn has_unique_gift_modal_open(ctx: &Context) -> bool {
        let state_arc = Self::get_or_init_state(ctx);
        state_arc.lock().is_ok_and(|g| {
            g.screen_host.gifts_view.as_ref().is_some_and(|gv| {
                matches!(
                    gv.modal,
                    Some(crate::payments::star_gifts::StarGiftModal::UniqueSlugDetail(_))
                )
            })
        })
    }

    /// Drives keyboard toggle (`Ctrl+Shift+P`) and renders the floating panel when open.
    pub fn update_and_render(ctx: &Context, palette: &Palette, _theme_mode: ThemeMode) {
        let toggle_pressed = dev_overlay_enabled()
            && ctx.input(|i| {
                i.modifiers
                    .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT)
                    && i.key_pressed(Key::P)
            });

        let state_arc = Self::get_or_init_state(ctx);

        let Ok(mut guard) = state_arc.lock() else {
            return;
        };
        if toggle_pressed {
            guard.open = !guard.open;
        }
        if !guard.open {
            return;
        }

        let screen_rect = ctx.content_rect();
        let panel_w = guard.screen_host.panel_width_hint().map_or_else(
            || (screen_rect.width() * 0.62).clamp(440.0, 620.0),
            |w| w.min(screen_rect.width() - 16.0),
        );
        let panel_h = (screen_rect.height() - 48.0).max(420.0);
        let panel_pos = Pos2::new(
            screen_rect.center().x - panel_w / 2.0,
            screen_rect.top() + 24.0,
        );

        let mut close_requested = ctx.input(|i| i.key_pressed(Key::Escape));
        egui::Area::new(Id::unique("tg_payments_overlay_backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                let (_, resp) = ui.allocate_exact_size(screen_rect.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen_rect, CornerRadius::ZERO, palette.layer_backdrop);
                if resp.clicked() {
                    close_requested = true;
                }
            });

        egui::Area::new(Id::unique("tg_payments_overlay_panel"))
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .show(ctx, |ui| {
                let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
                ui.painter().rect_filled(
                    panel_rect.expand(1.0),
                    CornerRadius::same(10),
                    palette.divider,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                    if guard.screen_host.show(ui, palette, true) {
                        close_requested = true;
                    }
                });
            });

        if close_requested {
            guard.open = false;
        }
    }
}

#[cfg(test)]
mod tests_tgb2892;

#[cfg(test)]
mod tests_link_decode;

#[cfg(test)]
mod tests {
    use tg_passport::types::{ScopeType, ValueMap, ValueType};
    use tg_payments::card::{
        CardBrand, CardField, NativeMethodDetails, ValidationState, YearMonth,
    };
    use tg_payments::checkout::CheckoutMode;
    use tg_payments::star_gifts::StarGiftLifecycleState;

    use super::*;
    use crate::payments::backend::ReceivedGiftAction;
    use crate::payments::checkout::{
        CardFormState, CheckoutModal, CheckoutSubView, DOC_TEST_AMEX_NUMBER, DOC_TEST_MC_NUMBER,
    };
    use crate::payments::credits::{StarsHistoryFilter, format_stars_count};
    use crate::payments::mock::{
        DEMO_CLOUD_PASSWORD, MockPassportBackend, MockPaymentsBackend, SAMPLE_INVOICE_ID,
        SAMPLE_PASSPORT_BOT,
    };
    use crate::payments::passport::PassportSubView;
    use crate::payments::star_gifts::{StarGiftsTab, is_collectible_state};

    #[test]
    fn checkout_summary_totals_tips_shipping_and_submit_gates() {
        let mut backend = MockPaymentsBackend::new();
        let bundle = backend.load_checkout_form(SAMPLE_INVOICE_ID).unwrap();
        let mut view = CheckoutView::new(bundle);

        // Base prices: 2900 + 1200 - 400 = 3700; default shipping = 599; default tip = 300 => 4599 ($45.99)
        assert_eq!(view.total_amount(), 4599);
        assert_eq!(view.format_amount(view.total_amount(), false), "$45.99");

        // Toggling active tip clears it to 0; toggling another selects it.
        view.toggle_suggested_tip(300);
        assert_eq!(view.form.invoice.tips_selected, 0);
        assert_eq!(view.total_amount(), 4299);
        view.toggle_suggested_tip(500);
        assert_eq!(view.form.invoice.tips_selected, 500);
        assert_eq!(view.total_amount(), 4799);

        // Switching shipping to Express (1499) updates total immediately.
        view.select_shipping_option("ship_express");
        assert_eq!(view.total_amount(), 3700 + 1499 + 500);

        // First submit opens Terms Acceptance modal.
        let _ = view.try_submit(&mut backend);
        assert_eq!(view.modal, Some(CheckoutModal::TermsAcceptance));
        view.form.accept_terms();

        // Next submit opens Bot Trust modal.
        let _ = view.try_submit(&mut backend);
        assert_eq!(view.modal, Some(CheckoutModal::BotTrust));
        view.form.trust_bot();

        // Next submit requires 2FA tmp password for saved card.
        let _ = view.try_submit(&mut backend);
        assert!(matches!(
            view.modal,
            Some(CheckoutModal::TmpPassword { .. })
        ));

        // Wrong password fails; demo password succeeds and routes into 3-DS webview verification.
        assert!(backend.verify_tmp_password("wrong").is_err());
        view.tmp_password = Some(backend.verify_tmp_password(DEMO_CLOUD_PASSWORD).unwrap());
        let (fake_host, ctrl) = crate::payments::checkout::FakePaymentVerificationHost::new();
        view.set_verification_host(Box::new(fake_host));
        let msg = view.try_submit(&mut backend);
        assert!(msg.is_some());
        assert_eq!(view.sub_view, CheckoutSubView::VerificationWebView);
        assert_eq!(ctrl.opened_requests().len(), 1);

        // Simulated tg:// redirect completes 3-DS and transitions cleanly into Receipt mode.
        assert!(!ctrl.simulate_redirect("tg://resolve?domain=northwind_audio_bot&start=paid"));
        let completed_toast = view.poll_verification_host();
        assert!(completed_toast.is_some());
        assert_eq!(view.mode, CheckoutMode::Receipt);
        assert!(view.form.invoice.receipt.paid);
        assert_eq!(view.sub_view, CheckoutSubView::Summary);
    }

    #[test]
    fn card_form_live_luhn_brand_expiry_cvc_and_billing_validation() {
        let native = NativeMethodDetails {
            default_country: "US".to_owned(),
            supported: true,
            need_country: true,
            need_zip: true,
            need_cardholder_name: true,
            can_save_information: true,
        };
        let threshold = YearMonth::new(2026, 11);

        // Documented Stripe test Visa passes all checks.
        let mut visa = CardFormState::with_documented_test_visa(threshold);
        visa.normalize_inputs();
        let v = visa.evaluate(&native);
        assert_eq!(v.brand, CardBrand::Visa);
        assert_eq!(v.number_state, ValidationState::Valid);
        assert_eq!(v.expiry_state, ValidationState::Valid);
        assert_eq!(v.cvc_state, ValidationState::Valid);
        assert!(v.is_ready());
        assert!(visa.validate_for_submit(&native).is_ok());

        // Documented Stripe test Amex uses 4-digit CVC & 4-6-5 grouping.
        let mut amex = CardFormState::new("US", threshold);
        amex.number = DOC_TEST_AMEX_NUMBER.to_owned();
        amex.expire_date = "0629".to_owned();
        amex.cvc = "1234".to_owned();
        amex.cardholder_name = "alex river".to_owned();
        amex.address_zip = "97477".to_owned();
        amex.normalize_inputs();
        assert_eq!(amex.number, "3782 822463 10005");
        assert_eq!(amex.expire_date, "06/29");
        assert_eq!(amex.cardholder_name, "ALEX RIVER");
        let va = amex.evaluate(&native);
        assert_eq!(va.brand, CardBrand::Amex);
        assert!(va.is_ready());

        // Documented MasterCard with broken last digit fails Luhn.
        let mut bad = CardFormState::new("US", threshold);
        bad.number = DOC_TEST_MC_NUMBER.replace("4444", "4445");
        bad.expire_date = "09/29".to_owned();
        bad.cvc = "555".to_owned();
        bad.cardholder_name = "ALEX RIVER".to_owned();
        bad.address_zip = "97477".to_owned();
        bad.normalize_inputs();
        let vb = bad.evaluate(&native);
        assert_eq!(vb.brand, CardBrand::MasterCard);
        assert_eq!(vb.number_state, ValidationState::Invalid);
        assert!(!vb.is_ready());
        let err = bad.validate_for_submit(&native).unwrap_err();
        assert_eq!(err.0, CardField::Number);

        // Expired card date fails expiry check.
        let mut expired = CardFormState::with_documented_test_visa(threshold);
        expired.expire_date = "01/25".to_owned();
        assert_eq!(
            expired.evaluate(&native).expiry_state,
            ValidationState::Invalid
        );
        assert_eq!(
            expired.validate_for_submit(&native).unwrap_err().0,
            CardField::ExpireDate
        );
    }

    #[test]
    fn stars_balance_filtering_paid_media_and_paid_reaction() {
        let mut backend = MockPaymentsBackend::new();
        let mut stars = StarsView::new(backend.load_credits_overview().unwrap());

        assert_eq!(stars.balance.whole(), 1_450);
        assert_eq!(format_stars_count(1_450), "1,450");
        assert_eq!(stars.visible_topup_options().len(), 3);
        stars.show_extended_topups = true;
        assert_eq!(stars.visible_topup_options().len(), 5);

        let all_count = stars.filtered_history().len();
        stars.filter = StarsHistoryFilter::Incoming;
        let in_count = stars.filtered_history().len();
        stars.filter = StarsHistoryFilter::Outgoing;
        let out_count = stars.filtered_history().len();
        assert!(in_count > 0 && out_count > 0);
        assert_eq!(in_count + out_count, all_count);

        // Paid media confirmation deducts Stars and appends transaction.
        let after_media = backend.confirm_paid_media(77_001, 75).unwrap();
        assert_eq!(after_media.whole(), 1_375);

        // Paid reaction confirmation deducts Stars too.
        let after_react = backend.confirm_paid_reaction(512, 50, false).unwrap();
        assert_eq!(after_react.whole(), 1_325);
        stars.refresh_from_backend(&backend);
        assert_eq!(stars.balance.whole(), 1_325);
        assert!(stars.sample_paid_media.cost.unlocked);
    }

    #[test]
    fn star_gifts_filters_pricing_and_lifecycle_actions() {
        let mut backend = MockPaymentsBackend::new();
        let mut view = StarGiftsView::new(backend.load_star_gifts().unwrap());

        assert_eq!(view.filtered_catalog().len(), 6);
        view.active_tab = StarGiftsTab::Limited;
        assert_eq!(view.filtered_catalog().len(), 3);
        view.active_tab = StarGiftsTab::Upgradable;
        assert_eq!(view.filtered_catalog().len(), 5);

        let prism = view.catalog[1].clone();
        assert_eq!(StarGiftsView::compute_send_cost(&prism, false), 50);
        assert_eq!(StarGiftsView::compute_send_cost(&prism, true), 75);

        // Lifecycle states across received gifts
        assert_eq!(
            view.received[0].saved.lifecycle_state(),
            StarGiftLifecycleState::UniqueOwned
        );
        assert!(is_collectible_state(
            view.received[0].saved.lifecycle_state()
        ));
        assert_eq!(
            view.received[1].saved.lifecycle_state(),
            StarGiftLifecycleState::Upgradable
        );
        assert_eq!(
            view.received[2].saved.lifecycle_state(),
            StarGiftLifecycleState::Regular
        );

        let upgrade_msg = backend
            .apply_received_gift_action(88_002, ReceivedGiftAction::Upgrade)
            .unwrap();
        assert!(upgrade_msg.contains("Placeholder"));
    }

    #[test]
    fn premium_options_limits_and_boosts_progress() {
        let backend = MockPaymentsBackend::new();
        let prem = PremiumView::new(backend.load_premium_promo().unwrap());
        assert_eq!(prem.bundle.promo.period_options.len(), 3);
        assert!(prem.selected_option().unwrap().discount_percent > 0);
        assert!(
            !prem
                .subscribe_button_label()
                .to_ascii_lowercase()
                .contains("placeholder")
        );
        assert_eq!(prem.bundle.default_limits.channels(), 500);
        assert_eq!(prem.bundle.premium_limits.channels(), 1_000);

        let boosts = BoostsGiveawaysView::new(backend.load_boosts_giveaway().unwrap());
        let pct = BoostsGiveawaysView::boost_level_progress(&boosts.bundle.boost_status);
        assert!(pct > 0.5 && pct < 0.6);
        let perks = BoostsGiveawaysView::unlocked_perks(
            boosts.bundle.boost_status.overview.level,
            &boosts.bundle.level_limits,
        );
        assert!(perks.iter().any(|(_, _, unlocked)| *unlocked));
        assert!(perks.iter().any(|(_, _, unlocked)| !*unlocked));
    }

    #[test]
    fn passport_password_flow_scopes_and_validation() {
        let mut backend = MockPassportBackend::new();
        let bundle = backend
            .load_authorization_form(SAMPLE_PASSPORT_BOT)
            .unwrap();
        let mut view = PassportView::new(bundle, false);
        assert_eq!(view.sub_view, PassportSubView::AskPassword);

        // Wrong password fails; demo password unlocks.
        assert!(backend.verify_cloud_password("nope").is_err());
        let unlocked = backend.verify_cloud_password(DEMO_CLOUD_PASSWORD).unwrap();
        view.form = unlocked.form;
        view.sub_view = PassportSubView::AuthorizationForm;

        // Initial form has Address missing postcode -> cannot authorize yet.
        assert!(!view.can_authorize());
        let rows = view.scope_rows();
        assert_eq!(rows.len(), 4); // Identity (with inlined PersonalDetails), Address (with inlined details), Phone, Email
        let addr_scope = rows
            .iter()
            .find(|(s, _)| s.scope_type == ScopeType::Address)
            .unwrap()
            .0
            .clone();

        // Open address editor -> fails validation while postcode is empty.
        view.open_scope_editor(addr_scope);
        assert!(!view.validate_address_draft());
        assert!(view.address_draft.errors.contains_key("post_code"));

        // Fill valid postcode -> passes validation and enables Authorize!
        view.address_draft.address.set_text("post_code", "97477");
        assert!(view.validate_address_draft());
        let updated = backend
            .save_passport_value(
                ValueType::Address,
                view.address_draft.address.clone(),
                Vec::new(),
            )
            .unwrap();
        view.form = updated;
        assert!(view.can_authorize());

        // Setup new password flow also validates length.
        assert!(backend.setup_cloud_password("12", "short").is_err());
        assert!(backend.setup_cloud_password("newpass99", "hint99").is_ok());
        assert!(backend.verify_cloud_password("newpass99").is_ok());

        // Personal details validation catches bad Latin name.
        let mut bad_personal = ValueMap::default();
        bad_personal.set_text("first_name", "Alex@Invalid");
        view.identity_draft.personal = bad_personal;
        assert!(!view.validate_identity_draft());
        assert!(view.identity_draft.errors.contains_key("first_name"));
    }

    // Documentation screenshots through the shared software rasterizer (tg_ui::soft_raster).

    fn render_egui_to_png(
        logical_w: u32,
        logical_h: u32,
        ppp: f32,
        draw: impl FnMut(&mut egui::Ui),
    ) -> Vec<u8> {
        let ctx = egui::Context::default();
        ctx.set_visuals(Palette::day().to_egui_visuals(ThemeMode::Day));
        tg_ui::soft_raster::render_egui_to_png(&ctx, logical_w, logical_h, ppp, draw)
    }

    #[test]
    fn renders_payments_stars_gifts_and_passport_screenshots_via_egui() {
        let write_files = std::env::var("RUSTGRAM_UPDATE_SCREENSHOTS").as_deref() == Ok("1")
            || std::env::var("TGFORK_UPDATE_SCREENSHOTS").as_deref() == Ok("1");
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        if write_files {
            let _ = std::fs::create_dir_all(&docs_dir);
        }
        let palette = Palette::day();

        // 1. Checkout summary screenshot
        let mut host = PaymentsScreenHost::with_mock();
        host.select_screen(PaymentsScreen::Checkout);
        let checkout_png = render_egui_to_png(560, 930, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(checkout_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("payments_checkout.png"), &checkout_png)
                .expect("write payments_checkout.png");
        }

        // 2. Native Card Form screenshot (documented test Visa with live Luhn & brand badge)
        host.open_card_form();
        let card_png = render_egui_to_png(560, 740, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(card_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("payments_card_form.png"), &card_png)
                .expect("write payments_card_form.png");
        }

        // 3. Telegram Stars balance + top-ups + subscriptions + transaction history screenshot
        host.select_screen(PaymentsScreen::Stars);
        let stars_png = render_egui_to_png(560, 920, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(stars_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("payments_stars.png"), &stars_png)
                .expect("write payments_stars.png");
        }

        // 4. Star Gifts picker grid + received gifts screenshot
        host.select_screen(PaymentsScreen::StarGifts);
        let gifts_png = render_egui_to_png(560, 840, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(gifts_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("payments_gifts.png"), &gifts_png)
                .expect("write payments_gifts.png");
        }

        // 5. Telegram Passport bot authorization form screenshot
        host.select_screen(PaymentsScreen::Passport);
        let passport_png = render_egui_to_png(560, 760, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(passport_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("payments_passport.png"), &passport_png)
                .expect("write payments_passport.png");
        }
    }
}
