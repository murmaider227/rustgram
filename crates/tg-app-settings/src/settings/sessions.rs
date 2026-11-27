//! Active Sessions / Devices settings section.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_active_sessions.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Ui, Vec2};
use tg_core::CoreSettings;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::{Icon, st};
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use super::backend::{
    ActiveSession, DeviceKind, SESSION_TTL_OPTIONS, SettingsBackend, SettingsRead,
};
use super::privacy::format_ttl_days;
use super::strings;
use super::widgets::{
    paint_tinted_icon, settings_card, settings_divider, settings_radio_row, settings_row,
    settings_row_pad_x, settings_toggle_row, subsection_title,
};

/// Maximum length of custom device model string (`kMaxDeviceModelLength` in `settings_active_sessions.cpp`).
pub const MAX_DEVICE_MODEL_LEN: usize = 32;

/// Persistent UI state for the Active Sessions section.
#[derive(Debug, Clone, Default)]
pub struct SessionsState {
    /// Currently inspected session hash (detail editor).
    pub selected_hash: Option<u64>,
    /// Whether the Terminate All confirmation banner is visible.
    pub confirm_terminate_all: bool,
    /// Whether the Rename Current Device editor is open.
    pub rename_open: bool,
    /// Draft custom device name.
    pub rename_draft: String,
    /// Whether the Auto-Terminate TTL picker is open.
    pub editing_ttl: bool,
}

/// Renders the Active Sessions (`settings_active_sessions.cpp`) section.
pub fn render_sessions_section(
    ui: &mut Ui,
    state: &mut SessionsState,
    core: &mut CoreSettings,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast = None;
    let pad_x = settings_row_pad_x();
    let sessions = backend.active_sessions();
    let current = sessions.iter().find(|s| s.is_current()).cloned();
    let others: Vec<_> = sessions.into_iter().filter(|s| !s.is_current()).collect();

    // 1. This Device section.
    subsection_title(ui, &tr::lng_sessions_header(), colors);
    if let Some(cur) = current {
        let mut display = cur.clone();
        if !core.custom_device_model.is_empty() {
            display.device_model = core.custom_device_model.clone();
        }
        if render_session_row(ui, &display, true, colors) {
            state.rename_open = !state.rename_open;
            state.rename_draft = core.custom_device_model.clone();
        }
    }

    if state.rename_open {
        ui.add_space(6.0);
        settings_card(ui, colors, |ui| {
            ui.label(
                egui::RichText::new(strings::RENAME_DEVICE_TITLE)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(4.0);
            let default_model = tg_platform::system_info::device_model_pretty();
            InputField::single_line(
                "settings_device_rename",
                &mut state.rename_draft,
                &default_model,
                InputFieldStyle::default_input(colors),
            )
            .max_length(MAX_DEVICE_MODEL_LEN)
            .show(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    let cleaned: String = state
                        .rename_draft
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(MAX_DEVICE_MODEL_LEN)
                        .collect();
                    core.custom_device_model = cleaned;
                    state.rename_open = false;
                    toast = Some(strings::DEVICE_NAME_UPDATED_TOAST.to_owned());
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    state.rename_open = false;
                }
            });
        });
    }

    let term_all_lbl = tr::lng_sessions_terminate_all();
    let term_all_sub = tr::lng_sessions_terminate_all_about();
    if !others.is_empty()
        && settings_row(
            ui,
            &term_all_lbl,
            Some(&term_all_sub),
            None,
            None,
            true,
            false,
            colors,
        )
        .clicked()
    {
        state.confirm_terminate_all = true;
    }

    if state.confirm_terminate_all {
        ui.add_space(6.0);
        settings_card(ui, colors, |ui| {
            ui.label(
                egui::RichText::new(tr::lng_settings_reset_sure())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_reset_button(),
                    RoundButtonStyle::attention_box(colors),
                )
                .show(ui)
                .clicked()
                {
                    let n = backend.terminate_all_other_sessions();
                    state.confirm_terminate_all = false;
                    state.selected_hash = None;
                    toast = Some(format!("{}: {n}", tr::lng_settings_reset_button()));
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    state.confirm_terminate_all = false;
                }
            });
        });
    }

    settings_divider(ui, None, colors);

    // 2. Active sessions list.
    subsection_title(ui, &tr::lng_sessions_other_header(), colors);
    // `SessionsContent::paintEvent`: "Loading..." until `account.getAuthorizations` answers.
    let loading = backend.settings_read_pending(SettingsRead::ActiveSessions);
    if loading || others.is_empty() {
        let text = if loading {
            tr::lng_contacts_loading()
        } else {
            strings::NO_OTHER_SESSIONS.to_owned()
        };
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(text)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        });
    } else {
        for session in &others {
            if render_session_row(ui, session, false, colors) {
                if state.selected_hash == Some(session.hash) {
                    state.selected_hash = None;
                } else {
                    state.selected_hash = Some(session.hash);
                }
            }
            if state.selected_hash == Some(session.hash) {
                ui.add_space(4.0);
                let mut accept_calls = session.accept_calls;
                let mut accept_secret = session.accept_secret_chats;
                let mut terminate_clicked = false;
                settings_card(ui, colors, |ui| {
                    ui.label(
                        egui::RichText::new(&session.device_model)
                            .font(font_id_from_style(st::semibold_font))
                            .color(colors.color("windowBoldFg")),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  ·  {}  ·  {}: {}",
                            session.app_name_version,
                            session.system_version,
                            tr::lng_sessions_ip(),
                            session.ip
                        ))
                        .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                        .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(6.0);
                    let calls_changed = settings_toggle_row(
                        ui,
                        strings::ACCEPT_CALLS_LABEL,
                        None,
                        None,
                        &mut accept_calls,
                        true,
                        colors,
                    );
                    let secret_changed = settings_toggle_row(
                        ui,
                        strings::ACCEPT_SECRET_CHATS_LABEL,
                        None,
                        None,
                        &mut accept_secret,
                        true,
                        colors,
                    );
                    if calls_changed || secret_changed {
                        backend.update_session_permissions(
                            session.hash,
                            accept_calls,
                            accept_secret,
                        );
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(
                        &tr::lng_sessions_terminate(),
                        RoundButtonStyle::attention_box(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        terminate_clicked = true;
                    }
                });
                if terminate_clicked && backend.terminate_session(session.hash) {
                    state.selected_hash = None;
                    toast = Some(format!(
                        "{}: {}",
                        tr::lng_settings_reset_button(),
                        session.device_model
                    ));
                }
                ui.add_space(4.0);
            }
        }
    }

    settings_divider(ui, None, colors);

    // 3. Auto-terminate old sessions (`SelfDestructionBox::Type::Sessions`).
    subsection_title(ui, strings::AUTO_TERMINATE_SESSIONS_TITLE, colors);
    let mut ttl_days = backend.session_ttl_days();
    let ttl_str = format_ttl_days(ttl_days);
    if settings_row(
        ui,
        strings::IF_INACTIVE_FOR_LABEL,
        None,
        Some(&ttl_str),
        None,
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editing_ttl = !state.editing_ttl;
    }

    if state.editing_ttl {
        ui.add_space(4.0);
        settings_card(ui, colors, |ui| {
            for &days in &SESSION_TTL_OPTIONS {
                if settings_radio_row(
                    ui,
                    &mut ttl_days,
                    days,
                    &format_ttl_days(days),
                    true,
                    colors,
                )
                .changed()
                {
                    backend.set_session_ttl_days(ttl_days);
                    state.editing_ttl = false;
                    toast = Some(format!(
                        "{}: {}",
                        tr::lng_self_destruct_sessions_title(),
                        format_ttl_days(ttl_days)
                    ));
                }
            }
        });
    }

    ui.add_space(16.0);
    toast
}

fn device_badge_color(kind: DeviceKind, colors: &dyn Colors) -> Color32 {
    let key = match kind {
        DeviceKind::Windows | DeviceKind::Mac | DeviceKind::IPhone | DeviceKind::IPad => {
            "settingsIconBg4"
        }
        DeviceKind::Ubuntu => "settingsIconBg8",
        DeviceKind::Linux => "settingsIconBg5",
        DeviceKind::Android => "settingsIconBg2",
        DeviceKind::Web
        | DeviceKind::Chrome
        | DeviceKind::Edge
        | DeviceKind::Firefox
        | DeviceKind::Safari => "settingsIconBg1",
        DeviceKind::Other => "settingsIconBg6",
    };
    let c = colors.color(key);
    if c.a() == 0 {
        colors.color("windowBgActive")
    } else {
        c
    }
}

fn device_style_icon(kind: DeviceKind) -> Icon {
    match kind {
        DeviceKind::Windows => st::session_icon_windows,
        DeviceKind::Mac => st::session_icon_mac,
        DeviceKind::Ubuntu => st::session_icon_ubuntu,
        DeviceKind::Linux => st::session_icon_linux,
        DeviceKind::IPhone | DeviceKind::IPad => st::menu_icon_devices,
        DeviceKind::Android => st::session_icon_android,
        DeviceKind::Web => st::session_icon_web,
        DeviceKind::Chrome => st::session_icon_chrome,
        DeviceKind::Edge => st::session_icon_edge,
        DeviceKind::Firefox => st::session_icon_firefox,
        DeviceKind::Safari => st::session_icon_safari,
        DeviceKind::Other => st::session_icon_other,
    }
}

fn render_session_row(
    ui: &mut Ui,
    session: &ActiveSession,
    is_current: bool,
    colors: &dyn Colors,
) -> bool {
    let w = ui.available_width().max(220.0);
    let pad_x = settings_row_pad_x();
    let h = 68.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    let kind = session.device_kind();
    let badge_rect = Rect::from_center_size(
        Pos2::new(rect.left() + pad_x + 18.0, rect.center().y),
        Vec2::splat(36.0),
    );
    ui.painter().rect_filled(
        badge_rect,
        CornerRadius::same(st::settings_icon_radius.scaled().clamp(4, 16) as u8),
        device_badge_color(kind, colors),
    );
    paint_tinted_icon(
        ui.painter(),
        badge_rect.shrink(5.0),
        device_style_icon(kind),
        colors,
        Some(Color32::WHITE),
    );

    let text_x = badge_rect.right() + 14.0;
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 10.0),
        Align2::LEFT_TOP,
        &session.device_model,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 29.0),
        Align2::LEFT_TOP,
        format!("{} ({})", session.app_name_version, session.system_version),
        egui::FontId::new(12.0, tg_ui::fonts::family_regular()),
        colors.color("windowFg"),
    );
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 46.0),
        Align2::LEFT_TOP,
        session.location_and_date(),
        egui::FontId::new(11.5, tg_ui::fonts::family_regular()),
        colors.color("windowSubTextFg"),
    );

    if is_current {
        ui.painter().text(
            Pos2::new(rect.right() - pad_x, rect.top() + 14.0),
            Align2::RIGHT_TOP,
            strings::RENAME_BUTTON,
            egui::FontId::new(12.0, tg_ui::fonts::family_semibold()),
            colors.color("windowActiveTextFg"),
        );
    }

    resp.clicked()
}
