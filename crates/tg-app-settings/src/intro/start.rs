//! Welcome cover step (`Intro::StartWidget`) and language switch link.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_start.h`,
//! `Telegram/SourceFiles/intro/intro_start.cpp`, and
//! `Telegram/SourceFiles/intro/intro_widget.cpp` (`createLanguageLink`).

use egui::{Align, Key, Layout, RichText, Ui};
use tg_lang::{Language, tr, with_current, with_current_mut};

use crate::intro::style::{INTRO_NEXT_BUTTON_WIDTH, link_button, primary_intro_button};
use tg_app_core::stand_in::theme::Palette;

/// Actions emitted by the Start step UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartStepAction {
    /// User clicked "Start Messaging" (or pressed Enter).
    StartMessaging,
    /// User clicked the language switch link below the primary button.
    ToggleLanguage,
}

/// Local UI state for the Start step (`intro_start.cpp`).
#[derive(Debug, Clone)]
pub struct StartStepState {
    /// Suggested alternate language code offered by the bottom switch link.
    pub suggested_lang_id: String,
    /// Localized prompt text for switching to the suggested language.
    pub suggested_lang_label: String,
}

impl Default for StartStepState {
    fn default() -> Self {
        Self::new()
    }
}

impl StartStepState {
    /// Creates a new `StartStepState` with a default alternate language suggestion.
    #[must_use]
    pub fn new() -> Self {
        Self {
            suggested_lang_id: "de".to_owned(),
            suggested_lang_label: "Auf Deutsch fortfahren".to_owned(),
        }
    }

    /// Returns the label for the language switch link (`Intro::Widget::createLanguageLink`).
    ///
    /// When the active `tg-lang` pack is not the default English pack, displays the original
    /// `"Continue in English"` (`get_original_value(lng_switch_to_this)`); otherwise offers
    /// the suggested language label.
    #[must_use]
    pub fn language_link_label(&self) -> String {
        let current_id = with_current(|inst| tg_lang::language_id_or_default(inst.id()).to_owned());
        let default_id = tg_lang::default_language_id();
        if current_id != default_id {
            tg_lang::get_original_value(tg_lang::key_ids::lng_switch_to_this).to_owned()
        } else {
            self.suggested_lang_label.clone()
        }
    }

    /// Toggles `tg-lang`'s active global language pack between default English and a sample
    /// localized intro pack (demonstrating live `tg-lang::tr::*` updates without network).
    pub fn toggle_language(&mut self) {
        let current_id = with_current(|inst| tg_lang::language_id_or_default(inst.id()).to_owned());
        let default_id = tg_lang::default_language_id();
        if current_id != default_id {
            with_current_mut(|inst| {
                inst.switch_to_id(&Language::default_english());
            });
        } else {
            let suggested = self.suggested_lang_id.clone();
            with_current_mut(|inst| {
                inst.switch_to_id(&Language {
                    id: suggested.clone(),
                    plural_id: suggested.clone(),
                    base_id: String::new(),
                    name: "German".to_owned(),
                    native_name: "Deutsch".to_owned(),
                });
                let diff = tg_lang::LangPackDifference {
                    lang_code: suggested,
                    from_version: 0,
                    version: 1,
                    strings: vec![
                        tg_lang::LangPackString::Normal {
                            key: "lng_intro_about".to_owned(),
                            value: "Willkommen bei Rustgram Desktop.\nSchnell und sicher."
                                .to_owned(),
                        },
                        tg_lang::LangPackString::Normal {
                            key: "lng_start_msgs".to_owned(),
                            value: "Messaging starten".to_owned(),
                        },
                        tg_lang::LangPackString::Normal {
                            key: "lng_intro_qr_title".to_owned(),
                            value: "Mit mobilem Telegram scannen".to_owned(),
                        },
                        tg_lang::LangPackString::Normal {
                            key: "lng_intro_qr_phone".to_owned(),
                            value: "Mit Telefonnummer anmelden".to_owned(),
                        },
                        tg_lang::LangPackString::Normal {
                            key: "lng_phone_title".to_owned(),
                            value: "Deine Telefonnummer".to_owned(),
                        },
                        tg_lang::LangPackString::Normal {
                            key: "lng_intro_next".to_owned(),
                            value: "Weiter".to_owned(),
                        },
                    ],
                };
                let _ = inst.apply_difference(tg_lang::Pack::Current, &diff);
            });
        }
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

            ui.add_space(18.0);
            let lang_label = self.language_link_label();
            if link_button(ui, &lang_label, palette).clicked() {
                action = Some(StartStepAction::ToggleLanguage);
            }
        });

        action
    }
}
