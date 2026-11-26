//! Wave 5 COPY package fixtures and evidence (TGB-3016). Mock data only.
//!
//! `w5_copy_snapshot` writes `w5_copy_menu.png` (the context menu opened over a text selection)
//! and `w5_copy_clipboard.txt` (what `Ctrl+C` put on the clipboard for a part of a message and
//! for a drag across two messages) into `$W5_COPY_SNAP_DIR` when that variable is set. The file
//! uses only APIs that predate the fix, so the same snapshot renders the "before" evidence on
//! the old code.

use egui::{Event, Modifiers, OutputCommand, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;

use crate::history::backend::HistoryMessage;
use crate::history::tests_w4_text_snap::{InnerState, group_peer, inner_harness, message};
use tg_app_core::stand_in::types::MsgId;

pub(crate) const FIRST_MSG: i32 = 1;
pub(crate) const SECOND_MSG: i32 = 2;
pub(crate) const FIRST_TEXT: &str = "Harbor lights come on at seven tonight";
pub(crate) const SECOND_TEXT: &str = "Bring the blue lantern";

/// Two outgoing messages (no name line, so the text starts at the bubble's top-left padding).
pub(crate) fn copy_messages() -> Vec<HistoryMessage> {
    let peer = group_peer();
    let mut first = message(peer, FIRST_MSG, 1001, "Mira Vance", FIRST_TEXT);
    first.outgoing = true;
    let mut second = message(peer, SECOND_MSG, 1001, "Mira Vance", SECOND_TEXT);
    second.outgoing = true;
    vec![first, second]
}

pub(crate) fn copy_harness() -> Harness<'static, InnerState> {
    inner_harness(copy_messages(), Vec2::new(720.0, 360.0))
}

/// The first glyph of message `id`'s text.
pub(crate) fn text_start(harness: &Harness<'static, InnerState>, id: i32) -> Pos2 {
    harness
        .state()
        .inner
        .last_painted_bubble_rect(MsgId(id))
        .expect("message painted")
        .left_top()
        + Vec2::new(14.0, 16.0)
}

/// A real left-button drag from `from` to `to` in a few pointer moves.
pub(crate) fn drag(harness: &mut Harness<'static, InnerState>, from: Pos2, to: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(from));
    harness.step();
    harness.input_mut().events.push(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    for t in [0.25, 0.5, 0.75, 1.0] {
        let pos = from + (to - from) * t;
        harness.input_mut().events.push(Event::PointerMoved(pos));
        harness.step();
    }
    harness.input_mut().events.push(Event::PointerButton {
        pos: to,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.step();
}

/// A real right click at `pos`.
pub(crate) fn right_click(harness: &mut Harness<'static, InnerState>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
    for pressed in [true, false] {
        harness.input_mut().events.push(Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        harness.step();
    }
    harness.step();
}

/// Runs a few frames and returns the last text the app put on the clipboard.
pub(crate) fn step_until_copied(harness: &mut Harness<'static, InnerState>) -> Option<String> {
    for _ in 0..3 {
        harness.step();
        let copied = harness
            .output()
            .platform_output
            .commands
            .iter()
            .rev()
            .find_map(|c| match c {
                OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            });
        if copied.is_some() {
            return copied;
        }
    }
    None
}

/// What the platform backend sends for `Ctrl+C` on every keyboard layout.
pub(crate) fn press_copy(harness: &mut Harness<'static, InnerState>) -> Option<String> {
    harness.input_mut().events.push(Event::Copy);
    step_until_copied(harness)
}

#[test]
fn w5_copy_snapshot() {
    let Some(dir) = std::env::var_os("W5_COPY_SNAP_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let mut report = String::new();

    let mut harness = copy_harness();
    let start = text_start(&harness, FIRST_MSG);
    drag(&mut harness, start, start + Vec2::new(110.0, 0.0));
    report.push_str(&format!(
        "Ctrl+C after selecting part of one message: {:?}\n",
        press_copy(&mut harness)
    ));
    right_click(&mut harness, start + Vec2::new(40.0, 0.0));
    let img = harness.render().expect("render snapshot");
    img.save(dir.join("w5_copy_menu.png"))
        .expect("save snapshot");

    let mut harness = copy_harness();
    let from = text_start(&harness, FIRST_MSG);
    let to = text_start(&harness, SECOND_MSG) + Vec2::new(30.0, 0.0);
    drag(&mut harness, from, to);
    report.push_str(&format!(
        "Ctrl+C after a drag across two messages: {:?}\n",
        press_copy(&mut harness)
    ));
    let img = harness.render().expect("render snapshot");
    img.save(dir.join("w5_copy_two_messages.png"))
        .expect("save snapshot");

    std::fs::write(dir.join("w5_copy_clipboard.txt"), report).expect("save report");
}
