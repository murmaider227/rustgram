//! Security review #3 (SEC3-A, A1): the call UI must not show over the passcode lock.

use egui::{Event, Key, Modifiers, Pos2, Rect, Vec2};

use crate::calls::AppCallsHost;
use crate::calls::mock::MockCallsBackend;

fn ctrl_shift(key: Key) -> Vec<Event> {
    vec![
        Event::ModifiersChanged(Modifiers::COMMAND | Modifiers::SHIFT),
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
        },
    ]
}

/// Runs one frame and returns the number of painted shapes.
fn frame(ctx: &egui::Context, host: &mut AppCallsHost, events: Vec<Event>) -> usize {
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| host.update_and_render(ui));
    out.textures_delta.clear();
    out.shapes.len()
}

fn mock_host() -> AppCallsHost {
    AppCallsHost::with_backend(Box::new(MockCallsBackend::new()), false)
}

#[test]
fn a1_lock_hides_call_panel_and_keeps_the_call_running() {
    let ctx = egui::Context::default();
    let mut host = mock_host();
    host.backend_mut()
        .simulate_incoming_call(MockCallsBackend::PEER_DESIGN_STUDIO, false, 0);
    assert!(host.backend().active_call().is_some());
    assert!(
        frame(&ctx, &mut host, Vec::new()) > 0,
        "unlocked: the call panel and top bar are painted"
    );

    host.set_passcode_locked(true);
    for _ in 0..3 {
        assert_eq!(
            frame(&ctx, &mut host, Vec::new()),
            0,
            "locked: no call window, top bar or toast is painted"
        );
    }
    assert!(
        host.backend().active_call().is_some(),
        "the lock does not end the call"
    );

    host.set_passcode_locked(false);
    assert!(
        frame(&ctx, &mut host, Vec::new()) > 0,
        "after unlock the call panel shows again"
    );
}

#[test]
fn a1_recent_calls_closed_on_lock_and_not_reopened_by_shortcut_or_request() {
    let ctx = egui::Context::default();
    let mut host = mock_host();
    host.open_recent_calls();
    assert!(host.is_recent_calls_open());

    host.set_passcode_locked(true);
    assert!(!host.is_recent_calls_open(), "the lock closes Recent Calls");

    let _ = frame(&ctx, &mut host, ctrl_shift(Key::H));
    assert!(
        !host.is_recent_calls_open(),
        "Ctrl+Shift+H must not open Recent Calls while locked"
    );
    host.open_recent_calls();
    assert!(
        !host.is_recent_calls_open(),
        "open request refused while locked"
    );

    host.start_outgoing_call(MockCallsBackend::PEER_AUDIO_LAB, false);
    assert!(
        host.backend().active_call().is_none(),
        "no call can be started while locked"
    );
    let _ = frame(&ctx, &mut host, ctrl_shift(Key::C));
    assert!(
        host.backend().active_call().is_none(),
        "demo call shortcuts do nothing while locked"
    );

    host.set_passcode_locked(false);
    let _ = frame(&ctx, &mut host, ctrl_shift(Key::H));
    assert!(
        host.is_recent_calls_open(),
        "Ctrl+Shift+H works after unlock"
    );
}
