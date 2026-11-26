//! Chat Settings section: embedded themes, accent colors, chat background picker,
//! text scale & font, send shortcuts, emoji/stickers, and media autoplay.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_chat.cpp`
//! and `Telegram/SourceFiles/boxes/background_box.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_core::settings::{DoubleClickQuickAction, InputSubmitSettings, QuickDialogAction};
use tg_core::{CoreSettings, GlobalConfig};
use tg_lang::tr;
use tg_theme::{
    Color, EmbeddedType, colorize_scheme, colorizer_from, default_accent_colors, embedded_scheme,
    embedded_themes, ensure_contrast,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use super::backend::SettingsBackend;
use super::strings;
use super::theme_bridge::{
    LiveThemeColors, core_color_to_theme, theme_color_to_core, theme_color_to_egui,
    theme_to_core_embedded,
};
use super::widgets::{
    SETTINGS_LIST_SKIP, SettingsIconKind, settings_card, settings_checkbox_row, settings_divider,
    settings_divider_text, settings_radio_row, settings_row, settings_row_pad_x,
    settings_section_divider, settings_toggle_row, subsection_title,
};

/// Persistent UI state for Chat Settings.
#[derive(Debug, Clone)]
pub struct ChatSettingsState {
    /// Selected built-in wallpaper preset ID.
    pub selected_bg_id: String,
    /// Tile background pattern toggle.
    pub bg_tiled: bool,
    /// Blur background toggle.
    pub bg_blurred: bool,
    /// Whether the custom accent hex editor is expanded.
    pub custom_accent_open: bool,
    /// Draft custom accent hex string (`#rrggbb`).
    pub custom_accent_hex: String,
    /// Whether the installed/archived sticker sets manager is expanded.
    pub sticker_sets_open: bool,
    /// Show archived sets tab inside sticker sets manager.
    pub sticker_sets_show_archived: bool,
    /// Whether the desktop Emoji Sets manager (`chat_helpers/emoji_sets_manager`) is expanded.
    pub emoji_sets_open: bool,
    /// Stateful desktop Emoji Sets manager (`Mac` / `Android` / `Twemoji` / `JoyPixels`).
    pub emoji_sets_manager: crate::chat_helpers::EmojiSetsManager,
    /// Whether custom font family editor is expanded.
    pub font_editor_open: bool,
    /// Whether "Choose from gallery" opened the Chat Background gallery box.
    pub gallery_open: bool,
    /// The system dark-mode value (`Core::Settings::systemDarkMode`), set by the window.
    /// `None`: the system reports none, and the "Auto-night mode" row is not shown.
    pub system_dark_mode: Option<bool>,
}

impl Default for ChatSettingsState {
    fn default() -> Self {
        Self {
            selected_bg_id: "classic_meadow".to_owned(),
            bg_tiled: false,
            bg_blurred: false,
            custom_accent_open: false,
            custom_accent_hex: "#40a7e3".to_owned(),
            sticker_sets_open: false,
            sticker_sets_show_archived: false,
            emoji_sets_open: false,
            emoji_sets_manager: crate::chat_helpers::EmojiSetsManager::default(),
            font_editor_open: false,
            gallery_open: false,
            system_dark_mode: None,
        }
    }
}

/// Side-effect events emitted by [`render_chat_settings_section`].
#[derive(Debug, Clone, Default)]
pub struct ChatSettingsOutput {
    /// Theme switched between light and dark (sync with shell `ThemeMode`).
    pub toggled_dark_mode: Option<bool>,
    /// Vertical folders sidebar toggled (sync with shell `folders_sidebar_enabled`).
    pub toggled_folders_sidebar: bool,
    /// User clicked "Choose from gallery": the Chat Background gallery box
    /// (`AppThemesHost` on [`crate::themes::ThemesTab::Backgrounds`]) was opened.
    pub open_theme_studio: bool,
    /// User clicked the "Keyboard Shortcuts" navigation row.
    pub open_shortcuts: bool,
    /// Toast message to display.
    pub toast: Option<String>,
}

/// Renders the Chat Settings (`settings_chat.cpp`) section.
#[allow(clippy::too_many_arguments)]
pub fn render_chat_settings_section(
    ui: &mut Ui,
    state: &mut ChatSettingsState,
    core: &mut CoreSettings,
    _global: &mut GlobalConfig,
    live_theme: &mut LiveThemeColors,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> ChatSettingsOutput {
    let mut out = ChatSettingsOutput::default();
    let was_dark = live_theme.is_dark();
    ui.spacing_mut().item_spacing.y = 0.0;

    // 1. Themes (`SetupThemeOptions` / `SetupDefaultThemes` in `settings_chat.cpp`).
    ui.add_space(st::settings_privacy_skip.scaled() as f32);
    subsection_title(ui, &tr::lng_settings_themes(), colors);
    ui.add_space(st::settings_themes_top_skip.scaled() as f32);
    render_embedded_theme_cards(ui, core, live_theme, colors, &mut out);

    // 2. Accent colors (`Window::Theme::DefaultAccentColors`) + "System accent color".
    render_accent_palette(ui, state, core, live_theme, colors, &mut out);
    if live_theme.is_dark() != was_dark {
        out.toggled_dark_mode = Some(live_theme.is_dark());
    }
    settings_checkbox_row(
        ui,
        &mut core.system_accent_color_enabled,
        &tr::lng_settings_theme_system_accent_color(),
        true,
        colors,
    );
    // `AddSkip` closing `SetupDefaultThemes`, another closing `SetupThemeOptions`, then
    // `SetupThemeSettings`: divider and `settingsPrivacySkip`.
    ui.add_space(SETTINGS_LIST_SKIP * 2.0);
    settings_section_divider(ui, colors);
    ui.add_space(st::settings_privacy_skip.scaled() as f32);

    // 3. Theme settings (`SetupThemeSettings`): name color, auto-night mode, font family.
    subsection_title(ui, &tr::lng_settings_theme_settings(), colors);
    let profile = backend.profile();
    let name_resp = settings_row(
        ui,
        &tr::lng_settings_theme_name_color(),
        None,
        None,
        Some(SettingsIconKind::NameColor),
        false,
        false,
        colors,
    );
    paint_name_color_pill(
        ui,
        &name_resp,
        &profile.first_name,
        profile.photo_color_index,
        colors,
    );
    if name_resp.clicked() {
        out.toast = Some(tr::lng_settings_theme_name_color());
    }

    // `SetupThemeSettings`: the row exists only `if (settings->systemDarkMode().has_value())`.
    if state.system_dark_mode.is_some() {
        let auto_night_label = if core.system_dark_mode_enabled {
            tr::lng_settings_auto_night_mode_on()
        } else {
            tr::lng_settings_auto_night_mode_off()
        };
        if settings_row(
            ui,
            &tr::lng_settings_auto_night_mode(),
            None,
            Some(&auto_night_label),
            Some(SettingsIconKind::NightMode),
            false,
            false,
            colors,
        )
        .clicked()
        {
            core.system_dark_mode_enabled = !core.system_dark_mode_enabled;
        }
    }

    let default_lbl = tr::lng_settings_call_device_default();
    let font_label = if core.custom_font_family.is_empty() {
        default_lbl.clone()
    } else {
        core.custom_font_family.clone()
    };
    let font_row_lbl = tr::lng_settings_font_family();
    if settings_row(
        ui,
        &font_row_lbl,
        None,
        Some(&font_label),
        Some(SettingsIconKind::Brush),
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.font_editor_open = !state.font_editor_open;
    }
    if state.font_editor_open {
        ui.add_space(4.0);
        settings_card(ui, colors, |ui| {
            ui.horizontal_wrapped(|ui| {
                for preset in ["", "Segoe UI", "Inter", "Consolas", "Arial"] {
                    let label = if preset.is_empty() {
                        default_lbl.as_str()
                    } else {
                        preset
                    };
                    if ui
                        .selectable_label(core.custom_font_family == preset, label)
                        .clicked()
                    {
                        core.custom_font_family = preset.to_owned();
                        out.toast = Some(format!("{font_row_lbl}: {label}"));
                    }
                }
            });
            ui.add_space(6.0);
            InputField::single_line(
                "settings_custom_font",
                &mut core.custom_font_family,
                strings::CUSTOM_FONT_PH,
                InputFieldStyle::default_input(colors),
            )
            .show(ui);
        });
    }
    ui.add_space(st::settings_checkboxes_skip.scaled() as f32);
    settings_section_divider(ui, colors);
    ui.add_space(SETTINGS_LIST_SKIP);

    // 4. Chat wallpaper (`SetupChatBackground` / `BackgroundRow`): current wallpaper thumbnail
    // with "Choose from gallery" / "Choose from file" links.
    subsection_title(ui, &tr::lng_settings_section_background(), colors);
    render_background_row(ui, state, colors, &mut out);
    ui.add_space(st::settings_tile_skip.scaled() as f32);
    settings_section_divider(ui, colors);
    ui.add_space(SETTINGS_LIST_SKIP);

    // 5. Chat list quick action (`SetupChatListQuickAction`).
    let dialog_quick_title = tr::lng_settings_quick_dialog_action_title();
    let dialog_quick_about = tr::lng_settings_quick_dialog_action_about();
    subsection_title(ui, &dialog_quick_title, colors);
    let dialog_quick_val = match core.quick_dialog_action {
        QuickDialogAction::Mute => tr::lng_settings_quick_dialog_action_mute(),
        QuickDialogAction::Pin => tr::lng_settings_quick_dialog_action_pin(),
        QuickDialogAction::Read => tr::lng_settings_quick_dialog_action_read(),
        QuickDialogAction::Archive => tr::lng_settings_quick_dialog_action_archive(),
        QuickDialogAction::Delete => tr::lng_settings_quick_dialog_action_delete(),
        QuickDialogAction::Disabled => tr::lng_settings_quick_dialog_action_disabled(),
    };
    if settings_row(
        ui,
        &dialog_quick_val,
        None,
        None,
        Some(SettingsIconKind::Folder),
        false,
        false,
        colors,
    )
    .clicked()
    {
        core.quick_dialog_action = match core.quick_dialog_action {
            QuickDialogAction::Disabled => QuickDialogAction::Mute,
            QuickDialogAction::Mute => QuickDialogAction::Pin,
            QuickDialogAction::Pin => QuickDialogAction::Read,
            QuickDialogAction::Read => QuickDialogAction::Archive,
            QuickDialogAction::Archive => QuickDialogAction::Delete,
            QuickDialogAction::Delete => QuickDialogAction::Disabled,
        };
    }
    ui.add_space(SETTINGS_LIST_SKIP);
    settings_divider_text(ui, &dialog_quick_about, colors);
    ui.add_space(SETTINGS_LIST_SKIP);

    // 5. Messages & Layout options.
    subsection_title(ui, strings::MESSAGES_SECTION, colors);
    settings_radio_row(
        ui,
        &mut core.send_submit_way,
        InputSubmitSettings::Enter,
        &tr::lng_settings_send_enter(),
        true,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.send_submit_way,
        InputSubmitSettings::CtrlEnter,
        &tr::lng_settings_send_ctrlenter(),
        true,
        colors,
    );

    ui.add_space(6.0);
    let quick_action_label = match core.chat_quick_action {
        DoubleClickQuickAction::Reply => strings::QUICK_ACTION_REPLY,
        DoubleClickQuickAction::React => strings::QUICK_ACTION_REACT,
        DoubleClickQuickAction::None => strings::QUICK_ACTION_DISABLED,
    };
    if settings_row(
        ui,
        strings::DOUBLE_CLICK_MESSAGE,
        None,
        Some(quick_action_label),
        None,
        false,
        false,
        colors,
    )
    .clicked()
    {
        core.chat_quick_action = match core.chat_quick_action {
            DoubleClickQuickAction::Reply => DoubleClickQuickAction::React,
            DoubleClickQuickAction::React => DoubleClickQuickAction::None,
            DoubleClickQuickAction::None => DoubleClickQuickAction::Reply,
        };
    }

    let adaptive_lbl = tr::lng_settings_adaptive_wide();
    settings_toggle_row(
        ui,
        &adaptive_lbl,
        None,
        None,
        &mut core.adaptive_for_wide,
        true,
        colors,
    );

    let mut vertical_sidebar = !core.chat_filters_horizontal;
    if settings_toggle_row(
        ui,
        strings::VERTICAL_FOLDERS_SIDEBAR,
        Some(strings::VERTICAL_FOLDERS_SUBTITLE),
        None,
        &mut vertical_sidebar,
        true,
        colors,
    ) {
        core.chat_filters_horizontal = !vertical_sidebar;
        out.toggled_folders_sidebar = true;
    }

    let corner_reply_lbl = tr::lng_settings_chat_corner_reply();
    settings_toggle_row(
        ui,
        &corner_reply_lbl,
        None,
        None,
        &mut core.corner_reply,
        true,
        colors,
    );
    let corner_react_lbl = tr::lng_settings_chat_corner_reaction();
    settings_toggle_row(
        ui,
        &corner_react_lbl,
        None,
        None,
        &mut core.corner_reaction,
        true,
        colors,
    );
    settings_toggle_row(
        ui,
        strings::PULL_NEXT_CHANNEL,
        None,
        None,
        &mut core.pull_to_next_channel,
        true,
        colors,
    );
    if settings_row(
        ui,
        "Keyboard Shortcuts",
        None,
        None,
        None,
        false,
        true,
        colors,
    )
    .clicked()
    {
        out.open_shortcuts = true;
    }

    settings_divider(ui, None, colors);

    // 6. Stickers & Emoji options (`CoreSettings::large_emoji`, etc. + sticker sets manager).
    subsection_title(ui, &tr::lng_settings_stickers_emoji(), colors);
    let large_emoji_lbl = tr::lng_settings_large_emoji();
    settings_toggle_row(
        ui,
        &large_emoji_lbl,
        None,
        None,
        &mut core.large_emoji,
        true,
        colors,
    );
    let replace_emoji_lbl = tr::lng_settings_replace_emojis();
    settings_toggle_row(
        ui,
        &replace_emoji_lbl,
        None,
        None,
        &mut core.replace_emoji,
        true,
        colors,
    );
    let suggest_emoji_lbl = tr::lng_settings_suggest_emoji();
    settings_toggle_row(
        ui,
        &suggest_emoji_lbl,
        None,
        None,
        &mut core.suggest_emoji,
        true,
        colors,
    );
    let suggest_by_emoji_lbl = tr::lng_settings_suggest_by_emoji();
    settings_toggle_row(
        ui,
        &suggest_by_emoji_lbl,
        None,
        None,
        &mut core.suggest_stickers_by_emoji,
        true,
        colors,
    );
    let suggest_anim_lbl = tr::lng_settings_suggest_animated_emoji();
    settings_toggle_row(
        ui,
        &suggest_anim_lbl,
        None,
        None,
        &mut core.suggest_animated_emoji,
        true,
        colors,
    );
    let loop_stickers_lbl = tr::lng_settings_loop_stickers();
    settings_toggle_row(
        ui,
        &loop_stickers_lbl,
        None,
        None,
        &mut core.loop_animated_stickers,
        true,
        colors,
    );

    let sets = backend.sticker_sets();
    let active_sets_count = sets.iter().filter(|s| !s.archived).count().to_string();
    if settings_row(
        ui,
        strings::MANAGE_STICKER_SETS,
        None,
        Some(&active_sets_count),
        Some(SettingsIconKind::Sticker),
        false,
        true,
        colors,
    )
    .clicked()
    {
        state.sticker_sets_open = !state.sticker_sets_open;
    }

    if state.sticker_sets_open {
        ui.add_space(4.0);
        render_sticker_sets_manager(ui, state, backend, colors, &mut out);
    }

    let active_emoji_set_name = crate::chat_helpers::EmojiSetsManager::descriptor(
        state.emoji_sets_manager.current_set_id(),
    )
    .map_or("Mac", |d| d.name);
    if settings_row(
        ui,
        "Emoji Sets",
        None,
        Some(active_emoji_set_name),
        Some(SettingsIconKind::Sticker),
        false,
        true,
        colors,
    )
    .clicked()
    {
        state.emoji_sets_open = !state.emoji_sets_open;
    }

    if state.emoji_sets_open {
        ui.add_space(4.0);
        let palette = tg_app_core::stand_in::theme::Palette::for_mode(
            if matches!(
                live_theme.kind,
                EmbeddedType::Night | EmbeddedType::NightGreen
            ) {
                tg_app_core::stand_in::theme::ThemeMode::Night
            } else {
                tg_app_core::stand_in::theme::ThemeMode::Day
            },
        );
        if let Some(act) = state.emoji_sets_manager.show_manage_sets_box(ui, &palette) {
            match act {
                crate::chat_helpers::ManageEmojiSetsAction::Switched(id) => {
                    let name = crate::chat_helpers::EmojiSetsManager::descriptor(id)
                        .map_or("Custom", |d| d.name);
                    out.toast = Some(format!("Emoji set switched to {name}"));
                }
                crate::chat_helpers::ManageEmojiSetsAction::DownloadAndSwitch(id) => {
                    let name = crate::chat_helpers::EmojiSetsManager::descriptor(id)
                        .map_or("Custom", |d| d.name);
                    match backend.download_and_switch_emoji_set(&mut state.emoji_sets_manager, id) {
                        Ok(_) => {
                            out.toast = Some(format!("Emoji set switched to {name}"));
                        }
                        Err(err) => {
                            if matches!(
                                state.emoji_sets_manager.state_for(id),
                                crate::chat_helpers::EmojiSetState::Loading { .. }
                            ) {
                                state.emoji_sets_manager.mark_failed(id, err.clone());
                            }
                            out.toast =
                                tg_app_core::user_error::emoji_set_download(&err).into_text();
                        }
                    }
                }
                crate::chat_helpers::ManageEmojiSetsAction::Close => {
                    state.emoji_sets_open = false;
                }
            }
        }
    }

    settings_divider(ui, None, colors);

    // 7. Media Autoplay (`CoreSettings::autoplay`, mirrored to `AutoDownloadSettings::autoplay_*`).
    subsection_title(ui, &tr::lng_media_auto_play(), colors);
    let mut mirror_to_backend = false;
    if settings_toggle_row(
        ui,
        strings::AUTOPLAY_VIDEOS,
        None,
        None,
        &mut core.autoplay.videos,
        true,
        colors,
    ) {
        mirror_to_backend = true;
    }
    if settings_toggle_row(
        ui,
        strings::AUTOPLAY_GIFS,
        None,
        None,
        &mut core.autoplay.gifs,
        true,
        colors,
    ) {
        mirror_to_backend = true;
    }
    if settings_toggle_row(
        ui,
        strings::AUTOPLAY_ROUND_VIDEOS,
        None,
        None,
        &mut core.autoplay.round_videos,
        true,
        colors,
    ) {
        mirror_to_backend = true;
    }
    settings_toggle_row(
        ui,
        strings::AUTOPLAY_VIDEO_STICKERS,
        None,
        None,
        &mut core.autoplay.video_stickers,
        true,
        colors,
    );
    if mirror_to_backend {
        sync_autoplay_to_backend(core, backend);
    }

    settings_divider(ui, None, colors);

    // 8. Message Translation toggles.
    subsection_title(ui, strings::TRANSLATION_SECTION, colors);
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

    ui.add_space(16.0);
    out
}

/// Mirrors the typed [`tg_core::settings::AutoplaySettings`] on `core` into the settings backend's
/// `AutoDownloadSettings` so legacy bridges stay in sync with `CoreSettings::autoplay`.
pub fn sync_autoplay_to_backend(core: &CoreSettings, backend: &mut dyn SettingsBackend) {
    let mut ad = backend.auto_download();
    ad.autoplay_videos = core.autoplay.videos;
    ad.autoplay_gifs = core.autoplay.gifs;
    ad.autoplay_round_videos = core.autoplay.round_videos;
    backend.set_auto_download(ad);
}

fn localized_theme_name(kind: EmbeddedType) -> String {
    match kind {
        EmbeddedType::Default => tr::lng_settings_theme_classic(),
        EmbeddedType::DayBlue => tr::lng_settings_theme_day(),
        EmbeddedType::Night => tr::lng_settings_theme_tinted(),
        EmbeddedType::NightGreen => tr::lng_settings_theme_night(),
    }
}

fn render_embedded_theme_cards(
    ui: &mut Ui,
    core: &CoreSettings,
    live_theme: &mut LiveThemeColors,
    colors: &dyn Colors,
    out: &mut ChatSettingsOutput,
) {
    // `SetupDefaultThemes`: 80x92 previews spread across the row, two 40x14 bubbles at
    // (6, 8) with a 6 px skip, a radio 12 px above the bottom, the name below (`settingsTheme`).
    let themes = embedded_themes();
    let pad_x = settings_row_pad_x();
    let card_w = 80.0_f32;
    let preview_h = 92.0_f32;
    let bubble = Vec2::new(40.0, 14.0);
    let bubble_r = st::settings_theme_bubble_radius.scaled().clamp(1, 12) as u8;
    let count = themes.len().max(1) as f32;
    let full_w = ui.available_width();
    let avail = (full_w - pad_x * 2.0).max(card_w * count);
    let gap = ((avail - card_w * count) / (count - 1.0).max(1.0)).max(4.0);
    let label_h = ui.fonts_mut(|f| f.row_height(&font_id_from_style(st::normal_font)));
    let block_h = (st::settings_theme.text_position.y.scaled() as f32 + label_h).round();
    let (row, _) = ui.allocate_exact_size(Vec2::new(full_w, block_h), Sense::hover());

    for (i, mut scheme) in themes.into_iter().enumerate() {
        let core_kind = theme_to_core_embedded(scheme.kind);
        let custom_accent = core
            .themes_accent_colors
            .get(core_kind)
            .map(core_color_to_theme);
        if let Some(acc) = custom_accent {
            let colorizer = colorizer_from(&scheme, acc);
            colorize_scheme(&mut scheme, &colorizer);
        }

        let selected = live_theme.kind == scheme.kind;
        let display_name = localized_theme_name(scheme.kind);
        let left = row.left() + pad_x + i as f32 * (card_w + gap);
        let rect = Rect::from_min_size(Pos2::new(left, row.top()), Vec2::new(card_w, row.height()));
        let resp = ui.interact(rect, ui.scope_id().with(("theme_card", i)), Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, &display_name)
        });
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            live_theme.update(scheme.kind, custom_accent);
            out.toast = Some(format!("{}: {display_name}", tr::lng_settings_themes()));
        }

        let preview = Rect::from_min_size(rect.min, Vec2::new(card_w, preview_h));
        ui.painter().rect_filled(
            preview,
            CornerRadius::same(4),
            theme_color_to_egui(scheme.background),
        );
        let in_bubble = Rect::from_min_size(preview.min + Vec2::new(6.0, 8.0), bubble);
        let out_bubble = Rect::from_min_size(
            Pos2::new(preview.right() - 6.0 - bubble.x, in_bubble.bottom() + 6.0),
            bubble,
        );
        ui.painter().rect_filled(
            in_bubble,
            CornerRadius::same(bubble_r),
            theme_color_to_egui(scheme.received),
        );
        ui.painter().rect_filled(
            out_bubble,
            CornerRadius::same(bubble_r),
            theme_color_to_egui(scheme.sent),
        );

        // `CloudListCheck`: a `st::default_radio` (22 px, 2 px ring inset by `outerSkip`, 10 px
        // dot) `settingsThemeRadioBottom` above the bottom. `ensureContrast` gives both states
        // the active color made readable over the background, the untoggled one at alpha 192.
        let radio = &st::default_radio;
        let diameter = radio.diameter.scaled() as f32;
        let thickness = radio.thickness.scaled() as f32;
        let radio_c = Pos2::new(
            preview.center().x,
            preview.bottom() - st::settings_theme_radio_bottom.scaled() as f32 - diameter * 0.5,
        );
        let contrast = ensure_contrast(scheme.radiobutton_active, scheme.background);
        let radio_fg = if selected {
            Color32::from_rgb(contrast.r, contrast.g, contrast.b)
        } else {
            Color32::from_rgba_unmultiplied(contrast.r, contrast.g, contrast.b, 192)
        };
        let ring_skip = radio.outer_skip.scaled() as f32 / 10.0 + thickness * 0.5;
        ui.painter().circle_stroke(
            radio_c,
            diameter * 0.5 - ring_skip,
            Stroke::new(thickness, radio_fg),
        );
        if selected {
            let dot_skip = radio.skip.scaled() as f32 / 10.0;
            ui.painter()
                .circle_filled(radio_c, diameter * 0.5 - dot_skip, radio_fg);
        }

        ui.painter().text(
            Pos2::new(rect.center().x, preview.bottom() + 7.0),
            Align2::CENTER_TOP,
            &display_name,
            font_id_from_style(st::normal_font),
            if selected {
                colors.color("windowActiveTextFg")
            } else {
                colors.color("windowSubTextFg")
            },
        );
    }
}

/// Paints the "Your name color" value: the first name in the user's peer color on a tinted pill.
fn paint_name_color_pill(
    ui: &Ui,
    row: &egui::Response,
    name: &str,
    color_index: u8,
    colors: &dyn Colors,
) {
    if name.is_empty() {
        return;
    }
    let token = format!("historyPeer{}NameFg", (color_index % 7) + 1);
    let mut fg = colors.color(&token);
    if fg.a() == 0 {
        fg = colors.color("windowActiveTextFg");
    }
    let font = font_id_from_style(st::normal_font);
    let galley = ui.painter().layout_no_wrap(name.to_owned(), font, fg);
    let pad = 8.0_f32;
    let pill = Rect::from_min_size(
        Pos2::new(
            row.rect.right() - settings_row_pad_x() - galley.size().x - pad * 2.0 + 8.0,
            row.rect.center().y - 11.0,
        ),
        Vec2::new(galley.size().x + pad * 2.0, 22.0),
    );
    ui.painter().rect_filled(
        pill,
        CornerRadius::same(11),
        Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 36),
    );
    ui.painter().galley(
        Pos2::new(pill.left() + pad, pill.center().y - galley.size().y * 0.5),
        galley,
        fg,
    );
}

/// Current wallpaper thumbnail with the "Choose from gallery" / "Choose from file" links
/// (`BackgroundRow` in `settings_chat.cpp`).
fn render_background_row(
    ui: &mut Ui,
    state: &mut ChatSettingsState,
    colors: &dyn Colors,
    out: &mut ChatSettingsOutput,
) {
    // `BackgroundRow` (76 px thumb) inside `settingsBackgroundPadding` (22, 11, 10, 12).
    let pad = st::settings_background_padding;
    let thumb = st::settings_background_thumb.scaled() as f32;
    let full_w = ui.available_width();
    let row_h = pad.top.scaled() as f32 + thumb + pad.bottom.scaled() as f32;
    let (row, _) = ui.allocate_exact_size(Vec2::new(full_w, row_h), Sense::hover());
    let thumb_rect = Rect::from_min_size(
        Pos2::new(
            row.left() + pad.left.scaled() as f32,
            row.top() + pad.top.scaled() as f32,
        ),
        Vec2::splat(thumb),
    );
    {
        let bg = crate::themes::active_chat_background(ui.ctx());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(thumb_rect));
        child.set_clip_rect(thumb_rect.intersect(ui.clip_rect()));
        if let Ok(mut bg) = bg.lock() {
            bg.paint_thumbnail(&mut child, thumb_rect, 480.0);
        };
    }

    // `BackgroundRow::resizeGetHeight`: `settingsLink` buttons at `settingsFromGalleryTop`,
    // the second `settingsFromFileTop` below the first.
    let link_x = thumb_rect.right() + st::settings_thumb_skip.scaled() as f32;
    let link_font = font_id_from_style(st::settings_link.font);
    let link_fg = colors.color("windowActiveTextFg");
    let link_h = ui.fonts_mut(|f| f.row_height(&link_font));
    let gallery_top = thumb_rect.top() + st::settings_from_gallery_top.scaled() as f32;
    let links = [
        (tr::lng_settings_bg_from_gallery(), gallery_top),
        (
            tr::lng_settings_bg_from_file(),
            gallery_top + link_h + st::settings_from_file_top.scaled() as f32,
        ),
    ];
    for (i, (label, y)) in links.iter().enumerate() {
        let r = ui.painter().text(
            Pos2::new(link_x, *y),
            Align2::LEFT_TOP,
            label,
            link_font.clone(),
            link_fg,
        );
        let resp = ui.interact(r, ui.scope_id().with(("bg_link", i)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, label));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter().line_segment(
                [
                    Pos2::new(r.left(), r.bottom() - 1.0),
                    Pos2::new(r.right(), r.bottom() - 1.0),
                ],
                Stroke::new(1.0, link_fg),
            );
        }
        if resp.clicked() {
            if i == 0 {
                // `controller->show(Box<BackgroundBox>(controller))`.
                open_background_gallery_box(ui.ctx(), state, out);
            } else {
                choose_background_file(ui.ctx(), out);
            }
        }
    }
}

/// "Choose from gallery": opens the Chat Background gallery box (`BackgroundBox`) instead of
/// expanding presets inline.
fn open_background_gallery_box(
    ctx: &egui::Context,
    state: &mut ChatSettingsState,
    out: &mut ChatSettingsOutput,
) {
    crate::themes::AppThemesHost::open(ctx, Some(crate::themes::ThemesTab::Backgrounds));
    state.gallery_open = true;
    out.open_theme_studio = true;
}

/// "Choose from file": loads an image and makes it the global chat wallpaper.
fn choose_background_file(ctx: &egui::Context, out: &mut ChatSettingsOutput) {
    let Some(path) = rfd::FileDialog::new()
        .set_title(tr::lng_settings_bg_from_file())
        .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
        .pick_file()
    else {
        return;
    };
    match image::open(&path) {
        Ok(img) => {
            let config = crate::themes::BackgroundConfig::from_image(img.to_rgba8(), false, false);
            if let Ok(mut bg) = crate::themes::active_chat_background(ctx).lock() {
                bg.set_global(config);
            }
            out.toast = Some(tr::lng_settings_section_background());
        }
        Err(err) => out.toast = Some(err.to_string()),
    }
}

fn render_accent_palette(
    ui: &mut Ui,
    state: &mut ChatSettingsState,
    core: &mut CoreSettings,
    live_theme: &mut LiveThemeColors,
    colors: &dyn Colors,
    out: &mut ChatSettingsOutput,
) {
    // `ColorsPalette::show`: the scheme's own accent first, then `DefaultAccentColors`; a
    // current color missing from the list replaces its last entry.
    let scheme_accent = embedded_scheme(live_theme.kind).accent_color;
    let mut list = default_accent_colors(live_theme.kind);
    list.insert(0, scheme_accent);
    let active_accent = live_theme.accent;
    let current = active_accent.unwrap_or(scheme_accent);
    let selected_idx = match list.iter().position(|&c| c == current) {
        Some(i) => i,
        None => {
            let last = list.len() - 1;
            list[last] = current;
            last
        }
    };
    let core_kind = theme_to_core_embedded(live_theme.kind);

    // `ColorsPalette::updateInnerGeometry`: `settingsAccentColorSize` buttons spread across the
    // `settingsButtonNoIcon` padding, `2 * defaultVerticalListSkip` above and one below.
    let size = st::settings_accent_color_size.scaled() as f32;
    let padding = st::settings_button_no_icon.padding;
    let pad_left = padding.left.scaled() as f32;
    let pad_right = padding.right.scaled() as f32;
    let top = SETTINGS_LIST_SKIP * 2.0;
    let full_w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(
        Vec2::new(full_w, top + size + SETTINGS_LIST_SKIP),
        Sense::hover(),
    );
    let count = list.len() + 1;
    let inner_w = full_w - pad_left - pad_right;
    let skip = ((inner_w - size * count as f32) / (count - 1) as f32).max(0.0);
    let button_rect = |index: usize| {
        let x = (row.left() + pad_left + index as f32 * (size + skip)).round();
        Rect::from_min_size(Pos2::new(x, row.top() + top), Vec2::splat(size))
    };

    for (idx, color) in list.iter().copied().enumerate() {
        let rect = button_rect(idx);
        let resp = ui.interact(rect, ui.scope_id().with(("accent", idx)), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            if color == scheme_accent {
                core.themes_accent_colors.clear(core_kind);
                live_theme.update(live_theme.kind, None);
            } else {
                core.themes_accent_colors
                    .set(core_kind, theme_color_to_core(color));
                live_theme.update(live_theme.kind, Some(color));
            }
            out.toast = Some(format!(
                "{} ({})",
                tr::lng_settings_theme_accent_title(),
                color.to_hex_string()
            ));
        }
        // `PaintRoundColorButton`: the filled circle; the chosen one gets a `boxBg` ring of
        // `settingsAccentColorLine` inset by `settingsAccentColorSkip`.
        ui.painter()
            .circle_filled(rect.center(), size * 0.5, theme_color_to_egui(color));
        if idx == selected_idx {
            let line = st::settings_accent_color_line.scaled() as f32;
            let inset = st::settings_accent_color_skip.scaled() as f32;
            ui.painter().circle_stroke(
                rect.center(),
                size * 0.5 - inset,
                Stroke::new(line, colors.color("boxBg")),
            );
        }
    }

    // `PaintCustomButton`: the first seven colors as dots, one in the center and six around.
    let cust_rect = button_rect(list.len());
    let cust_resp = ui.interact(
        cust_rect,
        ui.scope_id().with("accent_custom"),
        Sense::click(),
    );
    if cust_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let small = size / 8.0;
    let dot_at = |center: Vec2, color: Color| {
        let at = cust_rect.min + (Vec2::splat(1.0) + center) * (size * 0.5);
        ui.painter()
            .circle_filled(at, small, theme_color_to_egui(color));
    };
    if let Some(&first) = list.first() {
        dot_at(Vec2::ZERO, first);
    }
    for i in 0..6 {
        let angle = i as f32 * std::f32::consts::PI / 3.0;
        let point = Vec2::new(angle.cos(), angle.sin()) * (1.0 - 2.0 * small / size);
        if let Some(&color) = list.get(i + 1) {
            dot_at(point, color);
        }
    }
    if cust_resp
        .on_hover_text(strings::CUSTOM_ACCENT_TOOLTIP)
        .clicked()
    {
        state.custom_accent_open = !state.custom_accent_open;
        if let Some(cur) = active_accent {
            state.custom_accent_hex = cur.to_hex_string();
        }
    }

    if state.custom_accent_open {
        ui.add_space(6.0);
        settings_card(ui, colors, |ui| {
            ui.horizontal(|ui| {
                InputField::single_line(
                    "custom_accent_hex",
                    &mut state.custom_accent_hex,
                    "#40a7e3",
                    InputFieldStyle::default_input(colors),
                )
                .width(140.0)
                .show(ui);
                ui.add_space(8.0);
                if RoundButton::new(
                    strings::APPLY_HEX_BTN,
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    if let Some(parsed) = Color::from_hex(state.custom_accent_hex.trim()) {
                        core.themes_accent_colors
                            .set(core_kind, theme_color_to_core(parsed));
                        live_theme.update(live_theme.kind, Some(parsed));
                        state.custom_accent_open = false;
                        out.toast = Some(format!(
                            "{}: {}",
                            tr::lng_settings_theme_accent_title(),
                            parsed.to_hex_string()
                        ));
                    } else {
                        out.toast = Some(strings::INVALID_HEX_TOAST.to_owned());
                    }
                }
                ui.add_space(6.0);
                if RoundButton::new(
                    strings::BIO_RESET_BUTTON,
                    RoundButtonStyle::default_light(colors),
                )
                .show(ui)
                .clicked()
                {
                    core.themes_accent_colors.clear(core_kind);
                    live_theme.update(live_theme.kind, None);
                    state.custom_accent_open = false;
                    out.toast = Some(strings::RESET_ACCENT_TOAST.to_owned());
                }
            });
        });
    }
}

fn render_sticker_sets_manager(
    ui: &mut Ui,
    state: &mut ChatSettingsState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut ChatSettingsOutput,
) {
    let sets = backend.sticker_sets();
    settings_card(ui, colors, |ui| {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    !state.sticker_sets_show_archived,
                    strings::STICKERS_INSTALLED_TAB,
                )
                .clicked()
            {
                state.sticker_sets_show_archived = false;
            }
            if ui
                .selectable_label(
                    state.sticker_sets_show_archived,
                    strings::STICKERS_ARCHIVED_TAB,
                )
                .clicked()
            {
                state.sticker_sets_show_archived = true;
            }
        });
        ui.add_space(6.0);
        let filtered: Vec<_> = sets
            .into_iter()
            .filter(|s| s.archived == state.sticker_sets_show_archived)
            .collect();
        if filtered.is_empty() {
            ui.label(
                egui::RichText::new(strings::STICKERS_EMPTY_TAB)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        } else {
            let remove_lbl = tr::lng_filters_remove_yes();
            for set in filtered {
                ui.horizontal(|ui| {
                    let kind = if set.is_emoji { "emoji" } else { "stickers" };
                    let anim = if set.animated { " · animated" } else { "" };
                    ui.label(
                        egui::RichText::new(format!(
                            "{} ({} {}{})",
                            set.title, set.count, kind, anim
                        ))
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(&remove_lbl).clicked() {
                            backend.remove_sticker_set(set.id);
                            out.toast = Some(format!("{remove_lbl}: {}", set.title));
                        }
                        let arch_label = if set.archived {
                            strings::STICKERS_RESTORE_BTN
                        } else {
                            strings::STICKERS_ARCHIVE_BTN
                        };
                        if ui.small_button(arch_label).clicked() {
                            backend.set_sticker_set_archived(set.id, !set.archived);
                            out.toast = Some(format!("{arch_label}: {}", set.title));
                        }
                    });
                });
            }
        }
    });
}

/// Maps `EmbeddedType` to a display label.
#[must_use]
pub const fn embedded_theme_label(kind: EmbeddedType) -> &'static str {
    match kind {
        EmbeddedType::Default => "Classic",
        EmbeddedType::DayBlue => "Day",
        EmbeddedType::Night => "Tinted",
        EmbeddedType::NightGreen => "Night",
    }
}
