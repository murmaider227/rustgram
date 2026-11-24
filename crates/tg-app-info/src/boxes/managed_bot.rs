//! Managed Bots creation, Bot Info localization editor, and `tg://newbot` deep-link routing.
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/peers/create_managed_bot_box.h` / `.cpp`
//! - `Telegram/SourceFiles/boxes/peers/edit_bot_info_box.h` / `.cpp`
//! - `Telegram/SourceFiles/boxes/peers/edit_bot_info_box_content.cpp`
//! - `Telegram/SourceFiles/boxes/peers/translate_bot_info_box.cpp`
//! - `Telegram/SourceFiles/api/api_bot_info.cpp`
//! - `Telegram/SourceFiles/data/data_bot_info_draft.cpp`
//! - `Telegram/SourceFiles/data/data_bot_info_localization.cpp`

use egui::{FontId, Ui};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::info::backend::InfoBackend;
use crate::info::values::{MAX_GROUP_CHANNEL_TITLE, MAX_USERNAME_LENGTH, MIN_USERNAME_LENGTH};
use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::dto::managed_bot::*;

/// Create Managed Bot modal (`boxes/peers/create_managed_bot_box.cpp`).
#[derive(Debug, Clone)]
pub struct CreateManagedBotBoxState {
    /// Manager bot peer ID (`manager_id` in `bots.createBot`).
    pub manager_id: PeerId,
    /// Manager display name.
    pub manager_name: String,
    /// Manager handle without `@`.
    pub manager_username: String,
    /// Whether this box was launched via a `tg://newbot` deep link (`via_deeplink` flag).
    pub via_deeplink: bool,
    /// New bot display name.
    pub name: String,
    /// Raw username input before auto-completed suffix.
    pub username_input: String,
    /// Allowed bot username suffixes (always starts with `"bot"`).
    pub allowed_suffixes: Vec<String>,
    /// Current username validation / availability status.
    pub username_status: BotUsernameStatus,
    /// Whether user already encountered a taken username error.
    pub had_occupied_error: bool,
    /// Optional submit error message.
    pub error: Option<String>,
}

impl CreateManagedBotBoxState {
    /// Creates a new [`CreateManagedBotBoxState`].
    #[must_use]
    pub fn new(
        manager_id: PeerId,
        manager_name: impl Into<String>,
        manager_username: impl Into<String>,
        suggested_name: Option<String>,
        suggested_username: Option<String>,
        via_deeplink: bool,
    ) -> Self {
        let allowed_suffixes = vec!["bot".to_owned()];
        let raw_username = suggested_username
            .unwrap_or_default()
            .trim()
            .trim_start_matches('@')
            .to_owned();
        let username_input = enforce_bot_username_length(&raw_username, &allowed_suffixes);
        let mut state = Self {
            manager_id,
            manager_name: manager_name.into(),
            manager_username: manager_username.into(),
            via_deeplink,
            name: suggested_name.unwrap_or_default(),
            username_input,
            allowed_suffixes,
            username_status: BotUsernameStatus::Empty,
            had_occupied_error: false,
            error: None,
        };
        state.validate_syntax();
        state
    }

    /// Returns cleaned raw username without leading `@` or surrounding whitespace.
    #[must_use]
    pub fn cleaned_username(&self) -> String {
        self.username_input
            .trim()
            .trim_start_matches('@')
            .to_owned()
    }

    /// Returns the minimal missing suffix that completes the bot username.
    #[must_use]
    pub fn missing_suffix(&self) -> String {
        missing_bot_username_ending(&self.cleaned_username(), &self.allowed_suffixes)
    }

    /// Returns the complete bot username including auto-completed suffix (e.g. `myhelper_bot`).
    #[must_use]
    pub fn full_username(&self) -> String {
        let raw = self.cleaned_username();
        if raw.is_empty() {
            String::new()
        } else {
            format!("{raw}{}", self.missing_suffix())
        }
    }

    /// Validates username syntax locally (mirrors `usernameChanged` in `create_managed_bot_box.cpp`).
    pub fn validate_syntax(&mut self) {
        self.username_input =
            enforce_bot_username_length(&self.username_input, &self.allowed_suffixes);
        let raw = self.cleaned_username();
        if raw.is_empty() {
            self.username_status = BotUsernameStatus::Empty;
            return;
        }
        if !is_valid_bot_username_chars(&raw) {
            self.username_status = BotUsernameStatus::BadSymbols;
            return;
        }
        let full = self.full_username();
        if full.len() < MIN_USERNAME_LENGTH {
            self.username_status = BotUsernameStatus::TooShort;
            return;
        }
        self.username_status = BotUsernameStatus::Available(full);
    }

    /// Checks availability of [`Self::full_username`] against `backend` (`bots.checkUsername`).
    pub fn check_availability(&mut self, backend: &dyn InfoBackend) {
        self.validate_syntax();
        let BotUsernameStatus::Available(ref full) = self.username_status else {
            return;
        };
        match backend.check_bot_username(full) {
            Ok(true) => {
                self.error = None;
            }
            Ok(false) => {
                self.had_occupied_error = true;
                self.username_status = BotUsernameStatus::Taken;
            }
            Err(_) => {
                self.had_occupied_error = true;
                self.username_status = BotUsernameStatus::Taken;
            }
        }
    }

    /// Submits `bots.createBot` through `backend` and returns the created bot's [`PeerId`].
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<PeerId, String> {
        let name = self.name.trim().to_owned();
        if name.is_empty() {
            let msg = "Please enter a name for the bot.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        self.validate_syntax();
        let full_username = match &self.username_status {
            BotUsernameStatus::Available(u) => u.clone(),
            BotUsernameStatus::Empty => {
                let msg = "Please enter a username for the bot.".to_owned();
                self.error = Some(msg.clone());
                return Err(msg);
            }
            BotUsernameStatus::BadSymbols => {
                let msg = "Username can only contain a-z, 0-9, and underscores.".to_owned();
                self.error = Some(msg.clone());
                return Err(msg);
            }
            BotUsernameStatus::TooShort => {
                let msg = "Bot username must be at least 5 characters long.".to_owned();
                self.error = Some(msg.clone());
                return Err(msg);
            }
            BotUsernameStatus::Taken => {
                let msg = "Sorry, this username is already taken.".to_owned();
                self.error = Some(msg.clone());
                return Err(msg);
            }
        };

        match backend.create_managed_bot(self.manager_id, &name, &full_username, self.via_deeplink)
        {
            Ok(bot_id) => {
                self.error = None;
                Ok(bot_id)
            }
            Err(err) => {
                if err.contains("USERNAME_OCCUPIED") || err.contains("USERNAME_INVALID") {
                    self.had_occupied_error = true;
                    self.username_status = BotUsernameStatus::Taken;
                    let msg = "Sorry, this username is already taken.".to_owned();
                    self.error = Some(msg.clone());
                    Err(msg)
                } else if err.contains("BOT_CREATE_LIMIT_EXCEEDED") {
                    let msg =
                        "You have reached the maximum number of bots you can create.".to_owned();
                    self.error = Some(msg.clone());
                    Err(msg)
                } else {
                    self.error = Some(err.clone());
                    Err(err)
                }
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Create a Bot")
            .width(st::box_width.0 as f32 + 36.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("create_bot", "Create Bot"))
    }

    /// Renders the Create Managed Bot body.
    pub fn show_body(&mut self, ui: &mut Ui, backend: &dyn InfoBackend, colors: &dyn Colors) {
        let input_st = InputFieldStyle::default_input(colors);

        FlatLabel::plain(
            &format!(
                "{} (@{}) will manage this bot.",
                self.manager_name, self.manager_username
            ),
            FlatLabelStyle::box_label(colors),
        )
        .id_salt("create_managed_bot_subtitle")
        .show(ui);
        ui.add_space(10.0);

        let name_err = self.error.is_some() && self.name.trim().is_empty();
        InputField::single_line("managed_bot_name", &mut self.name, "Bot Name", input_st)
            .max_length(MAX_GROUP_CHANNEL_TITLE)
            .error(name_err)
            .show(ui);

        ui.add_space(8.0);
        let resp = InputField::single_line(
            "managed_bot_username",
            &mut self.username_input,
            "Username (ends with bot)",
            input_st,
        )
        .max_length(MAX_USERNAME_LENGTH)
        .show(ui);
        if resp.changed() {
            self.check_availability(backend);
        }

        // Status line mirroring create_managed_bot_box.cpp
        let (status_text, status_color) = match &self.username_status {
            BotUsernameStatus::Empty => (String::new(), colors.color("windowSubTextFg")),
            BotUsernameStatus::BadSymbols => (
                "Username can only contain a-z, 0-9, and underscores.".to_owned(),
                colors.color("boxTextFgError"),
            ),
            BotUsernameStatus::TooShort => (
                "Username must be at least 5 characters.".to_owned(),
                colors.color("boxTextFgError"),
            ),
            BotUsernameStatus::Taken => (
                "Sorry, this username is already taken.".to_owned(),
                colors.color("boxTextFgError"),
            ),
            BotUsernameStatus::Available(full) => {
                let text = if self.had_occupied_error {
                    format!("@{full} is available.")
                } else {
                    format!("Link: t.me/{full}")
                };
                let color = if self.had_occupied_error {
                    colors.color("boxTextFgGood")
                } else {
                    colors.color("windowSubTextFg")
                };
                (text, color)
            }
        };
        if !status_text.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(status_text)
                    .font(FontId::new(12.0, family_regular()))
                    .color(status_color),
            );
        }

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

/// Edit Managed Bot Info modal (`boxes/peers/edit_bot_info_box.cpp`).
#[derive(Debug, Clone)]
pub struct EditBotInfoBoxState {
    /// Bot peer ID.
    pub bot_id: PeerId,
    /// Bot username without `@`.
    pub bot_username: String,
    /// Active localization language code (`""` = Default).
    pub active_lang: String,
    /// Per-language original & edited drafts.
    pub draft: BotInfoDraft,
    /// Set of languages already loaded from backend.
    pub loaded_langs: Vec<String>,
    /// Status / toast banner inside the box.
    pub status_message: Option<String>,
    /// Optional error message.
    pub error: Option<String>,
    /// Animated live Bot Intro chat bubble preview (`boxes/peers/bot_intro_preview.cpp`).
    pub intro_preview: crate::boxes::bot_info_extras::BotIntroPreviewState,
}

/// Optional sub-action emitted by [`EditBotInfoBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditBotInfoSubAction {
    /// Open [`crate::boxes::bot_info_extras::TranslateBotInfoBoxState`] for multi-language batch translation.
    OpenTranslateBox,
}

impl EditBotInfoBoxState {
    /// Creates a new [`EditBotInfoBoxState`] for `bot_id`.
    #[must_use]
    pub fn new(bot_id: PeerId, bot_username: impl Into<String>) -> Self {
        let username = bot_username.into();
        let preview_name = if username.is_empty() {
            "Bot".to_owned()
        } else {
            format!("@{username}")
        };
        Self {
            bot_id,
            bot_username: username,
            active_lang: String::new(),
            draft: BotInfoDraft::default(),
            loaded_langs: Vec::new(),
            status_message: None,
            error: None,
            intro_preview: crate::boxes::bot_info_extras::BotIntroPreviewState::new(
                preview_name,
                None,
            ),
        }
    }

    /// Loads `lang_code` from `backend` (`bots.getBotInfo`) if not already loaded.
    pub fn ensure_lang_loaded(&mut self, lang_code: &str, backend: &dyn InfoBackend) {
        if self.loaded_langs.iter().any(|l| l == lang_code) {
            return;
        }
        let has_local_draft = self.draft.current.contains_key(lang_code);
        match backend.get_bot_info(self.bot_id, lang_code) {
            Ok(texts) => {
                if has_local_draft {
                    self.draft.original.insert(lang_code.to_owned(), texts);
                } else {
                    self.draft.set_loaded(lang_code, texts);
                }
                self.loaded_langs.push(lang_code.to_owned());
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
    }

    /// Translates the Default (`""`) bot info fields into `self.active_lang` via `backend`.
    pub fn translate_from_default(&mut self, backend: &dyn InfoBackend) -> Result<(), String> {
        if self.active_lang.is_empty() {
            return Err("Select a non-default language to translate into.".to_owned());
        }
        self.ensure_lang_loaded("", backend);
        let src = self.draft.current.get("").cloned().unwrap_or_default();
        let inputs = vec![src.name.clone(), src.about.clone(), src.description.clone()];
        let translated = backend.translate_bot_info_texts(&inputs, &self.active_lang)?;
        let target = self.draft.current_mut(&self.active_lang);
        if let Some(n) = translated.first() {
            target.name = n.clone();
        }
        if let Some(a) = translated.get(1) {
            target.about = a.clone();
        }
        if let Some(d) = translated.get(2) {
            target.description = d.clone();
        }
        self.status_message = Some(format!("Translated to '{}'", self.active_lang));
        self.error = None;
        Ok(())
    }

    /// Persists all modified languages via `bots.setBotInfo`.
    /// Returns the number of languages written.
    pub fn save_all(&mut self, backend: &mut dyn InfoBackend) -> Result<usize, String> {
        let langs: Vec<String> = self.draft.current.keys().cloned().collect();
        let mut saved = 0usize;
        for lang in langs {
            let change = self.draft.diff_for(&lang);
            if change.is_empty() {
                continue;
            }
            if let Err(err) = backend.set_bot_info(self.bot_id, &lang, &change) {
                self.error = Some(err.clone());
                return Err(err);
            }
            self.draft.commit_saved(&lang);
            saved += 1;
        }
        self.error = None;
        Ok(saved)
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let changes = self.draft.changes_count();
        let title = if changes > 0 {
            format!("Edit Bot Info ({changes} unsaved)")
        } else {
            "Edit Bot Info".to_owned()
        };
        BoxModal::new(id, title)
            .width(st::box_wide_width.0 as f32 + 36.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn(
                "save_bot_info",
                tr::lng_settings_save(),
            ))
    }

    /// Renders the Edit Bot Info localization editor body.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<EditBotInfoSubAction> {
        let mut sub_action = None;
        let active = self.active_lang.clone();
        self.ensure_lang_loaded(&active, backend);

        // Reserve 12px on the right so `TgScrollArea`'s scrollbar never overlaps counters or action buttons.
        let inner_w = (ui.available_width() - 12.0).max(200.0);
        ui.set_max_width(inner_w);

        // Language selector header + batch translation modal launcher (`Translate to...`).
        ui.horizontal(|ui| {
            FlatLabel::plain("Language", FlatLabelStyle::subsection_title(colors))
                .id_salt("bot_info_lang_hdr")
                .show(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let tr_label = tr::lng_bot_languages_translate();
                if tg_ui::widgets::RoundButton::new(
                    &tr_label,
                    tg_ui::stand_in::RoundButtonStyle::default_light(colors),
                )
                .show(ui)
                .clicked()
                {
                    sub_action = Some(EditBotInfoSubAction::OpenTranslateBox);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (code, label) in BOT_INFO_LANGUAGES {
                let selected = self.active_lang == code;
                let has_diff = !self.draft.diff_for(code).is_empty();
                let chip_label = if has_diff {
                    format!("{label} *")
                } else {
                    label.to_owned()
                };
                if ui.selectable_label(selected, chip_label).clicked() {
                    self.active_lang = code.to_owned();
                    self.ensure_lang_loaded(code, backend);
                }
            }
        });

        ui.add_space(8.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(6.0);

        let input_st = InputFieldStyle::default_input(colors);
        let cur = self.draft.current_mut(&self.active_lang);

        InputField::single_line("edit_bot_name", &mut cur.name, "Bot Name", input_st)
            .max_length(MAX_BOT_NAME_LENGTH)
            .show(ui);
        ui.add_space(8.0);

        InputField::multi_line(
            "edit_bot_about",
            &mut cur.about,
            "About (short bio)",
            input_st,
        )
        .max_length(MAX_BOT_ABOUT_LENGTH)
        .show(ui);
        ui.add_space(8.0);

        InputField::multi_line(
            "edit_bot_description",
            &mut cur.description,
            "Description (shown in empty chat)",
            input_st,
        )
        .max_length(MAX_BOT_DESCRIPTION_LENGTH)
        .show(ui);

        let preview_desc = cur.description.clone();
        if !preview_desc.is_empty() {
            ui.add_space(8.0);
            self.intro_preview.show(ui, &preview_desc, colors);
        }

        if !self.active_lang.is_empty() {
            ui.add_space(6.0);
            if ui.button("Translate from Default").clicked()
                && let Err(err) = self.translate_from_default(backend)
            {
                self.error = Some(err);
            }
        }

        if let Some(ref msg) = self.status_message {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(msg)
                    .font(FontId::new(12.0, family_regular()))
                    .color(colors.color("boxTextFgGood")),
            );
        }

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }

        sub_action
    }
}
