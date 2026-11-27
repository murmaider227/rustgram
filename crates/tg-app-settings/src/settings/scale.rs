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

/// Distance between two interface scale values on the slider (`SetupInterfaceScale`: `step = 5`).
pub const SCALE_STEP: i32 = 5;

/// The interface scale values the slider steps through, as `SetupInterfaceScale` builds them:
/// `kScaleMin..=MaxScaleForRatio(1)` (50..=300 %) in 5 % steps, plus the configured scale when
/// it falls between two steps.
#[must_use]
pub fn scale_values(config_scale: i32) -> Vec<i32> {
    let mut values = Vec::with_capacity(((SCALE_MAX - SCALE_MIN) / SCALE_STEP + 2) as usize);
    let mut value = SCALE_MIN;
    while value != SCALE_MAX {
        values.push(value);
        if config_scale > value && config_scale < value + SCALE_STEP {
            values.push(config_scale);
        }
        value += SCALE_STEP;
    }
    values.push(SCALE_MAX);
    values
}

/// Clamps an interface scale percentage to the slider range and snaps it to the closest
/// 5 % step (`SCALE_AUTO` maps to the default 100 %).
#[must_use]
pub fn snap_scale_stop(scale: i32) -> i32 {
    if scale == SCALE_AUTO {
        return SCALE_DEFAULT;
    }
    let clamped = scale.clamp(SCALE_MIN, SCALE_MAX);
    let steps = ((clamped - SCALE_MIN) as f32 / SCALE_STEP as f32).round() as i32;
    (SCALE_MIN + steps * SCALE_STEP).clamp(SCALE_MIN, SCALE_MAX)
}

/// Maps a scale percentage to the normalized slider position `0.0..=1.0` (`valueFromScale`).
#[must_use]
pub fn scale_to_slider(scale: i32) -> f64 {
    scale_to_slider_in(&scale_values(scale), scale)
}

fn scale_to_slider_in(values: &[i32], scale: i32) -> f64 {
    let count = values.len().max(2);
    let index = values.iter().position(|&v| v == scale).unwrap_or_else(|| {
        values
            .iter()
            .position(|&v| v == snap_scale_stop(scale))
            .unwrap_or(count - 1)
    });
    index as f64 / (count - 1) as f64
}

/// Maps a normalized slider position `0.0..=1.0` back to a scale percentage.
#[must_use]
pub fn slider_to_scale(pos: f64) -> i32 {
    slider_to_scale_in(&scale_values(SCALE_DEFAULT), pos)
}

fn slider_to_scale_in(values: &[i32], pos: f64) -> i32 {
    let last = values.len().saturating_sub(1);
    let index = (pos.clamp(0.0, 1.0) * last as f64).round() as usize;
    values.get(index).copied().unwrap_or(SCALE_DEFAULT)
}

/// What a new interface scale chosen in Settings does (`setScale` in `SetupInterfaceScale`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleChange {
    /// Same as the configured scale: nothing to do.
    None,
    /// The evaluated scale stays the same (e.g. "Auto" switched off at the screen's own
    /// scale): the new value was written to the config and is saved without a restart.
    Saved,
    /// The evaluated scale changes: tdesktop asks "Restart now?" (`lng_settings_need_restart`)
    /// and only on "Restart Now" writes the value and restarts. The config is left unchanged.
    NeedsRestart(i32),
}

/// Applies a scale chosen with the slider or the "Auto" switch the way tdesktop's `setScale`
/// does: a value that changes the evaluated scale is not applied but returned for the restart
/// prompt ([`ScaleChange::NeedsRestart`]); any other new value is written at once.
pub fn choose_scale(global: &mut GlobalConfig, scale: i32) -> ScaleChange {
    if global.eval_scale(scale) != global.eval_scale(global.config_scale) {
        ScaleChange::NeedsRestart(scale)
    } else if scale != global.config_scale {
        global.config_scale = scale;
        global.validate_scale();
        ScaleChange::Saved
    } else {
        ScaleChange::None
    }
}

/// Writes a scale the user confirmed with "Restart Now" (`cSetConfigScale` before
/// `Core::Restart`); the running window keeps its scale until the restart.
pub fn confirm_scale(global: &mut GlobalConfig, scale: i32) {
    global.config_scale = scale;
    global.validate_scale();
}

/// Renders the Default Interface Scale controls (`Auto` toggle + scale slider + live preview bubble).
///
/// The slider label shows the evaluated scale times `device_pixel_ratio`, as tdesktop's
/// `labelText` does on Windows. Returns what the user's change does (see [`choose_scale`]).
pub fn render_scale_controls(
    ui: &mut Ui,
    global: &mut GlobalConfig,
    device_pixel_ratio: i32,
    show_preview: bool,
    colors: &dyn Colors,
) -> ScaleChange {
    let mut change = ScaleChange::None;
    // `st::settings_scale_padding`: the slider sits under the row text (60 px), 22 px on the right.
    let pad_x = st::settings_scale_padding
        .left
        .scaled()
        .max(settings_row_pad_x() as i32) as f32;
    let pad_right = st::settings_scale_padding.right.scaled().max(16) as f32;
    let mut is_auto = global.config_scale == SCALE_AUTO;
    if settings_toggle_row(
        ui,
        strings::SCALE_DEFAULT_TITLE,
        None,
        Some(SettingsIconKind::Scale),
        &mut is_auto,
        true,
        colors,
    ) {
        let scale = if is_auto {
            SCALE_AUTO
        } else {
            global.eval_scale(global.config_scale)
        };
        change = choose_scale(global, scale);
    }

    // While the slider is dragged its value lives here; the config changes on release
    // (`setPseudoDiscrete`: the label follows the drag, `setScale` runs on release).
    let drag_id = ui.make_persistent_id("tg_settings_scale_drag");
    let dragged_scale: Option<i32> = ui.data(|d| d.get_temp(drag_id));
    let values = scale_values(global.config_scale);
    let shown_scale = dragged_scale.unwrap_or_else(|| global.eval_scale(global.config_scale));
    let effective_scale = shown_scale;
    let mut slider_val = scale_to_slider_in(&values, shown_scale);

    // Slider row (`MakeSliderWithLabel` inside `settingsScalePadding` = (60, 7, 22, 4)): the
    // `settingsScale` slider (15 px seek) under the row text, the `300%`-wide label at the right.
    let label_font = font_id_from_style(st::settings_scale_label.style.font);
    let label_fg = colors.color("windowActiveTextFg");
    let label_w = ui
        .painter()
        .layout_no_wrap("300%".to_owned(), label_font.clone(), label_fg)
        .size()
        .x;
    let label_h = ui.fonts_mut(|f| f.row_height(&label_font));
    let pad_top = st::settings_scale_padding.top.scaled().max(0) as f32;
    let pad_bottom = st::settings_scale_padding.bottom.scaled().max(0) as f32;
    let full_w = ui.available_width();
    ui.add_space(pad_top);
    let (row_rect, _) = ui.allocate_exact_size(Vec2::new(full_w, label_h), Sense::hover());
    ui.add_space(pad_bottom);
    let label_skip = st::normal_font.scaled_size() as f32 * 0.5;
    let slider_w = (full_w - pad_x - pad_right - label_w - label_skip).max(120.0);
    let slider_rect = Rect::from_min_size(
        Pos2::new(row_rect.left() + pad_x, row_rect.top()),
        Vec2::new(slider_w, row_rect.height()),
    );
    let slider_resp = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(slider_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                MediaSlider::new(
                    &mut slider_val,
                    MediaSliderStyle::from_tg(&st::settings_scale, colors),
                )
                .always_display_marker(true)
                .width(slider_w)
                .show(ui)
            },
        )
        .inner;
    if slider_resp.changed() || slider_resp.drag_stopped() {
        let new_scale = slider_to_scale_in(&values, slider_val);
        if slider_resp.dragged() {
            ui.data_mut(|d| d.insert_temp(drag_id, new_scale));
        } else {
            ui.data_mut(|d| d.remove::<i32>(drag_id));
            change = choose_scale(global, new_scale);
        }
    }
    let value_text = format!(
        "{}%",
        effective_scale.saturating_mul(device_pixel_ratio.max(1))
    );
    let value_rect = ui.painter().text(
        Pos2::new(row_rect.right() - pad_right, row_rect.center().y),
        Align2::RIGHT_CENTER,
        &value_text,
        label_font,
        label_fg,
    );
    // `st::settings_scale_label` is a FlatLabel: keep it in the accessibility tree.
    ui.interact(
        value_rect,
        ui.scope_id().with("scale_value_label"),
        Sense::hover(),
    )
    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &value_text));

    if show_preview {
        ui.add_space(8.0);
        render_scale_preview_bubble(ui, effective_scale, colors);
    }

    change
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
