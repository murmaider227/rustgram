//! TGB-2908: a sent note flies from the field into its own place in the list
//! (`MessageSendingTextFlight`), not into a made-up bubble on the right.
//!
//! The owner's case: Saved Messages in a wide window, so own messages sit on the left. While the
//! note was pending the old overlay drew a bubble about as wide as the composer above it on the
//! right. Writes `flight_<frame>.png` into `$SEND_FLIGHT_SNAP_DIR` when that variable is set.

use egui::{Event, Key, Modifiers, Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_ui::fonts::register_tdesktop_fonts;

use super::SendingFieldText;
use crate::history::HistoryWidget;
use crate::history::backend::HistoryBackend;
use crate::history::mock::MockHistoryStore;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const SIZE: Vec2 = Vec2::new(900.0, 640.0);
const STEP_DT: f32 = 1.0 / 60.0;

/// One GPU device at a time: renderers starting together have crashed test processes.
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct FlightState {
    _gpu: std::sync::MutexGuard<'static, ()>,
    widget: HistoryWidget,
    store: MockHistoryStore,
    palette: Palette,
    peer: PeerId,
    wide: bool,
}

type FlightHarness = egui_kittest::Harness<'static, FlightState>;

fn open_saved_messages(wide: bool) -> FlightHarness {
    let store = MockHistoryStore::new();
    let peer = store.self_peer().0;
    let state = FlightState {
        _gpu: GPU
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        widget: HistoryWidget::new(),
        store,
        palette: Palette::day(),
        peer,
        wide,
    };
    let mut harness = HarnessBuilder::default()
        .with_size(SIZE)
        .with_step_dt(STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut FlightState| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        st.widget.show_with_backend(
                            ui,
                            st.peer,
                            &mut st.store,
                            true,
                            false,
                            st.wide,
                            &st.palette,
                            &EnglishStrings,
                        );
                    });
            },
            state,
        );
    harness.run_steps(6);
    let peer = harness.state().peer;
    harness.state_mut().widget.request_scroll_to_bottom(peer);
    harness.run_steps(12);
    harness
}

fn last_message(harness: &FlightHarness) -> MsgId {
    harness
        .state()
        .widget
        .inner()
        .messages()
        .last()
        .map(|m| m.id)
        .expect("the chat has messages")
}

/// Types `text` into the field, then sends it with Enter. Returns the new message's id.
fn send_note(harness: &mut FlightHarness, text: &str) -> MsgId {
    let before = last_message(harness);
    {
        let compose = harness.state_mut().widget.compose_mut();
        compose.text = text.to_owned();
        compose.rich_edit.text = text.to_owned();
        compose.focus_requested = true;
    }
    harness.run_steps(3);
    for pressed in [true, false] {
        harness.input_mut().events.push(Event::Key {
            key: Key::Enter,
            physical_key: Some(Key::Enter),
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
    }
    harness.step();
    let sent = last_message(harness);
    assert_ne!(sent, before, "the note was sent");
    sent
}

fn save(image: &image::RgbaImage, name: &str) {
    if let Some(dir) = std::env::var_os("SEND_FLIGHT_SNAP_DIR") {
        let path = std::path::PathBuf::from(dir).join(format!("flight_{name}.png"));
        image.save(&path).expect("save the flight snapshot");
    }
}

/// Pixels of `area` that differ between two frames.
fn changed_pixels(a: &image::RgbaImage, b: &image::RgbaImage, area: Rect) -> usize {
    let mut changed = 0;
    for y in area.top() as u32..area.bottom() as u32 {
        for x in area.left() as u32..area.right() as u32 {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            if p.0.iter().zip(q.0.iter()).any(|(u, v)| u.abs_diff(*v) > 24) {
                changed += 1;
            }
        }
    }
    changed
}

#[test]
fn a_note_in_wide_saved_messages_flies_on_its_own_side() {
    let mut harness = open_saved_messages(true);
    let sent = send_note(&mut harness, "B");
    let compose_top = harness.state().widget.last_compose_rect().top();
    // Two frames (33 ms) into the 280 ms flight: the owner's screenshot.
    harness.run_steps(2);
    let bubble = harness
        .state()
        .widget
        .inner()
        .last_painted_bubble_rect(sent)
        .expect("the note is in the list");
    let pending = harness.render().expect("render");
    save(&pending, "wide_33ms");
    harness.run_steps(30);
    let settled = harness.render().expect("render");
    save(&settled, "wide_settled");

    assert!(
        bubble.right() < SIZE.x * 0.5,
        "own notes sit on the left in a wide chat: {bubble:?}"
    );
    // The right half of the band above the field holds nothing but the chat background, during
    // the flight and after it. The old overlay drew a field-wide bubble there.
    let right_band = Rect::from_min_max(
        Pos2::new(SIZE.x * 0.55, compose_top - 60.0),
        Pos2::new(SIZE.x - 24.0, compose_top - 2.0),
    );
    assert_eq!(
        changed_pixels(&pending, &settled, right_band),
        0,
        "nothing flies on the right while the note is pending"
    );
    assert!(
        harness.state().widget.send_flight.is_none(),
        "the flight is over"
    );
}

#[test]
fn the_flight_carries_the_message_from_the_field_to_its_place() {
    let mut harness = open_saved_messages(false);
    let sent = send_note(&mut harness, "Note to self");
    harness.step();
    assert!(
        harness.state().widget.send_flight.is_some(),
        "a plain text note flies"
    );
    let bubble = harness
        .state()
        .widget
        .inner()
        .last_painted_bubble_rect(sent)
        .expect("the note is in the list");
    let early = harness.render().expect("render");
    save(&early, "narrow_16ms");
    harness.run_steps(30);
    let settled = harness.render().expect("render");
    save(&settled, "narrow_settled");
    assert!(
        bubble.left() > SIZE.x * 0.5,
        "own notes sit on the right in a narrow chat: {bubble:?}"
    );
    // Early in the flight the message is still near the field, so its place in the list looks
    // different from the settled frame; at the end it is drawn there.
    assert!(
        changed_pixels(&early, &settled, bubble.shrink(2.0)) > 0,
        "the message is not yet in its place 16 ms after sending"
    );
    assert!(harness.state().widget.send_flight.is_none());
}

#[test]
fn hidden_or_empty_field_text_does_not_fly() {
    let ctx = egui::Context::default();
    let mut galley = None;
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        galley = Some(ui.painter().layout_no_wrap(
            "B".to_owned(),
            egui::FontId::proportional(13.0),
            egui::Color32::BLACK,
        ));
    });
    output.textures_delta.clear();
    let galley = galley.expect("laid out");
    let at = Pos2::new(100.0, 500.0);
    let field = Rect::from_min_size(Pos2::new(90.0, 490.0), Vec2::new(300.0, 40.0));
    assert!(SendingFieldText::visible_in(galley.clone(), at, field).is_some());
    let scrolled = field.translate(Vec2::new(0.0, 15.0));
    assert!(
        SendingFieldText::visible_in(galley, at, scrolled).is_none(),
        "text scrolled out of the field does not fly"
    );
}
