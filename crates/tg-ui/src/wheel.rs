//! Mouse wheel step of every scroll area, as `Ui::ScrollArea` gets it from Qt.
//!
//! `Ui::ScrollArea` is a `QScrollArea` (single step 20px, scaled by `style::ConvertScale` in
//! `scroll_area.cpp:428`), and `QAbstractSlider::scrollByDelta` moves it by
//! `wheelScrollLines × singleStep` per notch. egui reports a notch as one line and multiplies
//! it by `InputOptions::line_scroll_speed` (40 points by default), so the step is set here.

/// `QScrollArea`'s single step in points (`QScrollAreaPrivate::init`).
pub const SCROLL_SINGLE_STEP: f32 = 20.0;

/// Lines per notch above which the system means "one screen at a time" (`WHEEL_PAGESCROLL`);
/// the step is capped there, as `QAbstractSlider` caps a notch at a page.
const MAX_WHEEL_LINES: u32 = 100;

/// Points one wheel notch scrolls for `lines` lines per notch.
#[must_use]
pub fn wheel_line_step(lines: u32) -> f32 {
    lines.min(MAX_WHEEL_LINES) as f32 * SCROLL_SINGLE_STEP
}

/// Makes one wheel notch scroll `lines` × [`SCROLL_SINGLE_STEP`] in every scroll area of `ctx`.
pub fn install_wheel_step(ctx: &egui::Context, lines: u32) {
    let step = wheel_line_step(lines);
    if ctx.options(|o| o.input_options.line_scroll_speed) != step {
        ctx.options_mut(|o| o.input_options.line_scroll_speed = step);
    }
}

#[cfg(test)]
mod tests {
    use egui::{
        CentralPanel, Context, Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, RawInput,
        Rect, ScrollArea, TouchPhase, Vec2,
    };

    use super::*;

    fn frame(ctx: &Context, time: f64, events: Vec<Event>, offset: &mut f32) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
            time: Some(time),
            events,
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            CentralPanel::default().show(ui, |ui| {
                let out = ScrollArea::vertical()
                    .id_salt("wheel_test")
                    .show(ui, |ui| ui.allocate_space(Vec2::new(300.0, 10_000.0)));
                *offset = out.state.clamped_offset().y;
            });
        });
        out.textures_delta.clear();
    }

    fn scrolled_by_notches(lines: u32, notches: u32) -> f32 {
        let ctx = Context::default();
        install_wheel_step(&ctx, lines);
        let mut offset = 0.0;
        let pointer = Event::PointerMoved(Pos2::new(200.0, 150.0));
        frame(&ctx, 0.0, vec![pointer], &mut offset);
        let mut time = 0.0;
        for _ in 0..notches {
            time += 0.05;
            let notch = Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: Vec2::new(0.0, -1.0),
                phase: TouchPhase::Move,
                source: MouseWheelSource::Unknown,
                modifiers: Modifiers::NONE,
            };
            frame(&ctx, time, vec![notch], &mut offset);
        }
        // egui spreads a notch over a few frames.
        for _ in 0..60 {
            time += 0.05;
            frame(&ctx, time, Vec::new(), &mut offset);
        }
        offset
    }

    #[test]
    fn a_notch_scrolls_three_single_steps_like_qt() {
        assert!((scrolled_by_notches(3, 1) - 60.0).abs() < 0.5);
        assert!((scrolled_by_notches(3, 25) - 1500.0).abs() < 0.5);
        assert!((scrolled_by_notches(1, 1) - 20.0).abs() < 0.5);
    }

    #[test]
    fn page_scroll_setting_is_capped() {
        assert_eq!(wheel_line_step(u32::MAX), 2000.0);
        assert_eq!(wheel_line_step(0), 0.0);
    }
}
