//! History scroll in a large chat (TGB-2952): a chat of 2,000 messages opened at the bottom
//! and scrolled with the mouse wheel while older pages arrive, the way the live backend
//! delivers them.
//!
//! The live session stores every message of a history page through `Session::insert_message`,
//! which announces it as a new message, so the open chat also receives the older page as
//! `HistoryLiveUpdate::MessageAdded` updates before the page itself is handed out.

use egui::{Event, Modifiers, MouseWheelUnit, RawInput, TouchPhase, pos2, vec2};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

use super::tests_perf_frame::{
    CountingBackend, PEER, VIEW_H, VIEW_W, list_frame, new_context, raw_input, text_message,
};
use crate::history::backend::{HistoryBackend, HistoryMessage};
use crate::history::inner::{
    HistoryInnerWidget, KEEP_LOADED_AFTER_UNLOAD, MAX_LOADED_AT_BOTTOM, OPEN_PAGE_BEFORE,
};
use crate::history::mock::MockHistoryStore;

const CHAT_SIZE: i32 = 2_000;

fn paged(mut backend: CountingBackend, kind: PeerKind) -> CountingBackend {
    backend.paged = true;
    backend.kind = kind;
    backend
}

/// 2,000 text messages with entities, in a private chat.
fn text_chat() -> CountingBackend {
    paged(
        CountingBackend::with_messages((1..=CHAT_SIZE).map(text_message).collect()),
        PeerKind::User,
    )
}

/// 2,000 messages of every kind the demo chats have (photos, polls, replies, link previews,
/// forwards, reactions, service messages, ...), renumbered into one large group.
fn mixed_group_chat() -> CountingBackend {
    let store = MockHistoryStore::new();
    let demo_peers = [
        PeerId::from_user(1001),
        PeerId::from_user(1002),
        PeerId::from_chat(2001),
        PeerId::from_chat(2002),
        PeerId::from_channel(3001),
        PeerId::from_channel(3002),
    ];
    let templates: Vec<HistoryMessage> = demo_peers
        .iter()
        .flat_map(|peer| {
            store
                .load_slice_around(*peer, MsgId::SHOW_AT_END, 10_000, 0)
                .messages
        })
        .filter(|m| m.id.0 > 0)
        .collect();
    assert!(
        templates.len() >= 10,
        "demo chats have {} messages",
        templates.len()
    );
    let messages = (0..CHAT_SIZE)
        .map(|i| {
            let mut msg = templates[i as usize % templates.len()].clone();
            msg.id = MsgId(i + 1);
            msg.peer_id = PEER;
            msg.date_unix = 1_700_000_000 + i * 300;
            msg.is_read = true;
            msg.is_sending = false;
            msg
        })
        .collect();
    paged(CountingBackend::with_messages(messages), PeerKind::Group)
}

struct Run {
    ctx: egui::Context,
    inner: HistoryInnerWidget,
    backend: CountingBackend,
    time: f64,
}

impl Run {
    fn open(backend: CountingBackend) -> Self {
        let ctx = new_context();
        let mut run = Self {
            ctx,
            inner: HistoryInnerWidget::new(),
            backend,
            time: 0.0,
        };
        let kind = run.backend.kind;
        run.inner
            .load_peer(PEER, MsgId::SHOW_AT_END, &run.backend, kind);
        run.frames(10);
        run
    }

    fn frame_with(&mut self, mut input: RawInput) {
        self.time += 1.0 / 60.0;
        input.time = Some(self.time);
        list_frame(&self.ctx, &mut self.inner, &mut self.backend, input, "");
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame_with(raw_input(0.0));
        }
    }

    /// One frame with a wheel step: positive `dy` moves the content down (scrolls up).
    fn wheel(&mut self, dy: f32) {
        let mut input = raw_input(0.0);
        input
            .events
            .push(Event::PointerMoved(pos2(VIEW_W / 2.0, VIEW_H / 2.0)));
        input.events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(0.0, dy),
            phase: TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        });
        self.frame_with(input);
    }

    /// The painted message whose bubble top is nearest the upper quarter of the view, and
    /// that top on screen (a wheel step up keeps it in view).
    fn middle_message(&self) -> Option<(MsgId, f32)> {
        let mid = VIEW_H / 4.0;
        self.inner
            .messages()
            .iter()
            .filter_map(|m| {
                self.inner
                    .last_painted_bubble_rect(m.id)
                    .map(|r| (m.id, r.top()))
                    .filter(|(_, top)| *top >= 0.0)
            })
            .min_by(|a, b| (a.1 - mid).abs().total_cmp(&(b.1 - mid).abs()))
    }

    fn top_of(&self, id: MsgId) -> Option<f32> {
        self.inner.last_painted_bubble_rect(id).map(|r| r.top())
    }

    /// Delivers the messages of the page before the first loaded one as live "new message"
    /// updates (what `HistoryWidget::sync_live_history_updates` does with the session's
    /// change stream when a history page is stored).
    fn deliver_older_page_as_live_updates(&mut self, limit: usize) {
        let Some(first) = self.inner.messages().first().map(|m| m.id) else {
            return;
        };
        let end = self.backend.messages.partition_point(|m| m.id < first);
        let page: Vec<_> = self.backend.messages[end.saturating_sub(limit)..end].to_vec();
        let kind = self.backend.kind;
        for msg in page {
            self.inner.apply_live_added_message(msg, kind);
        }
    }

    /// Scrolls up by `steps` wheel steps and lets each settle.
    fn scroll_up(&mut self, steps: usize, step_px: f32) {
        for _ in 0..steps {
            self.wheel(step_px);
            self.frames(6);
        }
    }

    fn assert_thumb_at_bottom(&self, what: &str) {
        let m = self.inner.last_scroll_metrics();
        assert!(
            m.thumb_position() > 0.999,
            "{what}: the view is at the bottom but the thumb is at {:.3} of the track ({m:?})",
            m.thumb_position()
        );
        assert!(
            (m.content_height - (m.list_height as f32).max(m.viewport_height)).abs() <= 1.0,
            "{what}: the scroll bar is sized from {} px of content, the list is {} px ({m:?})",
            m.content_height,
            m.list_height
        );
    }
}

/// Scrolls up in wheel steps. After each step settles, the message in the middle of the view
/// has moved down by exactly the wheel distance, and on no frame in between did it move
/// backwards or further than the step (a frame drawn with stale positions shows as a jump
/// that snaps back: the stutter).
fn assert_wheel_moves_content_exactly(run: &mut Run, steps: usize, step_px: f32, live: bool) {
    let opened_with = run.inner.messages().len();
    for step in 0..steps {
        let Some((anchor, before)) = run.middle_message() else {
            panic!("step {step}: nothing painted");
        };
        if live && run.backend.older_pending.get() && run.backend.older_requests.get() > 0 {
            // The page "arrives": the session announces its messages, then hands it out.
            run.deliver_older_page_as_live_updates(40);
            run.backend.older_pending.set(false);
        }
        let loaded_before = run.inner.messages().len();
        run.wheel(step_px);
        let mut last = before;
        for frame in 0..30 {
            if frame > 0 {
                run.frames(1);
            }
            let Some(now) = run.top_of(anchor) else {
                panic!(
                    "step {step}, frame {frame}: message {anchor:?} left the view after a \
                     {step_px} px wheel step (loaded {loaded_before} -> {} messages)",
                    run.inner.messages().len()
                );
            };
            assert!(
                now >= last - 0.5 && now <= before + step_px + 0.5,
                "step {step}, frame {frame}: message {anchor:?} went from {last} to {now} \
                 (wheel step {step_px} px from {before}; loaded {loaded_before} -> {} messages)",
                run.inner.messages().len()
            );
            last = now;
        }
        if live {
            run.backend.older_pending.set(true);
        }
        let moved = last - before;
        assert!(
            (moved - step_px).abs() <= 1.0,
            "step {step}: a wheel step of {step_px} px moved message {anchor:?} by {moved} px \
             (loaded {loaded_before} -> {} messages)",
            run.inner.messages().len()
        );
    }
    assert!(
        run.inner.messages().len() >= opened_with + 40,
        "scrolling up prepended at least one older page ({opened_with} -> {} messages)",
        run.inner.messages().len()
    );
}

#[test]
fn wheel_scroll_moves_by_the_wheel_distance_while_older_pages_arrive() {
    let mut run = Run::open(text_chat());
    assert_wheel_moves_content_exactly(&mut run, 40, 400.0, false);
}

#[test]
fn wheel_scroll_moves_by_the_wheel_distance_in_a_mixed_group() {
    let mut run = Run::open(mixed_group_chat());
    assert_wheel_moves_content_exactly(&mut run, 40, 300.0, false);
}

#[test]
fn wheel_scroll_moves_by_the_wheel_distance_when_pages_also_arrive_as_live_updates() {
    let mut run = Run::open(mixed_group_chat());
    run.backend.older_pending.set(true);
    assert_wheel_moves_content_exactly(&mut run, 40, 300.0, true);
}

/// A page announced as new messages while the view sits at the bottom does not enter the list
/// (the preload brings it when the user scrolls up) and the thumb stays at the bottom.
#[test]
fn a_page_announced_as_new_messages_does_not_grow_the_list_at_the_bottom() {
    let mut run = Run::open(mixed_group_chat());
    let opened_with = run.inner.messages().len();
    run.assert_thumb_at_bottom("opened");
    run.backend.older_pending.set(true);
    for page in 0..6 {
        let started = std::time::Instant::now();
        run.deliver_older_page_as_live_updates(100);
        println!(
            "measured: a 100-message page announced as new messages, page {page}: {:.2} ms of \
             list work ({} messages loaded)",
            started.elapsed().as_secs_f64() * 1e3,
            run.inner.messages().len()
        );
        run.frames(3);
        run.assert_thumb_at_bottom(&format!("after page {page}"));
    }
    assert_eq!(
        run.inner.messages().len(),
        opened_with,
        "older messages announced as new ones stay out of the list"
    );
}

#[test]
fn at_the_bottom_the_scroll_thumb_sits_at_the_bottom_of_the_track() {
    let mut run = Run::open(mixed_group_chat());
    run.assert_thumb_at_bottom("opened at the bottom");
    run.scroll_up(30, 300.0);
    // Back down with the wheel, past the bottom.
    for _ in 0..80 {
        run.wheel(-600.0);
        run.frames(2);
    }
    run.frames(30);
    run.assert_thumb_at_bottom("scrolled back down with the wheel");
    run.scroll_up(10, 300.0);
    // The wheel's easing has finished before the button is pressed.
    run.frames(60);
    run.inner.request_scroll_to_bottom();
    run.frames(10);
    run.assert_thumb_at_bottom("after the scroll-down button");
}

/// The owner scrolled far up, then went back to the bottom: the pages read on the way up are
/// dropped, and the chat holds what a fresh open holds.
#[test]
fn returning_to_the_bottom_unloads_the_old_pages() {
    let mut run = Run::open(mixed_group_chat());
    let opened = run.inner.last_scroll_metrics();
    run.scroll_up(60, 1_000.0);
    run.frames(60);
    let far_up = run.inner.messages().len();
    assert!(far_up > 250, "scrolled far up: {far_up} messages loaded");

    // The scroll-down button (or a click on the open chat in the chat list).
    run.inner.request_scroll_to_bottom();
    run.frames(20);
    let back = run.inner.last_scroll_metrics();
    run.assert_thumb_at_bottom("after the scroll-down button");
    assert!(
        back.loaded_messages <= OPEN_PAGE_BEFORE + 1 + 2 * 40,
        "back at the bottom: {} messages loaded (opened with {}, {far_up} far up)",
        back.loaded_messages,
        opened.loaded_messages
    );
    assert!(
        back.content_height <= opened.content_height * 2.5,
        "back at the bottom the content is {} px, it was {} px when opened",
        back.content_height,
        opened.content_height
    );

    // Scrolling far up again pages the history back in, and coming down with the wheel past
    // the cap drops the oldest pages again.
    run.scroll_up(150, 2_000.0);
    let far_up = run.inner.messages().len();
    assert!(
        far_up > MAX_LOADED_AT_BOTTOM,
        "scrolled far up again: {far_up} messages loaded"
    );
    for _ in 0..400 {
        run.wheel(-2_000.0);
        run.frames(1);
        if run.inner.last_scroll_metrics().thumb_position() > 0.999 {
            break;
        }
    }
    run.frames(20);
    run.assert_thumb_at_bottom("scrolled back down with the wheel");
    assert!(
        run.inner.messages().len() <= KEEP_LOADED_AFTER_UNLOAD,
        "back at the bottom with the wheel: {} messages loaded",
        run.inner.messages().len()
    );
}
