//! Add Contact modal box with live initials avatar and phone validation.
//!
//! Port origin: `Telegram/SourceFiles/boxes/add_contact_box.h`
//! and `Telegram/SourceFiles/boxes/add_contact_box.cpp` (`AddContactBox`).

use egui::{Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::input_field::InputField;

use crate::boxes::strings;
use crate::info::backend::InfoBackend;
use crate::info::values::{MAX_USER_FIRST_LAST_NAME, is_valid_phone};
use tg_app_core::stand_in::types::PeerId;

/// State for the Add Contact modal (`AddContactBox`).
#[derive(Debug, Clone, Default)]
pub struct AddContactBoxState {
    /// Contact first name (required).
    pub first_name: String,
    /// Contact last name (optional).
    pub last_name: String,
    /// Contact phone number (synthetic test number).
    pub phone: String,
    /// Validation error displayed beneath the inputs.
    pub error: Option<String>,
}

impl AddContactBoxState {
    /// Creates an empty Add Contact box state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an Add Contact box state pre-filled with a contact's name and phone number
    /// (used by `PhoneClickHandler` when adding a clicked phone number).
    #[must_use]
    pub fn with_prefill(
        first_name: impl Into<String>,
        last_name: impl Into<String>,
        phone: impl Into<String>,
    ) -> Self {
        Self {
            first_name: first_name.into(),
            last_name: last_name.into(),
            phone: phone.into(),
            error: None,
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_profile_add_contact())
            .width(st::box_wide_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("create", tr::lng_add_contact()))
    }

    /// Validates fields and creates the contact via `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<PeerId, String> {
        if self.first_name.trim().is_empty() {
            let msg = strings::ADD_CONTACT_FIRST_REQUIRED.to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        if !is_valid_phone(&self.phone) {
            let msg = strings::ADD_CONTACT_PHONE_INVALID.to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        match backend.add_contact(&self.first_name, &self.last_name, &self.phone) {
            Ok(id) => {
                self.error = None;
                Ok(id)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Renders the Add Contact modal interior.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let input_st = InputFieldStyle::default_input(colors);
        let full_name = format!("{} {}", self.first_name.trim(), self.last_name.trim());
        let fallback_name = tr::lng_confirm_contact_data();
        let display = if full_name.trim().is_empty() {
            fallback_name.as_str()
        } else {
            full_name.trim()
        };

        ui.horizontal(|ui| {
            let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
            EmptyUserpic::for_color_index(3, display, colors).paint_circle(ui.painter(), av_rect);

            ui.add_space(14.0);
            ui.vertical(|ui| {
                let field_w = ui.available_width().max(150.0);
                let first_err = self.error.is_some() && self.first_name.trim().is_empty();
                let first_ph = tr::lng_signup_firstname();
                InputField::single_line(
                    "add_contact_first",
                    &mut self.first_name,
                    &first_ph,
                    input_st,
                )
                .max_length(MAX_USER_FIRST_LAST_NAME)
                .error(first_err)
                .width(field_w)
                .show(ui);

                ui.add_space(4.0);
                InputField::single_line(
                    "add_contact_last",
                    &mut self.last_name,
                    strings::ADD_CONTACT_LAST_OPTIONAL,
                    input_st,
                )
                .max_length(MAX_USER_FIRST_LAST_NAME)
                .width(field_w)
                .show(ui);
            });
        });

        ui.add_space(10.0);
        let phone_err = self.error.is_some() && !is_valid_phone(&self.phone);
        InputField::single_line(
            "add_contact_phone",
            &mut self.phone,
            strings::ADD_CONTACT_PHONE_PH,
            input_st,
        )
        .error(phone_err)
        .show(ui);

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}
