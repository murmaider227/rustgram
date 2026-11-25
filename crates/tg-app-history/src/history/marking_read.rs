//! When the open chat may send read receipts (`messages.readHistory`, `readMessageContents`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/mainwindow.cpp:604-614` (`MainWindow::markingAsRead`): the main widget is
//!   shown with no layer over it, the window is exposed, not minimized and active, and the user is
//!   not idle (`Api::Updates::isIdle`, `offlineIdleTimeout` without input).
//! - `Telegram/SourceFiles/history/history_widget.cpp:5051-5059` (`markingContentsRead`).
//! - `Telegram/SourceFiles/history/history_inner_widget.cpp:1489-1557` (`readTill` is the last shown
//!   unread incoming item; broadcasts need 4/5 of the item's height in view).

use std::time::Duration;

/// `offlineIdleTimeout` default (`help.getConfig`, 30 s, same as
/// `tg_api::DEFAULT_OFFLINE_IDLE_TIMEOUT_MS`): after this long without input the user counts as
/// idle and nothing is marked read.
pub const OFFLINE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Window state that decides whether shown messages may be marked read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowReadState {
    /// The OS window has keyboard focus (`isActive`).
    pub focused: bool,
    /// The OS window is minimized.
    pub minimized: bool,
    /// A box, layer, menu, the media viewer or another overlay covers the chat (`_layer`).
    pub covered: bool,
    /// Time since the last user input anywhere in the OS, when known.
    pub idle: Option<Duration>,
}

/// `MainWindow::markingAsRead`: `true` only when the user can see the chat and is present.
#[must_use]
pub fn marking_as_read(state: WindowReadState) -> bool {
    state.focused
        && !state.minimized
        && !state.covered
        && state.idle.is_none_or(|idle| idle < OFFLINE_IDLE_TIMEOUT)
}

/// [`marking_as_read`] for the current egui window. `covered` is computed by the caller that knows
/// the boxes, layers and overlays drawn over the chat.
///
/// Idle time is measured like `Core::Application::lastNonIdleTime`: from the later of the last OS
/// input (`tg_platform::system_info::idle_time`) and the app's own last activity (first frame,
/// any input event in this window, a focus change).
#[must_use]
pub fn window_marking_as_read(ctx: &egui::Context, covered: bool) -> bool {
    let (focused, minimized, active) = ctx.input(|i| {
        let active = !i.events.is_empty()
            || i.pointer.delta() != egui::Vec2::ZERO
            || i.smooth_scroll_delta != egui::Vec2::ZERO
            || !i.keys_down.is_empty();
        (i.focused, i.viewport().minimized == Some(true), active)
    });
    let app_idle = app_idle_time(ctx, active);
    let idle = tg_platform::system_info::idle_time().map_or(app_idle, |os| os.min(app_idle));
    marking_as_read(WindowReadState {
        focused,
        minimized,
        covered,
        idle: Some(idle),
    })
}

/// Time since the app's own last activity, kept in the egui context (`updateNonIdle`).
fn app_idle_time(ctx: &egui::Context, active: bool) -> Duration {
    let id = egui::Id::unique("tg_history_last_non_idle");
    let now = std::time::Instant::now();
    let focused = ctx.input(|i| i.focused);
    let last = ctx.data_mut(|d| {
        let entry = d.get_temp::<(std::time::Instant, bool)>(id);
        let focus_changed = entry.is_some_and(|(_, was_focused)| was_focused != focused);
        let last = match entry {
            Some((last, _)) if !active && !focus_changed => last,
            _ => now,
        };
        d.insert_temp(id, (last, focused));
        last
    });
    now.saturating_duration_since(last)
}

/// `unreadShown` (`history_inner_widget.cpp:1531-1536`): whether an unread item spanning
/// `[top, bottom)` counts as seen in the visible range `[vis_top, vis_bottom)`. Broadcast posts
/// need the point at 4/5 of their height in view, other chats any overlap.
#[must_use]
pub const fn unread_item_shown(
    broadcast: bool,
    top: i32,
    bottom: i32,
    vis_top: i32,
    vis_bottom: i32,
) -> bool {
    if broadcast {
        let y = top + (bottom - top) * 4 / 5;
        vis_bottom >= y && vis_top <= y
    } else {
        vis_bottom > top && vis_top < bottom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present() -> WindowReadState {
        WindowReadState {
            focused: true,
            minimized: false,
            covered: false,
            idle: Some(Duration::from_secs(1)),
        }
    }

    #[test]
    fn a2_marking_as_read_requires_a_present_user_and_an_uncovered_chat() {
        assert!(marking_as_read(present()));
        assert!(marking_as_read(WindowReadState {
            idle: None,
            ..present()
        }));
        assert!(!marking_as_read(WindowReadState {
            focused: false,
            ..present()
        }));
        assert!(!marking_as_read(WindowReadState {
            minimized: true,
            ..present()
        }));
        assert!(!marking_as_read(WindowReadState {
            covered: true,
            ..present()
        }));
        assert!(!marking_as_read(WindowReadState {
            idle: Some(OFFLINE_IDLE_TIMEOUT),
            ..present()
        }));
        assert!(!marking_as_read(WindowReadState {
            idle: Some(Duration::from_secs(70)),
            ..present()
        }));
    }

    #[test]
    fn a2_app_activity_counts_as_non_idle() {
        let ctx = egui::Context::default();
        assert!(
            app_idle_time(&ctx, false) < Duration::from_secs(1),
            "first frame is active"
        );
        let id = egui::Id::unique("tg_history_last_non_idle");
        let long_ago = std::time::Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("clock");
        let focused = ctx.input(|i| i.focused);
        ctx.data_mut(|d| d.insert_temp(id, (long_ago, focused)));
        assert!(
            app_idle_time(&ctx, false) >= OFFLINE_IDLE_TIMEOUT,
            "no input: idle"
        );
        assert!(
            app_idle_time(&ctx, true) < Duration::from_secs(1),
            "input: active again"
        );
    }

    #[test]
    fn offline_idle_timeout_matches_the_api_default() {
        assert_eq!(
            OFFLINE_IDLE_TIMEOUT,
            Duration::from_millis(tg_api::DEFAULT_OFFLINE_IDLE_TIMEOUT_MS)
        );
    }

    #[test]
    fn a3_broadcast_posts_need_four_fifths_in_view() {
        // 100 px post at [0, 100): the 4/5 point is y = 80.
        assert!(unread_item_shown(true, 0, 100, 0, 80));
        assert!(!unread_item_shown(true, 0, 100, 0, 79));
        assert!(!unread_item_shown(true, 0, 100, 81, 400));
        // Any overlap is enough outside broadcasts.
        assert!(unread_item_shown(false, 0, 100, 99, 400));
        assert!(!unread_item_shown(false, 0, 100, 100, 400));
        assert!(!unread_item_shown(false, 200, 300, 0, 200));
    }
}
