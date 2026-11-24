//! Telegram Passport authorization panel UI: bot authorization form, cloud password unlock &
//! secret creation, identity/address/contact edit forms with validation, encrypted document
//! scans, and accept/decline controls.
//!
//! Port origin: `passport/passport_panel.cpp`, `passport/passport_panel_password.cpp`,
//! `passport/passport_panel_form.cpp`, `passport/passport_panel_edit_document.cpp`,
//! `passport/passport_panel_edit_scans.cpp`, and `passport/passport_panel_edit_contact.cpp`.

use std::collections::BTreeMap;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    TextEdit, Ui, Vec2,
};
use tg_passport::types::{
    DOCUMENT_SCANS_LIMIT, FileType, PassportFile, PassportForm, PassportScope, ScopeRow, ScopeType,
    TRANSLATION_SCANS_LIMIT, ValueMap, ValueType,
};
use tg_passport::validation::{
    FieldValidationError, compute_scope_row, compute_scopes, postprocess_email, postprocess_phone,
    validate_address, validate_contact, validate_identity_document, validate_personal_details,
};

use crate::payments::backend::{
    PassportBackend, PassportSessionBundle, PassportVerificationChallenge,
};
use crate::payments::ui_helpers::{
    SUCCESS_GREEN, SUCCESS_SURFACE, paint_avatar, pill_button, primary_button, section_divider,
    subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Active screen inside the Telegram Passport panel (`passport_panel.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassportSubView {
    /// Cloud password entry (`PanelAskPassword`).
    AskPassword,
    /// First-time cloud password & Passport master secret setup (`PanelNoPassword`).
    SetupPassword,
    /// Main bot authorization form listing requested scopes (`PanelForm`).
    AuthorizationForm,
    /// Edit identity details + identity document + scans (`PanelEditDocument`).
    EditIdentity(PassportScope),
    /// Edit residential address + proof-of-address scans (`PanelEditDocument`).
    EditAddress(PassportScope),
    /// Edit verified phone or email (`PanelEditContact`).
    EditContact(PassportScope),
    /// Final confirmation view after Accept or Decline.
    Completed {
        /// Whether the user authorized sharing.
        accepted: bool,
        /// Status summary message.
        summary: String,
    },
}

/// Editable draft state for identity / personal details + document scans.
#[derive(Debug, Clone, Default)]
pub struct IdentityDraft {
    /// Selected document type (`Passport`, `DriverLicense`, `IdentityCard`, `InternalPassport`).
    pub document_type: Option<ValueType>,
    /// Personal details fields (`first_name`, `middle_name`, `last_name`, `birth_date`, `gender`, `country_code`, `residence_country_code`).
    pub personal: ValueMap,
    /// Identity document fields (`document_no`, `expiry_date`).
    pub document: ValueMap,
    /// Whether native names are requested by the bot.
    pub native_names: bool,
    /// Whether selfie scan is required.
    pub selfie_required: bool,
    /// Whether certified translation scans are required.
    pub translation_required: bool,
    /// Attached scan labels (`"Front"`, `"Reverse"`, `"Selfie"`).
    pub scans: Vec<String>,
    /// Attached certified translation scan labels (up to [`TRANSLATION_SCANS_LIMIT`]).
    pub translations: Vec<String>,
    /// Field validation errors keyed by field name.
    pub errors: BTreeMap<String, String>,
}

/// Editable draft state for residential address + proof-of-address scans.
#[derive(Debug, Clone, Default)]
pub struct AddressDraft {
    /// Selected proof-of-address document type (`UtilityBill`, `BankStatement`, etc.).
    pub document_type: Option<ValueType>,
    /// Address fields (`street_line1`, `street_line2`, `city`, `state`, `country_code`, `post_code`).
    pub address: ValueMap,
    /// Whether certified translation scans are required.
    pub translation_required: bool,
    /// Attached proof-of-address scan labels (up to [`DOCUMENT_SCANS_LIMIT`]).
    pub scans: Vec<String>,
    /// Attached certified translation scan labels (up to [`TRANSLATION_SCANS_LIMIT`]).
    pub translations: Vec<String>,
    /// Field validation errors keyed by field name.
    pub errors: BTreeMap<String, String>,
}

/// Editable draft state for Phone or Email contact scope.
#[derive(Debug, Clone, Default)]
pub struct ContactDraft {
    /// `ValueType::Phone` or `ValueType::Email`.
    pub value_type: Option<ValueType>,
    /// Editable phone or email text.
    pub text: String,
    /// Pending server verification challenge (`account.sendVerifyPhoneCode` / `sendVerifyEmailCode`).
    pub pending_verification: Option<PassportVerificationChallenge>,
    /// User-entered verification code (`account.verifyPhone` / `verifyEmail`).
    pub verification_code: String,
    /// Validation or verification error, if any.
    pub error: Option<String>,
}

/// View-model & renderer for the Telegram Passport authorization panel.
pub struct PassportView {
    /// Requesting bot display name.
    pub bot_name: String,
    /// Requesting bot username without `@`.
    pub bot_username: String,
    /// Password hint text.
    pub password_hint: String,
    /// Whether the account has a recovery email configured.
    pub has_recovery: bool,
    /// Underlying `tg-passport` form model.
    pub form: PassportForm,
    /// Current panel sub-view.
    pub sub_view: PassportSubView,
    /// Password input for unlock or setup.
    pub password_input: String,
    /// Confirm-password input for first-time secret setup.
    pub password_confirm: String,
    /// Hint input for first-time secret setup.
    pub setup_hint: String,
    /// Inline password error message.
    pub password_error: Option<String>,
    /// True while the Reset Telegram Passport confirmation panel is open.
    pub reset_confirm_open: bool,
    /// Cloud password confirmation input for resetting Telegram Passport.
    pub reset_password_input: String,
    /// Inline error for reset confirmation.
    pub reset_error: Option<String>,
    /// Active identity editor draft.
    pub identity_draft: IdentityDraft,
    /// Active address editor draft.
    pub address_draft: AddressDraft,
    /// Active contact editor draft.
    pub contact_draft: ContactDraft,
}

impl PassportView {
    /// Creates a [`PassportView`] from `bundle`, starting unlocked or on the password gate.
    #[must_use]
    pub fn new(bundle: PassportSessionBundle, start_unlocked: bool) -> Self {
        let sub_view = if start_unlocked {
            PassportSubView::AuthorizationForm
        } else if bundle.has_cloud_password {
            PassportSubView::AskPassword
        } else {
            PassportSubView::SetupPassword
        };
        Self {
            bot_name: bundle.bot_name,
            bot_username: bundle.bot_username,
            password_hint: bundle.password_hint,
            has_recovery: bundle.has_recovery,
            form: bundle.form,
            sub_view,
            password_input: String::new(),
            password_confirm: String::new(),
            setup_hint: String::new(),
            password_error: None,
            reset_confirm_open: false,
            reset_password_input: String::new(),
            reset_error: None,
            identity_draft: IdentityDraft::default(),
            address_draft: AddressDraft::default(),
            contact_draft: ContactDraft::default(),
        }
    }

    /// Computes all UI scopes and their ready/error rows using `tg_passport::validation`.
    #[must_use]
    pub fn scope_rows(&self) -> Vec<(PassportScope, ScopeRow)> {
        compute_scopes(&self.form)
            .into_iter()
            .map(|scope| {
                let row = compute_scope_row(&self.form, &scope);
                (scope, row)
            })
            .collect()
    }

    /// Returns `true` when every requested scope is ready and free of errors.
    #[must_use]
    pub fn can_authorize(&self) -> bool {
        let rows = self.scope_rows();
        !rows.is_empty()
            && rows
                .iter()
                .all(|(_, r)| r.ready.is_some() && r.error.is_none())
    }

    /// Opens the appropriate editor sub-view for `scope`.
    pub fn open_scope_editor(&mut self, scope: PassportScope) {
        match scope.scope_type {
            ScopeType::PersonalDetails | ScopeType::Identity => {
                let doc_type = scope
                    .documents
                    .iter()
                    .copied()
                    .find(|vt| {
                        self.form
                            .values
                            .get(vt)
                            .is_some_and(|v| v.scans_are_filled())
                    })
                    .or_else(|| scope.documents.first().copied());
                let personal = self
                    .form
                    .values
                    .get(&ValueType::PersonalDetails)
                    .map(|v| v.data.parsed.clone())
                    .unwrap_or_default();
                let native_names = self
                    .form
                    .values
                    .get(&ValueType::PersonalDetails)
                    .is_some_and(|v| v.native_names);
                let (document, selfie_required, translation_required, scans, translations) =
                    doc_type
                        .and_then(|vt| self.form.values.get(&vt))
                        .map(|v| {
                            let mut s = Vec::new();
                            if v.special_scans.contains_key(&FileType::FrontSide) {
                                s.push("Front".to_owned());
                            }
                            if v.special_scans.contains_key(&FileType::ReverseSide) {
                                s.push("Reverse".to_owned());
                            }
                            if v.special_scans.contains_key(&FileType::Selfie) {
                                s.push("Selfie".to_owned());
                            }
                            let t: Vec<String> = (1..=v.translations.len())
                                .map(|i| format!("Translation Page {i}"))
                                .collect();
                            (
                                v.data.parsed.clone(),
                                v.selfie_required,
                                v.translation_required,
                                s,
                                t,
                            )
                        })
                        .unwrap_or_default();
                self.identity_draft = IdentityDraft {
                    document_type: doc_type,
                    personal,
                    document,
                    native_names,
                    selfie_required,
                    translation_required,
                    scans,
                    translations,
                    errors: BTreeMap::new(),
                };
                self.sub_view = PassportSubView::EditIdentity(scope);
            }
            ScopeType::AddressDetails | ScopeType::Address => {
                let doc_type = scope
                    .documents
                    .iter()
                    .copied()
                    .find(|vt| {
                        self.form
                            .values
                            .get(vt)
                            .is_some_and(|v| !v.scans.is_empty())
                    })
                    .or_else(|| scope.documents.first().copied());
                let address = self
                    .form
                    .values
                    .get(&ValueType::Address)
                    .map(|v| v.data.parsed.clone())
                    .unwrap_or_default();
                let (translation_required, scans, translations) = doc_type
                    .and_then(|vt| self.form.values.get(&vt))
                    .map(|v| {
                        let s = (1..=v.scans.len())
                            .map(|i| format!("Scan Page {i}"))
                            .collect();
                        let t = (1..=v.translations.len())
                            .map(|i| format!("Translation Page {i}"))
                            .collect();
                        (v.translation_required, s, t)
                    })
                    .unwrap_or_default();
                self.address_draft = AddressDraft {
                    document_type: doc_type,
                    address,
                    translation_required,
                    scans,
                    translations,
                    errors: BTreeMap::new(),
                };
                self.sub_view = PassportSubView::EditAddress(scope);
            }
            ScopeType::Phone | ScopeType::Email => {
                let vt = scope.scope_type.details_type();
                let text = self
                    .form
                    .values
                    .get(&vt)
                    .map(|v| v.data.parsed.text("value").to_owned())
                    .unwrap_or_default();
                self.contact_draft = ContactDraft {
                    value_type: Some(vt),
                    text,
                    pending_verification: None,
                    verification_code: String::new(),
                    error: None,
                };
                self.sub_view = PassportSubView::EditContact(scope);
            }
        }
    }

    /// Encrypts and uploads a scan, selfie, or translation file through `backend` and records
    /// its label in the active identity or address draft.
    pub fn attach_scan_bytes(
        &mut self,
        backend: &mut dyn PassportBackend,
        value_type: ValueType,
        file_type: FileType,
        label: String,
        raw_bytes: &[u8],
    ) -> Result<PassportFile, String> {
        let uploaded = backend.upload_passport_file(value_type, file_type, raw_bytes)?;
        match file_type {
            FileType::FrontSide | FileType::ReverseSide | FileType::Selfie => {
                let slot = match file_type {
                    FileType::FrontSide => "Front",
                    FileType::ReverseSide => "Reverse",
                    FileType::Selfie => "Selfie",
                    _ => unreachable!(),
                };
                if !self
                    .identity_draft
                    .scans
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(slot))
                {
                    self.identity_draft.scans.push(slot.to_owned());
                }
            }
            FileType::Scan => {
                if self.address_draft.scans.len() < DOCUMENT_SCANS_LIMIT {
                    self.address_draft.scans.push(label);
                }
            }
            FileType::Translation => {
                if self.identity_draft.document_type == Some(value_type) {
                    if self.identity_draft.translations.len() < TRANSLATION_SCANS_LIMIT {
                        self.identity_draft.translations.push(label);
                    }
                } else if self.address_draft.translations.len() < TRANSLATION_SCANS_LIMIT {
                    self.address_draft.translations.push(label);
                }
            }
        }
        Ok(uploaded)
    }

    /// Validates `self.identity_draft` using `tg_passport::validation`.
    pub fn validate_identity_draft(&mut self) -> bool {
        self.identity_draft.errors.clear();
        for (k, err) in validate_personal_details(
            &self.identity_draft.personal,
            self.identity_draft.native_names,
        ) {
            self.identity_draft
                .errors
                .insert(k.to_owned(), field_error_text(err).to_owned());
        }
        if let Some(doc_type) = self.identity_draft.document_type {
            for (k, err) in validate_identity_document(&self.identity_draft.document) {
                self.identity_draft
                    .errors
                    .insert(k.to_owned(), field_error_text(err).to_owned());
            }
            if !self
                .identity_draft
                .scans
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Front"))
            {
                self.identity_draft.errors.insert(
                    "scans".to_owned(),
                    "Front side document scan is required.".to_owned(),
                );
            }
            if matches!(doc_type, ValueType::DriverLicense | ValueType::IdentityCard)
                && !self
                    .identity_draft
                    .scans
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("Reverse"))
            {
                self.identity_draft.errors.insert(
                    "scans".to_owned(),
                    "Reverse side document scan is required for this document.".to_owned(),
                );
            }
            if self.identity_draft.selfie_required
                && !self
                    .identity_draft
                    .scans
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("Selfie"))
            {
                self.identity_draft.errors.insert(
                    "scans".to_owned(),
                    "Selfie with document scan is required by this bot.".to_owned(),
                );
            }
        }
        self.identity_draft.errors.is_empty()
    }

    /// Validates `self.address_draft` using `tg_passport::validation::validate_address`.
    pub fn validate_address_draft(&mut self) -> bool {
        self.address_draft.errors.clear();
        for (k, err) in validate_address(&self.address_draft.address) {
            self.address_draft
                .errors
                .insert(k.to_owned(), field_error_text(err).to_owned());
        }
        if self.address_draft.document_type.is_some() && self.address_draft.scans.is_empty() {
            self.address_draft.errors.insert(
                "scans".to_owned(),
                "Attach at least one proof-of-address document scan.".to_owned(),
            );
        }
        self.address_draft.errors.is_empty()
    }

    /// Renders the active Telegram Passport sub-view.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PassportBackend,
        palette: &Palette,
    ) -> Option<String> {
        match self.sub_view.clone() {
            PassportSubView::AskPassword => self.show_ask_password(ui, backend, palette),
            PassportSubView::SetupPassword => self.show_setup_password(ui, backend, palette),
            PassportSubView::AuthorizationForm => {
                self.show_authorization_form(ui, backend, palette)
            }
            PassportSubView::EditIdentity(scope) => {
                self.show_edit_identity(ui, scope, backend, palette)
            }
            PassportSubView::EditAddress(scope) => {
                self.show_edit_address(ui, scope, backend, palette)
            }
            PassportSubView::EditContact(scope) => {
                self.show_edit_contact(ui, scope, backend, palette)
            }
            PassportSubView::Completed { accepted, summary } => {
                self.show_completed(ui, accepted, &summary, palette)
            }
        }
    }

    fn show_bot_header(&self, ui: &mut Ui, subtitle: &str, palette: &Palette) {
        ui.vertical_centered(|ui| {
            let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(58.0), Sense::hover());
            paint_avatar(
                ui.painter(),
                av_rect.center(),
                26.0,
                palette.window_bg_active,
                "VC",
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("{} requests access", self.bot_name))
                    .size(15.5)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.label(
                egui::RichText::new(subtitle)
                    .size(12.0)
                    .color(palette.window_sub_text_fg),
            );
        });
    }

    fn show_ask_password(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PassportBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        Frame::NONE
            .inner_margin(Margin::symmetric(24, 18))
            .show(ui, |ui| {
                self.show_bot_header(
                    ui,
                    "Enter your Telegram cloud password to decrypt your Telegram Passport data.",
                    palette,
                );
                ui.add_space(16.0);

                ui.label(
                    egui::RichText::new("Cloud Password")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add_sized(
                    Vec2::new(ui.available_width().max(200.0), 32.0),
                    TextEdit::singleline(&mut self.password_input)
                        .password(true)
                        .hint_text("Enter password (demo: demo1234)"),
                );
                if !self.password_hint.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Hint: {}", self.password_hint))
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                }
                if let Some(ref err) = self.password_error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(err)
                            .size(12.0)
                            .color(palette.attention_fg),
                    );
                }

                ui.add_space(14.0);
                if primary_button(
                    ui,
                    "NEXT",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                {
                    match backend.verify_cloud_password(&self.password_input) {
                        Ok(res) => {
                            self.form = res.form;
                            self.password_error = None;
                            self.password_input.clear();
                            self.sub_view = PassportSubView::AuthorizationForm;
                            toast = Some(res.notice);
                        }
                        Err(err) => {
                            self.password_error = Some(err);
                        }
                    }
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if pill_button(
                        ui,
                        "Switch to Setup New Password / Secret",
                        palette.window_bg_over,
                        palette.window_active_text_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.password_error = None;
                        self.sub_view = PassportSubView::SetupPassword;
                    }
                });
            });
        toast
    }

    fn show_setup_password(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PassportBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        Frame::NONE
            .inner_margin(Margin::symmetric(24, 18))
            .show(ui, |ui| {
                self.show_bot_header(
                    ui,
                    "Create a Two-Step Verification password to encrypt your Passport master secret.",
                    palette,
                );
                ui.add_space(14.0);

                ui.label(
                    egui::RichText::new("New Cloud Password")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add_sized(
                    Vec2::new(ui.available_width().max(200.0), 30.0),
                    TextEdit::singleline(&mut self.password_input)
                        .password(true)
                        .hint_text("At least 4 characters"),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Re-enter Password")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add_sized(
                    Vec2::new(ui.available_width().max(200.0), 30.0),
                    TextEdit::singleline(&mut self.password_confirm)
                        .password(true)
                        .hint_text("Confirm password"),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Password Hint (Optional)")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add_sized(
                    Vec2::new(ui.available_width().max(200.0), 30.0),
                    TextEdit::singleline(&mut self.setup_hint)
                        .hint_text("Optional reminder"),
                );

                if let Some(ref err) = self.password_error {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(err)
                            .size(12.0)
                            .color(palette.attention_fg),
                    );
                }

                ui.add_space(14.0);
                if primary_button(
                    ui,
                    "CREATE PASSWORD & MASTER SECRET",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                {
                    if self.password_input != self.password_confirm {
                        self.password_error = Some("Passwords do not match.".to_owned());
                    } else {
                        match backend.setup_cloud_password(&self.password_input, &self.setup_hint) {
                            Ok(res) => {
                                self.form = res.form;
                                self.password_hint = self.setup_hint.clone();
                                self.password_error = None;
                                self.password_input.clear();
                                self.password_confirm.clear();
                                self.sub_view = PassportSubView::AuthorizationForm;
                                toast = Some(res.notice);
                            }
                            Err(err) => self.password_error = Some(err),
                        }
                    }
                }
                ui.add_space(8.0);
                if pill_button(
                    ui,
                    "< Back to Existing Password Unlock",
                    palette.window_bg_over,
                    palette.window_fg,
                    Some(palette.divider),
                )
                .clicked()
                {
                    self.password_error = None;
                    self.sub_view = PassportSubView::AskPassword;
                }
            });
        toast
    }

    fn show_authorization_form(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PassportBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                self.show_bot_header(
                    ui,
                    "Telegram Passport stores end-to-end encrypted copies of your documents.",
                    palette,
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if pill_button(
                        ui,
                        "Lock / Password Screen",
                        palette.window_bg_over,
                        palette.window_sub_text_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.sub_view = PassportSubView::AskPassword;
                    }
                    if pill_button(
                        ui,
                        "Create Secret Flow",
                        palette.window_bg_over,
                        palette.window_sub_text_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.sub_view = PassportSubView::SetupPassword;
                    }
                    if pill_button(
                        ui,
                        "Reset Passport",
                        with_alpha(palette.attention_fg, 0.12),
                        palette.attention_fg,
                        Some(palette.attention_fg),
                    )
                    .clicked()
                    {
                        self.reset_confirm_open = !self.reset_confirm_open;
                        self.reset_error = None;
                    }
                });
                if self.reset_confirm_open {
                    ui.add_space(8.0);
                    Frame::NONE
                        .fill(with_alpha(palette.attention_fg, 0.08))
                        .stroke(Stroke::new(1.0, palette.attention_fg))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "Reset Telegram Passport? This permanently deletes all stored documents and resets your Passport secret.",
                                )
                                .size(12.0)
                                .color(palette.attention_fg),
                            );
                            ui.add_space(6.0);
                            ui.add_sized(
                                Vec2::new(ui.available_width().max(180.0), 28.0),
                                TextEdit::singleline(&mut self.reset_password_input)
                                    .password(true)
                                    .hint_text("Enter cloud password to confirm reset"),
                            );
                            if let Some(ref err) = self.reset_error {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(err)
                                        .size(11.5)
                                        .color(palette.attention_fg),
                                );
                            }
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if pill_button(
                                    ui,
                                    "Confirm Reset",
                                    palette.attention_fg,
                                    Color32::WHITE,
                                    None,
                                )
                                .clicked()
                                {
                                    match backend.reset_passport(&self.reset_password_input) {
                                        Ok(res) => {
                                            self.form = res.form;
                                            self.reset_confirm_open = false;
                                            self.reset_password_input.clear();
                                            self.reset_error = None;
                                            toast = Some(res.notice);
                                        }
                                        Err(err) => {
                                            self.reset_error = Some(err);
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
                                    self.reset_confirm_open = false;
                                    self.reset_password_input.clear();
                                    self.reset_error = None;
                                }
                            });
                        });
                }
            });

        section_divider(ui, palette);

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                subsection_title(ui, "Requested Information", palette);
                ui.add_space(6.0);

                let rows = self.scope_rows();
                let mut clicked_scope: Option<PassportScope> = None;

                for (scope, row) in &rows {
                    let title = scope_title(scope);
                    let is_ready = row.ready.is_some() && row.error.is_none();
                    let subtitle = if let Some(ref err) = row.error {
                        err.clone()
                    } else if let Some(ref ready) = row.ready {
                        ready.clone()
                    } else {
                        scope_missing_hint(scope)
                    };

                    let width = ui.available_width().max(220.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(8),
                            palette.window_bg_over,
                        );
                    }

                    // Status circle (green check, red error, or blue plus)
                    let (dot_bg, dot_fg, dot_sym) = if row.error.is_some() {
                        (with_alpha(palette.attention_fg, 0.16), palette.attention_fg, "!")
                    } else if is_ready {
                        (SUCCESS_SURFACE, SUCCESS_GREEN, "OK")
                    } else {
                        (
                            with_alpha(palette.window_bg_active, 0.14),
                            palette.window_active_text_fg,
                            "+",
                        )
                    };
                    let dot_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + 4.0, rect.center().y - 14.0),
                        Vec2::splat(28.0),
                    );
                    ui.painter()
                        .rect_filled(dot_rect, CornerRadius::same(14), dot_bg);
                    ui.painter().text(
                        dot_rect.center(),
                        Align2::CENTER_CENTER,
                        dot_sym,
                        FontId::proportional(10.5),
                        dot_fg,
                    );

                    ui.painter().text(
                        Pos2::new(dot_rect.right() + 10.0, rect.top() + 18.0),
                        Align2::LEFT_CENTER,
                        title,
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    ui.painter().text(
                        Pos2::new(dot_rect.right() + 10.0, rect.top() + 37.0),
                        Align2::LEFT_CENTER,
                        &subtitle,
                        FontId::proportional(11.5),
                        if row.error.is_some() {
                            palette.attention_fg
                        } else if is_ready {
                            palette.window_sub_text_fg
                        } else {
                            palette.window_active_text_fg
                        },
                    );
                    ui.painter().text(
                        Pos2::new(rect.right() - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        if is_ready { "Edit  >" } else { "Fill  >" },
                        FontId::proportional(12.0),
                        palette.window_active_text_fg,
                    );

                    if resp.clicked() {
                        clicked_scope = Some(scope.clone());
                    }
                    thin_divider(ui, palette);
                }

                if let Some(sc) = clicked_scope {
                    self.open_scope_editor(sc);
                }

                ui.add_space(10.0);

                // Privacy policy disclaimer (`PanelForm::setupContent`)
                let policy_text = if self.form.privacy_policy_url.is_empty() {
                    format!(
                        "By clicking Authorize, you allow @{} to receive the requested Telegram Passport data.",
                        self.bot_username
                    )
                } else {
                    format!(
                        "By clicking Authorize, you accept {}'s Privacy Policy ({}) and allow @{} to receive these encrypted fields.",
                        self.bot_name, self.form.privacy_policy_url, self.bot_username
                    )
                };
                ui.label(
                    egui::RichText::new(policy_text)
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                );

                ui.add_space(12.0);
                let ready_all = self.can_authorize();
                let auth_bg = if ready_all {
                    palette.window_bg_active
                } else {
                    with_alpha(palette.window_bg_active, 0.45)
                };
                if primary_button(
                    ui,
                    if ready_all {
                        "AUTHORIZE"
                    } else {
                        "COMPLETE REQUIRED FIELDS TO AUTHORIZE"
                    },
                    auth_bg,
                    palette.window_fg_active,
                    40.0,
                )
                .clicked()
                {
                    if ready_all {
                        match backend.accept_authorization() {
                            Ok(msg) => {
                                self.sub_view = PassportSubView::Completed {
                                    accepted: true,
                                    summary: msg.clone(),
                                };
                                toast = Some(msg);
                            }
                            Err(err) => toast = Some(err),
                        }
                    } else {
                        // Jump straight to the first incomplete scope
                        if let Some((first_incomplete, _)) = rows
                            .into_iter()
                            .find(|(_, r)| r.ready.is_none() || r.error.is_some())
                        {
                            self.open_scope_editor(first_incomplete);
                            toast = Some("Please complete the highlighted section.".to_owned());
                        }
                    }
                }

                ui.add_space(6.0);
                if primary_button(
                    ui,
                    "DECLINE",
                    palette.window_bg_over,
                    palette.attention_fg,
                    34.0,
                )
                .clicked()
                {
                    match backend.decline_authorization() {
                        Ok(msg) => {
                            self.sub_view = PassportSubView::Completed {
                                accepted: false,
                                summary: msg.clone(),
                            };
                            toast = Some(msg);
                        }
                        Err(err) => toast = Some(err),
                    }
                }
            });

        toast
    }

    fn show_edit_identity(
        &mut self,
        ui: &mut Ui,
        scope: PassportScope,
        backend: &mut dyn PassportBackend,
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
                        self.sub_view = PassportSubView::AuthorizationForm;
                    }
                    ui.label(
                        egui::RichText::new("Identity & Personal Details")
                            .size(15.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                });
                ui.add_space(8.0);

                // Document type picker if one_of has multiple options
                if scope.documents.len() > 1 {
                    subsection_title(ui, "Document Type", palette);
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for &doc in &scope.documents {
                            let active = self.identity_draft.document_type == Some(doc);
                            if pill_button(
                                ui,
                                doc.display_name(),
                                if active {
                                    palette.window_bg_active
                                } else {
                                    palette.window_bg_over
                                },
                                if active {
                                    palette.window_fg_active
                                } else {
                                    palette.window_fg
                                },
                                None,
                            )
                            .clicked()
                            {
                                self.identity_draft.document_type = Some(doc);
                            }
                        }
                    });
                    ui.add_space(8.0);
                }

                // Document Scans (`passport_panel_edit_scans.cpp`)
                if let Some(doc_type) = self.identity_draft.document_type {
                    subsection_title(ui, "Document Scans & Selfie", palette);
                    ui.add_space(4.0);
                    let slots: Vec<(&str, FileType)> = match doc_type {
                        ValueType::DriverLicense | ValueType::IdentityCard => {
                            if self.identity_draft.selfie_required {
                                vec![
                                    ("Front", FileType::FrontSide),
                                    ("Reverse", FileType::ReverseSide),
                                    ("Selfie", FileType::Selfie),
                                ]
                            } else {
                                vec![
                                    ("Front", FileType::FrontSide),
                                    ("Reverse", FileType::ReverseSide),
                                ]
                            }
                        }
                        _ => {
                            if self.identity_draft.selfie_required {
                                vec![("Front", FileType::FrontSide), ("Selfie", FileType::Selfie)]
                            } else {
                                vec![("Front", FileType::FrontSide)]
                            }
                        }
                    };
                    ui.horizontal_wrapped(|ui| {
                        for (slot, file_type) in slots {
                            let present = self
                                .identity_draft
                                .scans
                                .iter()
                                .any(|s| s.eq_ignore_ascii_case(slot));
                            let label = if present {
                                format!("[OK] {slot} Scan (Remove)")
                            } else {
                                format!("+ Upload {slot} Scan")
                            };
                            if pill_button(
                                ui,
                                &label,
                                if present {
                                    SUCCESS_SURFACE
                                } else {
                                    palette.window_bg_over
                                },
                                if present {
                                    SUCCESS_GREEN
                                } else {
                                    palette.window_active_text_fg
                                },
                                Some(palette.divider),
                            )
                            .clicked()
                            {
                                if present {
                                    self.identity_draft
                                        .scans
                                        .retain(|s| !s.eq_ignore_ascii_case(slot));
                                } else if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
                                    .pick_file()
                                {
                                    match std::fs::read(&path) {
                                        Ok(bytes) => match self.attach_scan_bytes(
                                            backend,
                                            doc_type,
                                            file_type,
                                            slot.to_owned(),
                                            &bytes,
                                        ) {
                                            Ok(_) => {
                                                toast = Some(format!(
                                                    "Encrypted and uploaded {slot} scan."
                                                ));
                                            }
                                            Err(err) => toast = Some(err),
                                        },
                                        Err(err) => {
                                            toast =
                                                Some(format!("Failed to read scan file: {err}"));
                                        }
                                    }
                                }
                            }
                        }
                    });
                    if self.identity_draft.translation_required {
                        ui.add_space(6.0);
                        subsection_title(
                            ui,
                            &format!(
                                "Certified Translations ({}/{})",
                                self.identity_draft.translations.len(),
                                TRANSLATION_SCANS_LIMIT
                            ),
                            palette,
                        );
                        ui.horizontal_wrapped(|ui| {
                            let mut remove_idx = None;
                            for (idx, lbl) in self.identity_draft.translations.iter().enumerate() {
                                if pill_button(
                                    ui,
                                    &format!("[TR] {lbl}  ×"),
                                    SUCCESS_SURFACE,
                                    SUCCESS_GREEN,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    remove_idx = Some(idx);
                                }
                            }
                            if let Some(i) = remove_idx {
                                self.identity_draft.translations.remove(i);
                            }
                            if self.identity_draft.translations.len() < TRANSLATION_SCANS_LIMIT
                                && pill_button(
                                    ui,
                                    "+ Upload Translation",
                                    palette.window_bg_over,
                                    palette.window_active_text_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
                                    .pick_file()
                            {
                                let n = self.identity_draft.translations.len() + 1;
                                match std::fs::read(&path) {
                                    Ok(bytes) => match self.attach_scan_bytes(
                                        backend,
                                        doc_type,
                                        FileType::Translation,
                                        format!("Translation Page {n}"),
                                        &bytes,
                                    ) {
                                        Ok(_) => {
                                            toast = Some(
                                                "Encrypted and uploaded translation scan."
                                                    .to_owned(),
                                            );
                                        }
                                        Err(err) => toast = Some(err),
                                    },
                                    Err(err) => {
                                        toast =
                                            Some(format!("Failed to read translation file: {err}"));
                                    }
                                }
                            }
                        });
                    }
                    if let Some(err) = self.identity_draft.errors.get("scans") {
                        ui.label(
                            egui::RichText::new(err)
                                .size(11.5)
                                .color(palette.attention_fg),
                        );
                    }
                    ui.add_space(8.0);
                }

                subsection_title(ui, "Personal Details", palette);
                ui.add_space(4.0);
                passport_field(
                    ui,
                    "First Name (Latin) *",
                    &mut self.identity_draft.personal,
                    "first_name",
                    "Alex",
                    self.identity_draft.errors.get("first_name"),
                    palette,
                );
                passport_field(
                    ui,
                    "Last Name (Latin) *",
                    &mut self.identity_draft.personal,
                    "last_name",
                    "River",
                    self.identity_draft.errors.get("last_name"),
                    palette,
                );
                ui.columns(2, |cols| {
                    passport_field(
                        &mut cols[0],
                        "Birth Date (DD.MM.YYYY) *",
                        &mut self.identity_draft.personal,
                        "birth_date",
                        "14.05.1994",
                        self.identity_draft.errors.get("birth_date"),
                        palette,
                    );
                    passport_field(
                        &mut cols[1],
                        "Gender (male/female) *",
                        &mut self.identity_draft.personal,
                        "gender",
                        "male",
                        self.identity_draft.errors.get("gender"),
                        palette,
                    );
                });
                ui.columns(2, |cols| {
                    passport_field(
                        &mut cols[0],
                        "Citizenship ISO2 *",
                        &mut self.identity_draft.personal,
                        "country_code",
                        "US",
                        self.identity_draft.errors.get("country_code"),
                        palette,
                    );
                    passport_field(
                        &mut cols[1],
                        "Residence ISO2 *",
                        &mut self.identity_draft.personal,
                        "residence_country_code",
                        "US",
                        self.identity_draft.errors.get("residence_country_code"),
                        palette,
                    );
                });

                if self.identity_draft.document_type.is_some() {
                    ui.add_space(6.0);
                    subsection_title(ui, "Document Fields", palette);
                    ui.add_space(4.0);
                    ui.columns(2, |cols| {
                        passport_field(
                            &mut cols[0],
                            "Document Number *",
                            &mut self.identity_draft.document,
                            "document_no",
                            "X00019283",
                            self.identity_draft.errors.get("document_no"),
                            palette,
                        );
                        passport_field(
                            &mut cols[1],
                            "Expiry Date (DD.MM.YYYY)",
                            &mut self.identity_draft.document,
                            "expiry_date",
                            "14.05.2031",
                            self.identity_draft.errors.get("expiry_date"),
                            palette,
                        );
                    });
                }

                ui.add_space(12.0);
                if primary_button(
                    ui,
                    "SAVE IDENTITY",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                    && self.validate_identity_draft()
                {
                    let _ = backend.save_passport_value(
                        ValueType::PersonalDetails,
                        self.identity_draft.personal.clone(),
                        Vec::new(),
                    );
                    if let Some(doc_type) = self.identity_draft.document_type {
                        if let Ok(updated) = backend.save_passport_value(
                            doc_type,
                            self.identity_draft.document.clone(),
                            self.identity_draft.scans.clone(),
                        ) {
                            self.form = updated;
                        }
                    } else if let Ok(bundle) = backend.load_authorization_form(&self.bot_username) {
                        self.form = bundle.form;
                    }
                    self.sub_view = PassportSubView::AuthorizationForm;
                    toast = Some("Identity details saved.".to_owned());
                }
                ui.add_space(6.0);
                if pill_button(
                    ui,
                    "Delete Identity Value",
                    with_alpha(palette.attention_fg, 0.12),
                    palette.attention_fg,
                    Some(palette.attention_fg),
                )
                .clicked()
                {
                    let mut to_delete = vec![ValueType::PersonalDetails];
                    if let Some(doc_type) = self.identity_draft.document_type {
                        to_delete.push(doc_type);
                    }
                    match backend.delete_passport_values(&to_delete) {
                        Ok(updated) => {
                            self.form = updated;
                            self.sub_view = PassportSubView::AuthorizationForm;
                            toast = Some("Identity value deleted.".to_owned());
                        }
                        Err(err) => {
                            self.identity_draft.errors.insert("scans".to_owned(), err);
                        }
                    }
                }
            });
        toast
    }

    fn show_edit_address(
        &mut self,
        ui: &mut Ui,
        scope: PassportScope,
        backend: &mut dyn PassportBackend,
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
                        self.sub_view = PassportSubView::AuthorizationForm;
                    }
                    ui.label(
                        egui::RichText::new("Residential Address")
                            .size(15.5)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                });
                ui.add_space(8.0);

                if scope.documents.len() > 1 {
                    subsection_title(ui, "Proof of Address Document", palette);
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for &doc in &scope.documents {
                            let active = self.address_draft.document_type == Some(doc);
                            if pill_button(
                                ui,
                                doc.display_name(),
                                if active {
                                    palette.window_bg_active
                                } else {
                                    palette.window_bg_over
                                },
                                if active {
                                    palette.window_fg_active
                                } else {
                                    palette.window_fg
                                },
                                None,
                            )
                            .clicked()
                            {
                                self.address_draft.document_type = Some(doc);
                            }
                        }
                    });
                    ui.add_space(6.0);
                }

                // Multi-page address scans & certified translations (encrypted & uploaded via upload.saveFilePart)
                if let Some(doc_type) = self.address_draft.document_type {
                    subsection_title(
                        ui,
                        &format!(
                            "Address Scans ({}/{})",
                            self.address_draft.scans.len(),
                            DOCUMENT_SCANS_LIMIT
                        ),
                        palette,
                    );
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let mut remove_idx = None;
                        for (idx, label) in self.address_draft.scans.iter().enumerate() {
                            if pill_button(
                                ui,
                                &format!("[IMG] {label}  ×"),
                                SUCCESS_SURFACE,
                                SUCCESS_GREEN,
                                Some(palette.divider),
                            )
                            .clicked()
                            {
                                remove_idx = Some(idx);
                            }
                        }
                        if let Some(i) = remove_idx {
                            self.address_draft.scans.remove(i);
                        }
                        if self.address_draft.scans.len() < DOCUMENT_SCANS_LIMIT
                            && pill_button(
                                ui,
                                "+ Upload Address Scan...",
                                palette.window_bg_over,
                                palette.window_active_text_fg,
                                Some(palette.divider),
                            )
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .set_title("Select Address Document Scan")
                                .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
                                .pick_file()
                        {
                            match std::fs::read(&path) {
                                Ok(bytes) => {
                                    let name = path
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("address_scan.jpg")
                                        .to_owned();
                                    match self.attach_scan_bytes(
                                        backend,
                                        doc_type,
                                        FileType::Scan,
                                        name.clone(),
                                        &bytes,
                                    ) {
                                        Ok(_) => {
                                            self.address_draft.errors.remove("scans");
                                            toast = Some(format!("Uploaded address scan {name}"));
                                        }
                                        Err(e) => {
                                            self.address_draft.errors.insert("scans".to_owned(), e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    self.address_draft
                                        .errors
                                        .insert("scans".to_owned(), format!("Read failed: {e}"));
                                }
                            }
                        }
                    });
                    if let Some(err) = self.address_draft.errors.get("scans") {
                        ui.label(
                            egui::RichText::new(err)
                                .size(11.5)
                                .color(palette.attention_fg),
                        );
                    }

                    if self.address_draft.translation_required
                        || !self.address_draft.translations.is_empty()
                    {
                        ui.add_space(6.0);
                        subsection_title(
                            ui,
                            &format!(
                                "Certified Translations ({}/{})",
                                self.address_draft.translations.len(),
                                TRANSLATION_SCANS_LIMIT
                            ),
                            palette,
                        );
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            let mut remove_idx = None;
                            for (idx, label) in self.address_draft.translations.iter().enumerate() {
                                if pill_button(
                                    ui,
                                    &format!("[DOC] {label}  ×"),
                                    SUCCESS_SURFACE,
                                    SUCCESS_GREEN,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    remove_idx = Some(idx);
                                }
                            }
                            if let Some(i) = remove_idx {
                                self.address_draft.translations.remove(i);
                            }
                            if self.address_draft.translations.len() < TRANSLATION_SCANS_LIMIT
                                && pill_button(
                                    ui,
                                    "+ Upload Translation...",
                                    palette.window_bg_over,
                                    palette.window_active_text_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_title("Select Certified Translation")
                                    .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
                                    .pick_file()
                            {
                                match std::fs::read(&path) {
                                    Ok(bytes) => {
                                        let name = path
                                            .file_name()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("translation.jpg")
                                            .to_owned();
                                        match self.attach_scan_bytes(
                                            backend,
                                            doc_type,
                                            FileType::Translation,
                                            name.clone(),
                                            &bytes,
                                        ) {
                                            Ok(_) => {
                                                toast =
                                                    Some(format!("Uploaded translation {name}"));
                                            }
                                            Err(e) => {
                                                self.address_draft
                                                    .errors
                                                    .insert("scans".to_owned(), e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        self.address_draft.errors.insert(
                                            "scans".to_owned(),
                                            format!("Read failed: {e}"),
                                        );
                                    }
                                }
                            }
                        });
                    }
                    ui.add_space(8.0);
                }

                subsection_title(ui, "Address Details", palette);
                ui.add_space(4.0);
                passport_field(
                    ui,
                    "Street Line 1 *",
                    &mut self.address_draft.address,
                    "street_line1",
                    "742 Evergreen Terrace",
                    self.address_draft.errors.get("street_line1"),
                    palette,
                );
                passport_field(
                    ui,
                    "Street Line 2 (Optional)",
                    &mut self.address_draft.address,
                    "street_line2",
                    "Suite 4B",
                    None,
                    palette,
                );
                ui.columns(2, |cols| {
                    passport_field(
                        &mut cols[0],
                        "City *",
                        &mut self.address_draft.address,
                        "city",
                        "Springfield",
                        self.address_draft.errors.get("city"),
                        palette,
                    );
                    passport_field(
                        &mut cols[1],
                        "State / Region",
                        &mut self.address_draft.address,
                        "state",
                        "OR",
                        None,
                        palette,
                    );
                });
                ui.columns(2, |cols| {
                    passport_field(
                        &mut cols[0],
                        "Country ISO2 *",
                        &mut self.address_draft.address,
                        "country_code",
                        "US",
                        self.address_draft.errors.get("country_code"),
                        palette,
                    );
                    passport_field(
                        &mut cols[1],
                        "Postcode *",
                        &mut self.address_draft.address,
                        "post_code",
                        "97477",
                        self.address_draft.errors.get("post_code"),
                        palette,
                    );
                });

                ui.add_space(12.0);
                if primary_button(
                    ui,
                    "SAVE ADDRESS",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                    && self.validate_address_draft()
                {
                    let _ = backend.save_passport_value(
                        ValueType::Address,
                        self.address_draft.address.clone(),
                        Vec::new(),
                    );
                    if let Some(doc_type) = self.address_draft.document_type {
                        if let Ok(updated) = backend.save_passport_value(
                            doc_type,
                            ValueMap::default(),
                            self.address_draft.scans.clone(),
                        ) {
                            self.form = updated;
                        }
                    } else if let Ok(bundle) = backend.load_authorization_form(&self.bot_username) {
                        self.form = bundle.form;
                    }
                    self.sub_view = PassportSubView::AuthorizationForm;
                    toast = Some("Residential address saved.".to_owned());
                }
                ui.add_space(6.0);
                if pill_button(
                    ui,
                    "Delete Address Value",
                    with_alpha(palette.attention_fg, 0.12),
                    palette.attention_fg,
                    Some(palette.attention_fg),
                )
                .clicked()
                {
                    let mut to_delete = vec![ValueType::Address];
                    if let Some(doc_type) = self.address_draft.document_type {
                        to_delete.push(doc_type);
                    }
                    match backend.delete_passport_values(&to_delete) {
                        Ok(updated) => {
                            self.form = updated;
                            self.sub_view = PassportSubView::AuthorizationForm;
                            toast = Some("Residential address deleted.".to_owned());
                        }
                        Err(err) => {
                            self.address_draft.errors.insert("scans".to_owned(), err);
                        }
                    }
                }
            });
        toast
    }

    fn show_edit_contact(
        &mut self,
        ui: &mut Ui,
        scope: PassportScope,
        backend: &mut dyn PassportBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        let is_phone = scope.scope_type == ScopeType::Phone;
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
                        self.sub_view = PassportSubView::AuthorizationForm;
                    }
                    ui.label(
                        egui::RichText::new(if is_phone {
                            "Phone Number"
                        } else {
                            "Email Address"
                        })
                        .size(15.5)
                        .strong()
                        .color(palette.window_bold_fg),
                    );
                });
                ui.add_space(10.0);
                ui.add_sized(
                    Vec2::new(ui.available_width().max(200.0), 32.0),
                    TextEdit::singleline(&mut self.contact_draft.text).hint_text(if is_phone {
                        "15550102468"
                    } else {
                        "alex.river@example.test"
                    }),
                );
                if let Some(ref challenge) = self.contact_draft.pending_verification.clone() {
                    ui.add_space(8.0);
                    let prompt = if is_phone {
                        format!(
                            "Enter the verification code sent to {} (expected {} digits):",
                            challenge.target,
                            if challenge.code_length > 0 {
                                challenge.code_length.to_string()
                            } else {
                                "5".to_owned()
                            }
                        )
                    } else {
                        format!(
                            "Enter the verification code sent to {}:",
                            challenge
                                .email_pattern
                                .as_deref()
                                .unwrap_or(&challenge.target)
                        )
                    };
                    ui.label(
                        egui::RichText::new(prompt)
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.add_space(4.0);
                    ui.add_sized(
                        Vec2::new(ui.available_width().max(200.0), 32.0),
                        TextEdit::singleline(&mut self.contact_draft.verification_code)
                            .hint_text(if is_phone { "12345" } else { "123456" }),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if is_phone
                            && let Some(ref hash) = challenge.phone_code_hash
                            && pill_button(
                                ui,
                                "Resend Code",
                                palette.window_bg_over,
                                palette.window_active_text_fg,
                                Some(palette.divider),
                            )
                            .clicked()
                        {
                            match backend.resend_phone_verification_code(&challenge.target, hash) {
                                Ok(next) => {
                                    self.contact_draft.pending_verification = Some(next);
                                    self.contact_draft.error = None;
                                    toast = Some("Verification code resent.".to_owned());
                                }
                                Err(err) => self.contact_draft.error = Some(err),
                            }
                        }
                        if pill_button(
                            ui,
                            "Change Contact",
                            palette.window_bg_over,
                            palette.window_sub_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.contact_draft.pending_verification = None;
                            self.contact_draft.verification_code.clear();
                            self.contact_draft.error = None;
                        }
                    });
                }
                if let Some(ref err) = self.contact_draft.error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(err)
                            .size(12.0)
                            .color(palette.attention_fg),
                    );
                }
                ui.add_space(12.0);
                if let Some(challenge) = self.contact_draft.pending_verification.clone() {
                    if primary_button(
                        ui,
                        "VERIFY & SAVE",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        38.0,
                    )
                    .clicked()
                    {
                        let vt = scope.scope_type.details_type();
                        match backend.confirm_contact_verification_code(
                            vt,
                            &challenge.target,
                            challenge.phone_code_hash.as_deref(),
                            &self.contact_draft.verification_code,
                        ) {
                            Ok(updated) => {
                                self.form = updated;
                                self.contact_draft.pending_verification = None;
                                self.contact_draft.verification_code.clear();
                                self.contact_draft.error = None;
                                self.sub_view = PassportSubView::AuthorizationForm;
                                toast = Some(if is_phone {
                                    "Phone number verified and saved.".to_owned()
                                } else {
                                    "Email address verified and saved.".to_owned()
                                });
                            }
                            Err(err) => {
                                self.contact_draft.error = Some(err);
                            }
                        }
                    }
                } else {
                    if primary_button(
                        ui,
                        "SEND VERIFICATION CODE",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        38.0,
                    )
                    .clicked()
                    {
                        let normalized = if is_phone {
                            postprocess_phone(&self.contact_draft.text)
                        } else {
                            postprocess_email(&self.contact_draft.text)
                        };
                        match validate_contact(scope.scope_type, &normalized) {
                            Ok(()) => {
                                let vt = scope.scope_type.details_type();
                                match backend.send_contact_verification_code(vt, &normalized) {
                                    Ok(challenge) => {
                                        self.contact_draft.pending_verification = Some(challenge);
                                        self.contact_draft.verification_code.clear();
                                        self.contact_draft.error = None;
                                        toast = Some("Verification code sent.".to_owned());
                                    }
                                    Err(err) => {
                                        self.contact_draft.error = Some(err);
                                    }
                                }
                            }
                            Err(err) => {
                                self.contact_draft.error = Some(field_error_text(err).to_owned());
                            }
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "Save Directly",
                            palette.window_bg_over,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            let normalized = if is_phone {
                                postprocess_phone(&self.contact_draft.text)
                            } else {
                                postprocess_email(&self.contact_draft.text)
                            };
                            match validate_contact(scope.scope_type, &normalized) {
                                Ok(()) => {
                                    let mut map = ValueMap::default();
                                    map.set_text("value", normalized);
                                    let vt = scope.scope_type.details_type();
                                    if let Ok(updated) =
                                        backend.save_passport_value(vt, map, Vec::new())
                                    {
                                        self.form = updated;
                                    }
                                    self.contact_draft.error = None;
                                    self.sub_view = PassportSubView::AuthorizationForm;
                                    toast = Some("Contact saved.".to_owned());
                                }
                                Err(err) => {
                                    self.contact_draft.error =
                                        Some(field_error_text(err).to_owned());
                                }
                            }
                        }
                        if pill_button(
                            ui,
                            "Delete Contact",
                            with_alpha(palette.attention_fg, 0.12),
                            palette.attention_fg,
                            Some(palette.attention_fg),
                        )
                        .clicked()
                        {
                            let vt = scope.scope_type.details_type();
                            match backend.delete_passport_values(&[vt]) {
                                Ok(updated) => {
                                    self.form = updated;
                                    self.contact_draft.text.clear();
                                    self.contact_draft.pending_verification = None;
                                    self.contact_draft.error = None;
                                    self.sub_view = PassportSubView::AuthorizationForm;
                                    toast = Some("Contact deleted.".to_owned());
                                }
                                Err(err) => self.contact_draft.error = Some(err),
                            }
                        }
                    });
                }
            });
        toast
    }

    fn show_completed(
        &mut self,
        ui: &mut Ui,
        accepted: bool,
        summary: &str,
        palette: &Palette,
    ) -> Option<String> {
        Frame::NONE
            .inner_margin(Margin::symmetric(24, 28))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let (badge, _) = ui.allocate_exact_size(Vec2::splat(60.0), Sense::hover());
                    ui.painter().circle_filled(
                        badge.center(),
                        28.0,
                        if accepted {
                            SUCCESS_SURFACE
                        } else {
                            with_alpha(palette.attention_fg, 0.14)
                        },
                    );
                    ui.painter().text(
                        badge.center(),
                        Align2::CENTER_CENTER,
                        if accepted { "OK" } else { "X" },
                        FontId::proportional(20.0),
                        if accepted {
                            SUCCESS_GREEN
                        } else {
                            palette.attention_fg
                        },
                    );
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(if accepted {
                            "Authorization Complete"
                        } else {
                            "Authorization Declined"
                        })
                        .size(17.0)
                        .strong()
                        .color(palette.window_bold_fg),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(summary)
                            .size(12.5)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.add_space(16.0);
                });
                if primary_button(
                    ui,
                    "Return to Authorization Form",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                {
                    self.sub_view = PassportSubView::AuthorizationForm;
                }
            });
        None
    }
}

fn passport_field(
    ui: &mut Ui,
    label: &str,
    map: &mut ValueMap,
    key: &str,
    hint: &str,
    error: Option<&String>,
    palette: &Palette,
) {
    ui.label(
        egui::RichText::new(label)
            .size(11.5)
            .color(if error.is_some() {
                palette.attention_fg
            } else {
                palette.window_sub_text_fg
            }),
    );
    let mut text = map.text(key).to_owned();
    let w = ui.available_width().max(80.0);
    let resp = ui.add_sized(
        Vec2::new(w, 28.0),
        TextEdit::singleline(&mut text).hint_text(hint),
    );
    if resp.changed() {
        map.set_text(key, text);
    }
    if let Some(err) = error {
        ui.painter().rect_stroke(
            resp.rect,
            CornerRadius::same(4),
            Stroke::new(1.5, palette.attention_fg),
            egui::StrokeKind::Outside,
        );
        ui.label(
            egui::RichText::new(err)
                .size(10.5)
                .color(palette.attention_fg),
        );
    }
    ui.add_space(4.0);
}

fn scope_title(scope: &PassportScope) -> String {
    match scope.scope_type {
        ScopeType::PersonalDetails => "Personal Details".to_owned(),
        ScopeType::Identity => {
            if scope.documents.len() == 1 {
                scope.documents[0].display_name().to_owned()
            } else {
                "Identity Document".to_owned()
            }
        }
        ScopeType::AddressDetails => "Residential Address".to_owned(),
        ScopeType::Address => "Residential Address".to_owned(),
        ScopeType::Phone => "Phone Number".to_owned(),
        ScopeType::Email => "Email".to_owned(),
    }
}

fn scope_missing_hint(scope: &PassportScope) -> String {
    match scope.scope_type {
        ScopeType::PersonalDetails => "Enter your name, birth date, and citizenship".to_owned(),
        ScopeType::Identity => {
            let names: Vec<&str> = scope.documents.iter().map(|d| d.display_name()).collect();
            format!("Upload {} + details", names.join(" or "))
        }
        ScopeType::AddressDetails | ScopeType::Address => {
            "Complete street, city, postcode & proof of address".to_owned()
        }
        ScopeType::Phone => "Add verified phone number".to_owned(),
        ScopeType::Email => "Add verified email address".to_owned(),
    }
}

fn field_error_text(err: FieldValidationError) -> &'static str {
    match err {
        FieldValidationError::InvalidLength => "Required field is empty or out of length bounds.",
        FieldValidationError::BadLatinName => "Use Latin letters and standard punctuation only.",
        FieldValidationError::InvalidDateFormat => "Expected date format DD.MM.YYYY.",
        FieldValidationError::InvalidGender => "Enter \"male\" or \"female\".",
        FieldValidationError::InvalidCountry => "Enter a 2-letter ISO country code (e.g. US).",
        FieldValidationError::InvalidPostcode => "Enter 2-10 alphanumeric/hyphen characters.",
        FieldValidationError::InvalidPhone => "Enter 2-12 digits.",
        FieldValidationError::InvalidEmail => "Enter a valid email address.",
    }
}
