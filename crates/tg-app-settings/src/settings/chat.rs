//! Chat Settings section: embedded themes, accent colors, chat background picker,
//! text scale & font, send shortcuts, emoji/stickers, and media autoplay.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_chat.cpp`
//! and `Telegram/SourceFiles/boxes/background_box.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use tg_core::settings::{DoubleClickQuickAction, InputSubmitSettings, QuickDialogAction};
use tg_core::{CoreSettings, GlobalConfig};
use tg_lang::tr;
use tg_theme::{
    Color, EmbeddedType, colorize_scheme, colorizer_from, default_accent_colors, embedded_themes,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use super::backend::SettingsBackend;
use super::scale::render_scale_controls;
use super::strings;
use super::theme_bridge::{
    LiveThemeColors, builtin_backgrounds, core_color_to_theme, theme_color_to_core,
    theme_color_to_egui, theme_to_core_embedded,
};
use super::widgets::{
    SettingsIconKind, paint_tinted_icon, settings_card, settings_checkbox_row, settings_divider,
    settings_radio_row, settings_row, settings_row_pad_x, settings_toggle_row, subsection_title,
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
    /// User clicked "Theme Studio / Cloud Wallpapers" (`AppThemesHost::open`).
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
    global: &mut GlobalConfig,
    live_theme: &mut LiveThemeColors,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> ChatSettingsOutput {
    let mut out = ChatSettingsOutput::default();
    let was_dark = live_theme.is_dark();

    // 1. Color Theme selector (`Window::Theme::EmbeddedThemes()`).
    subsection_title(ui, &tr::lng_settings_themes(), colors);
    render_embedded_theme_cards(ui, core, live_theme, colors, &mut out);

    ui.add_space(st::settings_themes_bottom_skip.scaled().max(8) as f32);
    // 2. Accent color swatches (`Window::Theme::DefaultAccentColors`).
    render_accent_palette(ui, state, core, live_theme, colors, &mut out);

    if live_theme.is_dark() != was_dark {
        out.toggled_dark_mode = Some(live_theme.is_dark());
    }

    ui.add_space(6.0);
    settings_toggle_row(
        ui,
        strings::FOLLOW_SYSTEM_DARK_MODE,
        None,
        None,
        &mut core.system_dark_mode_enabled,
        true,
        colors,
    );
    let sys_accent_lbl = tr::lng_settings_theme_system_accent_color();
    settings_toggle_row(
        ui,
        &sys_accent_lbl,
        None,
        None,
        &mut core.system_accent_color_enabled,
        true,
        colors,
    );

    settings_divider(ui, None, colors);

    // 3. Chat Background picker (`tg-theme::WallPaper`).
    subsection_title(ui, &tr::lng_settings_section_background(), colors);
    render_background_picker(ui, state, colors, &mut out);
    ui.add_space(4.0);
    if settings_row(
        ui,
        "Theme Studio / Cloud Wallpapers",
        Some("Edit colors, cloud themes, chat themes & wallpapers"),
        None,
        Some(SettingsIconKind::Brush),
        false,
        true,
        colors,
    )
    .clicked()
    {
        crate::themes::AppThemesHost::open(ui.ctx(), None);
        out.open_theme_studio = true;
    }

    settings_divider(ui, None, colors);

    // 4. Text Size / Interface Scale & Custom Font.
    subsection_title(ui, strings::TEXT_SIZE_AND_FONT, colors);
    render_scale_controls(ui, global, true, colors);
    ui.add_space(6.0);
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

    settings_divider(ui, None, colors);

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

    let dialog_quick_title = tr::lng_settings_quick_dialog_action_title();
    let dialog_quick_about = tr::lng_settings_quick_dialog_action_about();
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
        &dialog_quick_title,
        Some(&dialog_quick_about),
        Some(&dialog_quick_val),
        None,
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
                            out.toast = Some(format!("Failed to download {name} emoji set: {err}"));
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
    let themes = embedded_themes();
    let pad_x = settings_row_pad_x();
    let preview_sz = st::settings_theme_preview_size;
    let bubble_r = st::settings_theme_bubble_radius.scaled().clamp(2, 12) as u8;
    let outline_w = (st::settings_theme_outline_width.scaled() as f32).max(2.0);
    let gap = st::settings_theme_min_skip.scaled().max(8) as f32;

    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x).max(260.0);
        let card_w =
            ((avail - gap * 3.0) / 4.0).clamp(preview_sz.width.scaled().min(64) as f32, 118.0);
        let preview_h = (preview_sz.height.scaled() as f32).clamp(56.0, 68.0);
        let card_h = preview_h + 24.0;

        for mut scheme in themes {
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
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                live_theme.update(scheme.kind, custom_accent);
                out.toast = Some(format!("{}: {display_name}", tr::lng_settings_themes()));
            }

            // Swatch card background.
            let bg_rect = Rect::from_min_size(rect.min, Vec2::new(card_w, preview_h));
            ui.painter().rect_filled(
                bg_rect,
                CornerRadius::same(8),
                theme_color_to_egui(scheme.background),
            );
            // Incoming & outgoing mini message bubbles inside preview card.
            let in_bubble = Rect::from_min_size(
                Pos2::new(bg_rect.left() + 8.0, bg_rect.top() + 10.0),
                Vec2::new(card_w * 0.56, 13.0),
            );
            let out_bubble = Rect::from_min_size(
                Pos2::new(bg_rect.right() - 8.0 - card_w * 0.56, bg_rect.top() + 28.0),
                Vec2::new(card_w * 0.56, 13.0),
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

            // Selection ring.
            let border = if selected {
                Stroke::new(outline_w, colors.color("windowBgActive"))
            } else {
                Stroke::new(1.0, colors.color("shadowFg"))
            };
            ui.painter()
                .rect_stroke(bg_rect, CornerRadius::same(8), border, StrokeKind::Inside);

            // Real checkmark badge when selected.
            if selected {
                let c = Pos2::new(bg_rect.right() - 11.0, bg_rect.bottom() - 11.0);
                ui.painter()
                    .circle_filled(c, 7.5, colors.color("windowBgActive"));
                paint_tinted_icon(
                    ui.painter(),
                    Rect::from_center_size(c, Vec2::splat(10.0)),
                    st::default_checkbox_icon,
                    colors,
                    Some(Color32::WHITE),
                );
            }

            // Theme name label underneath.
            ui.painter().text(
                Pos2::new(rect.center().x, bg_rect.bottom() + 12.0),
                Align2::CENTER_CENTER,
                &display_name,
                egui::FontId::new(12.0, tg_ui::fonts::family_regular()),
                if selected {
                    colors.color("windowActiveTextFg")
                } else {
                    colors.color("windowFg")
                },
            );

            ui.add_space(gap);
        }
    });
}

fn render_accent_palette(
    ui: &mut Ui,
    state: &mut ChatSettingsState,
    core: &mut CoreSettings,
    live_theme: &mut LiveThemeColors,
    colors: &dyn Colors,
    out: &mut ChatSettingsOutput,
) {
    let presets = default_accent_colors(live_theme.kind);
    let active_accent = live_theme.accent;
    let core_kind = theme_to_core_embedded(live_theme.kind);
    let pad_x = settings_row_pad_x();
    let swatch_sz = (st::settings_accent_color_size.scaled() as f32).clamp(24.0, 32.0);
    let swatch_r = swatch_sz * 0.42;
    let swatch_line = (st::settings_accent_color_line.scaled() as f32).max(2.0);

    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        ui.label(
            egui::RichText::new(tr::lng_settings_theme_accent_title())
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowSubTextFg")),
        );
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        for (idx, preset) in presets.iter().copied().enumerate() {
            let selected = match active_accent {
                Some(a) => a == preset,
                None => idx == 0,
            };
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(swatch_sz), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                if idx == 0 {
                    core.themes_accent_colors.clear(core_kind);
                    live_theme.update(live_theme.kind, None);
                } else {
                    core.themes_accent_colors
                        .set(core_kind, theme_color_to_core(preset));
                    live_theme.update(live_theme.kind, Some(preset));
                }
                out.toast = Some(format!(
                    "{} ({})",
                    tr::lng_settings_theme_accent_title(),
                    preset.to_hex_string()
                ));
            }
            ui.painter()
                .circle_filled(rect.center(), swatch_r, theme_color_to_egui(preset));
            if selected {
                ui.painter().circle_stroke(
                    rect.center(),
                    swatch_r + 2.0,
                    Stroke::new(swatch_line, colors.color("windowBoldFg")),
                );
                paint_tinted_icon(
                    ui.painter(),
                    Rect::from_center_size(rect.center(), Vec2::splat(11.0)),
                    st::default_checkbox_icon,
                    colors,
                    Some(Color32::WHITE),
                );
            }
            ui.add_space(4.0);
        }

        // Custom color hex toggle button with real `st::menu_icon_add`.
        let (cust_rect, cust_resp) = ui.allocate_exact_size(Vec2::splat(swatch_sz), Sense::click());
        if cust_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .circle_filled(cust_rect.center(), swatch_r, colors.color("windowBgOver"));
        ui.painter().circle_stroke(
            cust_rect.center(),
            swatch_r,
            Stroke::new(1.5, colors.color("windowActiveTextFg")),
        );
        paint_tinted_icon(
            ui.painter(),
            cust_rect.shrink(6.0),
            st::menu_icon_add,
            colors,
            Some(colors.color("windowActiveTextFg")),
        );
        if cust_resp
            .on_hover_text(strings::CUSTOM_ACCENT_TOOLTIP)
            .clicked()
        {
            state.custom_accent_open = !state.custom_accent_open;
            if let Some(cur) = active_accent {
                state.custom_accent_hex = cur.to_hex_string();
            }
        }
    });

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

fn render_background_picker(
    ui: &mut Ui,
    state: &mut ChatSettingsState,
    colors: &dyn Colors,
    out: &mut ChatSettingsOutput,
) {
    let presets = builtin_backgrounds();
    let pad_x = settings_row_pad_x();
    let thumb_w = (st::settings_background_thumb.scaled() as f32).clamp(56.0, 72.0);
    let thumb_h = (thumb_w * 0.78).max(46.0);

    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        ui.horizontal_wrapped(|ui| {
            for bg in &presets {
                let selected = state.selected_bg_id == bg.id;
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(thumb_w, thumb_h), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    state.selected_bg_id = bg.id.to_owned();
                    out.toast = Some(format!(
                        "{}: {}",
                        tr::lng_settings_section_background(),
                        bg.name
                    ));
                }

                // Multi-band gradient preview swatch.
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(6), bg.color_a);
                let lower = Rect::from_min_max(Pos2::new(rect.left(), rect.center().y), rect.max);
                ui.painter()
                    .rect_filled(lower, CornerRadius::same(4), bg.color_b);
                let dot = Pos2::new(rect.center().x, rect.center().y);
                ui.painter().circle_filled(dot, 12.0, bg.color_c);

                let stroke = if selected {
                    Stroke::new(2.2, colors.color("windowBgActive"))
                } else {
                    Stroke::new(1.0, colors.color("shadowFg"))
                };
                ui.painter()
                    .rect_stroke(rect, CornerRadius::same(6), stroke, StrokeKind::Inside);

                resp.on_hover_text(bg.name);
            }
        });
    });

    ui.add_space(6.0);
    settings_checkbox_row(
        ui,
        &mut state.bg_tiled,
        &tr::lng_settings_bg_tile(),
        true,
        colors,
    );
    settings_checkbox_row(
        ui,
        &mut state.bg_blurred,
        strings::BLUR_BACKGROUND,
        true,
        colors,
    );
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
