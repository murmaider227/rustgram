//! Checkout panel UI: invoice summary, customer info editor, native card form with live
//! Luhn/expiry/CVC validation, shipping options picker, payment confirmation (password/terms/trust),
//! and `tg-webview`-hosted 3-D Secure verification handoff via [`PaymentVerificationHost`].
//!
//! Port origin: `payments/ui/payments_form_summary.cpp`, `payments/ui/payments_edit_information.cpp`,
//! `payments/ui/payments_edit_card.cpp`, and `payments/ui/payments_panel.cpp`.

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke, TextEdit, Ui,
    Vec2,
};
use tg_lang::tr;
use tg_payments::card::{
    CardBrand, CardField, NativeMethodDetails, UncheckedCardDetails, ValidationState, YearMonth,
    detect_card_brand, format_card_number, format_cardholder_name, format_expire_date_input,
    is_valid_country_iso2, parse_expire_date_mm_yy, remove_non_digits, validate_card,
    validate_card_details, validate_cvc, validate_expire_date,
};
use tg_payments::checkout::{CheckoutMode, VerificationFlowState};
use tg_payments::currency::{
    fill_amount_and_currency, format_money_field_value, parse_money_field_value,
};
use tg_payments::form::{
    InformationField, PaymentForm, PostAddress, Receipt, RequestedInformation, ShippingOption,
};

use crate::payments::backend::{CheckoutBundle, PaymentSubmitOutcome, PaymentsBackend};
use crate::payments::ui_helpers::{
    SUCCESS_GREEN, SUCCESS_SURFACE, format_unix_datetime, paint_card_brand_icon,
    paint_radial_spinner, pill_button, primary_button, section_divider, settings_value_row,
    subsection_title, thin_divider, with_alpha,
};
pub use tg_app_core::payments::verification::{
    FakePaymentVerificationController, FakePaymentVerificationHost, PaymentVerificationEvent,
    PaymentVerificationHost, TgWebViewVerificationHost, classify_verification_redirect,
    is_telegram_verification_redirect,
};
use tg_app_core::stand_in::theme::Palette;

/// Documented Stripe test Visa card number (never a real card).
pub const DOC_TEST_VISA_NUMBER: &str = "4242 4242 4242 4242";
/// Documented Stripe test MasterCard number (never a real card).
pub const DOC_TEST_MC_NUMBER: &str = "5555 5555 5555 4444";
/// Documented Stripe test Amex number (never a real card).
pub const DOC_TEST_AMEX_NUMBER: &str = "3782 8224 6310 005";

/// Active sub-view inside the Checkout panel (`payments_panel.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckoutSubView {
    /// Main invoice summary or paid receipt (`payments_form_summary.cpp`).
    #[default]
    Summary,
    /// Edit customer shipping address & contact info (`payments_edit_information.cpp`).
    EditInformation,
    /// Native card entry form (`payments_edit_card.cpp`).
    EditCard,
    /// `tg-webview`-hosted 3-D Secure verification view (`payments_panel.cpp`).
    VerificationWebView,
}

/// Active modal dialog over the checkout summary (`payments_panel.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutModal {
    /// Shipping option radio picker (`chooseShippingOption`).
    ChooseShipping,
    /// Saved payment method picker (`choosePaymentMethod`).
    ChoosePaymentMethod,
    /// Custom tip money entry box (`chooseTips`).
    CustomTip {
        /// Editable tip string.
        input: String,
        /// Inline validation error.
        error: Option<String>,
    },
    /// Accept merchant terms of service (`askTermsAcceptance`).
    TermsAcceptance,
    /// Confirm trusting the merchant bot (`askBotTrust`).
    BotTrust,
    /// Enter 2FA cloud password to authorize saved card (`requestTmpPassword`).
    TmpPassword {
        /// Password field text.
        input: String,
        /// Inline error text.
        error: Option<String>,
    },
}

/// Live validation snapshot for the native card form (`payments_edit_card.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardFormValidation {
    /// Detected card brand from BIN prefix.
    pub brand: CardBrand,
    /// Number validation state (length + Luhn).
    pub number_state: ValidationState,
    /// Expiry date validation state (`MM/YY` against threshold).
    pub expiry_state: ValidationState,
    /// CVC validation state (brand-aware 3 or 4 digits).
    pub cvc_state: ValidationState,
    /// Cardholder name valid (or not required).
    pub name_valid: bool,
    /// Billing country valid (or not required).
    pub country_valid: bool,
    /// Billing ZIP valid (or not required).
    pub zip_valid: bool,
}

impl CardFormValidation {
    /// Returns `true` when every required field is valid and ready to tokenize.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self.number_state, ValidationState::Valid)
            && matches!(self.expiry_state, ValidationState::Valid)
            && matches!(self.cvc_state, ValidationState::Valid)
            && self.name_valid
            && self.country_valid
            && self.zip_valid
    }
}

/// Editable state for the native card entry form (`payments_edit_card.cpp`).
#[derive(Debug, Clone)]
pub struct CardFormState {
    /// Formatted card number input.
    pub number: String,
    /// Formatted `MM/YY` expiry date input.
    pub expire_date: String,
    /// Numeric CVC input.
    pub cvc: String,
    /// Uppercase cardholder name input.
    pub cardholder_name: String,
    /// Billing country ISO2 code.
    pub address_country: String,
    /// Billing postal / ZIP code.
    pub address_zip: String,
    /// Whether to save credentials on Telegram servers.
    pub save_card: bool,
    /// Expiry threshold year/month.
    pub threshold: YearMonth,
    /// First field error shown on submit attempt.
    pub submit_error: Option<(CardField, String)>,
}

impl Default for CardFormState {
    fn default() -> Self {
        Self::new("US", YearMonth::new(2026, 11))
    }
}

impl CardFormState {
    /// Creates a blank card form prefilled with `default_country` and expiry `threshold`.
    #[must_use]
    pub fn new(default_country: &str, threshold: YearMonth) -> Self {
        Self {
            number: String::new(),
            expire_date: String::new(),
            cvc: String::new(),
            cardholder_name: String::new(),
            address_country: default_country.to_owned(),
            address_zip: String::new(),
            save_card: true,
            threshold,
            submit_error: None,
        }
    }

    /// Creates a card form pre-populated with Stripe's documented test Visa card for preview/demo.
    #[must_use]
    pub fn with_documented_test_visa(threshold: YearMonth) -> Self {
        Self {
            number: DOC_TEST_VISA_NUMBER.to_owned(),
            expire_date: "12/28".to_owned(),
            cvc: "424".to_owned(),
            cardholder_name: "ALEX RIVER".to_owned(),
            address_country: "US".to_owned(),
            address_zip: "97477".to_owned(),
            save_card: true,
            threshold,
            submit_error: None,
        }
    }

    /// Applies tdesktop's live post-processing to all card inputs (`payments_edit_card.cpp`).
    pub fn normalize_inputs(&mut self) {
        self.number = format_card_number(&self.number);
        self.expire_date = format_expire_date_input(&self.expire_date);
        let digits = remove_non_digits(&self.cvc);
        let max_cvc = detect_card_brand(&self.number).max_cvc_length();
        self.cvc = digits.chars().take(max_cvc).collect();
        self.cardholder_name = format_cardholder_name(&self.cardholder_name);
        self.address_country = self
            .address_country
            .chars()
            .filter(char::is_ascii_alphabetic)
            .take(2)
            .map(|c| c.to_ascii_uppercase())
            .collect();
    }

    /// Computes live validation state across all card fields using `tg_payments::card`.
    #[must_use]
    pub fn evaluate(&self, native: &NativeMethodDetails) -> CardFormValidation {
        let card_res = validate_card(&self.number);
        let exp_res = validate_expire_date(&self.expire_date, self.threshold);
        let cvc_res = validate_cvc(&self.number, &self.cvc);
        let name_valid = !native.need_cardholder_name || !self.cardholder_name.trim().is_empty();
        let country_valid = !native.need_country || is_valid_country_iso2(&self.address_country);
        let zip_valid = !native.need_zip || !self.address_zip.trim().is_empty();
        CardFormValidation {
            brand: card_res.brand,
            number_state: card_res.state,
            expiry_state: exp_res.state,
            cvc_state: cvc_res.state,
            name_valid,
            country_valid,
            zip_valid,
        }
    }

    /// Converts current form fields into [`UncheckedCardDetails`] for `tg-payments` validation.
    #[must_use]
    pub fn to_unchecked(&self) -> UncheckedCardDetails {
        let (expire_month, expire_year) =
            parse_expire_date_mm_yy(&self.expire_date).unwrap_or((0, 0));
        UncheckedCardDetails {
            number: remove_non_digits(&self.number),
            cvc: self.cvc.clone(),
            expire_year,
            expire_month,
            cardholder_name: self.cardholder_name.clone(),
            address_country: self.address_country.clone(),
            address_zip: self.address_zip.clone(),
        }
    }

    /// Validates against `native` and sets `submit_error` on failure.
    pub fn validate_for_submit(
        &mut self,
        native: &NativeMethodDetails,
    ) -> Result<UncheckedCardDetails, (CardField, String)> {
        let details = self.to_unchecked();
        match validate_card_details(&details, native, self.threshold) {
            Ok(()) => {
                self.submit_error = None;
                Ok(details)
            }
            Err(code) => {
                let err = match code {
                    "LOCAL_CARD_NUMBER_INVALID" => (
                        CardField::Number,
                        "Enter a valid card number (Luhn checksum failed).".to_owned(),
                    ),
                    "LOCAL_CARD_EXPIRE_DATE_INVALID" => (
                        CardField::ExpireDate,
                        "Enter a valid future expiration date (MM/YY).".to_owned(),
                    ),
                    "LOCAL_CARD_CVC_INVALID" => (
                        CardField::Cvc,
                        "Enter a valid 3- or 4-digit security code.".to_owned(),
                    ),
                    "LOCAL_CARD_HOLDER_NAME_INVALID" => {
                        (CardField::Name, "Cardholder name is required.".to_owned())
                    }
                    "LOCAL_CARD_BILLING_COUNTRY_INVALID" => (
                        CardField::AddressCountry,
                        "Enter a 2-letter ISO country code (e.g. US).".to_owned(),
                    ),
                    "LOCAL_CARD_BILLING_ZIP_INVALID" => (
                        CardField::AddressZip,
                        "Billing postal / ZIP code is required.".to_owned(),
                    ),
                    other => (CardField::Number, format!("Validation error: {other}")),
                };
                self.submit_error = Some(err.clone());
                Err(err)
            }
        }
    }
}

/// Editable draft state for customer information (`payments_edit_information.cpp`).
#[derive(Debug, Clone, Default)]
pub struct EditInfoState {
    /// Working copy of customer info.
    pub info: RequestedInformation,
    /// Inline field error if validation failed.
    pub error: Option<(InformationField, String)>,
}

impl EditInfoState {
    /// Validates `self.info` against `form.invoice` length & presence rules.
    pub fn validate(&mut self, form: &PaymentForm) -> Result<(), (InformationField, String)> {
        if form.invoice.is_shipping_address_requested {
            let a = &self.info.shipping_address;
            if a.address1.trim().is_empty() || a.address1.chars().count() > 64 {
                let err = (
                    InformationField::ShippingStreet,
                    "Street address line 1 is required (max 64 chars).".to_owned(),
                );
                self.error = Some(err.clone());
                return Err(err);
            }
            if a.city.trim().len() < 2 || a.city.chars().count() > 64 {
                let err = (
                    InformationField::ShippingCity,
                    "City must be 2 to 64 characters.".to_owned(),
                );
                self.error = Some(err.clone());
                return Err(err);
            }
            if !is_valid_country_iso2(&a.country_iso2) {
                let err = (
                    InformationField::ShippingCountry,
                    "Enter a 2-letter ISO country code (e.g. US, DE).".to_owned(),
                );
                self.error = Some(err.clone());
                return Err(err);
            }
            if a.postcode.trim().is_empty() || a.postcode.chars().count() > 10 {
                let err = (
                    InformationField::ShippingPostcode,
                    "Postcode is required (max 10 chars).".to_owned(),
                );
                self.error = Some(err.clone());
                return Err(err);
            }
        }
        if form.invoice.is_name_requested
            && (self.info.name.trim().is_empty() || self.info.name.chars().count() > 64)
        {
            let err = (
                InformationField::Name,
                "Full name is required (max 64 chars).".to_owned(),
            );
            self.error = Some(err.clone());
            return Err(err);
        }
        if form.invoice.is_email_requested {
            let em = self.info.email.trim();
            let valid = em
                .find('@')
                .and_then(|at| {
                    em.rfind('.')
                        .map(|dot| at > 0 && dot > at + 1 && dot + 1 < em.len())
                })
                .unwrap_or(false);
            if !valid {
                let err = (
                    InformationField::Email,
                    "Enter a valid email address.".to_owned(),
                );
                self.error = Some(err.clone());
                return Err(err);
            }
        }
        if form.invoice.is_phone_requested && self.info.phone.trim().len() < 5 {
            let err = (
                InformationField::Phone,
                "Enter a valid phone number.".to_owned(),
            );
            self.error = Some(err.clone());
            return Err(err);
        }
        self.error = None;
        Ok(())
    }
}

/// Interactive Checkout panel view-model & renderer (`payments_panel.cpp`).
pub struct CheckoutView {
    /// Active invoice identifier.
    pub invoice_id: String,
    /// Underlying `tg-payments` form model.
    pub form: PaymentForm,
    /// Display mode (interactive Payment vs read-only Receipt).
    pub mode: CheckoutMode,
    /// Cover badge label (e.g. `"TEST"`).
    pub cover_badge: String,
    /// Current sub-view inside the panel.
    pub sub_view: CheckoutSubView,
    /// Active modal dialog, if any.
    pub modal: Option<CheckoutModal>,
    /// Editable customer info state.
    pub edit_info: EditInfoState,
    /// Editable native card form state.
    pub card_form: CardFormState,
    /// Cached temporary 2FA password token for saved card confirmation.
    pub tmp_password: Option<Vec<u8>>,
    /// 3-D Secure / verification URL flow state.
    pub verification: VerificationFlowState,
    /// Injectable `tg-webview` host driving 3-D Secure verification and redirect capture.
    verification_host: Option<Box<dyn PaymentVerificationHost>>,
    /// Most recent URL observed inside the verification webview.
    pub verification_current_url: String,
    /// Optional error returned when opening or running the verification webview.
    pub verification_host_error: Option<String>,
    /// Whether the demo should route submit through the 3-D Secure verification URL step.
    pub demo_require_3ds: bool,
    /// Provider name shown in verification webview footer.
    pub verification_provider: String,
    /// Receipt reference ID after payment succeeds.
    pub receipt_id: Option<String>,
}

impl CheckoutView {
    /// Constructs a [`CheckoutView`] from a loaded [`CheckoutBundle`].
    #[must_use]
    pub fn new(bundle: CheckoutBundle) -> Self {
        let mode = if bundle.form.invoice.receipt.paid {
            CheckoutMode::Receipt
        } else {
            CheckoutMode::Payment
        };
        let edit_info = EditInfoState {
            info: bundle.form.information.clone(),
            error: None,
        };
        let card_form = CardFormState::with_documented_test_visa(YearMonth::new(2026, 11));
        let verification_provider = bundle.form.invoice.provider.clone();
        let initial_url = bundle.verification_url.clone();
        Self {
            invoice_id: bundle.invoice_id,
            form: bundle.form,
            mode,
            cover_badge: bundle.cover_badge,
            sub_view: CheckoutSubView::Summary,
            modal: None,
            edit_info,
            card_form,
            tmp_password: None,
            verification: VerificationFlowState::Idle,
            verification_host: None,
            verification_current_url: initial_url,
            verification_host_error: None,
            demo_require_3ds: bundle.requires_3ds_demo,
            verification_provider,
            receipt_id: None,
        }
    }

    /// Attaches an injectable [`PaymentVerificationHost`] (e.g. [`TgWebViewVerificationHost`]
    /// in live builds or [`FakePaymentVerificationHost`] in tests).
    pub fn set_verification_host(&mut self, host: Box<dyn PaymentVerificationHost>) {
        self.verification_host = Some(host);
    }

    /// Builder variant attaching a [`PaymentVerificationHost`].
    #[must_use]
    pub fn with_verification_host(mut self, host: Box<dyn PaymentVerificationHost>) -> Self {
        self.verification_host = Some(host);
        self
    }

    /// Computes the total amount across line items, selected shipping option, and selected tip.
    #[must_use]
    pub fn total_amount(&self) -> i64 {
        if self.form.invoice.receipt.paid {
            self.form.invoice.receipt.total_amount
        } else {
            self.form
                .invoice
                .total_amount(self.form.shipping_options.selected())
        }
    }

    /// Formats a minor-unit amount in the invoice's currency.
    #[must_use]
    pub fn format_amount(&self, amount: i64, strip_dot_zero: bool) -> String {
        fill_amount_and_currency(amount, &self.form.invoice.currency, strip_dot_zero)
    }

    /// Returns the display title of the currently selected payment method.
    #[must_use]
    pub fn selected_payment_method_label(&self) -> String {
        if let Some(saved) = self.form.payment_method.selected_saved() {
            saved.title.clone()
        } else if !self.form.payment_method.new_credentials.is_empty() {
            self.form.payment_method.new_credentials.title.clone()
        } else {
            String::new()
        }
    }

    /// Returns whether the currently selected payment method is a saved card (requiring 2FA password).
    #[must_use]
    pub fn using_saved_card(&self) -> bool {
        self.form.payment_method.selected_saved().is_some()
    }

    /// Toggles a suggested tip chip (clicking the active chip clears the tip to 0, matching C++).
    pub fn toggle_suggested_tip(&mut self, amount: i64) {
        if self.form.invoice.tips_selected == amount {
            self.form.set_tips(0);
        } else {
            self.form.set_tips(amount);
        }
    }

    /// Selects a shipping option by ID.
    pub fn select_shipping_option(&mut self, id: &str) {
        self.form.set_shipping_option(id);
    }

    /// Switches into the native card entry form sub-view.
    pub fn open_card_form(&mut self) {
        self.modal = None;
        self.sub_view = CheckoutSubView::EditCard;
    }

    /// Switches into the edit shipping/contact info sub-view.
    pub fn open_edit_info(&mut self) {
        self.edit_info.info = self.form.information.clone();
        self.edit_info.error = None;
        self.modal = None;
        self.sub_view = CheckoutSubView::EditInformation;
    }

    /// Runs the Checkout submit gate sequence (shipping -> method -> terms -> bot trust ->
    /// tmp_password for saved card -> submit / 3-D Secure verification).
    pub fn try_submit(&mut self, backend: &mut dyn PaymentsBackend) -> Option<String> {
        if self.form.invoice.receipt.paid {
            return Some("Invoice is already paid.".to_owned());
        }
        if !self.form.shipping_options.list.is_empty()
            && self.form.shipping_options.selected_id.is_empty()
        {
            self.modal = Some(CheckoutModal::ChooseShipping);
            return None;
        }
        if self.form.invoice.requires_information()
            && self
                .form
                .validate_information_local(&self.form.information)
                .is_err()
        {
            self.open_edit_info();
            return Some("Please complete the required shipping/contact info.".to_owned());
        }
        if !self.form.payment_method.has_selected_method() {
            self.open_card_form();
            return None;
        }
        if !self.form.invoice.terms_url.is_empty() && !self.form.details.terms_accepted {
            self.modal = Some(CheckoutModal::TermsAcceptance);
            return None;
        }
        if !self.form.bot_trusted {
            self.modal = Some(CheckoutModal::BotTrust);
            return None;
        }
        if self.using_saved_card() && self.tmp_password.is_none() {
            self.modal = Some(CheckoutModal::TmpPassword {
                input: String::new(),
                error: None,
            });
            return None;
        }

        let ship_id = (!self.form.shipping_options.selected_id.is_empty())
            .then_some(self.form.shipping_options.selected_id.as_str());
        let cred_title = self.selected_payment_method_label();
        match backend.submit_payment_form(
            &self.invoice_id,
            self.form.details.form_id,
            ship_id,
            self.form.invoice.tips_selected,
            &cred_title,
            self.demo_require_3ds,
        ) {
            Ok(PaymentSubmitOutcome::Paid {
                date,
                total_amount,
                currency,
                receipt_id,
            }) => {
                self.finish_payment(date, total_amount, currency, receipt_id);
                Some("Test payment completed (offline demo).".to_owned())
            }
            Ok(PaymentSubmitOutcome::VerificationNeeded { url, provider }) => {
                self.begin_verification(url, provider)
            }
            Err(err) => Some(format!("Payment error: {err}")),
        }
    }

    /// Enters the 3-D Secure verification sub-view and opens `url` in the injected
    /// [`PaymentVerificationHost`] when one is attached.
    pub fn begin_verification(&mut self, url: String, provider: String) -> Option<String> {
        self.verification = VerificationFlowState::Pending { url: url.clone() };
        self.verification_current_url = url.clone();
        self.verification_provider = provider.clone();
        self.verification_host_error = None;
        self.sub_view = CheckoutSubView::VerificationWebView;

        if let Some(ref mut host) = self.verification_host
            && let Err(err) = host.open_verification(&url, &provider)
        {
            self.verification_host_error = Some(err.clone());
            self.verification = VerificationFlowState::Failed { error: err.clone() };
            return Some(format!("Could not open 3-D Secure verification: {err}"));
        }
        Some(format!("3-D Secure verification required ({provider})."))
    }

    /// Polls the attached [`PaymentVerificationHost`] for navigation, completion, cancel, or
    /// failure events and advances checkout state accordingly.
    pub fn poll_verification_host(&mut self) -> Option<String> {
        let mut last_toast = None;
        loop {
            let event = self
                .verification_host
                .as_mut()
                .and_then(|host| host.poll_event());
            let Some(ev) = event else {
                break;
            };
            match ev {
                PaymentVerificationEvent::Navigating(next_url) => {
                    self.verification_current_url = next_url;
                }
                PaymentVerificationEvent::Completed { redirect_url } => {
                    last_toast = Some(self.complete_verification(&redirect_url));
                    break;
                }
                PaymentVerificationEvent::Cancelled => {
                    last_toast = Some(self.cancel_verification());
                    break;
                }
                PaymentVerificationEvent::Failed(err) => {
                    self.verification_host_error = Some(err.clone());
                    self.verification = VerificationFlowState::Failed { error: err.clone() };
                    last_toast = Some(format!("3-D Secure verification failed: {err}"));
                    break;
                }
            }
        }
        last_toast
    }

    /// Handles a redirect URL directly (e.g. from a deep link or simulated webview redirect).
    pub fn handle_verification_redirect(&mut self, redirect_url: &str) -> Option<String> {
        match classify_verification_redirect(redirect_url)? {
            PaymentVerificationEvent::Completed { redirect_url } => {
                Some(self.complete_verification(&redirect_url))
            }
            PaymentVerificationEvent::Cancelled => Some(self.cancel_verification()),
            PaymentVerificationEvent::Failed(err) => {
                self.verification_host_error = Some(err.clone());
                self.verification = VerificationFlowState::Failed { error: err };
                Some("3-D Secure verification failed.".to_owned())
            }
            PaymentVerificationEvent::Navigating(url) => {
                self.verification_current_url = url;
                None
            }
        }
    }

    /// Marks 3-D Secure verification complete after receiving a Telegram redirect and transitions
    /// into Receipt mode.
    pub fn complete_verification(&mut self, redirect_url: &str) -> String {
        if let Some(ref mut host) = self.verification_host {
            host.close();
        }
        let total = self.total_amount();
        let currency = self.form.invoice.currency.clone();
        self.verification = VerificationFlowState::Completed;
        self.verification_host_error = None;
        let receipt_ref = if redirect_url.is_empty() {
            format!("3DS-{}", self.form.details.form_id)
        } else {
            format!("3DS-VERIFIED-{}", self.form.details.form_id)
        };
        self.finish_payment(1_763_760_000, total, currency, receipt_ref);
        self.sub_view = CheckoutSubView::Summary;
        "3-D Secure verification completed.".to_owned()
    }

    /// Cancels an active 3-D Secure verification session and returns to the checkout summary.
    pub fn cancel_verification(&mut self) -> String {
        if let Some(ref mut host) = self.verification_host {
            host.close();
        }
        self.verification = VerificationFlowState::Idle;
        self.verification_host_error = None;
        self.sub_view = CheckoutSubView::Summary;
        "3-D Secure verification cancelled.".to_owned()
    }

    fn finish_payment(
        &mut self,
        date: i32,
        total_amount: i64,
        currency: String,
        receipt_id: String,
    ) {
        self.form.invoice.receipt = Receipt {
            date,
            total_amount,
            currency,
            paid: true,
        };
        self.mode = CheckoutMode::Receipt;
        self.receipt_id = Some(receipt_id);
        self.modal = None;
    }

    /// Renders the active Checkout sub-view and any open modal dialog.
    ///
    /// Returns an optional toast message for the host.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = if self.sub_view == CheckoutSubView::VerificationWebView {
            self.poll_verification_host()
        } else {
            None
        };

        let view_toast = match self.sub_view {
            CheckoutSubView::Summary => self.show_summary(ui, backend, palette),
            CheckoutSubView::EditInformation => self.show_edit_information(ui, backend, palette),
            CheckoutSubView::EditCard => self.show_edit_card(ui, backend, palette),
            CheckoutSubView::VerificationWebView => self.show_verification_webview(ui, palette),
        };
        if view_toast.is_some() {
            toast = view_toast;
        }

        if let Some(modal_toast) = self.show_modal_if_any(ui, backend, palette) {
            toast = Some(modal_toast);
        }
        toast
    }

    fn show_summary(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        let is_receipt = self.form.invoice.receipt.paid;

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                // Header status strip (Receipt vs Test Checkout).
                ui.horizontal(|ui| {
                    let heading = if is_receipt {
                        "Payment Receipt"
                    } else {
                        "Checkout"
                    };
                    ui.label(
                        egui::RichText::new(heading)
                            .size(16.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    if self.form.invoice.is_test {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(72.0, 20.0), Sense::hover());
                        ui.painter().rect_filled(
                            r,
                            CornerRadius::same(10),
                            with_alpha(palette.window_bg_active, 0.14),
                        );
                        ui.painter().text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            "TEST MODE",
                            FontId::proportional(9.5),
                            palette.window_active_text_fg,
                        );
                    }
                    if is_receipt {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(56.0, 20.0), Sense::hover());
                        ui.painter()
                            .rect_filled(r, CornerRadius::same(10), SUCCESS_SURFACE);
                        ui.painter().text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            "PAID",
                            FontId::proportional(10.0),
                            SUCCESS_GREEN,
                        );
                    }
                });

                ui.add_space(10.0);

                // Cover card (`FormSummary::setupCover`)
                ui.horizontal(|ui| {
                    let (thumb_rect, _) =
                        ui.allocate_exact_size(Vec2::new(78.0, 78.0), Sense::hover());
                    ui.painter().rect_filled(
                        thumb_rect,
                        CornerRadius::same(10),
                        Color32::from_rgb(0x23, 0x3D, 0x5B),
                    );
                    // Stylized synth waveform artwork inside product thumbnail
                    let bars = [0.35_f32, 0.62, 0.88, 0.54, 0.78, 0.42];
                    let bar_w = 7.0;
                    let start_x = thumb_rect.center().x - (bars.len() as f32 * (bar_w + 3.0)) / 2.0;
                    for (i, &frac) in bars.iter().enumerate() {
                        let h = 42.0 * frac;
                        let x = start_x + (i as f32) * (bar_w + 3.0);
                        let bar = Rect::from_min_size(
                            Pos2::new(x, thumb_rect.center().y - h / 2.0),
                            Vec2::new(bar_w, h),
                        );
                        ui.painter().rect_filled(
                            bar,
                            CornerRadius::same(3),
                            Color32::from_rgb(0x5E, 0xC4, 0xF7),
                        );
                    }
                    ui.painter().text(
                        Pos2::new(thumb_rect.right() - 6.0, thumb_rect.top() + 8.0),
                        Align2::RIGHT_TOP,
                        &self.cover_badge,
                        FontId::proportional(8.5),
                        Color32::from_rgba_unmultiplied(255, 255, 255, 190),
                    );

                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&self.form.invoice.cover.title)
                                .size(14.5)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(&self.form.invoice.cover.description)
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        );
                        ui.add_space(3.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Seller: {}  •  {}",
                                self.form.invoice.cover.seller, self.form.invoice.provider
                            ))
                            .size(11.5)
                            .color(palette.window_active_text_fg),
                        );
                    });
                });
            });

        section_divider(ui, palette);

        // Itemized prices + tips + total (`FormSummary::setupPrices`)
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                if is_receipt {
                    price_row(
                        ui,
                        "Payment Date",
                        &format_unix_datetime(self.form.invoice.receipt.date),
                        palette,
                        true,
                    );
                    if let Some(ref rid) = self.receipt_id {
                        price_row(ui, "Receipt ID", rid, palette, false);
                    }
                    ui.add_space(4.0);
                    thin_divider(ui, palette);
                    ui.add_space(6.0);
                }

                for item in &self.form.invoice.prices {
                    let formatted = self.format_amount(item.price, false);
                    price_row(ui, &item.label, &formatted, palette, false);
                }

                if let Some(ship) = self.form.shipping_options.selected().cloned() {
                    for p in &ship.prices {
                        let formatted = self.format_amount(p.price, false);
                        price_row(ui, &p.label, &formatted, palette, false);
                    }
                }

                if !is_receipt && self.form.invoice.tips_max > 0 {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Tip")
                                .size(13.0)
                                .color(palette.window_fg),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let tip_text =
                                self.format_amount(self.form.invoice.tips_selected, false);
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(format!("{tip_text} (Edit)"))
                                            .size(13.0)
                                            .color(palette.window_active_text_fg),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                self.modal = Some(CheckoutModal::CustomTip {
                                    input: format_money_field_value(
                                        self.form.invoice.tips_selected,
                                        &self.form.invoice.currency,
                                    ),
                                    error: None,
                                });
                            }
                        });
                    });

                    // Suggested tip chips (`FormSummary::setupSuggestedTips`)
                    if !self.form.invoice.suggested_tips.is_empty() {
                        ui.add_space(4.0);
                        let tips = self.form.invoice.suggested_tips.clone();
                        let selected_tip = self.form.invoice.tips_selected;
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                            for tip in tips {
                                let chosen = tip == selected_tip;
                                let bg = if chosen {
                                    palette.window_bg_active
                                } else {
                                    with_alpha(palette.window_bg_active, 0.12)
                                };
                                let fg = if chosen {
                                    palette.window_fg_active
                                } else {
                                    palette.window_active_text_fg
                                };
                                let label = self.format_amount(tip, true);
                                if pill_button(ui, &label, bg, fg, None).clicked() {
                                    self.toggle_suggested_tip(tip);
                                }
                            }
                        });
                    }
                } else if is_receipt && self.form.invoice.tips_selected > 0 {
                    let tip_str = self.format_amount(self.form.invoice.tips_selected, false);
                    price_row(ui, "Tip", &tip_str, palette, false);
                }

                ui.add_space(8.0);
                thin_divider(ui, palette);
                ui.add_space(8.0);

                let total_str = self.format_amount(self.total_amount(), false);
                price_row(ui, "Total", &total_str, palette, true);
            });

        section_divider(ui, palette);

        // Editable summary rows (`FormSummary::setupSections`)
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 4))
            .show(ui, |ui| {
                let interactive = !is_receipt;
                let method_label = self.selected_payment_method_label();
                if settings_value_row(
                    ui,
                    "Payment Method",
                    &method_label,
                    "CC",
                    palette.window_bg_active,
                    palette,
                    interactive,
                )
                .clicked()
                    && interactive
                {
                    if self.form.payment_method.saved_credentials.is_empty() {
                        self.open_card_form();
                    } else {
                        self.modal = Some(CheckoutModal::ChoosePaymentMethod);
                    }
                }

                if self.form.invoice.is_shipping_address_requested {
                    let addr = format_post_address(&self.form.information.shipping_address);
                    if settings_value_row(
                        ui,
                        "Shipping Address",
                        &addr,
                        "AD",
                        Color32::from_rgb(0x46, 0xA8, 0x62),
                        palette,
                        interactive,
                    )
                    .clicked()
                        && interactive
                    {
                        self.open_edit_info();
                    }
                }

                if !self.form.shipping_options.list.is_empty() {
                    let ship_title = self
                        .form
                        .shipping_options
                        .selected()
                        .map(|o| o.title.as_str())
                        .unwrap_or("");
                    if settings_value_row(
                        ui,
                        "Shipping Method",
                        ship_title,
                        "SH",
                        Color32::from_rgb(0xF0, 0x93, 0x2B),
                        palette,
                        interactive,
                    )
                    .clicked()
                        && interactive
                    {
                        self.modal = Some(CheckoutModal::ChooseShipping);
                    }
                }

                if self.form.invoice.is_name_requested
                    && settings_value_row(
                        ui,
                        "Name",
                        &self.form.information.name,
                        "NM",
                        Color32::from_rgb(0x6C, 0x7A, 0xE0),
                        palette,
                        interactive,
                    )
                    .clicked()
                    && interactive
                {
                    self.open_edit_info();
                }

                if self.form.invoice.is_email_requested
                    && settings_value_row(
                        ui,
                        "Email",
                        &self.form.information.email,
                        "@",
                        Color32::from_rgb(0x9B, 0x59, 0xB6),
                        palette,
                        interactive,
                    )
                    .clicked()
                    && interactive
                {
                    self.open_edit_info();
                }

                if self.form.invoice.is_phone_requested
                    && settings_value_row(
                        ui,
                        "Phone",
                        &self.form.information.phone,
                        "PH",
                        Color32::from_rgb(0x2D, 0xB8, 0x9A),
                        palette,
                        interactive,
                    )
                    .clicked()
                    && interactive
                {
                    self.open_edit_info();
                }

                if !is_receipt {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(
                            &mut self.demo_require_3ds,
                            egui::RichText::new("Demonstrate 3-D Secure verification webview step")
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                        );
                    });
                }

                ui.add_space(10.0);

                // Bottom action bar (`Pay $XX.XX` + `Add New Card` shortcut)
                if is_receipt {
                    if primary_button(
                        ui,
                        "Reset Demo Invoice",
                        palette.window_bg_over,
                        palette.window_bold_fg,
                        38.0,
                    )
                    .clicked()
                        && let Ok(bundle) = backend.load_checkout_form(&self.invoice_id)
                    {
                        *self = Self::new(bundle);
                        toast = Some("Demo invoice reset.".to_owned());
                    }
                } else {
                    let pay_label =
                        format!("Pay {}", self.format_amount(self.total_amount(), false));
                    if primary_button(
                        ui,
                        &pay_label,
                        palette.window_bg_active,
                        palette.window_fg_active,
                        40.0,
                    )
                    .clicked()
                    {
                        toast = self.try_submit(backend);
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "Enter New Card...",
                            palette.window_bg_over,
                            palette.window_active_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.open_card_form();
                        }
                        if pill_button(
                            ui,
                            "Edit Info...",
                            palette.window_bg_over,
                            palette.window_active_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.open_edit_info();
                        }
                    });
                }
            });

        toast
    }

    fn show_edit_information(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if pill_button(
                        ui,
                        "< Back",
                        palette.window_bg_over,
                        palette.window_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.sub_view = CheckoutSubView::Summary;
                    }
                    ui.label(
                        egui::RichText::new("Shipping & Contact Information")
                            .size(15.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                });
                ui.add_space(10.0);

                let err_field = self.edit_info.error.as_ref().map(|(f, _)| *f);
                if let Some((_, ref msg)) = self.edit_info.error {
                    ui.label(
                        egui::RichText::new(msg)
                            .size(12.0)
                            .color(palette.attention_fg),
                    );
                    ui.add_space(6.0);
                }

                if self.form.invoice.is_shipping_address_requested {
                    subsection_title(ui, "Shipping Address", palette);
                    ui.add_space(4.0);
                    labeled_field(
                        ui,
                        "Street Line 1 *",
                        &mut self.edit_info.info.shipping_address.address1,
                        "742 Evergreen Terrace",
                        err_field == Some(InformationField::ShippingStreet),
                        palette,
                    );
                    labeled_field(
                        ui,
                        "Street Line 2 (Optional)",
                        &mut self.edit_info.info.shipping_address.address2,
                        "Apartment, suite, unit",
                        false,
                        palette,
                    );
                    ui.columns(2, |cols| {
                        labeled_field(
                            &mut cols[0],
                            "City *",
                            &mut self.edit_info.info.shipping_address.city,
                            "Springfield",
                            err_field == Some(InformationField::ShippingCity),
                            palette,
                        );
                        labeled_field(
                            &mut cols[1],
                            "State / Region",
                            &mut self.edit_info.info.shipping_address.state,
                            "OR",
                            err_field == Some(InformationField::ShippingState),
                            palette,
                        );
                    });
                    ui.columns(2, |cols| {
                        labeled_field(
                            &mut cols[0],
                            "Country ISO2 *",
                            &mut self.edit_info.info.shipping_address.country_iso2,
                            "US",
                            err_field == Some(InformationField::ShippingCountry),
                            palette,
                        );
                        labeled_field(
                            &mut cols[1],
                            "Postcode *",
                            &mut self.edit_info.info.shipping_address.postcode,
                            "97477",
                            err_field == Some(InformationField::ShippingPostcode),
                            palette,
                        );
                    });
                    ui.add_space(6.0);
                }

                subsection_title(ui, "Contact Details", palette);
                ui.add_space(4.0);
                if self.form.invoice.is_name_requested {
                    labeled_field(
                        ui,
                        "Full Name *",
                        &mut self.edit_info.info.name,
                        "Alex River",
                        err_field == Some(InformationField::Name),
                        palette,
                    );
                }
                if self.form.invoice.is_email_requested {
                    labeled_field(
                        ui,
                        "Email Address *",
                        &mut self.edit_info.info.email,
                        "alex.river@example.test",
                        err_field == Some(InformationField::Email),
                        palette,
                    );
                }
                if self.form.invoice.is_phone_requested {
                    labeled_field(
                        ui,
                        "Phone Number *",
                        &mut self.edit_info.info.phone,
                        "+1 555 010 2468",
                        err_field == Some(InformationField::Phone),
                        palette,
                    );
                }

                let email_to_prov =
                    self.form.invoice.is_email_requested && self.form.invoice.email_sent_to_provider;
                let phone_to_prov =
                    self.form.invoice.is_phone_requested && self.form.invoice.phone_sent_to_provider;
                if email_to_prov || phone_to_prov {
                    ui.add_space(4.0);
                    let notice = match (phone_to_prov, email_to_prov) {
                        (true, true) => format!(
                            "Your phone number and email will be sent to {} for payment processing.",
                            self.form.invoice.provider
                        ),
                        (true, false) => format!(
                            "Your phone number will be sent to {}.",
                            self.form.invoice.provider
                        ),
                        (false, true) => format!(
                            "Your email address will be sent to {}.",
                            self.form.invoice.provider
                        ),
                        _ => String::new(),
                    };
                    ui.label(
                        egui::RichText::new(notice)
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                }

                ui.add_space(6.0);
                ui.checkbox(&mut self.edit_info.info.save, "Save information for future payments");

                ui.add_space(12.0);
                if primary_button(
                    ui,
                    "SAVE INFORMATION",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                    && self.edit_info.validate(&self.form).is_ok()
                {
                    match backend.validate_requested_info(&self.invoice_id, &self.edit_info.info) {
                        Ok(res) => {
                            self.form.requested_information_id = res.requested_info_id;
                            self.form.information = self.edit_info.info.clone();
                            if self.edit_info.info.save {
                                self.form.saved_information = self.edit_info.info.clone();
                            }
                            let prev_id = self.form.shipping_options.selected_id.clone();
                            self.form.shipping_options.list = res.shipping_options;
                            if !self
                                .form
                                .shipping_options
                                .list
                                .iter()
                                .any(|o| o.id == prev_id)
                            {
                                self.form.shipping_options.selected_id = self
                                    .form
                                    .shipping_options
                                    .list
                                    .first()
                                    .map(|o| o.id.clone())
                                    .unwrap_or_default();
                            }
                            self.sub_view = CheckoutSubView::Summary;
                            toast = Some("Shipping & contact information validated.".to_owned());
                        }
                        Err(err) => {
                            toast = Some(format!("Validation rejected: {err}"));
                        }
                    }
                }
            });
        toast
    }

    fn show_edit_card(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        let native = self.form.payment_method.native_details.clone();
        self.card_form.normalize_inputs();
        let validation = self.card_form.evaluate(&native);

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if pill_button(
                        ui,
                        "< Back",
                        palette.window_bg_over,
                        palette.window_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.sub_view = CheckoutSubView::Summary;
                    }
                    ui.label(
                        egui::RichText::new("Payment Card")
                            .size(15.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                });

                ui.add_space(8.0);

                // Safety banner + documented test card quick-fill presets
                Frame::NONE
                    .fill(with_alpha(palette.window_bg_active, 0.08))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                "Offline demo mode — never enter real bank cards. Use documented test patterns:",
                            )
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                        );
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if pill_button(
                                ui,
                                "Test Visa",
                                palette.window_bg,
                                palette.window_active_text_fg,
                                Some(palette.divider),
                            )
                            .clicked()
                            {
                                self.card_form.number = DOC_TEST_VISA_NUMBER.to_owned();
                                self.card_form.expire_date = "12/28".to_owned();
                                self.card_form.cvc = "424".to_owned();
                                self.card_form.submit_error = None;
                            }
                            if pill_button(
                                ui,
                                "Test MasterCard",
                                palette.window_bg,
                                palette.window_active_text_fg,
                                Some(palette.divider),
                            )
                            .clicked()
                            {
                                self.card_form.number = DOC_TEST_MC_NUMBER.to_owned();
                                self.card_form.expire_date = "09/29".to_owned();
                                self.card_form.cvc = "555".to_owned();
                                self.card_form.submit_error = None;
                            }
                            if pill_button(
                                ui,
                                "Test Amex (4-digit CVC)",
                                palette.window_bg,
                                palette.window_active_text_fg,
                                Some(palette.divider),
                            )
                            .clicked()
                            {
                                self.card_form.number = DOC_TEST_AMEX_NUMBER.to_owned();
                                self.card_form.expire_date = "06/29".to_owned();
                                self.card_form.cvc = "1234".to_owned();
                                self.card_form.submit_error = None;
                            }
                        });
                    });

                ui.add_space(12.0);

                // Live stylized virtual card preview banner
                let preview_w = ui.available_width().max(260.0);
                let (card_rect, _) =
                    ui.allocate_exact_size(Vec2::new(preview_w, 108.0), Sense::hover());
                let card_bg = match validation.brand {
                    CardBrand::Visa => Color32::from_rgb(0x1E, 0x3A, 0x7A),
                    CardBrand::MasterCard => Color32::from_rgb(0x28, 0x2F, 0x3A),
                    CardBrand::Amex => Color32::from_rgb(0x0D, 0x67, 0x9C),
                    CardBrand::Discover => Color32::from_rgb(0xA4, 0x4E, 0x16),
                    _ => Color32::from_rgb(0x30, 0x45, 0x5C),
                };
                ui.painter()
                    .rect_filled(card_rect, CornerRadius::same(12), card_bg);
                // Decorative highlight circle
                ui.painter().circle_filled(
                    Pos2::new(card_rect.right() - 28.0, card_rect.top() + 26.0),
                    46.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                );
                let icon_rect = Rect::from_min_size(
                    Pos2::new(card_rect.right() - 62.0, card_rect.top() + 14.0),
                    Vec2::new(48.0, 26.0),
                );
                paint_card_brand_icon(ui.painter(), icon_rect, validation.brand, palette);

                let preview_num = if self.card_form.number.is_empty() {
                    "•••• •••• •••• ••••"
                } else {
                    &self.card_form.number
                };
                ui.painter().text(
                    Pos2::new(card_rect.left() + 16.0, card_rect.top() + 50.0),
                    Align2::LEFT_CENTER,
                    preview_num,
                    FontId::monospace(16.0),
                    Color32::WHITE,
                );
                let holder_disp = if self.card_form.cardholder_name.is_empty() {
                    "CARDHOLDER NAME"
                } else {
                    &self.card_form.cardholder_name
                };
                let exp_disp = if self.card_form.expire_date.is_empty() {
                    "MM/YY"
                } else {
                    &self.card_form.expire_date
                };
                ui.painter().text(
                    Pos2::new(card_rect.left() + 16.0, card_rect.bottom() - 20.0),
                    Align2::LEFT_CENTER,
                    holder_disp,
                    FontId::proportional(11.0),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 210),
                );
                ui.painter().text(
                    Pos2::new(card_rect.right() - 16.0, card_rect.bottom() - 20.0),
                    Align2::RIGHT_CENTER,
                    format!("EXP {exp_disp}"),
                    FontId::monospace(11.5),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                );

                ui.add_space(12.0);

                // Card Number row with live brand icon + Luhn badge
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Card Number")
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (status_text, status_col) = match validation.number_state {
                            ValidationState::Valid => (
                                format!("{} • Luhn OK", validation.brand.as_str()),
                                SUCCESS_GREEN,
                            ),
                            ValidationState::Invalid => (
                                "Invalid number (Luhn failed)".to_owned(),
                                palette.attention_fg,
                            ),
                            ValidationState::Incomplete => (
                                if validation.brand == CardBrand::Unknown {
                                    "Enter card digits".to_owned()
                                } else {
                                    format!("{} • Incomplete", validation.brand.as_str())
                                },
                                palette.window_sub_text_fg,
                            ),
                        };
                        ui.label(egui::RichText::new(status_text).size(11.5).color(status_col));
                    });
                });
                ui.horizontal(|ui| {
                    let (badge_rect, _) =
                        ui.allocate_exact_size(Vec2::new(44.0, 26.0), Sense::hover());
                    paint_card_brand_icon(ui.painter(), badge_rect, validation.brand, palette);
                    let resp = ui.add_sized(
                        Vec2::new((ui.available_width() - 4.0).max(120.0), 30.0),
                        TextEdit::singleline(&mut self.card_form.number)
                            .hint_text("4242 4242 4242 4242")
                            .font(FontId::monospace(13.5)),
                    );
                    if resp.changed() {
                        self.card_form.normalize_inputs();
                        self.card_form.submit_error = None;
                    }
                });

                ui.add_space(8.0);

                // Side-by-side Expiry (`MM/YY`) and CVC (`payments_edit_card.cpp`)
                ui.columns(2, |cols| {
                    cols[0].horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Expiry (MM/YY)")
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (txt, col) = validation_badge(validation.expiry_state, palette);
                            ui.label(egui::RichText::new(txt).size(11.0).color(col));
                        });
                    });
                    let exp_resp = cols[0].add_sized(
                        Vec2::new(cols[0].available_width().max(80.0), 30.0),
                        TextEdit::singleline(&mut self.card_form.expire_date)
                            .hint_text("MM/YY")
                            .font(FontId::monospace(13.0)),
                    );
                    if exp_resp.changed() {
                        self.card_form.normalize_inputs();
                        self.card_form.submit_error = None;
                    }

                    let max_cvc = validation.brand.max_cvc_length();
                    cols[1].horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("CVC ({max_cvc} digits)"))
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (txt, col) = validation_badge(validation.cvc_state, palette);
                            ui.label(egui::RichText::new(txt).size(11.0).color(col));
                        });
                    });
                    let cvc_resp = cols[1].add_sized(
                        Vec2::new(cols[1].available_width().max(80.0), 30.0),
                        TextEdit::singleline(&mut self.card_form.cvc)
                            .hint_text(if max_cvc == 4 { "1234" } else { "123" })
                            .password(false)
                            .font(FontId::monospace(13.0)),
                    );
                    if cvc_resp.changed() {
                        self.card_form.normalize_inputs();
                        self.card_form.submit_error = None;
                    }
                });

                if native.need_cardholder_name {
                    ui.add_space(8.0);
                    let err = self
                        .card_form
                        .submit_error
                        .as_ref()
                        .is_some_and(|(f, _)| *f == CardField::Name);
                    labeled_field(
                        ui,
                        "Cardholder Name",
                        &mut self.card_form.cardholder_name,
                        "ALEX RIVER",
                        err,
                        palette,
                    );
                }

                if native.need_country || native.need_zip {
                    ui.add_space(6.0);
                    subsection_title(ui, "Billing Address", palette);
                    ui.add_space(4.0);
                    ui.columns(2, |cols| {
                        if native.need_country {
                            let err = self
                                .card_form
                                .submit_error
                                .as_ref()
                                .is_some_and(|(f, _)| *f == CardField::AddressCountry);
                            labeled_field(
                                &mut cols[0],
                                "Country ISO2",
                                &mut self.card_form.address_country,
                                "US",
                                err,
                                palette,
                            );
                        }
                        if native.need_zip {
                            let err = self
                                .card_form
                                .submit_error
                                .as_ref()
                                .is_some_and(|(f, _)| *f == CardField::AddressZip);
                            labeled_field(
                                &mut cols[1],
                                "ZIP / Postcode",
                                &mut self.card_form.address_zip,
                                "97477",
                                err,
                                palette,
                            );
                        }
                    });
                }

                if native.can_save_information {
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.card_form.save_card, "Save card information for future payments");
                }

                if let Some((_, ref err_msg)) = self.card_form.submit_error {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(err_msg)
                            .size(12.0)
                            .color(palette.attention_fg),
                    );
                }

                ui.add_space(12.0);
                let btn_bg = if validation.is_ready() {
                    palette.window_bg_active
                } else {
                    with_alpha(palette.window_bg_active, 0.45)
                };
                if primary_button(ui, "DONE", btn_bg, palette.window_fg_active, 38.0).clicked()
                    && let Ok(details) = self.card_form.validate_for_submit(&native)
                {
                    match backend.tokenize_test_card(&details, self.card_form.save_card) {
                        Ok(creds) => {
                            let title = creds.title.clone();
                            self.form.set_payment_credentials(creds);
                            self.sub_view = CheckoutSubView::Summary;
                            toast = Some(format!(
                                "Test card {title} tokenized locally (no network request)."
                            ));
                        }
                        Err(err) => {
                            self.card_form.submit_error = Some((CardField::Number, err));
                        }
                    }
                }
            });

        toast
    }

    fn show_verification_webview(&mut self, ui: &mut Ui, palette: &Palette) -> Option<String> {
        let mut toast = None;
        let url = match &self.verification {
            VerificationFlowState::Pending { url } => url.clone(),
            _ if !self.verification_current_url.is_empty() => self.verification_current_url.clone(),
            _ => "https://verify.example.test/3ds/challenge".to_owned(),
        };
        let display_url = if !self.verification_current_url.is_empty() {
            self.verification_current_url.clone()
        } else {
            url.clone()
        };
        let host_attached = self.verification_host.is_some();
        let host_active = self
            .verification_host
            .as_ref()
            .is_some_and(|h| h.is_active());
        let host_error = self.verification_host_error.clone();
        let margin_h = if ui.available_width() < 420.0 { 12 } else { 18 };
        let outer_w = (ui.available_width() - f32::from(margin_h) * 2.0).max(200.0);

        Frame::NONE
            .inner_margin(Margin::symmetric(margin_h, 14))
            .show(ui, |ui| {
                ui.set_max_width(outer_w);
                ui.horizontal_wrapped(|ui| {
                    if pill_button(
                        ui,
                        "< Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        toast = Some(self.cancel_verification());
                    }
                    ui.label(
                        egui::RichText::new("3-D Secure Verification")
                            .size(15.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                });

                ui.add_space(10.0);

                // Embedded tg-webview verification handoff card (`payments_panel.cpp`).
                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        let inner_w = (outer_w - 24.0).max(160.0);
                        ui.set_max_width(inner_w);

                        // Address / HTTPS lock bar (wraps cleanly at 390px without overflow).
                        Frame::NONE
                            .fill(palette.window_bg)
                            .stroke(Stroke::new(1.0, palette.divider))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.set_max_width((inner_w - 20.0).max(140.0));
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new("HTTPS")
                                            .size(10.5)
                                            .strong()
                                            .color(SUCCESS_GREEN),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&display_url)
                                                .size(11.0)
                                                .monospace()
                                                .color(palette.window_sub_text_fg),
                                        )
                                        .wrap(),
                                    );
                                });
                            });

                        ui.add_space(16.0);

                        if let Some(ref err) = host_error {
                            ui.vertical_centered(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(tr::lng_payments_webview_no_use())
                                            .size(14.0)
                                            .strong()
                                            .color(palette.attention_fg),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(6.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(err)
                                            .size(12.0)
                                            .color(palette.window_sub_text_fg),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.add_space(12.0);
                            if primary_button(
                                ui,
                                "Retry Verification",
                                palette.window_bg_active,
                                palette.window_fg_active,
                                36.0,
                            )
                            .clicked()
                            {
                                let prov = self.verification_provider.clone();
                                toast = self.begin_verification(url.clone(), prov);
                            }
                        } else if host_active {
                            ui.vertical_centered(|ui| {
                                let (spin_rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
                                paint_radial_spinner(
                                    ui,
                                    spin_rect.center(),
                                    12.0,
                                    palette.window_active_text_fg,
                                );
                                ui.add_space(10.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Waiting for bank verification")
                                            .size(14.5)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(6.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(
                                            "Complete 3-D Secure authentication in the secure tg-webview window. Checkout will update automatically when redirected back to Telegram.",
                                        )
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.add_space(14.0);
                            if primary_button(
                                ui,
                                "Cancel Verification",
                                palette.window_bg,
                                palette.attention_fg,
                                36.0,
                            )
                            .clicked()
                            {
                                toast = Some(self.cancel_verification());
                            }
                        } else {
                            ui.vertical_centered(|ui| {
                                let (spin_rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
                                paint_radial_spinner(
                                    ui,
                                    spin_rect.center(),
                                    12.0,
                                    palette.window_sub_text_fg,
                                );
                                ui.add_space(10.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("3-D Secure WebView Handoff")
                                            .size(14.5)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(6.0);
                                let body = if host_attached {
                                    "Click Open Verification Window to launch the embedded tg-webview challenge and await the tg:// redirect."
                                } else {
                                    "Complete 3-D Secure verification via the payment provider's secure WebView window."
                                };
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(body)
                                            .size(12.0)
                                            .color(palette.window_sub_text_fg),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.add_space(14.0);
                            if host_attached {
                                if primary_button(
                                    ui,
                                    "Open Verification Window",
                                    palette.window_bg_active,
                                    palette.window_fg_active,
                                    36.0,
                                )
                                .clicked()
                                {
                                    let prov = self.verification_provider.clone();
                                    toast = self.begin_verification(url.clone(), prov);
                                }
                            } else if self.form.invoice.is_test
                                && primary_button(
                                    ui,
                                    "Confirm Test Verification",
                                    SUCCESS_GREEN,
                                    Color32::WHITE,
                                    36.0,
                                )
                                .clicked()
                            {
                                toast = Some(
                                    self.complete_verification(
                                        "tg://resolve?domain=test_checkout&status=paid",
                                    ),
                                );
                            }
                        }
                    });

                ui.add_space(10.0);
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(tr::lng_payments_processed_by(
                                &self.verification_provider,
                            ))
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                        )
                        .wrap(),
                    );
                });
            });

        toast
    }

    fn show_modal_if_any(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let modal = self.modal.clone()?;
        let mut toast = None;
        let mut close_modal = false;

        ui.add_space(8.0);
        Frame::NONE
            .fill(palette.window_bg)
            .stroke(Stroke::new(1.5, palette.window_bg_active))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| match modal {
                CheckoutModal::ChooseShipping => {
                    ui.label(
                        egui::RichText::new("Choose Shipping Method")
                            .size(14.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(8.0);
                    let options: Vec<ShippingOption> = self.form.shipping_options.list.clone();
                    let current = self.form.shipping_options.selected_id.clone();
                    for opt in options {
                        let cost: i64 = opt.prices.iter().map(|p| p.price).sum();
                        let cost_label = self.format_amount(cost, false);
                        let selected = opt.id == current;
                        if ui
                            .selectable_label(selected, format!("{}  —  {}", opt.title, cost_label))
                            .clicked()
                        {
                            self.select_shipping_option(&opt.id);
                            close_modal = true;
                            toast = Some(format!("Selected shipping: {}", opt.title));
                        }
                    }
                    ui.add_space(6.0);
                    if pill_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        close_modal = true;
                    }
                }
                CheckoutModal::ChoosePaymentMethod => {
                    ui.label(
                        egui::RichText::new("Choose Payment Method")
                            .size(14.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(8.0);
                    let saved = self.form.payment_method.saved_credentials.clone();
                    let active_idx = self.form.payment_method.saved_credentials_index;
                    for (idx, cred) in saved.iter().enumerate() {
                        if ui
                            .selectable_label(idx == active_idx, format!("Saved Card: {}", cred.title))
                            .clicked()
                        {
                            self.form.choose_saved_method(&cred.id);
                            close_modal = true;
                            toast = Some(format!("Selected {}", cred.title));
                        }
                    }
                    if !self.form.payment_method.new_credentials.is_empty() {
                        let new_title = self.form.payment_method.new_credentials.title.clone();
                        if ui
                            .selectable_label(
                                active_idx >= saved.len(),
                                format!("New Card: {new_title}"),
                            )
                            .clicked()
                        {
                            self.form.choose_saved_method("");
                            close_modal = true;
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "+ Add New Card",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            None,
                        )
                        .clicked()
                        {
                            self.open_card_form();
                        }
                        if pill_button(
                            ui,
                            "Cancel",
                            palette.window_bg_over,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            close_modal = true;
                        }
                    });
                }
                CheckoutModal::CustomTip {
                    mut input,
                    mut error,
                } => {
                    ui.label(
                        egui::RichText::new("Custom Tip Amount")
                            .size(14.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(6.0);
                    let max_str = self.format_amount(self.form.invoice.tips_max, false);
                    ui.label(
                        egui::RichText::new(format!("Maximum allowed tip: {max_str}"))
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.add_space(4.0);
                    ui.add(TextEdit::singleline(&mut input).hint_text("0.00"));
                    if let Some(ref e) = error {
                        ui.label(egui::RichText::new(e).size(11.5).color(palette.attention_fg));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "Save Tip",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            None,
                        )
                        .clicked()
                        {
                            let parsed = parse_money_field_value(&input, &self.form.invoice.currency);
                            if parsed > self.form.invoice.tips_max {
                                error = Some(format!("Tip cannot exceed {max_str}."));
                            } else {
                                self.form.set_tips(parsed);
                                close_modal = true;
                                toast = Some(format!(
                                    "Tip updated to {}.",
                                    self.format_amount(self.form.invoice.tips_selected, false)
                                ));
                            }
                        }
                        if pill_button(
                            ui,
                            "Cancel",
                            palette.window_bg_over,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            close_modal = true;
                        }
                    });
                    if !close_modal {
                        self.modal = Some(CheckoutModal::CustomTip { input, error });
                    }
                }
                CheckoutModal::TermsAcceptance => {
                    ui.label(
                        egui::RichText::new("Terms of Service")
                            .size(14.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "To continue paying @{}, please accept the seller's Terms of Service ({}).",
                            self.form.details.terms_bot_username, self.form.invoice.terms_url
                        ))
                        .size(12.0)
                        .color(palette.window_fg),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "Accept & Continue",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            None,
                        )
                        .clicked()
                        {
                            self.form.accept_terms();
                            self.modal = None;
                            toast = self.try_submit(backend);
                            return;
                        }
                        if pill_button(
                            ui,
                            "Cancel",
                            palette.window_bg_over,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            close_modal = true;
                        }
                    });
                }
                CheckoutModal::BotTrust => {
                    ui.label(
                        egui::RichText::new("Confirm Payment Bot")
                            .size(14.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Do you trust {} to process your test payment through {}?",
                            self.form.invoice.cover.seller, self.form.invoice.provider
                        ))
                        .size(12.0)
                        .color(palette.window_fg),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "Trust & Pay",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            None,
                        )
                        .clicked()
                        {
                            self.form.trust_bot();
                            self.modal = None;
                            toast = self.try_submit(backend);
                            return;
                        }
                        if pill_button(
                            ui,
                            "Cancel",
                            palette.window_bg_over,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            close_modal = true;
                        }
                    });
                }
                CheckoutModal::TmpPassword {
                    mut input,
                    mut error,
                } => {
                    ui.label(
                        egui::RichText::new("Cloud Password Required")
                            .size(14.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Enter your Two-Step Verification password to pay with your saved card (demo password: \"demo1234\").",
                        )
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        TextEdit::singleline(&mut input)
                            .password(true)
                            .hint_text("Cloud password"),
                    );
                    if let Some(ref e) = error {
                        ui.label(egui::RichText::new(e).size(11.5).color(palette.attention_fg));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "Confirm Payment",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            None,
                        )
                        .clicked()
                        {
                            match backend.verify_tmp_password(&input) {
                                Ok(token) => {
                                    self.tmp_password = Some(token);
                                    self.modal = None;
                                    toast = self.try_submit(backend);
                                    return;
                                }
                                Err(e) => {
                                    error = Some(e);
                                }
                            }
                        }
                        if pill_button(
                            ui,
                            "Cancel",
                            palette.window_bg_over,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            close_modal = true;
                        }
                    });
                    if !close_modal {
                        self.modal = Some(CheckoutModal::TmpPassword { input, error });
                    }
                }
            });

        if close_modal {
            self.modal = None;
        }
        toast
    }
}

fn price_row(ui: &mut Ui, label: &str, value: &str, palette: &Palette, bold: bool) {
    ui.horizontal(|ui| {
        let mut left = egui::RichText::new(label)
            .size(if bold { 14.0 } else { 13.0 })
            .color(if bold {
                palette.window_bold_fg
            } else {
                palette.window_fg
            });
        if bold {
            left = left.strong();
        }
        ui.label(left);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut right = egui::RichText::new(value)
                .size(if bold { 14.5 } else { 13.0 })
                .color(if bold {
                    palette.window_bold_fg
                } else {
                    palette.window_fg
                });
            if bold {
                right = right.strong();
            }
            ui.label(right);
        });
    });
    ui.add_space(3.0);
}

fn labeled_field(
    ui: &mut Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    is_error: bool,
    palette: &Palette,
) {
    ui.label(egui::RichText::new(label).size(11.5).color(if is_error {
        palette.attention_fg
    } else {
        palette.window_sub_text_fg
    }));
    let w = ui.available_width().max(80.0);
    let resp = ui.add_sized(
        Vec2::new(w, 28.0),
        TextEdit::singleline(value).hint_text(hint),
    );
    if is_error {
        ui.painter().rect_stroke(
            resp.rect,
            CornerRadius::same(4),
            Stroke::new(1.5, palette.attention_fg),
            egui::StrokeKind::Outside,
        );
    }
    ui.add_space(4.0);
}

fn validation_badge(state: ValidationState, palette: &Palette) -> (&'static str, Color32) {
    match state {
        ValidationState::Valid => ("Valid", SUCCESS_GREEN),
        ValidationState::Invalid => ("Invalid", palette.attention_fg),
        ValidationState::Incomplete => ("", palette.window_sub_text_fg),
    }
}

/// Formats a [`PostAddress`] into a concise single-line summary matching `FormSummary::setupSections`.
#[must_use]
pub fn format_post_address(addr: &PostAddress) -> String {
    let parts: Vec<&str> = [
        addr.address1.as_str(),
        addr.city.as_str(),
        addr.country_iso2.as_str(),
        addr.postcode.as_str(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    parts.join(", ")
}
