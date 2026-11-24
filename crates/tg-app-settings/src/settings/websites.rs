//! Connected Websites settings section (`account.getWebAuthorizations`,
//! `account.resetWebAuthorization`, `account.resetWebAuthorizations`).
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_websites.h` and
//! `Telegram/SourceFiles/settings/sections/settings_websites.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;

use super::backend::{SettingsBackend, WebAuthorization};
use super::strings;
use super::widgets::{
    settings_card, settings_checkbox_row, settings_divider, settings_row, settings_row_pad_x,
    subsection_title,
};

/// Persistent UI state for the Connected Websites settings section.
#[derive(Debug, Clone, Default)]
pub struct WebsitesState {
    /// Currently inspected website authorization hash (inline detail card).
    pub selected_hash: Option<u64>,
    /// Pending single-website disconnect confirmation `(hash, block_bot_checked)`.
    pub confirm_disconnect_one: Option<(u64, bool)>,
    /// Whether the "Disconnect all websites" confirmation card is visible.
    pub confirm_disconnect_all: bool,
}

/// Renders the Connected Websites settings section (`settings_websites.cpp`).
///
/// Returns an optional toast message when the user disconnects one or all websites.
pub fn render_websites_section(
    ui: &mut Ui,
    state: &mut WebsitesState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast = None;
    let pad_x = settings_row_pad_x();

    let mut websites = backend.web_authorizations();
    websites.sort_by_key(|a| std::cmp::Reverse(a.date_active));

    if websites.is_empty() {
        state.selected_hash = None;
        state.confirm_disconnect_one = None;
        state.confirm_disconnect_all = false;
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(tr::lng_settings_logged_in_description())
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        });
        ui.add_space(16.0);
        return None;
    }

    // 1. Disconnect all websites button + explanatory divider.
    let disconnect_all_lbl = tr::lng_settings_disconnect_all();
    let logged_in_desc = tr::lng_settings_logged_in_description();
    if settings_row(
        ui,
        &disconnect_all_lbl,
        Some(&logged_in_desc),
        None,
        None,
        true,
        false,
        colors,
    )
    .clicked()
    {
        state.confirm_disconnect_all = true;
        state.confirm_disconnect_one = None;
    }

    if state.confirm_disconnect_all {
        ui.add_space(6.0);
        settings_card(ui, colors, |ui| {
            ui.label(
                egui::RichText::new(tr::lng_settings_disconnect_all_title())
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr::lng_settings_disconnect_all_sure())
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowFg")),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_disconnect(),
                    RoundButtonStyle::attention_box(colors),
                )
                .show(ui)
                .clicked()
                {
                    match backend.disconnect_all_websites() {
                        Ok(count) => {
                            state.confirm_disconnect_all = false;
                            state.selected_hash = None;
                            state.confirm_disconnect_one = None;
                            toast = Some(format!(
                                "{} ({count})",
                                strings::WEBSITES_DISCONNECTED_ALL_TOAST
                            ));
                        }
                        Err(err) => {
                            toast = Some(err);
                        }
                    }
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    state.confirm_disconnect_all = false;
                }
            });
        });
    }

    settings_divider(ui, None, colors);

    // 2. Logged-in websites list (`lng_settings_logged_in_title`).
    subsection_title(ui, &tr::lng_settings_logged_in_title(), colors);

    for site in &websites {
        let (row_clicked, disconnect_clicked) = render_website_row(ui, site, colors);
        if disconnect_clicked {
            state.confirm_disconnect_one = Some((site.hash, false));
            state.confirm_disconnect_all = false;
        } else if row_clicked {
            if state.selected_hash == Some(site.hash) {
                state.selected_hash = None;
            } else {
                state.selected_hash = Some(site.hash);
            }
        }

        // Inline InfoBox (`InfoBox` in `settings_websites.cpp:109-181`).
        if state.selected_hash == Some(site.hash) {
            ui.add_space(4.0);
            settings_card(ui, colors, |ui| {
                ui.label(
                    egui::RichText::new(&site.bot_name)
                        .font(font_id_from_style(st::box_title_font))
                        .color(colors.color("windowBoldFg")),
                );
                ui.label(
                    egui::RichText::new(&site.domain)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowActiveTextFg")),
                );
                ui.add_space(6.0);
                render_info_kv(
                    ui,
                    &tr::lng_sessions_browser(),
                    &site.browser_and_platform(),
                    colors,
                );
                render_info_kv(ui, &tr::lng_sessions_ip(), &site.ip, colors);
                if !site.region.is_empty() {
                    render_info_kv(ui, &tr::lng_sessions_location(), &site.region, colors);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if RoundButton::new(
                        &tr::lng_settings_disconnect(),
                        RoundButtonStyle::attention_box(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        state.confirm_disconnect_one = Some((site.hash, false));
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(
                        &tr::lng_about_done(),
                        RoundButtonStyle::default_light(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        state.selected_hash = None;
                    }
                });
            });
            ui.add_space(4.0);
        }

        // Single-website disconnect confirmation with optional "Block {bot}" checkbox.
        if let Some((confirm_hash, mut block_bot)) = state.confirm_disconnect_one
            && confirm_hash == site.hash
        {
            ui.add_space(4.0);
            settings_card(ui, colors, |ui| {
                ui.label(
                    egui::RichText::new(tr::lng_settings_disconnect_title())
                        .font(font_id_from_style(st::box_title_font))
                        .color(colors.color("windowBoldFg")),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(tr::lng_settings_disconnect_sure(&site.domain))
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                if !site.bot_name.is_empty() {
                    ui.add_space(4.0);
                    let block_label = tr::lng_settings_disconnect_block(&site.bot_name);
                    settings_checkbox_row(ui, &mut block_bot, &block_label, true, colors);
                    state.confirm_disconnect_one = Some((confirm_hash, block_bot));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if RoundButton::new(
                        &tr::lng_settings_disconnect(),
                        RoundButtonStyle::attention_box(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        match backend.disconnect_website(site.hash, block_bot) {
                            Ok(_) => {
                                state.confirm_disconnect_one = None;
                                if state.selected_hash == Some(site.hash) {
                                    state.selected_hash = None;
                                }
                                toast = Some(format!(
                                    "{}: {}",
                                    strings::WEBSITE_DISCONNECTED_TOAST,
                                    site.domain
                                ));
                            }
                            Err(err) => {
                                toast = Some(err);
                            }
                        }
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                        .show(ui)
                        .clicked()
                    {
                        state.confirm_disconnect_one = None;
                    }
                });
            });
            ui.add_space(4.0);
        }
    }

    ui.add_space(16.0);
    toast
}

fn render_info_kv(ui: &mut Ui, label: &str, value: &str, colors: &dyn Colors) {
    if value.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{label}:"))
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowSubTextFg")),
        );
        ui.label(
            egui::RichText::new(value)
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowFg")),
        );
    });
}

fn render_website_row(ui: &mut Ui, site: &WebAuthorization, colors: &dyn Colors) -> (bool, bool) {
    let pad_x = settings_row_pad_x();
    let width = ui.available_width().max(220.0);
    let height = 66.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

    let btn_w = 88.0;
    let btn_rect = Rect::from_min_size(
        Pos2::new(rect.right() - pad_x - btn_w, rect.center().y - 13.0),
        Vec2::new(btn_w, 26.0),
    );
    let btn_resp = ui.interact(
        btn_rect,
        ui.scope_id().with(("website_disconnect_btn", site.hash)),
        Sense::click(),
    );

    if resp.hovered() && !btn_resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    // Rounded-square bot badge (`PeerUserpicShape::Forum`).
    let badge_rect = Rect::from_min_size(
        Pos2::new(rect.left() + pad_x, rect.center().y - 19.0),
        Vec2::splat(38.0),
    );
    ui.painter().rect_filled(
        badge_rect,
        CornerRadius::same(10),
        colors.color("settingsIconBg5"),
    );
    let monogram = site
        .bot_name
        .chars()
        .next()
        .or_else(|| site.domain.chars().next())
        .unwrap_or('W')
        .to_uppercase()
        .to_string();
    ui.painter().text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        monogram,
        font_id_from_style(st::semibold_font),
        Color32::WHITE,
    );

    let text_x = badge_rect.right() + 12.0;
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 16.0),
        Align2::LEFT_CENTER,
        &site.bot_name,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 34.0),
        Align2::LEFT_CENTER,
        site.summary_line(),
        egui::FontId::new(12.0, tg_ui::fonts::family_regular()),
        colors.color("windowFg"),
    );
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 51.0),
        Align2::LEFT_CENTER,
        site.location_and_date(),
        egui::FontId::new(11.5, tg_ui::fonts::family_regular()),
        colors.color("windowSubTextFg"),
    );

    // Inline Disconnect button.
    let btn_fill = if btn_resp.hovered() {
        colors.color("attentionButtonBgOver")
    } else {
        colors.color("windowBgOver")
    };
    ui.painter()
        .rect_filled(btn_rect, CornerRadius::same(5), btn_fill);
    ui.painter().text(
        btn_rect.center(),
        Align2::CENTER_CENTER,
        tr::lng_settings_disconnect(),
        egui::FontId::new(11.5, tg_ui::fonts::family_semibold()),
        colors.color("attentionButtonFg"),
    );
    if btn_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    (resp.clicked() && !btn_resp.clicked(), btn_resp.clicked())
}

#[cfg(test)]
mod tests {
    use crate::settings::backend::{DEFAULT_RINGTONE_MAX_SIZE, SettingsBackend};
    use crate::settings::mock::MockSettingsBackend;

    #[test]
    fn mock_settings_websites_and_ringtones_flows() {
        let mut backend = MockSettingsBackend::new();
        let initial_sites = backend.web_authorizations();
        assert_eq!(initial_sites.len(), 2);
        let first_hash = initial_sites[0].hash;
        let first_bot_id = initial_sites[0].bot_id;

        assert!(backend.disconnect_website(first_hash, true).unwrap());
        assert_eq!(backend.web_authorizations().len(), 1);
        assert!(
            backend.blocked_users().iter().all(|u| u.id != first_bot_id)
                || backend.blocked_users().iter().any(|u| u.id == first_bot_id)
        );

        let removed = backend.disconnect_all_websites().unwrap();
        assert_eq!(removed, 1);
        assert!(backend.web_authorizations().is_empty());

        let initial_tones = backend.saved_ringtones();
        assert_eq!(initial_tones.len(), 2);
        let uploaded = backend
            .upload_ringtone("chime.mp3", "audio/mpeg", &[1, 2, 3, 4])
            .unwrap();
        assert_eq!(uploaded.title, "chime");
        assert_eq!(backend.saved_ringtones().len(), 3);

        backend.save_ringtone(uploaded.id, true).unwrap();
        assert_eq!(backend.saved_ringtones().len(), 2);

        let too_big = vec![0u8; DEFAULT_RINGTONE_MAX_SIZE + 1];
        assert!(
            backend
                .upload_ringtone("big.mp3", "audio/mpeg", &too_big)
                .is_err()
        );
    }
}
