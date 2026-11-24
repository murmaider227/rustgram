//! Terms of Service modal box with minimum-age confirmation and decline warning flow.
//!
//! Port origin: `Telegram/SourceFiles/window/window_session_controller.cpp`
//! (`TermsBox` / `showTermsOfService`).

use egui::Ui;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::FlatLabel;

use crate::info::backend::{InfoBackend, TermsOfServiceData};

/// Stateful Terms of Service modal (`TermsBox`).
#[derive(Debug, Clone)]
pub struct TermsOfServiceBoxState {
    /// Active ToS payload.
    pub data: TermsOfServiceData,
    /// User confirmation of the minimum age requirement (when `data.min_age_confirm.is_some()`).
    pub age_confirmed: bool,
    /// Validation error message if the user clicks Accept without confirming age.
    pub error: Option<String>,
}

impl TermsOfServiceBoxState {
    /// Initializes a Terms of Service dialog from [`TermsOfServiceData`].
    #[must_use]
    pub fn new(data: TermsOfServiceData) -> Self {
        Self {
            data,
            age_confirmed: false,
            error: None,
        }
    }

    /// Validates age confirmation (if required) and records acceptance in `backend`.
    pub fn accept(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        if let Some(min_age) = self.data.min_age_confirm
            && !self.age_confirmed
        {
            let msg = tr::lng_terms_age(i64::from(min_age));
            self.error = Some(msg.clone());
            return Err(msg);
        }
        backend.accept_terms(&self.data.id)?;
        self.data.accepted = true;
        self.error = None;
        Ok(())
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, &self.data.title)
            .width(st::box_wide_width.0 as f32 + 24.0)
            .with_left_button(BoxButton::attention("decline", tr::lng_terms_decline()))
            .add_button(BoxButton::default_btn("accept", tr::lng_terms_agree()))
    }

    /// Renders the Terms of Service body and optional minimum-age checkbox.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        FlatLabel::plain(&self.data.body, FlatLabelStyle::box_label(colors))
            .id_salt("tos_body")
            .show(ui);

        if let Some(min_age) = self.data.min_age_confirm {
            ui.add_space(12.0);
            let label = tr::lng_terms_age(i64::from(min_age));
            let resp = Checkbox::new(
                &mut self.age_confirmed,
                &label,
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);
            if resp.changed() {
                self.error = None;
            }
        }

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
