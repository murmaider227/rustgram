//! The Mute submenu's boxes: "Mute notifications for..." and "Notification Sound".
//!
//! Port origin: `PickMuteBox` in `Telegram/SourceFiles/menu/menu_mute.cpp` (with
//! `Ui::TimePickerBox` / `Ui::DefaultTimePickerValues` from `ui/boxes/time_picker_box.cpp`) and
//! `RingtonesBox` in `Telegram/SourceFiles/boxes/ringtones_box.cpp`.

use egui::{Align2, CornerRadius, Key, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckboxStyle, Colors, RadioStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Radio;

use tg_app_core::stand_in::types::PeerId;

/// `Ui::DefaultTimePickerValues`: the durations offered by the picker, in seconds.
pub const MUTE_PICKER_SECONDS: [i32; 16] = [
    60 * 15,
    60 * 30,
    3600,
    3600 * 2,
    3600 * 3,
    3600 * 4,
    3600 * 8,
    3600 * 12,
    86400,
    86400 * 2,
    86400 * 3,
    86400 * 7,
    86400 * 7 * 2,
    86400 * 31,
    86400 * 31 * 2,
    86400 * 31 * 3,
];

/// `Ui::FormatMuteFor` (`ui/text/format_values.cpp`) for the picker values.
#[must_use]
pub fn format_mute_for(seconds: i32) -> String {
    let sec = i64::from(seconds);
    if sec <= 60 * 59 {
        return tr::lng_minutes((sec / 60).max(1));
    }
    // `FormatTTL`.
    if sec < 86400 {
        tr::lng_hours(sec / 3600)
    } else if sec < 86400 * 7 {
        tr::lng_days(sec / 86400)
    } else if sec < 86400 * 31 {
        let days = sec / 86400;
        if days % 7 == 0 {
            tr::lng_weeks(days / 7)
        } else {
            format!("{} {}", tr::lng_weeks(days / 7), tr::lng_days(days % 7))
        }
    } else {
        tr::lng_months(sec / (86400 * 31))
    }
}

/// "Mute notifications for..." (`PickMuteBox`): a drum of [`MUTE_PICKER_SECONDS`] starting at
/// the first value, "Mute" / "Cancel".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuteForBoxState {
    /// The chat to mute.
    pub peer_id: PeerId,
    /// Selected index into [`MUTE_PICKER_SECONDS`].
    pub index: usize,
}

impl MuteForBoxState {
    /// Opens the picker at its first value (`TimePickerBox(box, seconds, phrases, 0)`).
    #[must_use]
    pub const fn new(peer_id: PeerId) -> Self {
        Self { peer_id, index: 0 }
    }

    /// The picked duration in seconds.
    #[must_use]
    pub fn seconds(&self) -> i32 {
        MUTE_PICKER_SECONDS[self.index.min(MUTE_PICKER_SECONDS.len() - 1)]
    }

    /// Moves the selection by `delta` rows, clamped to the list.
    pub fn step(&mut self, delta: isize) {
        let last = MUTE_PICKER_SECONDS.len() - 1;
        self.index = self.index.saturating_add_signed(delta).min(last);
    }

    /// Builds the modal: `lng_mute_box_title`, Cancel and "Mute".
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_mute_box_title())
            .width(st::box_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("mute", tr::lng_mute_menu_mute()))
    }

    /// Paints the drum (five visible rows, the picked one centred between two lines) and
    /// handles the wheel, clicks and Up / Down.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        const VISIBLE: usize = 5;
        let row_h = 32.0;
        let width = ui.available_width();
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(width, row_h * VISIBLE as f32), Sense::click());
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() >= 1.0 {
                self.step(if scroll > 0.0 { -1 } else { 1 });
            }
        }
        if ui.input(|i| i.key_pressed(Key::ArrowUp)) {
            self.step(-1);
        }
        if ui.input(|i| i.key_pressed(Key::ArrowDown)) {
            self.step(1);
        }
        let center_row = VISIBLE / 2;
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let row = ((pos.y - rect.top()) / row_h).floor() as isize;
            self.step(row - center_row as isize);
        }

        let painter = ui.painter().with_clip_rect(rect);
        let fg = colors.color("boxTextFg");
        let line = colors.color("activeLineFg");
        let center_top = rect.top() + row_h * center_row as f32;
        for y in [center_top, center_top + row_h] {
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(rect.left() + 40.0, y),
                    Vec2::new(width - 80.0, 1.0),
                ),
                CornerRadius::ZERO,
                line,
            );
        }
        for slot in 0..VISIBLE {
            let offset = slot as isize - center_row as isize;
            let Some(index) = self.index.checked_add_signed(offset) else {
                continue;
            };
            let Some(&seconds) = MUTE_PICKER_SECONDS.get(index) else {
                continue;
            };
            let distance = offset.unsigned_abs() as f32;
            let alpha = (1.0 - distance * 0.35).max(0.15);
            let font = if offset == 0 {
                font_id_from_style(st::semibold_font)
            } else {
                font_id_from_style(st::box_text_font)
            };
            painter.text(
                Pos2::new(rect.center().x, rect.top() + row_h * (slot as f32 + 0.5)),
                Align2::CENTER_CENTER,
                format_mute_for(seconds),
                font,
                fg.gamma_multiply(alpha),
            );
        }
    }
}

/// "Notification Sound" (`RingtonesBox`): "No sound" or "Default".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectToneBoxState {
    /// The chat whose tone changes.
    pub peer_id: PeerId,
    /// `true` when "No sound" is selected.
    pub silent: bool,
}

impl SelectToneBoxState {
    /// Opens the box with the chat's current choice.
    #[must_use]
    pub const fn new(peer_id: PeerId, silent: bool) -> Self {
        Self { peer_id, silent }
    }

    /// Builds the modal: `lng_ringtones_box_title`, Cancel and Save.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_ringtones_box_title())
            .width(st::box_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()))
    }

    /// Paints the two radio rows.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let mut choice = u8::from(!self.silent);
        for (value, label) in [
            (0_u8, tr::lng_ringtones_box_no_sound()),
            (1_u8, tr::lng_ringtones_box_default()),
        ] {
            Radio::new(
                &mut choice,
                value,
                &label,
                CheckboxStyle::box_checkbox(colors),
                RadioStyle::default_radio(colors),
            )
            .show(ui);
            ui.add_space(st::box_little_skip.0 as f32);
        }
        self.silent = choice == 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_values_and_phrases_follow_tdesktop() {
        assert_eq!(format_mute_for(60 * 15), tr::lng_minutes(15));
        assert_eq!(format_mute_for(3600 * 8), tr::lng_hours(8));
        assert_eq!(format_mute_for(86400 * 3), tr::lng_days(3));
        assert_eq!(format_mute_for(86400 * 14), tr::lng_weeks(2));
        assert_eq!(format_mute_for(86400 * 62), tr::lng_months(2));
    }

    #[test]
    fn mute_for_picker_steps_within_bounds() {
        let mut state = MuteForBoxState::new(PeerId::from_user(9));
        assert_eq!(state.seconds(), 15 * 60);
        state.step(-3);
        assert_eq!(state.index, 0);
        state.step(100);
        assert_eq!(state.seconds(), 86400 * 31 * 3);
    }
}
