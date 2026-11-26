//! TGB-2971: scrolling to the bottom of a channel does not open the next unread channel; a
//! deliberate pull past the threshold does (`HistoryView::PullToNextChannel`).
//!
//! Drives the whole history widget on the demo channel with real scroll events. Writes
//! `pull_<state>.png` into `$PULL_SNAP_DIR` when that variable is set.

use egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, TouchPhase, Vec2};
use egui_kittest::HarnessBuilder;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::mock::MockHistoryStore;
use crate::history::pull_to_next_channel::NextUnreadChannelHint;
use crate::history::{HistoryAction, HistoryWidget};
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::PeerId;

const SIZE: Vec2 = Vec2::new(900.0, 640.0);
const STEP_DT: f32 = 1.0 / 60.0;
/// The demo "dev log" channel and the channel the pull opens.
const CHANNEL: PeerId = PeerId::from_channel(3001);
const NEXT: PeerId = PeerId::from_channel(3002);

/// One GPU device at a time: renderers starting together have crashed test processes.
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct PullState {
    _gpu: std::sync::MutexGuard<'static, ()>,
    widget: HistoryWidget,
    store: MockHistoryStore,
    palette: Palette,
    /// Every "open the next unread channel" the widget asked for.
    opened: Vec<PeerId>,
}

type PullHarness = egui_kittest::Harness<'static, PullState>;

fn open_channel() -> PullHarness {
    let mut widget = HistoryWidget::new();
    widget.set_next_unread_channel(Some(NextUnreadChannelHint {
        peer_id: NEXT,
        title: "Release notes".to_owned(),
        unread_count: 3,
        color_index: 2,
    }));
    let state = PullState {
        _gpu: GPU
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        widget,
        store: MockHistoryStore::new(),
        palette: Palette::day(),
        opened: Vec::new(),
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
            |ui, st: &mut PullState| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let action = st.widget.show_with_backend(
                            ui,
                            CHANNEL,
                            &mut st.store,
                            true,
                            false,
                            false,
                            &st.palette,
                            &EnglishStrings,
                        );
                        if let Some(HistoryAction::OpenNextUnreadChannel(peer)) = action {
                            st.opened.push(peer);
                        }
                    });
            },
            state,
        );
    frames(&mut harness, 6);
    harness.state_mut().widget.request_scroll_to_bottom(CHANNEL);
    frames(&mut harness, 12);
    assert!(
        !harness.state().widget.inner().show_scroll_down_button(),
        "the channel opened at its bottom"
    );
    harness
}

fn frames(harness: &mut PullHarness, count: usize) {
    for _ in 0..count {
        harness.step();
    }
}

/// One scroll event over the list, then one frame. `down_px` scrolls towards newer messages.
fn scroll(harness: &mut PullHarness, phase: TouchPhase, down_px: f32) {
    let at = Pos2::new(SIZE.x * 0.5, SIZE.y * 0.5);
    harness.input_mut().events.push(Event::PointerMoved(at));
    harness.input_mut().events.push(Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: Vec2::new(0.0, -down_px),
        modifiers: Modifiers::NONE,
        phase,
        source: MouseWheelSource::Unknown,
    });
    harness.step();
}

fn stretch(harness: &mut PullHarness) -> f32 {
    harness.state_mut().widget.pull_next_channel_mut().pull_px()
}

fn save(harness: &mut PullHarness, name: &str) {
    if let Some(dir) = std::env::var_os("PULL_SNAP_DIR") {
        let img = harness.render().expect("render the channel");
        let path = std::path::PathBuf::from(dir).join(format!("pull_{name}.png"));
        img.save(&path).expect("save the pull snapshot");
    }
}

/// The owner's case: an ordinary mouse wheel (120 px notches, no scroll phase) taken down to the
/// bottom of the channel and on past it.
#[test]
fn wheel_scrolling_to_the_bottom_and_past_it_does_not_open_the_next_channel() {
    let mut harness = open_channel();
    for _ in 0..8 {
        scroll(&mut harness, TouchPhase::Move, -120.0);
        frames(&mut harness, 3);
    }
    for _ in 0..40 {
        scroll(&mut harness, TouchPhase::Move, 120.0);
        frames(&mut harness, 3);
    }
    // The wheel stops; the old gesture fired 220 ms after the last step.
    frames(&mut harness, 60);
    save(&mut harness, "wheel_past_bottom");
    assert_eq!(
        harness.state().opened,
        Vec::<PeerId>::new(),
        "wheel scrolling at the bottom of the channel opened the next one"
    );
    assert_eq!(
        stretch(&mut harness),
        0.0,
        "a wheel never stretches the list"
    );
}

/// A fast wheel: several notches in one frame, in bursts.
#[test]
fn a_fast_wheel_at_the_bottom_does_not_open_the_next_channel() {
    let mut harness = open_channel();
    for _ in 0..12 {
        for _ in 0..5 {
            harness.input_mut().events.push(Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: Vec2::new(0.0, -3.0),
                modifiers: Modifiers::NONE,
                phase: TouchPhase::Move,
                source: MouseWheelSource::Unknown,
            });
        }
        scroll(&mut harness, TouchPhase::Move, 120.0);
    }
    frames(&mut harness, 60);
    assert_eq!(harness.state().opened, Vec::<PeerId>::new());
}

/// Trackpad inertia after the fingers lifted (macOS reports it as momentum) reaches the bottom.
#[test]
fn inertia_reaching_the_bottom_does_not_open_the_next_channel() {
    let mut harness = open_channel();
    let at = Pos2::new(SIZE.x * 0.5, SIZE.y * 0.5);
    for _ in 0..60 {
        harness.input_mut().events.push(Event::PointerMoved(at));
        harness.input_mut().events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -40.0),
            modifiers: Modifiers::NONE,
            phase: TouchPhase::Move,
            source: MouseWheelSource::Momentum,
        });
        harness.step();
    }
    frames(&mut harness, 60);
    assert_eq!(harness.state().opened, Vec::<PeerId>::new());
}

/// Fingers down, a pull past the threshold (96 px of stretch = 240 px of travel), a short hold,
/// fingers up.
#[test]
fn a_pull_past_the_threshold_held_and_released_opens_the_next_channel() {
    let mut harness = open_channel();
    scroll(&mut harness, TouchPhase::Start, 0.0);
    for _ in 0..10 {
        scroll(&mut harness, TouchPhase::Move, 10.0);
    }
    assert!(
        (stretch(&mut harness) - 40.0).abs() < 0.5,
        "100 px of travel stretch the list by 40 px, got {}",
        stretch(&mut harness)
    );
    save(&mut harness, "gesture_pulling");
    for _ in 0..20 {
        scroll(&mut harness, TouchPhase::Move, 10.0);
    }
    // The hold: the dwell is 120 ms, and the card takes 250 ms to open.
    frames(&mut harness, 30);
    assert!(
        harness
            .state_mut()
            .widget
            .pull_next_channel_mut()
            .is_ready()
    );
    save(&mut harness, "gesture_armed");
    assert_eq!(
        harness.state().opened,
        Vec::<PeerId>::new(),
        "nothing opens while the fingers are down"
    );
    scroll(&mut harness, TouchPhase::End, 0.0);
    frames(&mut harness, 2);
    assert_eq!(harness.state().opened, vec![NEXT]);
}

/// A flick far past the threshold whose fingers lift at once, and a short pull held long.
#[test]
fn a_flick_or_a_short_pull_does_not_open_the_next_channel() {
    let mut harness = open_channel();
    scroll(&mut harness, TouchPhase::Start, 0.0);
    scroll(&mut harness, TouchPhase::Move, 400.0);
    scroll(&mut harness, TouchPhase::End, 0.0);
    frames(&mut harness, 90);
    assert_eq!(harness.state().opened, Vec::<PeerId>::new(), "a flick");
    assert_eq!(stretch(&mut harness), 0.0, "the stretch springs back");

    scroll(&mut harness, TouchPhase::Start, 0.0);
    for _ in 0..20 {
        scroll(&mut harness, TouchPhase::Move, 10.0);
    }
    frames(&mut harness, 60);
    scroll(&mut harness, TouchPhase::End, 0.0);
    frames(&mut harness, 90);
    assert_eq!(harness.state().opened, Vec::<PeerId>::new(), "a short pull");
}

/// The indicator and the hint at a held pull of 40 px and of 104 px (past the threshold).
#[test]
fn pull_indicator_snapshots() {
    for pull_px in [40.0f32, 104.0] {
        let mut harness = open_channel();
        let now = harness.ctx.input(|i| i.time);
        harness
            .state_mut()
            .widget
            .pull_next_channel_mut()
            .set_pull_preview(pull_px, now);
        frames(&mut harness, 4);
        save(&mut harness, &format!("held_{pull_px}"));
        assert_eq!(harness.state().opened, Vec::<PeerId>::new());
    }
}
