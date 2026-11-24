//! Language settings section with cloud list, search, selection, apply, and
//! `tg://setlanguage` / `t.me/setlanguage/...` confirmation dialogs.
//!
//! Port origin: `Telegram/SourceFiles/boxes/language_box.cpp` and
//! `ConfirmSwitchBox` / `NotReadyBox` / `showOfferSwitchBox` in
//! `Telegram/SourceFiles/lang/lang_cloud_manager.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, Frame, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_core::CoreSettings;
use tg_lang::{
    Instance as LangInstance, LangPackLanguageInfo, Language, format_set_language_link, tr,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RadioStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::checkbox::paint_radio_view;
use tg_ui::widgets::input_field::InputField;

use super::backend::{LanguagePrompt, SettingsBackend};
use super::strings;
use super::widgets::{
    paint_badge_pill, settings_divider, settings_row_pad_x, settings_toggle_row, subsection_title,
};

/// Persistent UI state for the Language section (`LanguageBox`).
#[derive(Debug, Clone, Default)]
pub struct LanguageState {
    /// Live search filter query.
    pub search_query: String,
    /// Currently applied active language code (e.g. `"en"`, `"de"`, `"#TEST_X"`).
    pub active_lang_code: String,
    /// Currently selected language code in the list (applied when clicking "Apply").
    pub selected_lang_code: String,
    /// Optional `tg://setlanguage?lang=...` / `t.me/setlanguage/...` link input.
    pub link_input: String,
    /// Local confirmation/notice prompt override (synced with `SettingsBackend`).
    pub pending_prompt: Option<LanguagePrompt>,
}

impl LanguageState {
    /// Creates a new [`LanguageState`] initialized to `active_lang_code`.
    #[must_use]
    pub fn new(active_lang_code: impl Into<String>) -> Self {
        let code = active_lang_code.into();
        Self {
            search_query: String::new(),
            selected_lang_code: code.clone(),
            active_lang_code: code,
            link_input: String::new(),
            pending_prompt: None,
        }
    }
}

/// Splits `recent` and `cloud` languages the way `PrepareLists()` does in `language_box.cpp:267`:
///
/// - Official/cloud languages are returned in `official` with any recently-used official entries
///   moved to the top in recency order.
/// - Non-official/custom recent languages remain in `non_official_recent`.
#[must_use]
pub fn prepare_language_lists(
    cloud: &[LangPackLanguageInfo],
    recent: &[Language],
    current_code: &str,
) -> (Vec<LangPackLanguageInfo>, Vec<LangPackLanguageInfo>) {
    let mut ordered_cloud = cloud.to_vec();
    let mut non_official = Vec::new();
    let mut front_idx = 0usize;

    for r in recent {
        if let Some(pos) = ordered_cloud[front_idx..]
            .iter()
            .position(|l| l.lang_code == r.id)
        {
            let abs_pos = front_idx + pos;
            let item = ordered_cloud.remove(abs_pos);
            ordered_cloud.insert(front_idx, item);
            front_idx += 1;
        } else if !non_official
            .iter()
            .any(|l: &LangPackLanguageInfo| l.lang_code == r.id)
        {
            non_official.push(LangPackLanguageInfo {
                official: false,
                rtl: false,
                beta: false,
                name: r.name.clone(),
                native_name: r.native_name.clone(),
                lang_code: r.id.clone(),
                base_lang_code: (!r.base_id.is_empty()).then(|| r.base_id.clone()),
                plural_code: r.plural_id.clone(),
                strings_count: 100,
                translated_count: 100,
                translations_url: format_set_language_link(&r.id),
            });
        }
    }

    // Ensure the currently active language is promoted to the front of its respective group.
    if let Some(pos) = non_official
        .iter()
        .position(|l| l.lang_code == current_code)
    {
        let cur = non_official.remove(pos);
        non_official.insert(0, cur);
    } else if let Some(pos) = ordered_cloud
        .iter()
        .position(|l| l.lang_code == current_code)
    {
        let cur = ordered_cloud.remove(pos);
        ordered_cloud.insert(0, cur);
    }

    (non_official, ordered_cloud)
}

/// Filters `languages` by `query` against English name, native name, and language code
/// (multi-word case-insensitive match mirroring `Rows::filter` in `language_box.cpp:680-725`).
#[must_use]
pub fn filter_languages<'a>(
    languages: &'a [LangPackLanguageInfo],
    query: &str,
) -> Vec<&'a LangPackLanguageInfo> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return languages.iter().collect();
    }
    languages
        .iter()
        .filter(|lang| {
            let name = lang.name.to_lowercase();
            let native = lang.native_name.to_lowercase();
            let code = lang.lang_code.to_lowercase();
            words
                .iter()
                .all(|w| name.contains(w) || native.contains(w) || code.contains(w))
        })
        .collect()
}

/// Builds a [`LanguagePrompt::ConfirmLinkSwitch`] or [`LanguagePrompt::NotReady`] from cloud
/// language metadata using exact `tg_lang::tr::*` strings (`ConfirmSwitchBox` / `NotReadyBox` in
/// `lang_cloud_manager.cpp:64-143`).
pub use super::backend::build_switch_confirmation_prompt;

/// Applies `info` to the live [`tg_lang::Instance`] and updates `state.active_lang_code`
/// and `state.selected_lang_code`.
pub fn apply_language_switch(
    lang_instance: &mut LangInstance,
    state: &mut LanguageState,
    info: &LangPackLanguageInfo,
) {
    let descriptor = info.to_language();
    lang_instance.switch_to_id(&descriptor);
    state.active_lang_code = info.lang_code.clone();
    state.selected_lang_code = info.lang_code.clone();
}

/// Renders the Language settings section (`language_box.cpp`).
pub fn render_language_section(
    ui: &mut Ui,
    state: &mut LanguageState,
    core: &mut CoreSettings,
    lang_instance: &mut LangInstance,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast = None;
    let pad_x = settings_row_pad_x();

    let backend_active = backend.active_language_code();
    if state.active_lang_code.is_empty() {
        let cur = if !backend_active.is_empty() {
            backend_active
        } else if !lang_instance.id().is_empty() {
            lang_instance.id().to_owned()
        } else {
            "en".to_owned()
        };
        state.active_lang_code = cur.clone();
        if state.selected_lang_code.is_empty() {
            state.selected_lang_code = cur;
        }
    } else if !backend_active.is_empty()
        && backend_active != state.active_lang_code
        && state.active_lang_code != "#TEST_X"
        && state.active_lang_code != "#TEST_0"
    {
        // Reflect external runtime switches (e.g., cloud diff or confirmed setlanguage link).
        if state.selected_lang_code == state.active_lang_code {
            state.selected_lang_code = backend_active.clone();
        }
        state.active_lang_code = backend_active;
    }
    if state.selected_lang_code.is_empty() {
        state.selected_lang_code = state.active_lang_code.clone();
    }

    // Sync any backend-originated prompt (e.g. from `help.getConfig` suggested_lang_code or a
    // `tg://setlanguage?lang=...` deep link).
    if state.pending_prompt.is_none()
        && let Some(backend_prompt) = backend.pending_language_prompt()
    {
        state.pending_prompt = Some(backend_prompt);
    }

    // Render active confirmation / suggested-language card at the top of the Language screen.
    if let Some(prompt) = state.pending_prompt.clone() {
        render_prompt_card(
            ui,
            &prompt,
            pad_x,
            state,
            lang_instance,
            backend,
            colors,
            &mut toast,
        );
        ui.add_space(6.0);
    }

    // Translation toggles at the top (matching Telegram Desktop's Language screen).
    subsection_title(ui, strings::LANG_TRANSLATE_MESSAGES, colors);
    settings_toggle_row(
        ui,
        strings::SHOW_TRANSLATE_BUTTON,
        Some(strings::SHOW_TRANSLATE_SUBTITLE),
        None,
        &mut core.translate_button_enabled,
        true,
        colors,
    );
    settings_toggle_row(
        ui,
        strings::TRANSLATE_ENTIRE_CHATS,
        None,
        None,
        &mut core.translate_chat_enabled,
        true,
        colors,
    );
    // Shown only when the platform has a provider (`language_box.cpp:1577`); Windows has none.
    if tg_platform::translate_win::is_translate_provider_available() {
        settings_toggle_row(
            ui,
            "Use system translation",
            Some("Prefer offline system translation before Telegram servers"),
            None,
            &mut core.use_platform_translation,
            true,
            colors,
        );
    }

    settings_divider(ui, None, colors);

    // Language Search + Apply bar.
    subsection_title(ui, strings::LANG_INTERFACE_LANGUAGE, colors);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x).max(180.0);
        InputField::single_line(
            "settings_lang_search",
            &mut state.search_query,
            strings::LANG_SEARCH_PH,
            InputFieldStyle::rounded_search(colors),
        )
        .width(avail)
        .show(ui);
    });
    ui.add_space(8.0);

    let all_languages = backend.languages();
    let recent_languages = backend.recent_languages();
    let (recent_custom, official_list) =
        prepare_language_lists(&all_languages, &recent_languages, &state.active_lang_code);

    let filtered_recent = filter_languages(&recent_custom, &state.search_query);
    let filtered_official = filter_languages(&official_list, &state.search_query);

    let mut copy_link_for: Option<String> = None;
    let mut remove_recent_id: Option<String> = None;

    if filtered_recent.is_empty() && filtered_official.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(tr::lng_languages_none())
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        });
    } else {
        let radio_st = RadioStyle::default_radio(colors);
        if !filtered_recent.is_empty() {
            subsection_title(ui, strings::LANG_RECENT_LANGUAGES, colors);
            for lang in &filtered_recent {
                render_language_row(
                    ui,
                    lang,
                    false,
                    pad_x,
                    &radio_st,
                    state,
                    colors,
                    &mut copy_link_for,
                    &mut remove_recent_id,
                );
            }
            if !filtered_official.is_empty() {
                settings_divider(ui, None, colors);
            }
        }
        for lang in &filtered_official {
            render_language_row(
                ui,
                lang,
                lang.official,
                pad_x,
                &radio_st,
                state,
                colors,
                &mut copy_link_for,
                &mut remove_recent_id,
            );
        }
    }

    if let Some(code) = copy_link_for {
        let link = format_set_language_link(&code);
        ui.ctx().copy_text(link.clone());
        toast = Some(format!("Copied {link}"));
    }
    if let Some(code) = remove_recent_id {
        backend.remove_recent_language(&code);
        toast = Some(format!("Removed recent language ({code})"));
    }

    // Explicit "Apply Language" button when the user has selected a different language.
    let pending_selection_differs =
        !state.selected_lang_code.is_empty() && state.selected_lang_code != state.active_lang_code;
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        if pending_selection_differs {
            let apply_label = tr::lng_language_switch_apply();
            if RoundButton::new(&apply_label, RoundButtonStyle::default_active(colors))
                .show(ui)
                .clicked()
            {
                let target_code = state.selected_lang_code.clone();
                match backend.apply_language(&target_code) {
                    Ok(display_name) => {
                        if let Some(info) = all_languages
                            .iter()
                            .chain(recent_custom.iter())
                            .find(|l| l.lang_code == target_code)
                        {
                            apply_language_switch(lang_instance, state, info);
                        } else {
                            state.active_lang_code = target_code.clone();
                        }
                        // Ensure the local settings `lang_instance` mirrors the global cloud pack.
                        tg_lang::with_current(|global| lang_instance.copy_from(global));
                        ui.ctx().request_repaint();
                        let label = if display_name.is_empty() {
                            target_code.clone()
                        } else {
                            display_name
                        };
                        toast = Some(format!(
                            "{}: {} ({})",
                            tr::lng_settings_language(),
                            label,
                            target_code
                        ));
                    }
                    Err(err) => {
                        toast = Some(err);
                    }
                }
            }
            let cancel_label = tr::lng_cancel();
            if RoundButton::new(&cancel_label, RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                state.selected_lang_code = state.active_lang_code.clone();
            }
        }
    });

    settings_divider(ui, None, colors);

    // Direct `tg://setlanguage?lang=...` / `t.me/setlanguage/...` link resolver.
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let input_w = (ui.available_width() - pad_x - 110.0).max(160.0);
        InputField::single_line(
            "settings_lang_link_input",
            &mut state.link_input,
            strings::LANG_LINK_PH,
            InputFieldStyle::default_input(colors),
        )
        .width(input_w)
        .show(ui);
        if RoundButton::new(
            strings::LANG_OPEN_LINK_BTN,
            RoundButtonStyle::default_light(colors),
        )
        .show(ui)
        .clicked()
        {
            let url = state.link_input.trim().to_owned();
            if !url.is_empty() {
                match backend.open_set_language_link(&url) {
                    Ok(Some(prompt)) => {
                        state.pending_prompt = Some(prompt);
                    }
                    Ok(None) => {
                        toast = Some(tr::lng_languages());
                    }
                    Err(err) => {
                        toast = Some(err);
                    }
                }
            }
        }
    });

    settings_divider(ui, None, colors);

    // Developer / Test pseudo-language toggle (`#TEST_X` in `tg-lang`).
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let active_label = format!(
            "{}: {} (plural rule: {:?})",
            tr::lng_languages_custom(),
            if lang_instance.id().is_empty() {
                "en"
            } else {
                lang_instance.id()
            },
            lang_instance.plural_rule()
        );
        ui.label(
            egui::RichText::new(active_label)
                .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                .color(colors.color("windowSubTextFg")),
        );
        if state.active_lang_code == "#TEST_X" {
            paint_badge_pill(ui, "TEST", colors.color("windowBgActive"), Color32::WHITE);
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        if RoundButton::new(
            strings::LANG_TEST_PSEUDO_BTN,
            RoundButtonStyle::default_light(colors),
        )
        .show(ui)
        .clicked()
        {
            let test_lang = Language {
                id: "#TEST_X".to_owned(),
                plural_id: "en".to_owned(),
                base_id: String::new(),
                name: "Test X".to_owned(),
                native_name: "Test X".to_owned(),
            };
            let _ = backend.apply_language("#TEST_X");
            lang_instance.switch_to_id(&test_lang);
            tg_lang::with_current_mut(|global| global.switch_to_id(&test_lang));
            state.active_lang_code = "#TEST_X".to_owned();
            state.selected_lang_code = "#TEST_X".to_owned();
            ui.ctx().request_repaint();
            toast = Some(strings::LANG_TEST_PSEUDO_TOAST.to_owned());
        }
    });

    ui.add_space(16.0);
    toast
}

#[allow(clippy::too_many_arguments)]
fn render_language_row(
    ui: &mut Ui,
    lang: &LangPackLanguageInfo,
    is_official: bool,
    pad_x: f32,
    radio_st: &RadioStyle,
    state: &mut LanguageState,
    colors: &dyn Colors,
    copy_link_for: &mut Option<String>,
    remove_recent_id: &mut Option<String>,
) {
    let selected = state.selected_lang_code == lang.lang_code;
    let is_active = state.active_lang_code == lang.lang_code;
    let w = ui.available_width().max(220.0);
    let h = 54.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    let radio_rect = Rect::from_min_size(
        Pos2::new(rect.left() + pad_x, rect.center().y - 10.0),
        Vec2::splat(20.0),
    );
    paint_radio_view(
        ui.painter(),
        radio_rect,
        radio_st,
        if selected { 1.0 } else { 0.0 },
        1.0,
    );

    let text_x = radio_rect.right() + 16.0;
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 9.0),
        Align2::LEFT_TOP,
        &lang.native_name,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );
    let mut subtitle = format!(
        "{}  ·  {}% translated",
        lang.name,
        lang.translated_percent()
    );
    if let Some(ref base) = lang.base_lang_code
        && !base.is_empty()
    {
        subtitle.push_str(&format!("  ·  base: {base}"));
    }
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 29.0),
        Align2::LEFT_TOP,
        subtitle,
        egui::FontId::new(12.0, tg_ui::fonts::family_regular()),
        colors.color("windowSubTextFg"),
    );

    // Right-side badges (BETA / UNOFFICIAL / RTL / ACTIVE / code).
    let mut tags: Vec<&str> = Vec::new();
    if !lang.official {
        tags.push("UNOFFICIAL");
    }
    if lang.beta {
        tags.push("BETA");
    }
    if lang.rtl {
        tags.push("RTL");
    }
    if tags.is_empty() {
        tags.push(lang.lang_code.as_str());
    }
    let badge_text = tags.join(" · ");
    ui.painter().text(
        Pos2::new(rect.right() - pad_x, rect.center().y),
        Align2::RIGHT_CENTER,
        badge_text,
        egui::FontId::new(11.5, tg_ui::fonts::family_regular()),
        if !lang.official || lang.beta {
            colors.color("windowActiveTextFg")
        } else {
            colors.color("windowSubTextFg")
        },
    );

    if resp.clicked() {
        state.selected_lang_code = lang.lang_code.clone();
    }

    // Context menu for non-official languages (`Rows::showMenu` in `language_box.cpp:560-643`).
    if !is_official {
        resp.context_menu(|ui| {
            if !lang.lang_code.starts_with('#') && ui.button(strings::LANG_COPY_LINK_BTN).clicked()
            {
                *copy_link_for = Some(lang.lang_code.clone());
                ui.close();
            }
            if !is_active && ui.button(strings::LANG_REMOVE_RECENT_BTN).clicked() {
                *remove_recent_id = Some(lang.lang_code.clone());
                ui.close();
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render_prompt_card(
    ui: &mut Ui,
    prompt: &LanguagePrompt,
    pad_x: f32,
    state: &mut LanguageState,
    lang_instance: &mut LangInstance,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    toast: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let card_w = (ui.available_width() - pad_x).max(220.0);
        Frame::NONE
            .fill(colors.color("windowBgOver"))
            .stroke(Stroke::new(1.0, colors.color("shadowFg")))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width((card_w - 24.0).max(180.0));
                match prompt {
                    LanguagePrompt::ConfirmLinkSwitch {
                        info,
                        title,
                        body,
                        apply_label,
                    } => {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(title)
                                    .font(font_id_from_style(st::semibold_font))
                                    .color(colors.color("windowBoldFg")),
                            );
                            if !info.official {
                                paint_badge_pill(
                                    ui,
                                    "UNOFFICIAL",
                                    colors.color("windowBgActive"),
                                    Color32::WHITE,
                                );
                            } else if info.beta {
                                paint_badge_pill(
                                    ui,
                                    "BETA",
                                    colors.color("windowBgActive"),
                                    Color32::WHITE,
                                );
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(body)
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowFg")),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if RoundButton::new(
                                apply_label,
                                RoundButtonStyle::default_active(colors),
                            )
                            .show(ui)
                            .clicked()
                            {
                                match backend.confirm_language_prompt() {
                                    Ok(applied) => {
                                        apply_language_switch(lang_instance, state, info);
                                        tg_lang::with_current(|global| {
                                            lang_instance.copy_from(global);
                                        });
                                        state.pending_prompt = None;
                                        ui.ctx().request_repaint();
                                        let name = applied.unwrap_or_else(|| info.name.clone());
                                        *toast = Some(format!(
                                            "{}: {name} ({})",
                                            tr::lng_settings_language(),
                                            info.lang_code
                                        ));
                                    }
                                    Err(err) => {
                                        *toast = Some(err);
                                    }
                                }
                            }
                            let cancel_label = tr::lng_cancel();
                            if RoundButton::new(
                                &cancel_label,
                                RoundButtonStyle::default_light(colors),
                            )
                            .show(ui)
                            .clicked()
                            {
                                backend.dismiss_language_prompt();
                                state.pending_prompt = None;
                            }
                        });
                    }
                    LanguagePrompt::NotReady { title, body, .. } => {
                        ui.label(
                            egui::RichText::new(title)
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowBoldFg")),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(body)
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowFg")),
                        );
                        ui.add_space(8.0);
                        let ok_label = tr::lng_box_ok();
                        if RoundButton::new(&ok_label, RoundButtonStyle::default_light(colors))
                            .show(ui)
                            .clicked()
                        {
                            backend.dismiss_language_prompt();
                            state.pending_prompt = None;
                        }
                    }
                    LanguagePrompt::AlreadyUsing { message, .. }
                    | LanguagePrompt::NotFound { message, .. } => {
                        ui.label(
                            egui::RichText::new(message)
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowFg")),
                        );
                        ui.add_space(8.0);
                        let ok_label = tr::lng_box_ok();
                        if RoundButton::new(&ok_label, RoundButtonStyle::default_light(colors))
                            .show(ui)
                            .clicked()
                        {
                            backend.dismiss_language_prompt();
                            state.pending_prompt = None;
                        }
                    }
                    LanguagePrompt::SuggestedLanguage {
                        language,
                        switch_cta,
                        prompt_text,
                    } => {
                        ui.label(
                            egui::RichText::new(prompt_text)
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowFg")),
                        );
                        ui.add_space(8.0);
                        let cta = if switch_cta.is_empty() {
                            strings::LANG_SUGGESTED_CHANGE_BTN.to_owned()
                        } else {
                            switch_cta.clone()
                        };
                        ui.horizontal(|ui| {
                            if RoundButton::new(&cta, RoundButtonStyle::default_active(colors))
                                .show(ui)
                                .clicked()
                            {
                                match backend.confirm_language_prompt() {
                                    Ok(applied) => {
                                        lang_instance.switch_to_id(language);
                                        tg_lang::with_current(|global| {
                                            lang_instance.copy_from(global);
                                        });
                                        state.active_lang_code = language.id.clone();
                                        state.selected_lang_code = language.id.clone();
                                        state.pending_prompt = None;
                                        ui.ctx().request_repaint();
                                        let name = applied.unwrap_or_else(|| language.name.clone());
                                        *toast = Some(format!(
                                            "{}: {name} ({})",
                                            tr::lng_settings_language(),
                                            language.id
                                        ));
                                    }
                                    Err(err) => {
                                        *toast = Some(err);
                                    }
                                }
                            }
                            let cancel_label = tr::lng_cancel();
                            if RoundButton::new(
                                &cancel_label,
                                RoundButtonStyle::default_light(colors),
                            )
                            .show(ui)
                            .clicked()
                            {
                                backend.dismiss_language_prompt();
                                state.pending_prompt = None;
                            }
                        });
                    }
                }
            });
    });
}
