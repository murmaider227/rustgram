//! Checkout process and 3-D Secure / verification URL state machine.
//!
//! Port of `payments/payments_checkout_process.h` and `payments/payments_checkout_process.cpp`
//! (pure logic, decoupled from Qt panels and WebView widgets).

use serde_json::Value as JsonValue;
use tg_data::UserId;
use tg_tl::{enums as tl_enums, functions as tl_fn};

use crate::card::{CardField, card_field_for_local_error, card_field_for_stripe_error};
use crate::form::{
    CreditsFormData, CreditsReceiptData, FormError, FormErrorKind, FormLoadEvent, InformationField,
    NewCredentials, PaymentForm, PaymentResultOutcome, SendPaymentAction,
};

/// Checkout display mode (`Payments::Mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutMode {
    /// Interactive checkout form.
    Payment,
    /// Read-only receipt view for an already-paid invoice.
    Receipt,
}

/// Final result reported when checkout closes (`Payments::CheckoutResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutResult {
    /// Invoice was paid successfully.
    Paid,
    /// Payment verification (3-D Secure) is still pending externally.
    Pending,
    /// User cancelled before payment completed.
    Cancelled,
    /// Payment failed.
    Failed,
    /// Free operation (e.g., free gift transfer) requiring no payment.
    Free,
}

/// Internal submission phase (`CheckoutProcess::SubmitState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubmitState {
    /// Initial state before validation or submission.
    #[default]
    None,
    /// Waiting for `payments.validateRequestedInfo`.
    Validating,
    /// Requested info has been validated; ready to submit payment form.
    Validated,
    /// Waiting for `payments.sendPaymentForm` or 3-D Secure verification.
    Finishing,
}

/// 3-D Secure / external browser verification flow state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VerificationFlowState {
    /// No verification active.
    #[default]
    Idle,
    /// Server returned `payments.paymentVerificationNeeded`; waiting for user verification at `url`.
    Pending {
        /// 3-D Secure or provider verification URL.
        url: String,
    },
    /// Redirected back to a Telegram local URL (`tg://` / `t.me/...`) or paid update received.
    Completed,
    /// Payment send or verification failed.
    Failed {
        /// Error identifier.
        error: String,
    },
}

/// Actions emitted by [`CheckoutStateMachine`] for the host/caller to execute.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckoutAction {
    /// Render/refresh the summary form screen.
    ShowForm,
    /// Send `payments.validateRequestedInfo` (either user-triggered or initial silent validation).
    SendValidateRequestedInfo {
        /// TL request to invoke.
        request: tl_fn::payments::ValidateRequestedInfo,
        /// `true` for the background silent validation run right after form load.
        silent: bool,
    },
    /// Prompt the user to choose one of the returned shipping options.
    ChooseShippingOption,
    /// Prompt the user to enter or pick a payment method.
    EditPaymentMethod,
    /// Prompt the user to accept the invoice terms of service.
    RequestTermsAcceptance {
        /// Merchant bot username.
        bot_username: String,
        /// Terms URL.
        terms_url: String,
        /// Whether this is a recurring subscription.
        is_recurring: bool,
    },
    /// Prompt the user to enter their 2FA cloud password to obtain a `tmp_password`.
    RequestTmpPassword,
    /// Prompt the user to set up a 2FA cloud password so credentials can be saved.
    RequestSetPassword,
    /// Prompt the user to confirm trusting the merchant bot + payment provider.
    RequestBotTrust {
        /// Merchant bot ID.
        bot_id: UserId,
        /// Payment provider bot ID.
        provider_id: UserId,
    },
    /// Send `payments.sendPaymentForm` to Telegram servers.
    SendPaymentForm(tl_fn::payments::SendPaymentForm),
    /// Open the 3-D Secure / provider verification URL in a webview or external browser.
    OpenVerificationUrl {
        /// Target HTTPS URL.
        url: String,
        /// Payment provider name for the bottom attribution label.
        provider: String,
    },
    /// Highlight an invalid customer information field.
    ShowInformationError(InformationField),
    /// Highlight an invalid native card field.
    ShowCardError(CardField),
    /// Show a non-fatal notification toast.
    ShowToast(String),
    /// Show a fatal form error.
    ShowCriticalError(String),
    /// Hand off to Telegram Stars checkout flow.
    StartCreditsPayment(CreditsFormData),
    /// Hand off to Telegram Stars receipt view.
    ShowCreditsReceipt(CreditsReceiptData),
    /// Ask the user to confirm closing with unsaved changes.
    ShowCloseConfirm,
    /// Close checkout with the given [`CheckoutResult`].
    Close(CheckoutResult),
}

/// Pure state machine porting `Payments::CheckoutProcess` (`payments_checkout_process.cpp`).
#[derive(Debug, Clone)]
pub struct CheckoutStateMachine {
    mode: CheckoutMode,
    form: PaymentForm,
    input_invoice: tl_enums::InputInvoice,
    submit_state: SubmitState,
    verification: VerificationFlowState,
    initial_silent_validation: bool,
    send_form_pending: bool,
    send_form_failed: bool,
    payment_registered: bool,
}

impl CheckoutStateMachine {
    /// Creates a new [`CheckoutStateMachine`].
    pub fn new(
        mode: CheckoutMode,
        form: PaymentForm,
        input_invoice: tl_enums::InputInvoice,
    ) -> Self {
        Self {
            mode,
            form,
            input_invoice,
            submit_state: SubmitState::None,
            verification: VerificationFlowState::Idle,
            initial_silent_validation: false,
            send_form_pending: false,
            send_form_failed: false,
            payment_registered: false,
        }
    }

    /// Returns the checkout mode.
    pub fn mode(&self) -> CheckoutMode {
        self.mode
    }

    /// Returns a reference to the underlying [`PaymentForm`].
    pub fn form(&self) -> &PaymentForm {
        &self.form
    }

    /// Returns a mutable reference to the underlying [`PaymentForm`].
    pub fn form_mut(&mut self) -> &mut PaymentForm {
        &mut self.form
    }

    /// Returns the current [`SubmitState`].
    pub fn submit_state(&self) -> SubmitState {
        self.submit_state
    }

    /// Returns the current 3-D Secure [`VerificationFlowState`].
    pub fn verification_state(&self) -> &VerificationFlowState {
        &self.verification
    }

    /// Returns `true` if a payment attempt is registered as in-flight.
    pub fn is_payment_registered(&self) -> bool {
        self.payment_registered
    }

    /// Processes a loaded `payments.PaymentForm` TL response.
    pub fn on_payment_form_loaded(
        &mut self,
        tl: &tl_enums::payments::PaymentForm,
    ) -> Vec<CheckoutAction> {
        match self.form.apply_payment_form_tl(tl) {
            FormLoadEvent::FormReady => self.on_form_ready(),
            FormLoadEvent::CreditsPaymentStarted(data) => {
                vec![
                    CheckoutAction::StartCreditsPayment(data),
                    CheckoutAction::Close(CheckoutResult::Cancelled),
                ]
            }
            FormLoadEvent::CreditsReceiptReady(data) => {
                vec![
                    CheckoutAction::ShowCreditsReceipt(data),
                    CheckoutAction::Close(CheckoutResult::Cancelled),
                ]
            }
            FormLoadEvent::Error(err) => self.on_error(err),
        }
    }

    /// Processes a loaded `payments.PaymentReceipt` TL response.
    pub fn on_payment_receipt_loaded(
        &mut self,
        tl: &tl_enums::payments::PaymentReceipt,
    ) -> Vec<CheckoutAction> {
        match self.form.apply_payment_receipt_tl(tl) {
            FormLoadEvent::FormReady => vec![CheckoutAction::ShowForm],
            FormLoadEvent::CreditsReceiptReady(data) => {
                vec![
                    CheckoutAction::ShowCreditsReceipt(data),
                    CheckoutAction::Close(CheckoutResult::Paid),
                ]
            }
            FormLoadEvent::CreditsPaymentStarted(data) => {
                vec![CheckoutAction::StartCreditsPayment(data)]
            }
            FormLoadEvent::Error(err) => self.on_error(err),
        }
    }

    /// Handles `FormReady`: performs silent initial validation of saved info if applicable
    /// (`CheckoutProcess::performInitialSilentValidation`).
    pub fn on_form_ready(&mut self) -> Vec<CheckoutAction> {
        if self.mode == CheckoutMode::Payment
            && self.form.invoice.requires_information()
            && self
                .form
                .validate_information_local(&self.form.information)
                .is_ok()
        {
            let info = self.form.information.clone();
            if let Ok(req) = self
                .form
                .build_validate_requested_info_request(info, self.input_invoice.clone())
            {
                self.initial_silent_validation = true;
                return vec![CheckoutAction::SendValidateRequestedInfo {
                    request: req,
                    silent: true,
                }];
            }
        }
        vec![CheckoutAction::ShowForm]
    }

    /// Handles completion of `payments.validateRequestedInfo` (`ValidateFinished`).
    pub fn on_validate_finished(
        &mut self,
        tl: &tl_enums::payments::ValidatedRequestedInfo,
        tmp_password: Option<&[u8]>,
    ) -> Vec<CheckoutAction> {
        self.form.apply_validated_requested_info_tl(tl);
        if self.initial_silent_validation {
            self.initial_silent_validation = false;
        }
        let mut actions = vec![CheckoutAction::ShowForm];
        let submitted = self.submit_state == SubmitState::Validating;
        self.submit_state = SubmitState::Validated;
        if !submitted {
            return actions;
        }
        if self.form.shipping_options.list.is_empty() {
            actions.extend(self.on_submit(tmp_password));
        } else if self.form.shipping_options.selected_id.is_empty() {
            actions.push(CheckoutAction::ChooseShippingOption);
        }
        actions
    }

    /// Handles user clicking Pay / Submit (`CheckoutProcess::panelSubmit`).
    pub fn on_submit(&mut self, tmp_password: Option<&[u8]>) -> Vec<CheckoutAction> {
        if self.form.invoice.receipt.paid {
            return vec![CheckoutAction::Close(CheckoutResult::Paid)];
        }
        if matches!(
            self.submit_state,
            SubmitState::Validating | SubmitState::Finishing
        ) {
            return Vec::new();
        }

        if !self.form.shipping_options.list.is_empty()
            && self.form.shipping_options.selected_id.is_empty()
        {
            return vec![CheckoutAction::ChooseShippingOption];
        }

        if self.submit_state != SubmitState::Validated
            && self.form.shipping_options.list.is_empty()
            && self.form.invoice.requires_information()
        {
            self.submit_state = SubmitState::Validating;
            let info = self.form.information.clone();
            return match self
                .form
                .build_validate_requested_info_request(info, self.input_invoice.clone())
            {
                Ok(request) => vec![CheckoutAction::SendValidateRequestedInfo {
                    request,
                    silent: false,
                }],
                Err(err) => self.on_error(err),
            };
        }

        if !self.form.payment_method.has_selected_method() {
            return vec![CheckoutAction::EditPaymentMethod];
        }

        if !self.form.invoice.terms_url.is_empty() && !self.form.details.terms_accepted {
            return vec![CheckoutAction::RequestTermsAcceptance {
                bot_username: self.form.details.terms_bot_username.clone(),
                terms_url: self.form.invoice.terms_url.clone(),
                is_recurring: self.form.invoice.is_recurring,
            }];
        }

        self.payment_registered = true;
        self.submit_state = SubmitState::Finishing;
        match self
            .form
            .build_send_payment_form_request(self.input_invoice.clone(), tmp_password)
        {
            Ok(SendPaymentAction::TmpPasswordRequired) => {
                self.payment_registered = false;
                self.submit_state = SubmitState::Validated;
                vec![CheckoutAction::RequestTmpPassword]
            }
            Ok(SendPaymentAction::BotTrustRequired {
                bot_id,
                provider_id,
            }) => {
                self.payment_registered = false;
                self.submit_state = SubmitState::Validated;
                vec![CheckoutAction::RequestBotTrust {
                    bot_id,
                    provider_id,
                }]
            }
            Ok(SendPaymentAction::Send(req)) => vec![CheckoutAction::SendPaymentForm(*req)],
            Err(err) => self.on_error(err),
        }
    }

    /// Marks the merchant bot as trusted and resumes submission (`panelTrustAndSubmit`).
    pub fn on_trust_and_submit(&mut self, tmp_password: Option<&[u8]>) -> Vec<CheckoutAction> {
        self.form.trust_bot();
        self.on_submit(tmp_password)
    }

    /// Marks terms of service as accepted and resumes submission (`panelAcceptTermsAndSubmit`).
    pub fn on_accept_terms_and_submit(
        &mut self,
        tmp_password: Option<&[u8]>,
    ) -> Vec<CheckoutAction> {
        self.form.accept_terms();
        self.on_submit(tmp_password)
    }

    /// Parses a payment webview JS bridge message (`CheckoutProcess::panelWebviewMessage`).
    ///
    /// Expected format: `["payment_form_submit", "{\"title\":\"...\",\"credentials\":{...}}"]`.
    pub fn on_webview_message(
        &mut self,
        message_json: &str,
        save_information: bool,
    ) -> Vec<CheckoutAction> {
        if self.send_form_pending {
            return Vec::new();
        }
        let Ok(JsonValue::Array(list)) = serde_json::from_str::<JsonValue>(message_json) else {
            return Vec::new();
        };
        if list.first().and_then(JsonValue::as_str) != Some("payment_form_submit") {
            return Vec::new();
        }
        let Some(inner_str) = list.get(1).and_then(JsonValue::as_str) else {
            return Vec::new();
        };
        let Ok(JsonValue::Object(root)) = serde_json::from_str::<JsonValue>(inner_str) else {
            return Vec::new();
        };
        let title = root
            .get("title")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let Some(creds @ JsonValue::Object(_)) = root.get("credentials") else {
            return Vec::new();
        };
        let Ok(data) = serde_json::to_string(creds) else {
            return Vec::new();
        };
        let update = self.form.set_payment_credentials(NewCredentials {
            title,
            data,
            save_on_server: save_information,
        });
        let mut actions = vec![CheckoutAction::ShowForm];
        if update.request_new_password {
            actions.push(CheckoutAction::RequestSetPassword);
        }
        actions
    }

    /// Handles the TL `payments.PaymentResult` response (`PaymentFinished` or `VerificationNeeded`).
    pub fn on_payment_result(
        &mut self,
        tl: tl_enums::payments::PaymentResult,
    ) -> Vec<CheckoutAction> {
        match PaymentForm::apply_payment_result_tl(tl) {
            PaymentResultOutcome::Finished(_updates) => {
                self.verification = VerificationFlowState::Completed;
                self.send_form_pending = false;
                self.payment_registered = false;
                vec![CheckoutAction::Close(CheckoutResult::Paid)]
            }
            PaymentResultOutcome::VerificationNeeded { url } => {
                self.send_form_failed = false;
                self.send_form_pending = true;
                self.verification = VerificationFlowState::Pending { url: url.clone() };
                vec![CheckoutAction::OpenVerificationUrl {
                    url,
                    provider: self.form.invoice.provider.clone(),
                }]
            }
        }
    }

    /// Handles a webview navigation attempt during 3-D Secure verification
    /// (`CheckoutProcess::panelWebviewNavigationAttempt`).
    ///
    /// Pass `is_local_tg_url = true` when the redirect URL resolves to a local Telegram link
    /// (e.g., `tg://...` or `https://t.me/...`). Returns `(allow_navigation, actions)`.
    pub fn on_verification_navigation_attempt(
        &mut self,
        is_local_tg_url: bool,
    ) -> (bool, Vec<CheckoutAction>) {
        if !is_local_tg_url {
            return (true, Vec::new());
        }
        self.verification = VerificationFlowState::Completed;
        self.send_form_pending = false;
        self.payment_registered = false;
        (false, vec![CheckoutAction::Close(CheckoutResult::Paid)])
    }

    /// Handles external payment completion notification (e.g., `updateNewMessage` with receipt).
    pub fn on_external_invoice_paid(&mut self) -> Vec<CheckoutAction> {
        self.verification = VerificationFlowState::Completed;
        self.send_form_pending = false;
        self.payment_registered = false;
        vec![CheckoutAction::Close(CheckoutResult::Paid)]
    }

    /// Handles errors across all checkout phases (`CheckoutProcess::handleError`).
    pub fn on_error(&mut self, error: FormError) -> Vec<CheckoutAction> {
        let id = error.id.as_str();
        match error.kind {
            FormErrorKind::Form => {
                if id == "INVOICE_ALREADY_PAID" {
                    vec![CheckoutAction::ShowCriticalError(
                        "Invoice already paid.".to_string(),
                    )]
                } else {
                    vec![CheckoutAction::ShowCriticalError(format!("Error: {id}"))]
                }
            }
            FormErrorKind::Validate => {
                if matches!(
                    self.submit_state,
                    SubmitState::Validating | SubmitState::Validated
                ) {
                    self.submit_state = SubmitState::None;
                }
                if self.initial_silent_validation {
                    self.initial_silent_validation = false;
                    return vec![CheckoutAction::ShowForm];
                }
                if let Some(info_field) = information_field_for_error(id) {
                    return vec![CheckoutAction::ShowInformationError(info_field)];
                }
                if let Some(card_field) = card_field_for_local_error(id) {
                    return vec![CheckoutAction::ShowCardError(card_field)];
                }
                let msg = match id {
                    "SHIPPING_BOT_TIMEOUT" => "Error: Bot Timeout!".to_string(),
                    "SHIPPING_NOT_AVAILABLE" => "Shipping is not available.".to_string(),
                    other => format!("Error: {other}"),
                };
                vec![CheckoutAction::ShowToast(msg)]
            }
            FormErrorKind::Stripe => {
                if let Some(card_field) = card_field_for_stripe_error(id) {
                    return vec![CheckoutAction::ShowCardError(card_field)];
                }
                let msg = match id {
                    "CardDeclined" => "Card declined.".to_string(),
                    "ProcessingError" => "Sorry, a processing error occurred.".to_string(),
                    other => format!("Stripe Error: {other}"),
                };
                vec![CheckoutAction::ShowToast(msg)]
            }
            FormErrorKind::SmartGlocal => {
                vec![CheckoutAction::ShowToast(format!(
                    "SmartGlocal Error: {id}"
                ))]
            }
            FormErrorKind::TmpPassword => {
                vec![CheckoutAction::ShowToast(format!(
                    "Error: Could not generate tmp password ({id})."
                ))]
            }
            FormErrorKind::Send => {
                self.send_form_failed = true;
                self.verification = VerificationFlowState::Failed {
                    error: id.to_string(),
                };
                if self.submit_state == SubmitState::Finishing {
                    self.payment_registered = false;
                    self.submit_state = SubmitState::Validated;
                }
                if id == "TMP_PASSWORD_INVALID" {
                    return vec![CheckoutAction::RequestTmpPassword];
                }
                let msg = match id {
                    "INVOICE_ALREADY_PAID" => "Invoice already paid.".to_string(),
                    "PAYMENT_FAILED" => "Payment failed.".to_string(),
                    "BOT_PRECHECKOUT_FAILED" => "Pre-checkout failed.".to_string(),
                    "BOT_PRECHECKOUT_TIMEOUT" => "Pre-checkout timed out.".to_string(),
                    "REQUESTED_INFO_INVALID"
                    | "SHIPPING_OPTION_INVALID"
                    | "PAYMENT_CREDENTIALS_INVALID"
                    | "PAYMENT_CREDENTIALS_ID_INVALID" => {
                        format!("Error: {id}. Your card has not been billed.")
                    }
                    other => format!("Error: {other}"),
                };
                vec![CheckoutAction::ShowToast(msg)]
            }
        }
    }

    /// Handles a user close request (`CheckoutProcess::panelRequestClose`).
    pub fn on_request_close(&mut self) -> Vec<CheckoutAction> {
        if self.form.has_changes() {
            vec![CheckoutAction::ShowCloseConfirm]
        } else {
            self.on_close_sure()
        }
    }

    /// Handles confirmed close (`CheckoutProcess::panelCloseSure`).
    pub fn on_close_sure(&mut self) -> Vec<CheckoutAction> {
        let result = if self.send_form_failed {
            CheckoutResult::Failed
        } else if self.send_form_pending {
            CheckoutResult::Pending
        } else {
            CheckoutResult::Cancelled
        };
        vec![CheckoutAction::Close(result)]
    }
}

/// Maps a Telegram `payments.validateRequestedInfo` error code to an [`InformationField`].
pub fn information_field_for_error(code: &str) -> Option<InformationField> {
    match code {
        "REQ_INFO_NAME_INVALID" => Some(InformationField::Name),
        "REQ_INFO_EMAIL_INVALID" => Some(InformationField::Email),
        "REQ_INFO_PHONE_INVALID" => Some(InformationField::Phone),
        "ADDRESS_STREET_LINE1_INVALID" => Some(InformationField::ShippingStreet),
        "ADDRESS_CITY_INVALID" => Some(InformationField::ShippingCity),
        "ADDRESS_STATE_INVALID" => Some(InformationField::ShippingState),
        "ADDRESS_COUNTRY_INVALID" => Some(InformationField::ShippingCountry),
        "ADDRESS_POSTCODE_INVALID" => Some(InformationField::ShippingPostcode),
        _ => None,
    }
}
