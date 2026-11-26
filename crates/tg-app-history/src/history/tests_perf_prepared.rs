//! Chat view frame path (TGB-2893, package E): the work an idle frame no longer repeats, and
//! the cases in which the kept results must be thrown away.

use egui::{CentralPanel, Context, Frame, RawInput, Rect, pos2, vec2};
use tg_app_core::core_types::dto::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerKind};
use tg_text::TextWithEntities;
use tg_ui::text::text_preparation_count;

use super::tests_perf_frame::{
    CountingBackend, MESSAGE_COUNT, PEER, VIEW_H, VIEW_W, list_frame, new_context, open_chat,
    raw_input, text_message,
};
use crate::history::HistoryWidget;
use crate::history::backend::{HistoryMedia, VoiceAttachment};
use crate::history::inner::HistoryInnerWidget;
use crate::history::theme::HistoryColors;

fn idle_frames(
    ctx: &Context,
    inner: &mut HistoryInnerWidget,
    backend: &mut CountingBackend,
    from_time: f64,
    frames: usize,
) -> u64 {
    let before = text_preparation_count();
    for frame in 0..frames {
        let time = from_time + frame as f64 / 60.0;
        list_frame(ctx, inner, backend, raw_input(time), "");
    }
    text_preparation_count() - before
}

#[test]
fn sixty_idle_frames_prepare_no_message_text() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let before_open = text_preparation_count();
    let mut inner = open_chat(&ctx, &mut backend, 8);
    let opening = text_preparation_count() - before_open;
    assert_eq!(inner.messages().len(), MESSAGE_COUNT as usize);
    let painted = inner.last_frame_painted_count();
    assert!(
        painted >= 4,
        "the viewport shows several bubbles: {painted}"
    );

    let idle = idle_frames(&ctx, &mut inner, &mut backend, 1.0, 60);
    println!(
        "measured: text preparations: {opening} while opening the chat, {idle} on 60 idle \
         frames ({painted} bubbles painted per frame)"
    );
    assert!(opening > 0, "opening the chat prepares the visible texts");
    assert_eq!(idle, 0, "an idle frame must reuse the prepared texts");
    assert_eq!(
        inner.last_frame_laid_out_count(),
        0,
        "an idle frame lays nothing out"
    );
}

#[test]
fn an_idle_frame_with_a_search_filter_prepares_nothing() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut inner = open_chat(&ctx, &mut backend, 8);
    list_frame(&ctx, &mut inner, &mut backend, raw_input(1.0), "Galley");
    let before = text_preparation_count();
    for frame in 0..60 {
        let time = 1.1 + frame as f64 / 60.0;
        list_frame(&ctx, &mut inner, &mut backend, raw_input(time), "Galley");
    }
    assert_eq!(text_preparation_count() - before, 0);
}

#[test]
fn an_edited_message_is_prepared_again_and_only_that_one() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut inner = open_chat(&ctx, &mut backend, 8);
    assert_eq!(idle_frames(&ctx, &mut inner, &mut backend, 1.0, 5), 0);

    // The newest message is in view at the bottom.
    let mut edited = text_message(MESSAGE_COUNT);
    edited.text = TextWithEntities::simple("edited text that was not there before");
    inner.update_message_in_place(edited);
    let after_edit = idle_frames(&ctx, &mut inner, &mut backend, 2.0, 3);
    assert!(
        after_edit > 0,
        "the edited message is laid out and prepared again"
    );
    // The edited message, plus what came into view because it got shorter: a couple of
    // messages, not the whole viewport on each of the three frames.
    assert!(
        after_edit <= 40,
        "one message was edited, {after_edit} preparations is more than a couple of messages"
    );
    assert_eq!(idle_frames(&ctx, &mut inner, &mut backend, 3.0, 30), 0);
}

fn frame_at(
    ctx: &Context,
    inner: &mut HistoryInnerWidget,
    backend: &mut CountingBackend,
    time: f64,
    width: f32,
    palette: &Palette,
) {
    let peer = backend.peer();
    let colors = HistoryColors::from_palette(palette);
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, VIEW_H))),
        time: Some(time),
        focused: true,
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let rect = ui.max_rect();
            let _ = inner.show(ui, rect, &peer, false, "", &colors, backend);
        });
    });
    out.textures_delta.clear();
}

#[test]
fn width_theme_and_scale_changes_prepare_the_text_again() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut inner = open_chat(&ctx, &mut backend, 8);
    let day = Palette::day();
    let night = Palette::night();
    let mut time = 1.0;
    let mut run = |inner: &mut HistoryInnerWidget,
                   backend: &mut CountingBackend,
                   width: f32,
                   palette: &Palette,
                   frames: usize| {
        let before = text_preparation_count();
        for _ in 0..frames {
            time += 1.0 / 60.0;
            frame_at(&ctx, inner, backend, time, width, palette);
        }
        text_preparation_count() - before
    };

    assert_eq!(run(&mut inner, &mut backend, VIEW_W, &day, 10), 0);

    // A narrower window wraps the text differently.
    assert!(run(&mut inner, &mut backend, VIEW_W - 260.0, &day, 4) > 0);
    assert_eq!(run(&mut inner, &mut backend, VIEW_W - 260.0, &day, 10), 0);

    // Another theme: the colours are part of the galley.
    assert!(run(&mut inner, &mut backend, VIEW_W - 260.0, &night, 4) > 0);
    assert_eq!(run(&mut inner, &mut backend, VIEW_W - 260.0, &night, 10), 0);

    // Another scale: the font atlas is rebuilt, kept galleys would point into the old one.
    ctx.set_pixels_per_point(1.5);
    assert!(run(&mut inner, &mut backend, VIEW_W - 260.0, &night, 4) > 0);
    assert_eq!(run(&mut inner, &mut backend, VIEW_W - 260.0, &night, 10), 0);
}

/// Every text painted by a frame.
fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => out.push(text.galley.job.text.clone()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in shapes {
        collect(&clipped.shape, &mut out);
    }
    out
}

/// One frame of the whole chat widget; returns the texts it painted.
fn widget_frame(
    ctx: &Context,
    widget: &mut HistoryWidget,
    backend: &mut CountingBackend,
    time: f64,
) -> Vec<String> {
    let palette = Palette::day();
    let mut out = ctx.run_ui(raw_input(time), |ui| {
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let _ = widget.show_with_backend(
                ui,
                PEER,
                backend,
                true,
                false,
                false,
                &palette,
                &EnglishStrings,
            );
        });
    });
    out.textures_delta.clear();
    painted_texts(&out.shapes)
}

struct Reads {
    peer_info: usize,
    pinned: usize,
    self_peer: usize,
}

/// Backend reads made by `frames` idle frames of the whole chat widget.
fn idle_widget_reads(unversioned: bool, frames: usize) -> Reads {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    backend.unversioned = unversioned;
    let mut widget = HistoryWidget::new();
    for frame in 0..5 {
        widget_frame(&ctx, &mut widget, &mut backend, frame as f64 / 60.0);
    }
    let before = (
        backend.peer_info_calls.get(),
        backend.pinned_calls.get(),
        backend.self_peer_calls.get(),
    );
    for frame in 0..frames {
        widget_frame(&ctx, &mut widget, &mut backend, 1.0 + frame as f64 / 60.0);
    }
    Reads {
        peer_info: backend.peer_info_calls.get() - before.0,
        pinned: backend.pinned_calls.get() - before.1,
        self_peer: backend.self_peer_calls.get() - before.2,
    }
}

/// The gate re-reads once per wall-clock second, and 60 headless frames take well under a few
/// seconds: a few reads, not one per frame.
const GATED_READS_ALLOWED: usize = 4;

#[test]
fn backend_reads_wait_for_the_chat_version() {
    const FRAMES: usize = 60;
    // A backend without versions is read on every frame, as before the gate.
    let every_frame = idle_widget_reads(true, FRAMES);
    let gated = idle_widget_reads(false, FRAMES);
    println!(
        "measured: backend reads on {FRAMES} idle frames of the chat widget, without versions / \
         with an unchanged version: peer_info {} / {}, pinned_messages {} / {}, self_peer {} / {}",
        every_frame.peer_info,
        gated.peer_info,
        every_frame.pinned,
        gated.pinned,
        every_frame.self_peer,
        gated.self_peer
    );
    assert_eq!(every_frame.pinned, FRAMES, "one pinned read per frame");
    assert!(gated.pinned <= GATED_READS_ALLOWED, "{}", gated.pinned);
    // The compose bar (outside this package) still reads `peer_info` and `self_peer` on each
    // frame, so the widget totals are compared: the chat view's own read is gone.
    assert!(
        every_frame.peer_info - gated.peer_info >= FRAMES - GATED_READS_ALLOWED,
        "peer_info {} -> {}",
        every_frame.peer_info,
        gated.peer_info
    );
    assert!(
        every_frame.self_peer - gated.self_peer >= FRAMES - GATED_READS_ALLOWED,
        "self_peer {} -> {}",
        every_frame.self_peer,
        gated.self_peer
    );
}

#[test]
fn a_version_change_is_shown_on_the_next_frame() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut widget = HistoryWidget::new();
    let mut texts = Vec::new();
    for frame in 0..10 {
        texts = widget_frame(&ctx, &mut widget, &mut backend, frame as f64 / 60.0);
    }
    assert!(texts.iter().any(|t| t.contains("Test Peer")));

    // A change moves the version: the very next frame reads again and shows the new data.
    backend.version += 1;
    backend.title = "Renamed Peer".to_owned();
    let texts = widget_frame(&ctx, &mut widget, &mut backend, 3.0);
    assert!(
        texts.iter().any(|t| t.contains("Renamed Peer")),
        "the top bar shows the new title on the frame after the change"
    );
    assert!(!texts.iter().any(|t| t.contains("Test Peer")));
}

/// Without a version change the gate still re-reads within a second, so a change the backend
/// did not count is never shown stale for longer.
#[test]
fn an_uncounted_change_is_shown_within_a_second() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut widget = HistoryWidget::new();
    for frame in 0..10 {
        widget_frame(&ctx, &mut widget, &mut backend, frame as f64 / 60.0);
    }
    backend.title = "Renamed Peer".to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2_500);
    let mut shown = false;
    let mut time = 1.0;
    while !shown && std::time::Instant::now() < deadline {
        time += 1.0 / 60.0;
        let texts = widget_frame(&ctx, &mut widget, &mut backend, time);
        shown = texts.iter().any(|t| t.contains("Renamed Peer"));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(shown, "the new title appears without a version change");
}

/// Input makes the chat read again at once: an action the user takes is read back on the same
/// and the next frame, whatever the backend does with its version.
#[test]
fn input_makes_the_chat_read_again() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut widget = HistoryWidget::new();
    for frame in 0..10 {
        widget_frame(&ctx, &mut widget, &mut backend, frame as f64 / 60.0);
    }
    let palette = Palette::day();
    let mut reads = Vec::new();
    for step in 0..4 {
        let before = backend.pinned_calls.get();
        let mut input = raw_input(2.0 + step as f64 / 60.0);
        if step == 1 {
            input
                .events
                .push(egui::Event::PointerMoved(pos2(40.0, 400.0)));
        }
        let mut out = ctx.run_ui(input, |ui| {
            CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
                let _ = widget.show_with_backend(
                    ui,
                    PEER,
                    &mut backend,
                    true,
                    false,
                    false,
                    &palette,
                    &EnglishStrings,
                );
            });
        });
        out.textures_delta.clear();
        reads.push(backend.pinned_calls.get() - before);
    }
    // Frame 1 has the input, frame 2 follows it; frames 0 and 3 may only read when the
    // wall-clock second happens to change.
    assert_eq!(reads[1], 1, "the frame with input reads: {reads:?}");
    assert_eq!(reads[2], 1, "the frame after input reads: {reads:?}");
    assert!(
        reads[0] + reads[3] <= 1,
        "idle frames keep the result: {reads:?}"
    );
}

#[test]
fn opening_a_chat_asks_for_media_near_the_viewport_first() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut inner = HistoryInnerWidget::new();
    inner.load_peer(PEER, MsgId::SHOW_AT_END, &backend, PeerKind::User);
    assert_eq!(
        backend.descriptor_calls.get(),
        0,
        "no media is asked for before the first frame (no voice or music in this chat)"
    );

    list_frame(&ctx, &mut inner, &mut backend, raw_input(0.0), "");
    let first_frame = backend.descriptor_calls.get();
    let painted = inner.last_frame_painted_count();
    println!(
        "measured: media descriptor requests when opening {MESSAGE_COUNT} messages: 0 before \
         the first frame, {first_frame} on it ({painted} bubbles painted)"
    );
    assert!(
        first_frame >= painted,
        "every painted message was asked for"
    );
    assert!(
        first_frame <= 120,
        "the first frame asks for the messages near the viewport, not all: {first_frame}"
    );
    // The newest messages are the visible ones.
    let asked = backend.descriptor_ids.borrow().clone();
    for id in (MESSAGE_COUNT - painted as i32 + 1)..=MESSAGE_COUNT {
        assert!(asked.contains(&MsgId(id)), "visible message {id} was asked");
    }

    // The rest of the slice follows over the next frames, each message exactly once.
    let mut frames = 1;
    while backend.descriptor_calls.get() < MESSAGE_COUNT as usize && frames < 2_000 {
        list_frame(
            &ctx,
            &mut inner,
            &mut backend,
            raw_input(frames as f64 / 60.0),
            "",
        );
        frames += 1;
    }
    for _ in 0..5 {
        list_frame(
            &ctx,
            &mut inner,
            &mut backend,
            raw_input(frames as f64 / 60.0),
            "",
        );
        frames += 1;
    }
    let mut asked = backend.descriptor_ids.borrow().clone();
    assert_eq!(asked.len(), MESSAGE_COUNT as usize, "each message once");
    asked.sort_unstable();
    asked.dedup();
    assert_eq!(asked.len(), MESSAGE_COUNT as usize, "no message twice");
    println!("measured: the whole slice was asked for after {frames} frames");
}

#[test]
fn scrolling_through_the_chat_keeps_a_bounded_number_of_prepared_texts() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut inner = open_chat(&ctx, &mut backend, 8);
    // Scroll up through a few hundred messages, a screen at a time.
    let mut y = inner.scroll_offset_y();
    let mut painted_total = std::collections::HashSet::new();
    for frame in 0..400 {
        y = (y - 600.0).max(0.0);
        inner.set_scroll_offset(y);
        list_frame(
            &ctx,
            &mut inner,
            &mut backend,
            raw_input(1.0 + frame as f64 / 60.0),
            "",
        );
        painted_total.extend(inner.painted_message_ids().iter().copied());
    }
    let kept = inner.prepared_message_count();
    println!(
        "measured: {} messages were painted while scrolling, {kept} keep prepared text",
        painted_total.len()
    );
    assert!(
        painted_total.len() > 300,
        "the scroll went through the chat: {}",
        painted_total.len()
    );
    assert!(kept <= 128 + inner.last_frame_painted_count(), "{kept}");
    // The scroll settles (a message that just came into view is prepared once), then what is
    // in view stays prepared: idle frames build nothing.
    idle_frames(&ctx, &mut inner, &mut backend, 20.0, 5);
    assert_eq!(idle_frames(&ctx, &mut inner, &mut backend, 21.0, 30), 0);
}

#[test]
fn voice_messages_get_their_media_before_the_first_frame() {
    let mut messages: Vec<_> = (1..=200).map(text_message).collect();
    for msg in messages.iter_mut().filter(|m| m.id.0 % 50 == 0) {
        msg.media = Some(HistoryMedia::Voice(VoiceAttachment {
            duration_secs: 7,
            waveform_5bit: Vec::new(),
            size_bytes: 0,
            is_unread: false,
            transcription: None,
        }));
    }
    let backend = CountingBackend::with_messages(messages);
    let mut inner = HistoryInnerWidget::new();
    inner.load_peer(PEER, MsgId::SHOW_AT_END, &backend, PeerKind::User);
    // The audio playlist is built when the slice is applied and needs the voice bytes then.
    let mut asked = backend.descriptor_ids.borrow().clone();
    asked.sort_unstable();
    assert_eq!(asked, vec![MsgId(50), MsgId(100), MsgId(150), MsgId(200)]);
}

#[test]
fn a_photo_set_loading_from_outside_is_still_polled() {
    let ctx = new_context();
    let mut backend = CountingBackend::text_chat();
    let mut inner = open_chat(&ctx, &mut backend, 8);
    // Let the opening backlog finish so later requests are the poll's own.
    let mut time = 1.0;
    while backend.descriptor_calls.get() < MESSAGE_COUNT as usize {
        time += 1.0 / 60.0;
        list_frame(&ctx, &mut inner, &mut backend, raw_input(time), "");
    }
    let settled = backend.descriptor_calls.get();
    for _ in 0..40 {
        time += 1.0 / 60.0;
        list_frame(&ctx, &mut inner, &mut backend, raw_input(time), "");
    }
    assert_eq!(
        backend.descriptor_calls.get(),
        settled,
        "nothing is loading, so nothing is polled"
    );

    // Something other than the list (a test, the window) marks a photo as downloading.
    inner.media_runtime_mut().register_descriptor(
        MsgId(MESSAGE_COUNT),
        InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Loading {
                progress: None,
                bytes_loaded: 0,
                total_bytes: 1_000,
            }),
            ..InlineMediaDescriptor::default()
        },
    );
    for _ in 0..40 {
        time += 1.0 / 60.0;
        list_frame(&ctx, &mut inner, &mut backend, raw_input(time), "");
    }
    assert!(
        backend.descriptor_calls.get() > settled,
        "the downloading photo is polled again"
    );
}
