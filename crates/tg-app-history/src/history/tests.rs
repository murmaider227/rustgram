//! Unit tests for non-visual history view logic: `HistoryBackend`, compose timers/shortcuts,
//! entity bridging, virtualization across 5,000+ messages, scroll anchoring, selection,
//! waveforms, stripped thumbnails, and Lottie sticker parsing/rendering.

use tg_history::consts::ACTIVE_FADE_IN_DURATION_MS;
use tg_history::{FlatSelectionEndpoint, ListSelectionEndpoint};
use tg_lottie::{Composition, Size as LottieSize, render_composition};
use tg_media::audio::waveform_decode_5bit;
use tg_media::image::inline::expand_inline_bytes;
use tg_text::{EntityType, TextWithEntities};
use tg_ui::text::TextEntityKind;

use crate::history::backend::{
    DraftMessage, HistoryBackend, HistoryMedia, SendMessageRequest, to_hist_full,
};
use crate::history::compose::{
    DraftSaveDue, DraftSaveScheduler, SAVE_CLOUD_DRAFT_IDLE_TIMEOUT_MS,
    SAVE_DRAFT_ANYWAY_TIMEOUT_MS, SAVE_DRAFT_TIMEOUT_MS, SEND_MY_TYPING_INTERVAL_MS, SendShortcut,
    TypingThrottle,
};
use crate::history::inner::{HistoryInnerWidget, format_date_separator};
use crate::history::mock::{
    MockHistoryStore, reset_mock_backend_history, sample_lottie_star_json,
    sample_voice_waveform_5bit, synthetic_stripped_thumb,
};
use crate::history::text_bridge::{
    entities_to_rich_text, parse_compose_markdown, slice_by_char_range,
};
use tg_app_core::stand_in::types::{FullMsgId, MsgId, PeerId, PeerKind};

#[test]
fn mock_history_store_crud_reactions_pins_and_polls() {
    let mut store = MockHistoryStore::new();
    let group_id = PeerId::from_chat(2001);
    let dm_id = PeerId::from_user(1002);

    // Rich slice contains albums, photos, videos, documents, voice, stickers, polls, service msgs.
    let slice = store.load_slice_around(group_id, MsgId::SHOW_AT_END, 50, 10);
    assert!(slice.loaded_at_top && slice.loaded_at_bottom);
    assert!(slice.messages.iter().any(|m| m.is_service()));
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m.media, Some(HistoryMedia::Photo(_))))
    );
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m.media, Some(HistoryMedia::Album(_))))
    );
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m.media, Some(HistoryMedia::Video(_))))
    );
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m.media, Some(HistoryMedia::Document(_))))
    );
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m.media, Some(HistoryMedia::Voice(_))))
    );
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m.media, Some(HistoryMedia::Sticker(_))))
    );
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m.media, Some(HistoryMedia::Poll(_))))
    );
    assert!(slice.messages.iter().any(|m| m.reply.is_some()));
    assert!(slice.messages.iter().any(|m| m.forward.is_some()));

    // Send -> Edit -> React -> Pin -> Unpin -> Forward -> Delete
    let sent = store
        .send_message(
            group_id,
            SendMessageRequest {
                text: parse_compose_markdown("Hello **bold** and `code`"),
                reply_to: Some(MsgId(102)),
                silent: false,
            },
        )
        .expect("send succeeds");
    assert!(sent.outgoing);
    assert_eq!(
        sent.reply.as_ref().map(|r| r.reply_to_msg_id),
        Some(MsgId(102))
    );

    let edited = store
        .edit_message(group_id, sent.id, TextWithEntities::simple("Edited body"))
        .expect("edit own message succeeds");
    assert!(edited.edited);
    assert_eq!(edited.text.text, "Edited body");

    let reactions = store
        .toggle_reaction(group_id, sent.id, "🔥")
        .expect("toggle reaction on");
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].count, 1);
    assert!(reactions[0].chosen);
    let reactions_off = store
        .toggle_reaction(group_id, sent.id, "🔥")
        .expect("toggle reaction off");
    assert!(reactions_off.is_empty());

    let pins = store.pin_message(group_id, sent.id, false).expect("pin");
    assert_eq!(pins[0].msg_id, sent.id);
    let pins_after = store.pin_message(group_id, sent.id, true).expect("unpin");
    assert!(!pins_after.iter().any(|p| p.msg_id == sent.id));

    // Poll vote switches choice cleanly.
    store
        .vote_poll(group_id, MsgId(110), &[1])
        .expect("vote poll");
    let poll_slice = store.load_slice_around(group_id, MsgId(110), 0, 0);
    let Some(HistoryMedia::Poll(ref poll)) = poll_slice.messages[0].media else {
        panic!("expected poll");
    };
    assert!(!poll.options[0].chosen);
    assert!(poll.options[1].chosen);

    // Forward into DM peer.
    let fwds = store
        .forward_messages(group_id, &[sent.id], dm_id)
        .expect("forward succeeds");
    assert_eq!(fwds.len(), 1);
    assert!(fwds[0].forward.is_some());

    // Delete removes message.
    store
        .delete_messages(group_id, &[sent.id], true)
        .expect("delete succeeds");
    let after_del = store.load_slice_around(group_id, MsgId::SHOW_AT_END, 50, 10);
    assert!(!after_del.messages.iter().any(|m| m.id == sent.id));
}

#[test]
fn mock_backend_implements_history_backend_trait() {
    reset_mock_backend_history();
    let mut mock = crate::history::mock::MockHistoryBackendAdapter::new(true);
    let group_id = PeerId::from_chat(2001);

    let slice = HistoryBackend::load_slice_around(&mock, group_id, MsgId::SHOW_AT_END, 20, 5);
    assert!(!slice.messages.is_empty());

    let sent = HistoryBackend::send_message(
        &mut mock,
        group_id,
        SendMessageRequest {
            text: TextWithEntities::simple("Sent through HistoryBackend for MockBackend"),
            reply_to: None,
            silent: false,
        },
    )
    .expect("send via MockBackend");
    assert!(sent.outgoing);

    HistoryBackend::save_draft(
        &mut mock,
        group_id,
        Some(DraftMessage {
            text: "work in progress".to_owned(),
            reply_to: Some(sent.id),
        }),
    );
    let draft = HistoryBackend::load_draft(&mock, group_id).expect("draft persisted");
    assert_eq!(draft.text, "work in progress");
    assert_eq!(draft.reply_to, Some(sent.id));

    HistoryBackend::mark_history_read(&mut mock, group_id, sent.id);
    let info = HistoryBackend::peer_info(&mock, group_id).expect("peer info");
    assert_eq!(info.unread_count, 0);
    // Inbox read must never flip our own outgoing message's read checkmark to true.
    let slice = HistoryBackend::load_slice_around(&mock, group_id, MsgId::SHOW_AT_END, 50, 0);
    let sent_after = slice
        .messages
        .iter()
        .find(|m| m.id == sent.id)
        .expect("sent outgoing message present");
    assert!(
        !sent_after.is_read,
        "mark_history_read (inbox read) must not mark outgoing messages read"
    );
}

#[test]
fn send_shortcut_rules_enter_and_ctrl_enter() {
    assert!(SendShortcut::Enter.should_send(true, false, false));
    assert!(!SendShortcut::Enter.should_send(true, true, false));
    assert!(!SendShortcut::Enter.should_send(true, false, true));
    assert!(!SendShortcut::Enter.should_send(false, false, false));

    assert!(!SendShortcut::CtrlEnter.should_send(true, false, false));
    assert!(SendShortcut::CtrlEnter.should_send(true, false, true));
    assert!(!SendShortcut::CtrlEnter.should_send(true, true, true));
}

#[test]
fn draft_save_scheduler_matches_tdesktop_delays() {
    let mut sched = DraftSaveScheduler::default();

    // 1. Debounce: saves 1000ms after editing stops.
    sched.mark_edited(100);
    assert_eq!(sched.poll(500), DraftSaveDue::None);
    assert_eq!(
        sched.poll(100 + SAVE_DRAFT_TIMEOUT_MS - 1),
        DraftSaveDue::None
    );
    assert_eq!(sched.poll(100 + SAVE_DRAFT_TIMEOUT_MS), DraftSaveDue::Local);
    assert_eq!(sched.poll(2000), DraftSaveDue::None);

    // 2. Cloud sync triggers 14000ms after last edit.
    assert_eq!(
        sched.poll(100 + SAVE_CLOUD_DRAFT_IDLE_TIMEOUT_MS),
        DraftSaveDue::Cloud
    );
    assert_eq!(
        sched.poll(100 + SAVE_CLOUD_DRAFT_IDLE_TIMEOUT_MS + 5000),
        DraftSaveDue::None
    );

    // 3. Continuous typing: forced save after 5000ms (`kSaveDraftAnywayTimeout`).
    sched.reset();
    sched.mark_edited(10_000);
    for t in (10_500..15_000).step_by(500) {
        sched.mark_edited(t);
        assert_eq!(sched.poll(t + 100), DraftSaveDue::None);
    }
    sched.mark_edited(14_900);
    assert_eq!(
        sched.poll(10_000 + SAVE_DRAFT_ANYWAY_TIMEOUT_MS),
        DraftSaveDue::Local
    );
}

#[test]
fn typing_throttle_respects_5s_interval_and_peer_switch() {
    let mut throttle = TypingThrottle::default();
    let p1 = PeerId::from_user(1001);
    let p2 = PeerId::from_user(1002);

    assert!(throttle.should_send(p1, 1_000));
    assert!(!throttle.should_send(p1, 3_500));
    assert!(!throttle.should_send(p1, 1_000 + SEND_MY_TYPING_INTERVAL_MS - 1));
    assert!(throttle.should_send(p1, 1_000 + SEND_MY_TYPING_INTERVAL_MS));

    // Switching peer resets immediately.
    assert!(throttle.should_send(p2, 6_100));
}

#[test]
fn markdown_and_utf16_entities_bridge_cleanly_with_emoji() {
    // Leading surrogate-pair emoji tests UTF-16 -> UTF-8 offset conversion!
    let parsed = parse_compose_markdown(
        "🚀 **Launch** __now__ `cargo test` ~~old~~ ||secret|| https://example.org @rustgram_core",
    );
    assert!(
        parsed
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Bold)
    );
    assert!(
        parsed
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Italic)
    );
    assert!(
        parsed
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Code)
    );
    assert!(
        parsed
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::StrikeOut)
    );
    assert!(
        parsed
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Spoiler)
    );

    let rich = entities_to_rich_text(&parsed);
    let bold_span = rich
        .spans
        .iter()
        .find(|s| s.kind == TextEntityKind::Bold)
        .expect("bold span converted");
    assert_eq!(&rich.text[bold_span.start..bold_span.end], "Launch");

    let code_span = rich
        .spans
        .iter()
        .find(|s| s.kind == TextEntityKind::Monospace)
        .expect("code span converted");
    assert_eq!(&rich.text[code_span.start..code_span.end], "cargo test");

    let spoiler_span = rich
        .spans
        .iter()
        .find(|s| s.kind == TextEntityKind::Spoiler)
        .expect("spoiler span converted");
    assert_eq!(&rich.text[spoiler_span.start..spoiler_span.end], "secret");
}

#[test]
fn virtualized_layout_handles_5000_messages_by_laying_out_only_visible_window() {
    let mut store = MockHistoryStore::new();
    let stress_peer = PeerId::from_chat(9999);
    store.populate_many_messages(stress_peer, 5_000);

    let mut inner = HistoryInnerWidget::new();
    let slice = store.load_slice_around(stress_peer, MsgId::SHOW_AT_END, 5_000, 0);
    assert_eq!(slice.messages.len(), 5_000);
    inner.apply_slice(stress_peer, slice, PeerKind::Group, true);

    // Viewport of 700px height at the middle of 5,000 messages should lay out only ~25-40 items,
    // NEVER all 5,000!
    let mid_y = inner.list().total_height() / 2;
    let laid_out =
        inner.layout_visible_range_headless(640, mid_y, mid_y + 700, false, PeerKind::Group);
    assert!(
        laid_out > 5 && laid_out < 80,
        "expected visible slice only, got {laid_out}"
    );

    // Second pass settles at most a few boundary items whose Y shifted after exact heights;
    // steady-state pass hits cache with 0 newly laid out items.
    let second_pass =
        inner.layout_visible_range_headless(640, mid_y, mid_y + 700, false, PeerKind::Group);
    assert!(
        second_pass < 10,
        "expected minimal boundary settle, got {second_pass}"
    );
    let steady_pass =
        inner.layout_visible_range_headless(640, mid_y, mid_y + 700, false, PeerKind::Group);
    assert_eq!(steady_pass, 0);
}

#[test]
fn pagination_scroll_anchor_jump_highlight_and_selection_extraction() {
    let mut store = MockHistoryStore::new();
    let peer = PeerId::from_chat(8888);
    store.populate_many_messages(peer, 200);

    let mut inner = HistoryInnerWidget::new();
    // Load only the newest 40 messages -> loaded_at_top is false.
    let newest = store.load_slice_around(peer, MsgId::SHOW_AT_END, 39, 0);
    assert_eq!(newest.messages.len(), 40);
    assert!(!newest.loaded_at_top);
    inner.apply_slice(peer, newest, PeerKind::Group, true);

    let first_id = inner.messages()[0].id;
    let older = store.load_slice_before(peer, first_id, 40);
    assert_eq!(older.messages.len(), 40);
    inner.prepend_older_slice(older, PeerKind::Group);
    assert_eq!(inner.messages().len(), 80);

    // Jump to an unloaded message (#5) loads a slice around it and starts highlighter.
    let target = MsgId(10_005);
    inner.jump_to_message(target, &store, PeerKind::Group, 1_000);
    assert!(inner.messages().iter().any(|m| m.id == target));
    let full_target = to_hist_full(FullMsgId { peer, msg: target });
    let hl = inner
        .highlighter()
        .state_for(full_target, 1_000 + ACTIVE_FADE_IN_DURATION_MS);
    assert!((hl.opacity - 1.0).abs() < 1e-6);

    // Multi-select & partial drag-select text extraction.
    inner.toggle_select_message(target);
    assert_eq!(inner.selected_count(), 1);
    let copied = inner.extract_selected_text();
    assert!(copied.contains("Message #6"));

    inner.clear_selection();
    assert_eq!(slice_by_char_range("Hello 🌍 world", 6, 13), "🌍 world");
    let idx = inner
        .messages()
        .iter()
        .position(|m| m.id == target)
        .expect("target index");
    let _ep = ListSelectionEndpoint {
        item_id: full_target,
        item_index: idx,
        text: FlatSelectionEndpoint::new(0, false),
    };
    assert_eq!(format_date_separator(1_795_255_200), "November 21");
}

#[test]
fn stripped_thumbs_waveforms_and_lottie_sticker_decode_cleanly() {
    let thumb = synthetic_stripped_thumb(0x3C);
    let expanded = expand_inline_bytes(&thumb).expect("valid 0x01 stripped header");
    assert!(expanded.starts_with(&[0xFF, 0xD8]));
    assert!(expanded.ends_with(&[0xFF, 0xD9]));

    let wave_5bit = sample_voice_waveform_5bit(7);
    let bars = waveform_decode_5bit(&wave_5bit);
    assert_eq!(bars.len(), 100);
    assert!(bars.iter().all(|&b| b <= 31));

    let json = sample_lottie_star_json();
    let comp = Composition::from_json(&json).expect("valid procedural Lottie JSON");
    let fb = render_composition(&comp, 15.0, LottieSize::new(128, 128));
    assert_eq!(fb.width, 128);
    assert_eq!(fb.height, 128);
    // Verify non-empty rasterized pixels (transparent margins + opaque star/badge center).
    let opaque_count = fb
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[3] > 200)
        .count();
    assert!(
        opaque_count > 1_000,
        "expected rasterized sticker pixels, got {opaque_count}"
    );
}

#[test]
fn history_game_media_layout_and_preview_snippet() {
    use crate::history::backend::{GameAttachment, HistoryMessage, HistorySlice};

    let msg = HistoryMessage {
        extra: Default::default(),
        id: MsgId(777),
        peer_id: PeerId::from_user(777_001),
        from_id: PeerId::from_user(777_001),
        sender_name: "Catalog Bot".to_owned(),
        date_unix: 1_795_340_000,
        outgoing: false,
        is_post: false,
        text: tg_text::TextWithEntities::default(),
        media: Some(HistoryMedia::Game(GameAttachment {
            game: tg_bots::mini_app::GameData {
                id: tg_bots::stand_in::GameId(50_001),
                access_hash: 0x5A17_CAFE,
                short_name: "star_hopper".to_owned(),
                title: "Star Hopper".to_owned(),
                description: "Guide an orbital glider through asteroid rings.".to_owned(),
                photo_id: Some(tg_bots::stand_in::PhotoId(50_100)),
                document_id: None,
            },
            bot_username: "catalog_demo_bot".to_owned(),
            media_width: 640,
            media_height: 360,
            is_animation: false,
            stripped_thumb: None,
            fallback_rgb: ([0x1D, 0x2B, 0x64], [0x53, 0x35, 0x7A]),
            high_scores: Vec::new(),
        })),
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
    };

    assert_eq!(msg.preview_snippet(), "🎮 Star Hopper");

    let mut inner = HistoryInnerWidget::new();
    let slice = HistorySlice {
        messages: vec![msg],
        loaded_at_top: true,
        loaded_at_bottom: true,
        unread_count: 0,
        first_unread_id: None,
        total_count: 1,
    };
    inner.apply_slice(
        PeerId::from_user(777_001),
        slice,
        tg_app_core::stand_in::types::PeerKind::User,
        true,
    );
    let laid = inner.layout_visible_range_headless(
        640,
        0,
        600,
        false,
        tg_app_core::stand_in::types::PeerKind::User,
    );
    assert_eq!(laid, 1);
    assert!(
        inner.list().total_height() >= 200,
        "game card layout must reserve space for title, description, and preview media, got {}",
        inner.list().total_height()
    );
}
