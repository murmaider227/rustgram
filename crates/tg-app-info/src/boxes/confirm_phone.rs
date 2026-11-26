//! Phone-confirmation code modal (`Ui::ConfirmPhoneBox`) for `tg://confirmphone?...`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/ui/boxes/confirm_phone_box.h` & `.cpp`
//! - `Telegram/SourceFiles/api/api_confirm_phone.h` & `.cpp`

use egui::accesskit::Role;
use egui::{CornerRadius, CursorIcon, FontId, Key, Sense, Stroke, StrokeKind, Ui, WidgetInfo};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::FlatLabel;

/// Secondary action emitted from inside [`ConfirmPhoneCodeBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmPhoneBodyAction {
    /// User pressed Enter inside the code input (or hit auto-submit length).
    SubmitCode,
    /// User clicked the optional `"Open Fragment"` SMS delivery link.
    OpenFragmentUrl(String),
}

/// Formats a raw phone string from `tg://confirmphone?phone=...` with a single leading `+`.
#[must_use]
pub fn format_display_phone(phone: &str) -> String {
    let trimmed = phone.trim();
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.starts_with('+') {
        trimmed.to_owned()
    } else {
        format!("+{trimmed}")
    }
}

/// Stateful phone-confirmation verification code modal (`Ui::ConfirmPhoneBox`).
///
/// Opened after `account.sendConfirmPhoneCode` succeeds for a `tg://confirmphone?phone=...&hash=...`
/// deep link, and submits the entered code to `account.confirmPhone`.
#[derive(Debug, Clone)]
pub struct ConfirmPhoneCodeBoxState {
    /// Raw or formatted target phone number from the deep link.
    pub phone: String,
    /// Server-returned `phone_code_hash` from `account.sendConfirmPhoneCode`.
    pub phone_code_hash: String,
    /// Expected verification code length (typically 5 digits).
    pub code_length: usize,
    /// Current user-entered verification code (digits only).
    pub code_input: String,
    /// Optional Fragment SMS URL (`auth.sentCodeTypeFragmentSms.url`).
    pub fragment_url: Option<String>,
    /// Optional call/resend countdown in seconds.
    pub timeout_secs: Option<u32>,
    /// Validation or server RPC error text shown below the input field.
    pub error: Option<String>,
    /// True while an `account.confirmPhone` request is in flight.
    pub submitting: bool,
}

impl ConfirmPhoneCodeBoxState {
    /// Creates a new phone-confirmation code modal for `(phone, phone_code_hash)`.
    #[must_use]
    pub fn new(phone: impl Into<String>, phone_code_hash: impl Into<String>) -> Self {
        Self {
            phone: phone.into(),
            phone_code_hash: phone_code_hash.into(),
            code_length: 5,
            code_input: String::new(),
            fragment_url: None,
            timeout_secs: None,
            error: None,
            submitting: false,
        }
    }

    /// Sets the expected verification code length (for placeholder/auto-submit hints).
    #[must_use]
    pub const fn with_code_length(mut self, len: usize) -> Self {
        self.code_length = len;
        self
    }

    /// Attaches an optional Fragment SMS URL button (`tr::lng_intro_fragment_button()`).
    #[must_use]
    pub fn with_fragment_url(mut self, url: Option<String>) -> Self {
        self.fragment_url = url.filter(|u| !u.trim().is_empty());
        self
    }

    /// Returns the formatted phone number (`+<digits>`) shown in the modal copy.
    #[must_use]
    pub fn display_phone(&self) -> String {
        format_display_phone(&self.phone)
    }

    /// Extracts digits only from [`Self::code_input`] (`ConfirmPhoneBox::sendCode`).
    #[must_use]
    pub fn digits_only(&self) -> String {
        self.code_input
            .chars()
            .filter(char::is_ascii_digit)
            .collect()
    }

    /// Validates [`Self::code_input`] before dispatching `account.confirmPhone`.
    ///
    /// Sets [`Self::error`] to `tr::lng_confirm_phone_enter_code()` if no digits were entered.
    pub fn validate_code(&mut self) -> Result<String, String> {
        let digits = self.digits_only();
        if digits.is_empty() {
            let msg = tr::lng_confirm_phone_enter_code();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        self.error = None;
        Ok(digits)
    }

    /// Maps an `account.confirmPhone` RPC error string onto tdesktop's localized copy
    /// (`api_confirm_phone.cpp:135-141`).
    pub fn show_server_error(&mut self, rpc_err: &str) {
        self.submitting = false;
        let upper = rpc_err.to_ascii_uppercase();
        let display = if upper.contains("FLOOD_WAIT") || upper.contains("FLOOD") {
            tr::lng_flood_error()
        } else if upper.contains("PHONE_CODE_EMPTY")
            || upper.contains("PHONE_CODE_INVALID")
            || rpc_err.trim().is_empty()
        {
            tr::lng_bad_code()
        } else {
            // `Lang::Hard::ServerError()`; the raw error only goes to the log.
            let _ = tg_app_core::user_error::confirm_phone(rpc_err);
            tg_lang::hardcoded::server_error().to_owned()
        };
        self.error = Some(display);
    }

    /// Returns the localized success confirmation copy shown once `account.confirmPhone` succeeds
    /// (`api_confirm_phone.cpp:124-127`).
    #[must_use]
    pub fn success_message(&self) -> String {
        tr::lng_confirm_phone_success(&self.display_phone())
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor (`ConfirmPhoneBox::prepare`).
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_confirm_phone_title())
            .width(st::box_width.0 as f32 + 20.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn(
                "send_code",
                tr::lng_confirm_phone_send(),
            ))
    }

    /// Renders the verification code input, inline helper/error caption, optional Fragment button,
    /// and formatted-phone explanation copy (`ConfirmPhoneBox::paintEvent` / `resizeEvent`).
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
    ) -> Option<ConfirmPhoneBodyAction> {
        let mut action = None;

        // 1. Code input field (`st::confirmPhoneCodeField`, `tr::lng_code_ph()`)
        let prev = self.code_input.clone();
        let placeholder = tr::lng_code_ph();
        let field_resp = InputField::single_line(
            "confirm_phone_code_input",
            &mut self.code_input,
            &placeholder,
            InputFieldStyle::default_input(colors),
        )
        .error(self.error.is_some())
        .show(ui);
        field_resp.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, "Verification code"));

        // Sanitize non-digit characters while preserving paste convenience.
        if self.code_input != prev {
            let digits: String = self
                .code_input
                .chars()
                .filter(char::is_ascii_digit)
                .take(12)
                .collect();
            self.code_input = digits;
            self.error = None;
        }

        if field_resp.lost_focus() && ui.ctx().input(|i| i.key_pressed(Key::Enter)) {
            action = Some(ConfirmPhoneBodyAction::SubmitCode);
        }

        // 2. Helper / error caption directly below input (`ConfirmPhoneBox::paintEvent:125-137`)
        ui.add_space(6.0);
        if let Some(ref err) = self.error {
            ui.label(
                egui::RichText::new(err.as_str())
                    .font(FontId::new(12.5, family_semibold()))
                    .color(colors.color("boxTextFgError")),
            );
        } else {
            ui.label(
                egui::RichText::new(tr::lng_confirm_phone_enter_code())
                    .font(FontId::new(12.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        }

        // 3. Optional Fragment SMS link button
        if let Some(ref url) = self.fragment_url {
            ui.add_space(10.0);
            let btn_label = tr::lng_intro_fragment_button();
            let avail_w = ui.available_width().max(120.0);
            let (rect, resp) =
                ui.allocate_exact_size(egui::Vec2::new(avail_w, 34.0), Sense::click());
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, btn_label.clone()));
            let bg = if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                colors.color("windowBgOver")
            } else {
                colors.color("windowBg")
            };
            ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(6),
                Stroke::new(1.0, colors.color("windowActiveTextFg")),
                StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &btn_label,
                FontId::new(13.0, family_semibold()),
                colors.color("windowActiveTextFg"),
            );
            if resp.clicked() {
                action = Some(ConfirmPhoneBodyAction::OpenFragmentUrl(url.clone()));
            }
        }

        // 4. Explanation paragraph (`lng_confirm_phone_about`)
        ui.add_space(12.0);
        let about = tr::lng_confirm_phone_about(&self.display_phone());
        FlatLabel::plain(&about, FlatLabelStyle::box_label(colors))
            .id_salt("confirm_phone_about")
            .show(ui);

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_phone_validates_digits_and_maps_server_errors() {
        let mut box_state = ConfirmPhoneCodeBoxState::new("15550100", "hash_abc");
        assert_eq!(box_state.display_phone(), "+15550100");
        assert!(box_state.validate_code().is_err());
        assert!(box_state.error.is_some());

        box_state.code_input = "12-345".to_owned();
        assert_eq!(box_state.validate_code().expect("valid digits"), "12345");
        assert!(box_state.error.is_none());

        box_state.show_server_error("PHONE_CODE_INVALID");
        assert_eq!(
            box_state.error.as_deref(),
            Some(tr::lng_bad_code().as_str())
        );

        box_state.show_server_error("FLOOD_WAIT_30");
        assert_eq!(
            box_state.error.as_deref(),
            Some(tr::lng_flood_error().as_str())
        );
    }
}
