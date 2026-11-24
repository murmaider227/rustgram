//! Interface scale slider, stop calculations, and live scaled message preview.
//!
//! Port origin: `Telegram/SourceFiles/settings/settings_scale_preview.cpp` and
//! `SetupDefaultScale` in `Telegram/SourceFiles/settings/sections/settings_main.cpp`.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_core::{GlobalConfig, SCALE_AUTO, SCALE_DEFAULT, SCALE_MAX, SCALE_MIN};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::stand_in::{Colors, MediaSliderStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::sliders::MediaSlider;

use super::strings;
use super::widgets::{SettingsIconKind, settings_row_pad_x, settings_toggle_row};

/// Standard tdesktop discrete interface scale stops (`100%`..=`300%`).
pub const SCALE_STOPS: [i32; 6] = [100, 125, 150, 200, 250, 300];

/// Clamps and snaps an interface scale percentage to the closest supported stop in [`SCALE_STOPS`].
#[must_use]
pub fn snap_scale_stop(scale: i32) -> i32 {
    if scale == SCALE_AUTO {
        return SCALE_DEFAULT;
    }
    let clamped = scale.clamp(SCALE_MIN, SCALE_MAX);
    SCALE_STOPS
        .iter()
        .copied()
        .min_by_key(|&stop| (stop - clamped).abs())
        .unwrap_or(SCALE_DEFAULT)
}

/// Maps a scale percentage to normalized slider position `0.0..=1.0`.
#[must_use]
pub fn scale_to_slider(scale: i32) -> f64 {
    let snapped = snap_scale_stop(scale);
    let idx = SCALE_STOPS.iter().position(|&s| s == snapped).unwrap_or(0);
    idx as f64 / (SCALE_STOPS.len() - 1) as f64
}

/// Maps a normalized slider position `0.0..=1.0` back to a scale percentage stop.
#[must_use]
pub fn slider_to_scale(pos: f64) -> i32 {
    let clamped = pos.clamp(0.0, 1.0);
    let idx = (clamped * (SCALE_STOPS.len() - 1) as f64).round() as usize;
    SCALE_STOPS.get(idx).copied().unwrap_or(SCALE_DEFAULT)
}

/// Renders the Default Interface Scale controls (`Auto` toggle + scale slider + live preview bubble).
///
/// Returns `true` if `global.config_scale` / `global.scale` changed.
pub fn render_scale_controls(
    ui: &mut Ui,
    global: &mut GlobalConfig,
    show_preview: bool,
    colors: &dyn Colors,
) -> bool {
    let mut changed = false;
    let pad_x = st::settings_scale_padding
        .left
        .scaled()
        .max(settings_row_pad_x() as i32) as f32;
    let mut is_auto = global.config_scale == SCALE_AUTO;
    let auto_subtitle = format!("{} ({}%)", tr::lng_settings_scale(), global.screen_scale);
    if settings_toggle_row(
        ui,
        strings::SCALE_DEFAULT_TITLE,
        Some(&auto_subtitle),
        Some(SettingsIconKind::Scale),
        &mut is_auto,
        true,
        colors,
    ) {
        if is_auto {
            global.config_scale = SCALE_AUTO;
            global.scale = global.screen_scale;
        } else {
            global.config_scale = snap_scale_stop(global.scale);
            global.scale = global.config_scale;
        }
        changed = true;
    }

    let effective_scale = global.eval_scale(global.config_scale);
    let mut slider_val = scale_to_slider(effective_scale);

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x - 52.0).max(120.0);
        let slider_resp = MediaSlider::new(
            &mut slider_val,
            MediaSliderStyle::default_media_slider(colors),
        )
        .always_display_marker(true)
        .width(avail)
        .show(ui);
        if slider_resp.changed() {
            let new_scale = slider_to_scale(slider_val);
            global.config_scale = new_scale;
            global.scale = new_scale;
            changed = true;
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("{effective_scale}%"))
                .font(font_id_from_style(st::settings_scale_label.style.font))
                .color(colors.color("windowActiveTextFg")),
        );
    });

    // Tick stop labels below slider.
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x - 52.0).max(120.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(avail, 16.0), Sense::hover());
        let step_count = (SCALE_STOPS.len() - 1) as f32;
        for (i, stop) in SCALE_STOPS.iter().enumerate() {
            let x = rect.left() + (i as f32 / step_count) * rect.width();
            let active = *stop == snap_scale_stop(effective_scale);
            let fg = if active {
                colors.color("windowActiveTextFg")
            } else {
                colors.color("windowSubTextFg")
            };
            let align = if i == 0 {
                Align2::LEFT_CENTER
            } else if i == SCALE_STOPS.len() - 1 {
                Align2::RIGHT_CENTER
            } else {
                Align2::CENTER_CENTER
            };
            ui.painter().text(
                Pos2::new(x, rect.center().y),
                align,
                format!("{stop}%"),
                egui::FontId::new(10.5, family_regular()),
                fg,
            );
        }
    });

    if show_preview {
        ui.add_space(8.0);
        render_scale_preview_bubble(ui, effective_scale, colors);
    }

    changed
}

/// Paints a live scaled chat message preview matching `settings_scale_preview.cpp`.
pub fn render_scale_preview_bubble(ui: &mut Ui, scale_percent: i32, colors: &dyn Colors) {
    let factor = (scale_percent as f32 / 100.0).clamp(0.85, 1.85);
    let pad_x = settings_row_pad_x();
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail_w = (ui.available_width() - pad_x).max(200.0);
        let h = (64.0 * factor).clamp(58.0, 118.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, h), Sense::hover());

        // Chat canvas backdrop.
        let canvas_bg = colors.color("windowBgOver");
        ui.painter()
            .rect_filled(rect, CornerRadius::same(10), canvas_bg);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(10),
            Stroke::new(1.0, colors.color("shadowFg")),
            egui::StrokeKind::Inside,
        );

        // Scaled avatar circle on the left.
        let avatar_r = (16.0 * factor).clamp(14.0, 26.0);
        let av_center = Pos2::new(
            rect.left() + 14.0 + avatar_r,
            rect.bottom() - 12.0 - avatar_r,
        );
        ui.painter()
            .circle_filled(av_center, avatar_r, colors.color("windowBgActive"));
        ui.painter().text(
            av_center,
            Align2::CENTER_CENTER,
            "AR",
            egui::FontId::new((11.0 * factor).clamp(10.0, 18.0), family_semibold()),
            Color32::WHITE,
        );

        // Scaled incoming message bubble.
        let bubble_left = av_center.x + avatar_r + 10.0;
        let bubble_rect = Rect::from_min_max(
            Pos2::new(bubble_left, rect.top() + 10.0),
            Pos2::new(
                (rect.right() - 16.0).max(bubble_left + 120.0),
                rect.bottom() - 10.0,
            ),
        );
        ui.painter()
            .rect_filled(bubble_rect, CornerRadius::same(10), colors.color("msgInBg"));

        ui.painter().text(
            Pos2::new(bubble_rect.left() + 12.0, bubble_rect.top() + 8.0),
            Align2::LEFT_TOP,
            strings::SCALE_PREVIEW_SENDER,
            egui::FontId::new((12.0 * factor).clamp(11.0, 20.0), family_semibold()),
            colors.color("windowActiveTextFg"),
        );
        ui.painter().text(
            Pos2::new(
                bubble_rect.left() + 12.0,
                bubble_rect.top() + (25.0 * factor).clamp(22.0, 42.0),
            ),
            Align2::LEFT_TOP,
            format!("{} ({scale_percent}%)", tr::lng_settings_chat_message()),
            egui::FontId::new((13.0 * factor).clamp(11.5, 22.0), family_regular()),
            colors.color("windowFg"),
        );
    });
}
