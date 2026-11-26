//! Modal boxes for OAuth URL authorization (`tg://oauth`), phone-number click handling
//! (`tel:...`), and bank-card click handling (`bankcard:...`), plus moderation box builder glue.
//!
//! Port origin:
//! - `Telegram/SourceFiles/core/phone_click_handler.cpp`
//! - `Telegram/SourceFiles/core/bank_card_click_handler.cpp`
//! - `Telegram/SourceFiles/boxes/url_auth_box.cpp`
//! - `Telegram/SourceFiles/boxes/moderate_messages_box.cpp`

use egui::{CornerRadius, FontId, Ui};
use tg_bots::callback::{
    AcceptUrlAuthDecision, UrlAuthAcceptOutcome, UrlAuthConfirmationBox, UrlAuthUserChoice,
    build_accept_url_auth, handle_url_auth_accept_result,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::boxes::moderate_messages::{ModerateMessagesBoxState, ModerateParticipant};
use crate::info::backend::InfoBackend;
use tg_app_core::session_api::{BankCardInfo, BankCardOpenUrl};
use tg_app_core::stand_in::types::PeerId;

// ---------------------------------------------------------------------------
// 1. OAuth / URL Authorization confirmation box (`boxes/url_auth_box.cpp`)
// ---------------------------------------------------------------------------

/// Modal state for confirming a Telegram Login / OAuth URL authorization request
/// (`messages.requestUrlAuth` -> `messages.acceptUrlAuth` / `messages.declineUrlAuth`).
#[derive(Debug, Clone)]
pub struct UrlAuthConfirmBoxState {
    /// Server-provided URL auth request details.
    pub data: UrlAuthConfirmationBox,
    /// Whether the user allows the bot to message them (`write_allowed`).
    pub write_allowed: bool,
    /// Whether the user agrees to share their phone number (`share_phone_number`).
    pub share_phone_number: bool,
    /// Selected match code when `data.match_codes` is non-empty.
    pub selected_match_code: Option<String>,
}

impl UrlAuthConfirmBoxState {
    /// Creates a new [`UrlAuthConfirmBoxState`] from `data`.
    #[must_use]
    pub fn new(data: UrlAuthConfirmationBox) -> Self {
        let write_allowed = data.write_access_bot_name.is_some();
        let share_phone_number = data.request_phone_number;
        let selected_match_code = data.match_codes.first().cloned();
        Self {
            data,
            write_allowed,
            share_phone_number,
            selected_match_code,
        }
    }

    /// Converts current UI selections into [`UrlAuthUserChoice`].
    #[must_use]
    pub fn to_user_choice(&self, accepted: bool) -> UrlAuthUserChoice {
        UrlAuthUserChoice {
            auth: accepted,
            allow_write: self.data.write_access_bot_name.is_some() && self.write_allowed,
            share_phone: self.data.request_phone_number && self.share_phone_number,
            match_code: if self.data.match_codes.is_empty() {
                None
            } else {
                self.selected_match_code.clone()
            },
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Telegram Login Authorization")
            .width(st::box_width.0 as f32 + 40.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("authorize", "Log In"))
    }

    /// Renders the OAuth confirmation modal body.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let summary = format!(
            "Log in to {} with your Telegram account?",
            self.data.display_name
        );
        FlatLabel::plain(&summary, FlatLabelStyle::box_label(colors))
            .id_salt("url_auth_summary")
            .show(ui);

        let cb_st = CheckboxStyle::box_checkbox(colors);
        let check_st = CheckStyle::default_check(colors);

        if let Some(ref bot_name) = self.data.write_access_bot_name {
            ui.add_space(10.0);
            Checkbox::new(
                &mut self.write_allowed,
                &format!("Allow {bot_name} to send you messages"),
                cb_st,
                check_st,
            )
            .show(ui);
        }

        if self.data.request_phone_number {
            ui.add_space(8.0);
            Checkbox::new(
                &mut self.share_phone_number,
                &format!("Share your phone number with {}", self.data.display_name),
                cb_st,
                check_st,
            )
            .show(ui);
        }

        if !self.data.match_codes.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 4.0, colors);
            ui.label(
                egui::RichText::new("Confirm login code:")
                    .font(FontId::new(12.5, family_semibold()))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for code in &self.data.match_codes {
                    let selected = self.selected_match_code.as_deref() == Some(code.as_str());
                    if ui.selectable_label(selected, code).clicked() {
                        self.selected_match_code = Some(code.clone());
                    }
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Phone Click Handler modal (`core/phone_click_handler.cpp`)
// ---------------------------------------------------------------------------

/// Strips formatting characters (`+`, spaces, hyphens, parentheses) from a phone number.
///
/// Mirrors `Trim(QString)` in `core/phone_click_handler.cpp:32-37`.
#[must_use]
pub fn normalize_phone_digits(raw: &str) -> String {
    raw.chars().filter(char::is_ascii_digit).collect()
}

/// Splits a display title into `(first_name, last_name)` for prefilling `AddContactBox`.
#[must_use]
pub fn split_display_name(full: &str) -> (String, String) {
    let trimmed = full.trim();
    if trimmed.is_empty() {
        return (String::new(), String::new());
    }
    match trimmed.split_once(char::is_whitespace) {
        Some((first, rest)) => (first.trim().to_owned(), rest.trim().to_owned()),
        None => (trimmed.to_owned(), String::new()),
    }
}

/// Action emitted by [`PhoneActionBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneActionBodyAction {
    /// Copy the phone number to clipboard.
    CopyPhone(String),
    /// Open `AddContactBox` pre-filled with name and phone.
    AddToContacts {
        /// Pre-filled first name.
        first_name: String,
        /// Pre-filled last name.
        last_name: String,
        /// Pre-filled phone number.
        phone: String,
    },
    /// Open the resolved Telegram user's profile (`InfoSection::Profile`).
    ViewProfile(PeerId),
}

/// Action modal for clicking a phone number entity (`core/phone_click_handler.cpp`).
#[derive(Debug, Clone)]
pub struct PhoneActionBoxState {
    /// Original clicked phone string (trimmed).
    pub phone: String,
    /// Digit-only normalized phone number.
    pub normalized_digits: String,
    /// Resolved Telegram user `(peer_id, display_name)` from `contacts.resolvePhone`, if found.
    pub resolved_peer: Option<(PeerId, String)>,
    /// `true` when the clicked number belongs to the current signed-in user.
    pub is_self_phone: bool,
    /// Optional informational note (e.g. `"Not registered on Telegram"`).
    pub status_note: Option<String>,
}

impl PhoneActionBoxState {
    /// Creates a [`PhoneActionBoxState`] from resolution results.
    #[must_use]
    pub fn new(
        phone: impl Into<String>,
        resolved_peer: Option<(PeerId, String)>,
        is_self_phone: bool,
        status_note: Option<String>,
    ) -> Self {
        let phone = phone.into().trim().to_owned();
        let normalized_digits = normalize_phone_digits(&phone);
        Self {
            phone,
            normalized_digits,
            resolved_peer,
            is_self_phone,
            status_note,
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Phone Number")
            .width(st::box_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_close()))
            .add_button(BoxButton::default_btn("copy_phone", "Copy Phone"))
    }

    /// Renders the phone action sheet body.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) -> Option<PhoneActionBodyAction> {
        let mut action = None;

        ui.label(
            egui::RichText::new(&self.phone)
                .font(FontId::new(16.0, family_semibold()))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(8.0);

        if ui.button("Copy Phone Number").clicked() {
            ui.ctx().copy_text(self.phone.clone());
            action = Some(PhoneActionBodyAction::CopyPhone(self.phone.clone()));
        }

        if !self.is_self_phone {
            ui.add_space(6.0);
            if ui.button("Add to Contacts").clicked() {
                let (first_name, last_name) = self
                    .resolved_peer
                    .as_ref()
                    .map(|(_, name)| split_display_name(name))
                    .unwrap_or_default();
                action = Some(PhoneActionBodyAction::AddToContacts {
                    first_name,
                    last_name,
                    phone: self.phone.clone(),
                });
            }
        }

        ui.add_space(10.0);
        divider_bar(ui, 4.0, colors);
        ui.add_space(6.0);

        if let Some((peer_id, ref name)) = self.resolved_peer {
            egui::Frame::NONE
                .fill(colors.color("windowBgOver"))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(name)
                            .font(FontId::new(13.5, family_semibold()))
                            .color(colors.color("windowFg")),
                    );
                    ui.add_space(4.0);
                    if ui.button("View Profile").clicked() {
                        action = Some(PhoneActionBodyAction::ViewProfile(peer_id));
                    }
                });
        } else {
            let note = self
                .status_note
                .as_deref()
                .unwrap_or("This phone number is not registered on Telegram.");
            ui.label(
                egui::RichText::new(note)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        }

        action
    }
}

// ---------------------------------------------------------------------------
// 3. Bank-Card Click Handler modal (`core/bank_card_click_handler.cpp`)
// ---------------------------------------------------------------------------

/// Action emitted by [`BankCardActionBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BankCardBodyAction {
    /// Copy the bank card number to clipboard.
    CopyNumber(String),
    /// Open one of the bank app/web URLs returned by `payments.getBankCardData`.
    OpenBankUrl(String),
}

/// Action modal for clicking a bank-card number entity (`core/bank_card_click_handler.cpp`).
#[derive(Debug, Clone)]
pub struct BankCardActionBoxState {
    /// Clicked bank card number.
    pub number: String,
    /// Issuing bank title returned by `payments.getBankCardData`, if resolved.
    pub title: Option<String>,
    /// Bank quick-open URLs returned by `payments.getBankCardData`.
    pub open_urls: Vec<BankCardOpenUrl>,
    /// Lookup error if `payments.getBankCardData` failed (copy remains available).
    pub lookup_error: Option<String>,
}

impl BankCardActionBoxState {
    /// Creates a [`BankCardActionBoxState`] from a successful `payments.getBankCardData` lookup.
    #[must_use]
    pub fn resolved(number: impl Into<String>, info: BankCardInfo) -> Self {
        let title = (!info.title.trim().is_empty()).then_some(info.title);
        Self {
            number: number.into().trim().to_owned(),
            title,
            open_urls: info.open_urls,
            lookup_error: None,
        }
    }

    /// Creates a [`BankCardActionBoxState`] when `payments.getBankCardData` fails (copy still works).
    #[must_use]
    pub fn failed(number: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            number: number.into().trim().to_owned(),
            title: None,
            open_urls: Vec::new(),
            lookup_error: Some(error.into()),
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Bank Card")
            .width(st::box_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_close()))
            .add_button(BoxButton::default_btn("copy_card", "Copy Card Number"))
    }

    /// Renders the bank-card action sheet body.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) -> Option<BankCardBodyAction> {
        let mut action = None;

        ui.label(
            egui::RichText::new(&self.number)
                .font(FontId::new(15.0, family_semibold()))
                .color(colors.color("windowBoldFg")),
        );

        if let Some(ref title) = self.title {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(title)
                    .font(FontId::new(13.0, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        }

        ui.add_space(8.0);
        if ui.button("Copy Card Number").clicked() {
            ui.ctx().copy_text(self.number.clone());
            action = Some(BankCardBodyAction::CopyNumber(self.number.clone()));
        }

        if !self.open_urls.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 4.0, colors);
            ui.add_space(6.0);
            for link in &self.open_urls {
                let label = if link.name.trim().is_empty() {
                    link.url.clone()
                } else {
                    format!("Open in {}", link.name)
                };
                if ui.button(label).clicked() {
                    action = Some(BankCardBodyAction::OpenBankUrl(link.url.clone()));
                }
                ui.add_space(4.0);
            }
        } else if let Some(text) = self
            .lookup_error
            .as_deref()
            .and_then(|err| tg_app_core::user_error::bank_card_data(err).into_text())
        {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(text)
                    .font(FontId::new(12.0, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        }

        action
    }
}

// ---------------------------------------------------------------------------
// 4. ModerateMessagesBox builder helper (shared by WindowController)
// ---------------------------------------------------------------------------

/// Outcome of executing a resolved OAuth URL authorization decision against `BotsBackend`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlAuthResolutionEffect {
    /// Open an external URL (either accepted redirect URL or fallback URL).
    OpenExternalUrl(String),
    /// Show a transient toast message.
    Toast(String),
    /// No visible UI action needed.
    None,
}

/// Executes `messages.acceptUrlAuth` or `messages.declineUrlAuth` for a resolved
/// [`UrlAuthConfirmBoxState`] decision.
pub fn execute_url_auth_decision(
    backend: &mut dyn tg_app_core::bots::backend::BotsBackend,
    data: UrlAuthConfirmationBox,
    choice: UrlAuthUserChoice,
) -> UrlAuthResolutionEffect {
    match build_accept_url_auth(data.source.clone(), choice) {
        AcceptUrlAuthDecision::Decline {
            url,
            open_fallback_url,
        } => {
            if !url.is_empty() {
                let _ = backend.decline_url_auth(&url);
            }
            if open_fallback_url && !url.is_empty() {
                UrlAuthResolutionEffect::OpenExternalUrl(url)
            } else {
                UrlAuthResolutionEffect::Toast("Authorization cancelled".to_owned())
            }
        }
        AcceptUrlAuthDecision::Accept(params) => match backend.accept_url_auth(&params) {
            Ok(result) => match handle_url_auth_accept_result(&data.source, result) {
                UrlAuthAcceptOutcome::OpenUrl { url, .. } => {
                    UrlAuthResolutionEffect::OpenExternalUrl(url)
                }
                UrlAuthAcceptOutcome::ShowSuccessToast => UrlAuthResolutionEffect::Toast(format!(
                    "Authorized login to {}",
                    data.display_name
                )),
                UrlAuthAcceptOutcome::DoneWithoutNavigation => UrlAuthResolutionEffect::None,
            },
            Err(err) => {
                let shown = match &data.source {
                    tg_bots::callback::UrlAuthSource::Url { .. } => {
                        tg_app_core::user_error::accept_url_auth_link(&err, &data.display_name)
                    }
                    _ => tg_app_core::user_error::accept_url_auth_button(&err),
                };
                shown.into_text().map_or(
                    UrlAuthResolutionEffect::None,
                    UrlAuthResolutionEffect::Toast,
                )
            }
        },
    }
}

/// Enriches raw history moderation participants with peer usernames and any linked community
/// before pushing [`crate::boxes::BoxKind::ModerateMessages`].
#[must_use]
pub fn build_moderate_box_state(
    peer_id: PeerId,
    peer_title: String,
    message_ids: Vec<i32>,
    mut participants: Vec<ModerateParticipant>,
    backend: &dyn InfoBackend,
) -> ModerateMessagesBoxState {
    for p in &mut participants {
        if p.username.is_none()
            && let Some(profile) = backend.peer_profile(p.peer_id)
        {
            p.username = profile.primary_username().map(ToOwned::to_owned);
        }
    }
    let state = ModerateMessagesBoxState::new(peer_id, peer_title, message_ids, participants);
    if let Some(comm) = backend.community_for_peer(peer_id) {
        state.with_linked_community(comm.id, comm.title)
    } else {
        state
    }
}
