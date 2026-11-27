//! TGB-3023: Ctrl+C in the media viewer copies the photo (`OverlayWidget::handleKeyPress`,
//! `QKeySequence::Copy`). eframe turns Ctrl+C into [`egui::Event::Copy`] and sends no `Key::C`.

use egui::{Event, Key, Modifiers, Pos2, RawInput, Rect, Vec2};

use crate::media::mock::{
    MOCK_CHAT_PEER_ID, MOCK_PHOTO_LANDSCAPE_ID, MockActionLog, MockMediaBackend,
};
use crate::media::overlay::{MediaOverlayWidget, OverlayEvent};

const VIEWPORT: Vec2 = Vec2::new(1000.0, 700.0);

fn run_frame(
    ctx: &egui::Context,
    overlay: &mut MediaOverlayWidget,
    backend: &MockMediaBackend,
    events: Vec<Event>,
) -> Vec<OverlayEvent> {
    let rect = Rect::from_min_size(Pos2::ZERO, VIEWPORT);
    let input = RawInput {
        screen_rect: Some(rect),
        events,
        ..RawInput::default()
    };
    let mut out = Vec::new();
    ctx.run_ui(input, |ui| {
        out = overlay.show(ui.ctx(), rect, backend, 1_000);
    })
    .textures_delta
    .clear();
    out
}

fn opened() -> (egui::Context, MediaOverlayWidget, MockMediaBackend) {
    let ctx = egui::Context::default();
    let backend = MockMediaBackend::new();
    let mut overlay = MediaOverlayWidget::new();
    overlay
        .open_item(&backend, MOCK_CHAT_PEER_ID, MOCK_PHOTO_LANDSCAPE_ID, 1_000)
        .expect("open sample photo");
    let _ = run_frame(&ctx, &mut overlay, &backend, Vec::new());
    (ctx, overlay, backend)
}

fn copied(backend: &MockMediaBackend) -> bool {
    backend
        .action_log()
        .contains(&MockActionLog::Copied(MOCK_PHOTO_LANDSCAPE_ID))
}

#[test]
fn copy_event_from_the_platform_copies_the_photo() {
    let (ctx, mut overlay, backend) = opened();
    let events = run_frame(&ctx, &mut overlay, &backend, vec![Event::Copy]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OverlayEvent::Copied(id) if *id == MOCK_PHOTO_LANDSCAPE_ID)),
        "Event::Copy must copy the current photo, got {events:?}"
    );
    assert!(copied(&backend));
}

#[test]
fn raw_ctrl_c_key_still_copies() {
    let (ctx, mut overlay, backend) = opened();
    let key = Event::Key {
        key: Key::C,
        physical_key: Some(Key::C),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    };
    let _ = run_frame(&ctx, &mut overlay, &backend, vec![key]);
    assert!(copied(&backend));
}

#[test]
fn plain_c_does_not_copy() {
    let (ctx, mut overlay, backend) = opened();
    let key = Event::Key {
        key: Key::C,
        physical_key: Some(Key::C),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let _ = run_frame(&ctx, &mut overlay, &backend, vec![key]);
    assert!(!copied(&backend));
}
