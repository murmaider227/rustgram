//! Welcome cover step (`Intro::StartWidget`) and language switch link.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_start.h`,
//! `Telegram/SourceFiles/intro/intro_start.cpp`, and
//! `Telegram/SourceFiles/intro/intro_widget.cpp` (`createLanguageLink`).

use egui::{Align, Key, Layout, RichText, Ui};
use tg_lang::{tr, with_current};

use crate::intro::language_link::LanguageSuggestion;
use crate::intro::style::{INTRO_NEXT_BUTTON_WIDTH, link_button, primary_intro_button};
use tg_app_core::stand_in::theme::Palette;

/// Actions emitted by the Start step UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartStepAction {
    /// User clicked "Start Messaging" (or pressed Enter).
    StartMessaging,
    /// User clicked the language link below the primary button (switch to the suggested
    /// language, or back to English).
    ToggleLanguage,
}

/// Local UI state for the Start step (`intro_start.cpp`).
#[derive(Debug, Clone)]
pub struct StartStepState {
    /// Server-suggested language and its `lng_switch_to_this` text; `None` until
    /// `help.getConfig` suggests a language whose strings loaded.
    pub suggestion: Option<LanguageSuggestion>,
    /// Whether the language link may be shown. tdesktop hides it when an account is already
    /// logged in (the add-account intro).
    pub language_link_enabled: bool,
}

impl Default for StartStepState {
    fn default() -> Self {
        Self::new()
    }
}

impl StartStepState {
    /// Creates a new `StartStepState` with no language suggestion (no link until the server
    /// suggests a language).
    #[must_use]
    pub fn new() -> Self {
        Self {
            suggestion: None,
            language_link_enabled: true,
        }
    }

    /// Returns the label for the language switch link, or `None` when there is no link
    /// (`Intro::Widget::createLanguageLink`).
    ///
    /// When the active `tg-lang` pack is not the default English pack, the link offers the
    /// original `"Continue in English"` (`get_original_value(lng_switch_to_this)`); otherwise it
    /// offers the server-suggested language with the server's `lng_switch_to_this` text.
    #[must_use]
    pub fn language_link_label(&self) -> Option<String> {
        let current_id = with_current(|inst| tg_lang::language_id_or_default(inst.id()).to_owned());
        self.language_link_label_for(&current_id)
    }

    /// [`Self::language_link_label`] for an explicit active language id.
    #[must_use]
    pub fn language_link_label_for(&self, current_id: &str) -> Option<String> {
        if !self.language_link_enabled {
            return None;
        }
        let current_id = tg_lang::language_id_or_default(current_id);
        if current_id != tg_lang::default_language_id() {
            return Some(
                tg_lang::get_original_value(tg_lang::key_ids::lng_switch_to_this).to_owned(),
            );
        }
        self.suggestion
            .as_ref()
            .filter(|s| s.lang_id != current_id)
            .map(|s| s.label.clone())
    }

    /// Renders the Start step body below the gradient cover (`StartWidget`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card_width: f32,
        palette: &Palette,
    ) -> Option<StartStepAction> {
        let mut action = None;
        let btn_width = card_width.min(INTRO_NEXT_BUTTON_WIDTH);

        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.label(
                RichText::new("Rustgram Desktop")
                    .size(22.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(10.0);

            let about = tr::lng_intro_about();
            ui.label(
                RichText::new(about)
                    .size(14.5)
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(34.0);

            let start_label = tr::lng_start_msgs();
            let start_btn = primary_intro_button(ui, btn_width, &start_label, palette);
            if start_btn.clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                action = Some(StartStepAction::StartMessaging);
            }

            if let Some(lang_label) = self.language_link_label() {
                ui.add_space(18.0);
                if link_button(ui, &lang_label, palette).clicked() {
                    action = Some(StartStepAction::ToggleLanguage);
                }
            }
        });

        action
    }
}
