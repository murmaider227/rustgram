//! Passkeys settings section (`Settings::Passkeys` and `PasskeysNoneBox`).
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_passkeys.h`,
//! `Telegram/SourceFiles/settings/sections/settings_passkeys.cpp`,
//! `Telegram/SourceFiles/data/components/passkeys.h`, and
//! `Telegram/SourceFiles/webauthn/cable_box.cpp`.

use egui::{
    Color32, ColorImage, CornerRadius, Frame, Label, Margin, Pos2, Rect, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2,
};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;

use crate::intro::qr::render_tdesktop_qr_rgba;

use super::backend::{PasskeyEntry, PasskeyHybridQrStatus, SettingsBackend};
use super::widgets::{
    SettingsIconKind, settings_card, settings_divider, settings_row, subsection_title,
};

/// Active modal/sub-view inside the Passkeys settings section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PasskeysModal {
    /// Showing the passkeys list only.
    #[default]
    None,
    /// Showing the onboarding / creation sheet (`PasskeysNoneBox`).
    CreateInfo,
    /// Showing the cross-device caBLE / hybrid QR code (`cable_box.cpp`).
    HybridQr {
        /// Encoded `"FIDO:/..."` URI.
        qr_uri: String,
    },
    /// Confirming deletion of a specific passkey (`lng_settings_passkeys_delete_sure_title`).
    ConfirmDelete {
        /// Passkey ID to delete.
        id: String,
        /// Passkey display name shown in the confirmation prompt.
        name: String,
    },
}

/// Persistent UI state for the Passkeys settings section.
pub struct PasskeysState {
    /// Active inline modal card (create intro, hybrid QR, or delete confirmation).
    pub modal: PasskeysModal,
    /// Whether an initial refresh from the backend has been triggered.
    pub loaded: bool,
    /// Cached egui texture for the active hybrid QR URI.
    cached_qr_uri: String,
    qr_texture: Option<TextureHandle>,
    /// Optional status/error note rendered inline.
    pub last_error: Option<String>,
}

impl std::fmt::Debug for PasskeysState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasskeysState")
            .field("modal", &self.modal)
            .field("loaded", &self.loaded)
            .field("cached_qr_uri", &self.cached_qr_uri)
            .field("has_qr_texture", &self.qr_texture.is_some())
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl Default for PasskeysState {
    fn default() -> Self {
        Self {
            modal: PasskeysModal::None,
            loaded: false,
            cached_qr_uri: String::new(),
            qr_texture: None,
            last_error: None,
        }
    }
}

impl PasskeysState {
    /// Clears cached modal state and error banner.
    pub fn reset(&mut self) {
        self.modal = PasskeysModal::None;
        self.cached_qr_uri.clear();
        self.qr_texture = None;
        self.last_error = None;
    }

    /// Formats a passkey's display name (`lng_settings_passkey_unknown` fallback if empty).
    #[must_use]
    pub fn display_name(entry: &PasskeyEntry) -> String {
        let trimmed = entry.name.trim();
        if trimmed.is_empty() {
            tr::lng_settings_passkey_unknown()
        } else {
            trimmed.to_owned()
        }
    }

    /// Formats a passkey's subtitle (`"Last used ..."` or `"Created ..."`), matching
    /// `settings_passkeys.cpp:278-286`.
    #[must_use]
    pub fn format_subtitle(entry: &PasskeyEntry) -> String {
        if let Some(last_used) = entry.last_usage_date.filter(|&ts| ts > 0) {
            tr::lng_settings_passkeys_last_used(&format_unix_date(last_used))
        } else {
            tr::lng_settings_passkeys_created(&format_unix_date(entry.date))
        }
    }
}

/// Formats a Unix timestamp (seconds) into a concise human-readable UTC date string (`Mon D, YYYY HH:MM`).
#[must_use]
pub fn format_unix_date(ts: i32) -> String {
    if ts <= 0 {
        return "unknown date".to_owned();
    }
    let secs = i64::from(ts);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let (year, month, day) = civil_from_days(days);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_name = MONTHS
        .get((month.saturating_sub(1)) as usize)
        .copied()
        .unwrap_or("Jan");
    format!("{month_name} {day}, {year} {hour:02}:{minute:02}")
}

/// Howard Hinnant's civil-from-days algorithm (days since 1970-01-01 -> `(year, month, day)`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    (year, m as u32, d as u32)
}

/// Renders the Passkeys settings section (`settings_passkeys.cpp`).
///
/// Returns an optional toast message when an action completes or fails.
pub fn render_passkeys_section(
    ui: &mut Ui,
    state: &mut PasskeysState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> Option<String> {
    if !state.loaded {
        state.loaded = true;
        backend.refresh_passkeys();
    }

    let mut toast: Option<String> = None;
    let passkeys = backend.passkeys();
    let can_register = backend.can_register_passkey();
    let supported = backend.is_passkey_supported();

    // Header description (`lng_settings_passkeys_about`)
    subsection_title(ui, &tr::lng_settings_passkeys_title(), colors);
    Frame::NONE
        .inner_margin(Margin {
            left: 22,
            right: 22,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_width((ui.available_width()).max(120.0));
            ui.add(
                Label::new(
                    egui::RichText::new(tr::lng_settings_passkeys_about())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                )
                .wrap(),
            );
        });
    ui.add_space(8.0);

    // Registered passkeys list
    if passkeys.is_empty() {
        settings_card(ui, colors, |ui| {
            ui.label(
                egui::RichText::new(tr::lng_settings_passkeys_none_title())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr::lng_settings_passkeys_none_about())
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        });
    } else {
        for entry in &passkeys {
            let name = PasskeysState::display_name(entry);
            let subtitle = PasskeysState::format_subtitle(entry);
            let delete_label = tr::lng_box_delete();
            if settings_row(
                ui,
                &name,
                Some(&subtitle),
                Some(&delete_label),
                Some(SettingsIconKind::Key),
                false,
                true,
                colors,
            )
            .clicked()
            {
                state.modal = PasskeysModal::ConfirmDelete {
                    id: entry.id.clone(),
                    name,
                };
            }
        }
    }

    ui.add_space(6.0);

    // "Add Passkey" action row (`lng_settings_passkeys_button`)
    if can_register {
        let add_title = tr::lng_settings_passkeys_button();
        if settings_row(
            ui,
            &add_title,
            None,
            None,
            Some(SettingsIconKind::Key),
            false,
            true,
            colors,
        )
        .clicked()
        {
            state.last_error = None;
            state.modal = PasskeysModal::CreateInfo;
        }
    } else if !supported {
        let unsupported_label = tr::lng_settings_passkeys_none_button_unsupported();
        let add_title = tr::lng_settings_passkeys_button();
        let _ = settings_row(
            ui,
            &add_title,
            Some(&unsupported_label),
            None,
            Some(SettingsIconKind::Key),
            false,
            false,
            colors,
        );
    }

    if let Some(err) = state.last_error.clone() {
        ui.add_space(6.0);
        Frame::NONE
            .inner_margin(Margin {
                left: 22,
                right: 22,
                top: 0,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.add(
                    Label::new(
                        egui::RichText::new(err)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("attentionButtonFg")),
                    )
                    .wrap(),
                );
            });
    }

    // Active modal card (CreateInfo / HybridQr / ConfirmDelete)
    match state.modal.clone() {
        PasskeysModal::None => {}
        PasskeysModal::CreateInfo => {
            ui.add_space(8.0);
            render_create_info_card(ui, state, backend, can_register, colors, &mut toast);
        }
        PasskeysModal::HybridQr { qr_uri } => {
            ui.add_space(8.0);
            render_hybrid_qr_card(ui, state, backend, &qr_uri, colors);
        }
        PasskeysModal::ConfirmDelete { id, name } => {
            ui.add_space(8.0);
            render_confirm_delete_card(ui, state, backend, &id, &name, colors, &mut toast);
        }
    }

    settings_divider(ui, None, colors);
    let about_text = tr::lng_settings_passkeys_button_about("");
    Frame::NONE
        .inner_margin(Margin {
            left: 22,
            right: 22,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.add(
                Label::new(
                    egui::RichText::new(about_text.trim())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                )
                .wrap(),
            );
        });
    ui.add_space(12.0);

    toast
}

fn render_create_info_card(
    ui: &mut Ui,
    state: &mut PasskeysState,
    backend: &mut dyn SettingsBackend,
    can_register: bool,
    colors: &dyn Colors,
    toast: &mut Option<String>,
) {
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(tr::lng_settings_passkeys_none_title())
                .font(font_id_from_style(st::box_title_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(tr::lng_settings_passkeys_none_about())
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(10.0);

        let bullets = [
            (
                tr::lng_settings_passkeys_none_info1_title(),
                tr::lng_settings_passkeys_none_info1_about(),
            ),
            (
                tr::lng_settings_passkeys_none_info2_title(),
                tr::lng_settings_passkeys_none_info2_about(),
            ),
            (
                tr::lng_settings_passkeys_none_info3_title(),
                tr::lng_settings_passkeys_none_info3_about(),
            ),
        ];
        for (title, body) in &bullets {
            ui.label(
                egui::RichText::new(title)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.label(
                egui::RichText::new(body)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(6.0);
        }

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let create_label = if can_register {
                tr::lng_settings_passkeys_none_button()
            } else {
                tr::lng_settings_passkeys_none_button_unsupported()
            };
            let clicked_create = ui
                .add_enabled_ui(can_register, |ui| {
                    RoundButton::new(&create_label, RoundButtonStyle::default_active(colors))
                        .show(ui)
                        .clicked()
                })
                .inner;
            if clicked_create {
                match backend.register_passkey() {
                    Ok(entry) => {
                        state.last_error = None;
                        state.modal = PasskeysModal::None;
                        *toast = Some(format!(
                            "Passkey \"{}\" added.",
                            PasskeysState::display_name(&entry)
                        ));
                    }
                    Err(err) => {
                        state.last_error = Some(err.clone());
                        *toast = Some(err);
                    }
                }
            }

            ui.add_space(6.0);
            if RoundButton::new(
                &tr::lng_passkey_cable_register_title(),
                RoundButtonStyle::default_light(colors),
            )
            .show(ui)
            .clicked()
            {
                match backend.start_passkey_hybrid_qr() {
                    Ok(qr_uri) => {
                        state.last_error = None;
                        state.modal = PasskeysModal::HybridQr { qr_uri };
                    }
                    Err(err) => {
                        state.last_error = Some(err.clone());
                        *toast = Some(err);
                    }
                }
            }

            ui.add_space(6.0);
            if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                state.modal = PasskeysModal::None;
            }
        });
    });
}

fn render_hybrid_qr_card(
    ui: &mut Ui,
    state: &mut PasskeysState,
    backend: &mut dyn SettingsBackend,
    qr_uri: &str,
    colors: &dyn Colors,
) {
    if qr_uri != state.cached_qr_uri
        && !qr_uri.is_empty()
        && let Some((rgba, w, h)) = render_tdesktop_qr_rgba(qr_uri, 180, 2, None)
    {
        let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        state.qr_texture = Some(ui.ctx().load_texture(
            "passkey_cable_qr",
            img,
            TextureOptions::LINEAR,
        ));
        state.cached_qr_uri = qr_uri.to_owned();
    }

    let cable_status = backend.passkey_hybrid_qr_status();
    let is_dark = {
        let bg = colors.color("windowBg");
        u16::from(bg.r()) + u16::from(bg.g()) + u16::from(bg.b()) < 384
    };
    let ok_color = if is_dark {
        Color32::from_rgb(0x52, 0xC8, 0x7A)
    } else {
        Color32::from_rgb(0x1E, 0x82, 0x42)
    };

    settings_card(ui, colors, |ui| {
        ui.add(
            Label::new(
                egui::RichText::new(tr::lng_passkey_cable_register_title())
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        ui.add(
            Label::new(
                egui::RichText::new(tr::lng_passkey_cable_register_about())
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            )
            .wrap(),
        );
        ui.add_space(10.0);

        if let Some(tex) = &state.qr_texture {
            let side = 196.0_f32.min(ui.available_width().max(160.0));
            let (card_rect, _) = ui.allocate_exact_size(Vec2::splat(side), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(card_rect, CornerRadius::same(12), Color32::WHITE);
            painter.rect_stroke(
                card_rect,
                CornerRadius::same(12),
                Stroke::new(1.0, colors.color("shadowFg")),
                egui::StrokeKind::Outside,
            );
            let qr_rect = Rect::from_center_size(card_rect.center(), Vec2::splat(side - 20.0));
            painter.image(
                tex.id(),
                qr_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        ui.add_space(10.0);

        // Live caBLEv2 BLE proximity status banner (`cable_box.cpp`).
        match &cable_status {
            PasskeyHybridQrStatus::Scanning | PasskeyHybridQrStatus::Idle => {
                let accent = colors.color("windowActiveTextFg");
                Frame::NONE
                    .fill(colors.color("windowBg"))
                    .stroke(Stroke::new(1.0, colors.color("windowBgActive")))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (dot_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot_rect.center(), 4.0, accent);
                            ui.add(
                                Label::new(
                                    egui::RichText::new("Bluetooth proximity scan active")
                                        .font(font_id_from_style(st::semibold_font))
                                        .color(accent),
                                )
                                .wrap(),
                            );
                        });
                        ui.add_space(2.0);
                        ui.add(
                            Label::new(
                                egui::RichText::new(tr::lng_passkey_cable_bluetooth())
                                    .font(font_id_from_style(st::normal_font))
                                    .color(colors.color("windowSubTextFg")),
                            )
                            .wrap(),
                        );
                    });
            }
            PasskeyHybridQrStatus::NoBluetooth => {
                Frame::NONE
                    .fill(colors.color("windowBg"))
                    .stroke(Stroke::new(1.0, colors.color("attentionButtonFg")))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            Label::new(
                                egui::RichText::new(format!(
                                    "! {}",
                                    tr::lng_passkey_cable_no_bluetooth()
                                ))
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("attentionButtonFg")),
                            )
                            .wrap(),
                        );
                    });
            }
            PasskeyHybridQrStatus::Connecting { tunnel_domain } => {
                Frame::NONE
                    .fill(colors.color("windowBg"))
                    .stroke(Stroke::new(1.5, ok_color))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (dot_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot_rect.center(), 4.0, ok_color);
                            ui.add(
                                Label::new(
                                    egui::RichText::new(tr::lng_passkey_cable_connecting())
                                        .font(font_id_from_style(st::semibold_font))
                                        .color(ok_color),
                                )
                                .wrap(),
                            );
                        });
                        ui.add_space(2.0);
                        ui.add(
                            Label::new(
                                egui::RichText::new(format!(
                                    "{} ({tunnel_domain})",
                                    tr::lng_passkey_cable_continue()
                                ))
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowSubTextFg")),
                            )
                            .wrap(),
                        );
                    });
            }
            PasskeyHybridQrStatus::Completed => {
                state.modal = PasskeysModal::None;
                return;
            }
            PasskeyHybridQrStatus::Error(err) => {
                Frame::NONE
                    .fill(colors.color("windowBg"))
                    .stroke(Stroke::new(1.0, colors.color("attentionButtonFg")))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            Label::new(
                                egui::RichText::new(format!(
                                    "! {} ({err})",
                                    tr::lng_passkey_cable_error()
                                ))
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("attentionButtonFg")),
                            )
                            .wrap(),
                        );
                    });
            }
        }
        ui.add_space(10.0);

        if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
            .show(ui)
            .clicked()
        {
            backend.cancel_passkey_hybrid_qr();
            state.modal = PasskeysModal::None;
        }
    });
}

fn render_confirm_delete_card(
    ui: &mut Ui,
    state: &mut PasskeysState,
    backend: &mut dyn SettingsBackend,
    id: &str,
    name: &str,
    colors: &dyn Colors,
    toast: &mut Option<String>,
) {
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(tr::lng_settings_passkeys_delete_sure_title())
                .font(font_id_from_style(st::box_title_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(4.0);
        let prompt = if name.trim().is_empty() {
            tr::lng_settings_passkeys_delete_sure_about()
        } else {
            tr::lng_settings_passkeys_delete_sure_about_name(name)
        };
        ui.label(
            egui::RichText::new(format!(
                "{prompt}\n\n{}",
                tr::lng_settings_passkeys_delete_sure_about2()
            ))
            .font(font_id_from_style(st::normal_font))
            .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let mut danger_style = RoundButtonStyle::default_active(colors);
            let attention = colors.color("attentionButtonFg");
            danger_style.text_bg = attention;
            danger_style.text_bg_over = attention;
            if RoundButton::new(&tr::lng_box_delete(), danger_style)
                .show(ui)
                .clicked()
            {
                match backend.delete_passkey(id) {
                    Ok(()) => {
                        state.last_error = None;
                        state.modal = PasskeysModal::None;
                        *toast = Some(format!("Passkey \"{name}\" deleted."));
                    }
                    Err(err) => {
                        state.last_error = Some(err.clone());
                        *toast = Some(err);
                    }
                }
            }
            ui.add_space(6.0);
            if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                state.modal = PasskeysModal::None;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_subtitle_prefers_last_usage_date_and_handles_empty_name() {
        let entry = PasskeyEntry {
            id: "pk_1".to_owned(),
            name: String::new(),
            date: 1_735_689_600, // 2025-01-01 00:00 UTC
            software_emoji_id: None,
            last_usage_date: None,
        };
        assert_eq!(
            PasskeysState::display_name(&entry),
            tr::lng_settings_passkey_unknown()
        );
        let created = PasskeysState::format_subtitle(&entry);
        assert!(created.contains("Jan 1, 2025"));

        let used_entry = PasskeyEntry {
            name: "Workstation Hello".to_owned(),
            last_usage_date: Some(1_738_368_000), // 2025-02-01 00:00 UTC
            ..entry
        };
        assert_eq!(
            PasskeysState::display_name(&used_entry),
            "Workstation Hello"
        );
        let last_used = PasskeysState::format_subtitle(&used_entry);
        assert!(last_used.contains("Feb 1, 2025"));
    }
}
