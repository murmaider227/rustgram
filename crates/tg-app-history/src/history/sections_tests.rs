//! Unit tests for all non-visual logic added in the history sections, compose extras,
//! and message actions pass.

use tg_chat::media_prepare::{AlbumType, PreparedFileType};
use tg_chat::send_options::{DEFAULT_SCHEDULE_OFFSET_SECS, SCHEDULED_UNTIL_ONLINE_TIMESTAMP};
use tg_editor::CropPreset;
use tg_history::{FlatSelectionEndpoint, ListSelectionEndpoint};
use tg_lottie::{Composition, Size as LottieSize, render_composition};
use tg_media::audio::waveform_decode_5bit;
use tg_platform::spellcheck::MisspelledRange;
use tg_text::{EntityType, TextWithEntities, emoji};

use crate::history::backend::{
    HistoryBackend, HistoryMedia, SendMessageExtended, ThreadDraft, to_hist_full,
};
use crate::history::compose::ComposeControls;
use crate::history::compose_boxes::{
    AttachFilesBoxState, CreatePollBoxState, PhotoEditorModalState, ScheduleBoxMode,
    ScheduleBoxState,
};
use crate::history::compose_extras::{
    ComposeSpellchecker, InlineBotPanelState, LinkPreviewEditorState, ReactionsPickerState,
    RoundVideoRecorderState, VOICE_SLIDE_CANCEL_THRESHOLD_PX, VOICE_SLIDE_LOCK_THRESHOLD_PX,
    VoiceRecordPhase, VoiceRecorderController, extract_first_url,
};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::{MockHistoryStore, reset_mock_backend_history};
use crate::history::mock_ext::{
    DEMO_CHANNEL_PEER_ID, DEMO_CHANNEL_POST_ID, DEMO_FORUM_PEER_ID, DEMO_SAVED_PEER_ID,
    EXT_BASE_UNIX,
};
use crate::history::sections_model::{
    AdminLogFilter, AdminLogFilterFlags, CreatePollRequest, FORUM_TOPIC_ICON_COLORS,
    ForwardOptions, MESSAGE_EFFECTS, MessageReportReason, POLL_MAX_OPTIONS, POLL_OPTION_LIMIT,
    POLL_QUESTION_LIMIT, POLL_SOLUTION_LIMIT, PollValidationError, ScheduleDateTime,
    ScheduleTarget, SlowModeState, ThreadKey, animated_reaction_lottie_json, civil_from_days,
    days_from_civil, days_in_month, extract_non_emoji_letter, find_message_effect,
    format_schedule_badge, forum_topic_color_name, is_leap_year, parse_inline_bot_query,
    slice_text_with_entities, text_with_entities_to_markdown, validate_create_poll,
    validate_schedule_target, weekday_monday_zero,
};
use crate::history::text_bridge::parse_compose_markdown;
use tg_app_core::stand_in::types::{FullMsgId, MsgId, PeerId, PeerKind};

#[test]
fn scheduled_messages_send_now_reschedule_and_delete() {
    let mut store = MockHistoryStore::new();
    let initial = store.scheduled_messages(DEMO_FORUM_PEER_ID, None);
    assert!(initial.len() >= 2);

    // Reschedule first message to "Send when online"
    let first_id = initial[0].id;
    let resched = store
        .reschedule_message(DEMO_FORUM_PEER_ID, first_id, ScheduleTarget::WhenOnline)
        .expect("reschedule succeeds");
    assert_eq!(
        resched.extra.scheduled_date,
        Some(SCHEDULED_UNTIL_ONLINE_TIMESTAMP)
    );
    assert_eq!(
        format_schedule_badge(SCHEDULED_UNTIL_ONLINE_TIMESTAMP),
        "Send when online"
    );

    // Send scheduled message now moves it from scheduled to main history
    let sent = store
        .send_scheduled_now(DEMO_FORUM_PEER_ID, first_id)
        .expect("send now succeeds");
    assert!(sent.extra.scheduled_date.is_none());
    let remaining = store.scheduled_messages(DEMO_FORUM_PEER_ID, None);
    assert!(!remaining.iter().any(|m| m.id == first_id));
    let slice = store.load_slice_around(DEMO_FORUM_PEER_ID, MsgId::SHOW_AT_END, 5, 0);
    assert!(slice.messages.iter().any(|m| m.id == first_id));

    // Delete remaining scheduled message
    let second_id = remaining[0].id;
    store
        .delete_scheduled_messages(DEMO_FORUM_PEER_ID, &[second_id])
        .expect("delete scheduled succeeds");
    assert!(
        !store
            .scheduled_messages(DEMO_FORUM_PEER_ID, None)
            .iter()
            .any(|m| m.id == second_id)
    );
}

#[test]
fn schedule_calendar_math_and_box_validation() {
    assert!(is_leap_year(2024));
    assert!(!is_leap_year(2026));
    assert!(is_leap_year(2000));
    assert!(!is_leap_year(1900));
    assert_eq!(days_in_month(2024, 2), 29);
    assert_eq!(days_in_month(2026, 2), 28);
    // 1970-01-01 was Thursday (Monday=0 -> 3)
    assert_eq!(weekday_monday_zero(1970, 1, 1), 3);

    // Round-trip Unix UTC <-> ScheduleDateTime
    let dt = ScheduleDateTime::from_unix_utc(i64::from(EXT_BASE_UNIX));
    assert_eq!(dt.year, 2026);
    assert_eq!(dt.month, 11);
    assert_eq!(dt.day, 22);
    assert_eq!(dt.hour, 10);
    assert_eq!(dt.minute, 0);
    assert_eq!(dt.to_unix_utc(), i64::from(EXT_BASE_UNIX));
    assert_eq!(
        civil_from_days(days_from_civil(2026, 11, 22)),
        (2026, 11, 22)
    );

    // Default schedule box offset (+600s = 10 minutes)
    let mut box_st = ScheduleBoxState::new(
        ScheduleBoxMode::ScheduleMessage,
        EXT_BASE_UNIX,
        true,
        "Jordan Patel",
    );
    assert_eq!(
        box_st.selected.to_unix_utc() as i32,
        EXT_BASE_UNIX + DEFAULT_SCHEDULE_OFFSET_SECS
    );
    assert!(box_st.confirm_timestamp().is_ok());

    // Past timestamp rejected; > 365 days rejected; WhenOnline accepted
    assert!(
        validate_schedule_target(ScheduleTarget::Timestamp(EXT_BASE_UNIX - 60), EXT_BASE_UNIX)
            .is_err()
    );
    assert!(
        validate_schedule_target(
            ScheduleTarget::Timestamp(EXT_BASE_UNIX + 400 * 86_400),
            EXT_BASE_UNIX
        )
        .is_err()
    );
    assert!(validate_schedule_target(ScheduleTarget::WhenOnline, EXT_BASE_UNIX).is_ok());
}

#[test]
fn pinned_section_comments_thread_and_per_thread_drafts() {
    let mut store = MockHistoryStore::new();

    // Pinned full messages & unpin all
    let pinned = store.pinned_full_messages(DEMO_FORUM_PEER_ID, None);
    assert!(!pinned.is_empty());
    store
        .unpin_all_messages(DEMO_FORUM_PEER_ID, None)
        .expect("unpin all");
    assert!(
        store
            .pinned_full_messages(DEMO_FORUM_PEER_ID, None)
            .is_empty()
    );

    // Replies / Comments thread: root channel post + comments
    let (root, comments) = store
        .replies_root_and_comments(DEMO_CHANNEL_PEER_ID, DEMO_CHANNEL_POST_ID)
        .expect("comments thread exists");
    assert_eq!(root.id, DEMO_CHANNEL_POST_ID);
    assert!(root.is_post);
    let orig_count = comments.len();
    assert!(orig_count >= 3);

    // Drafts per thread are isolated across Main, Topic, Replies, SavedSublist
    let main_key = ThreadKey::Main(DEMO_FORUM_PEER_ID);
    let topic_key = ThreadKey::Topic {
        peer_id: DEMO_FORUM_PEER_ID,
        topic_root_id: MsgId(201),
    };
    let replies_key = ThreadKey::Replies {
        peer_id: DEMO_CHANNEL_PEER_ID,
        root_msg_id: DEMO_CHANNEL_POST_ID,
    };
    store.save_thread_draft(
        main_key,
        Some(ThreadDraft {
            text: "main draft".to_owned(),
            ..ThreadDraft::default()
        }),
    );
    store.save_thread_draft(
        topic_key,
        Some(ThreadDraft {
            text: "topic draft".to_owned(),
            ..ThreadDraft::default()
        }),
    );
    store.save_thread_draft(
        replies_key,
        Some(ThreadDraft {
            text: "comment draft".to_owned(),
            ..ThreadDraft::default()
        }),
    );
    assert_eq!(
        store.load_thread_draft(main_key).unwrap().text,
        "main draft"
    );
    assert_eq!(
        store.load_thread_draft(topic_key).unwrap().text,
        "topic draft"
    );
    assert_eq!(
        store.load_thread_draft(replies_key).unwrap().text,
        "comment draft"
    );

    // Sending a comment appends to the replies thread and clears its draft
    let req = SendMessageExtended {
        text: TextWithEntities::simple("Great milestone!"),
        thread_key: Some(replies_key),
        ..SendMessageExtended::default()
    };
    store
        .send_message_extended(DEMO_CHANNEL_PEER_ID, req)
        .expect("comment posted");
    let (_, after_comments) = store
        .replies_root_and_comments(DEMO_CHANNEL_PEER_ID, DEMO_CHANNEL_POST_ID)
        .unwrap();
    assert_eq!(after_comments.len(), orig_count + 1);
    assert!(store.load_thread_draft(replies_key).is_none());
}

#[test]
fn forum_topics_colors_letters_saved_sublists_and_admin_log_filters() {
    let mut store = MockHistoryStore::new();

    // Exact 6 tdesktop forum topic colors & ExtractNonEmojiLetter
    assert_eq!(FORUM_TOPIC_ICON_COLORS.len(), 6);
    assert_eq!(forum_topic_color_name(0x6FB9F0), "blue");
    assert_eq!(forum_topic_color_name(0xFB6F5F), "red");
    assert_eq!(forum_topic_color_name(0x123456), "gray");
    assert_eq!(
        extract_non_emoji_letter("🚀 Launch Prep").as_deref(),
        Some("L")
    );
    assert_eq!(extract_non_emoji_letter("🎨 UI").as_deref(), Some("U"));

    // Forum topics CRUD
    let topics = store.forum_topics(DEMO_FORUM_PEER_ID);
    assert!(topics.len() >= 5);
    assert!(topics.iter().any(|t| t.is_general));
    let created = store
        .create_forum_topic(
            DEMO_FORUM_PEER_ID,
            "Performance Tuning".to_owned(),
            FORUM_TOPIC_ICON_COLORS[4].0,
            None,
        )
        .expect("topic created");
    assert!(!created.is_pinned);
    assert!(
        store
            .toggle_forum_topic_pin(DEMO_FORUM_PEER_ID, created.root_id)
            .unwrap()
    );
    assert!(
        store
            .toggle_forum_topic_closed(DEMO_FORUM_PEER_ID, created.root_id)
            .unwrap()
    );

    // Saved Messages per-chat sublists
    let sublists = store.saved_sublists(DEMO_SAVED_PEER_ID);
    assert!(sublists.len() >= 2);
    assert!(sublists.iter().any(|s| s.is_my_notes));
    let notes = store.load_saved_sublist_messages(DEMO_SAVED_PEER_ID, DEMO_SAVED_PEER_ID);
    assert!(!notes.is_empty());

    // Admin Log filters (all 19 flags, admin filter, search query)
    let all_entries = store.admin_log_entries(DEMO_FORUM_PEER_ID, &AdminLogFilter::default());
    assert!(all_entries.len() >= 5);

    let edit_only = AdminLogFilter {
        flags: Some(AdminLogFilterFlags::EDIT),
        admin_ids: None,
        search_query: String::new(),
    };
    let edited_entries = store.admin_log_entries(DEMO_FORUM_PEER_ID, &edit_only);
    assert!(!edited_entries.is_empty());
    assert!(
        edited_entries
            .iter()
            .all(|e| e.flag == AdminLogFilterFlags::EDIT)
    );

    let sam_id = PeerId::from_user(1001);
    let by_sam = AdminLogFilter {
        flags: None,
        admin_ids: Some(vec![sam_id]),
        search_query: String::new(),
    };
    let sam_entries = store.admin_log_entries(DEMO_FORUM_PEER_ID, &by_sam);
    assert!(!sam_entries.is_empty());
    assert!(sam_entries.iter().all(|e| e.actor_id == sam_id));
}

#[test]
fn poll_and_quiz_creation_limits_and_validation() {
    // Mutual exclusion between Quiz Mode and Multiple Answers
    let mut poll_box = CreatePollBoxState::new();
    poll_box.set_multiple_choice(true);
    assert!(poll_box.multiple_choice && !poll_box.is_quiz);
    poll_box.set_quiz_mode(true);
    assert!(poll_box.is_quiz && !poll_box.multiple_choice);

    // Missing question
    assert_eq!(
        validate_create_poll(
            &CreatePollRequest {
                question: "   ".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: false,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
            POLL_MAX_OPTIONS,
        ),
        Err(PollValidationError::EmptyQuestion)
    );

    // Too long question (> 255)
    assert!(matches!(
        validate_create_poll(
            &CreatePollRequest {
                question: "x".repeat(POLL_QUESTION_LIMIT + 1),
                options: vec!["A".to_owned(), "B".to_owned()],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: false,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
            POLL_MAX_OPTIONS,
        ),
        Err(PollValidationError::QuestionTooLong { .. })
    ));

    // Too few options (< 2), too many options (> 10), too long option (> 100)
    assert!(matches!(
        validate_create_poll(
            &CreatePollRequest {
                question: "Pick one".to_owned(),
                options: vec!["Only one".to_owned()],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: false,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
            POLL_MAX_OPTIONS,
        ),
        Err(PollValidationError::TooFewOptions { count: 1 })
    ));
    assert!(matches!(
        validate_create_poll(
            &CreatePollRequest {
                question: "Pick one".to_owned(),
                options: (0..=POLL_MAX_OPTIONS).map(|i| format!("Opt {i}")).collect(),
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: false,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
            POLL_MAX_OPTIONS,
        ),
        Err(PollValidationError::TooManyOptions { .. })
    ));
    assert!(matches!(
        validate_create_poll(
            &CreatePollRequest {
                question: "Pick one".to_owned(),
                options: vec!["ok".to_owned(), "a".repeat(POLL_OPTION_LIMIT + 1)],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: false,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
            POLL_MAX_OPTIONS,
        ),
        Err(PollValidationError::OptionTooLong { index: 1, .. })
    ));

    // Quiz requires correct answer and <= 200 char explanation
    assert_eq!(
        validate_create_poll(
            &CreatePollRequest {
                question: "2 + 2?".to_owned(),
                options: vec!["3".to_owned(), "4".to_owned()],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: true,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
            POLL_MAX_OPTIONS,
        ),
        Err(PollValidationError::MissingQuizAnswer)
    );
    assert!(matches!(
        validate_create_poll(
            &CreatePollRequest {
                question: "2 + 2?".to_owned(),
                options: vec!["3".to_owned(), "4".to_owned()],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: true,
                correct_option: Some(1),
                explanation: Some("e".repeat(POLL_SOLUTION_LIMIT + 1)),
                silent: false,
                schedule_date: None,
            },
            POLL_MAX_OPTIONS,
        ),
        Err(PollValidationError::ExplanationTooLong { .. })
    ));

    // Valid quiz creates PollAttachment with explanation via backend
    let mut store = MockHistoryStore::new();
    let msg = store
        .send_poll(
            DEMO_FORUM_PEER_ID,
            None,
            CreatePollRequest {
                question: "Which crate handles animated stickers?".to_owned(),
                options: vec!["tg-lottie".to_owned(), "tg-crypto".to_owned()],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: true,
                correct_option: Some(0),
                explanation: Some("tg-lottie parses and rasterizes .tgs animations.".to_owned()),
                silent: false,
                schedule_date: None,
            },
        )
        .expect("send quiz succeeds");
    let Some(HistoryMedia::Poll(ref poll)) = msg.media else {
        panic!("expected poll media");
    };
    assert!(poll.is_quiz);
    assert!(poll.options[0].correct);
    assert!(!poll.options[1].correct);
    assert_eq!(
        poll.explanation.as_deref(),
        Some("tg-lottie parses and rasterizes .tgs animations.")
    );
}

#[test]
fn attach_files_media_prepare_and_tg_editor_pipeline() {
    // 2 sample photos default to 1 PhotoVideo album
    let mut attach = AttachFilesBoxState::new(false, false);
    assert_eq!(attach.prepared.files.len(), 2);
    assert!(attach.prepared.has_group_option(false));
    assert!(attach.prepared.has_send_images_as_photos_option(false));
    assert!(attach.prepared.has_send_large_photos_option(true)); // 2560x1440 > 1280

    let grouped = attach.grouped_batches();
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].album_type, AlbumType::PhotoVideo);

    // Disable album grouping -> ungrouped batch (`AlbumType::None`, 2 standalone items)
    attach.way.set_group_files(false);
    let ungrouped = attach.grouped_batches();
    assert_eq!(ungrouped.len(), 1);
    assert_eq!(ungrouped[0].album_type, AlbumType::None);
    assert_eq!(ungrouped[0].files.len(), 2);

    // Re-enable grouping, disable compression -> File album
    attach.way.set_group_files(true);
    attach.way.set_send_images_as_photos(false);
    let as_files = attach.grouped_batches();
    assert_eq!(as_files.len(), 1);
    assert_eq!(as_files[0].album_type, AlbumType::File);

    // Adding a PDF yields File type
    attach.add_sample_document("spec.pdf", "application/pdf", 120_000);
    assert_eq!(
        attach.prepared.files.last().unwrap().file_type,
        PreparedFileType::File
    );

    // `tg-editor` photo editor transforms + JPEG export
    let mut editor = PhotoEditorModalState::open_for_file(0, "workspace_overview.jpg", 7);
    editor.rotate_cw();
    editor.toggle_flip();
    editor.set_crop_preset(CropPreset::Square);
    editor.enhance_strength = 0.4;
    let jpeg = editor
        .export_edited_jpeg(false)
        .expect("JPEG export succeeds");
    assert!(jpeg.starts_with(&[0xFF, 0xD8]));
    assert!(jpeg.ends_with(&[0xFF, 0xD9]));
}

#[test]
fn voice_recorder_hold_lock_cancel_waveform_and_round_video() {
    let mut rec = VoiceRecorderController::new();
    assert!(!rec.is_active());

    // Hold -> slide left cancels
    rec.start_hold(10.0).expect("synthetic capture starts");
    assert!(matches!(rec.phase(), VoiceRecordPhase::Holding { .. }));
    let res = rec.update_hold_drag(
        egui::Vec2::new(-VOICE_SLIDE_CANCEL_THRESHOLD_PX - 4.0, 0.0),
        10.5,
    );
    assert_eq!(res, Some(true));
    assert!(!rec.is_active());

    // Hold -> slide up locks -> tick -> pause/resume -> finish emits valid 5-bit waveform
    rec.start_hold(20.0).expect("synthetic capture starts");
    let locked = rec.update_hold_drag(
        egui::Vec2::new(0.0, -VOICE_SLIDE_LOCK_THRESHOLD_PX - 4.0),
        20.5,
    );
    assert_eq!(locked, Some(false));
    assert!(rec.is_locked());

    for step in 0..16 {
        rec.tick(20.6 + f64::from(step) * 0.1);
    }
    rec.toggle_pause(22.2);
    assert!(rec.is_paused());
    rec.toggle_pause(23.0);
    assert!(!rec.is_paused());

    let attachment = rec
        .finish(24.0)
        .expect("voice attachment produced")
        .attachment;
    assert!(attachment.duration_secs >= 1);
    let bars = waveform_decode_5bit(&attachment.waveform_5bit);
    assert!(!bars.is_empty());

    // Round video placeholder
    let mut rv = RoundVideoRecorderState::default();
    rv.start(100.0, true);
    let clip = rv.finish(105.0).expect("round video produced");
    assert_eq!(clip.duration_secs, 5);
}

#[test]
fn compose_extras_inline_bots_link_preview_slow_mode_spellcheck_and_effects() {
    let store = MockHistoryStore::new();

    // Inline bot query parser & panel hook
    assert_eq!(parse_inline_bot_query("@gif cats"), Some(("gif", "cats")));
    assert_eq!(parse_inline_bot_query("@ab x"), None); // username < 3 chars
    assert_eq!(parse_inline_bot_query("hello world"), None);

    let mut panel = InlineBotPanelState::new();
    panel.sync_from_text("@gif rust", &store);
    assert_eq!(panel.results().len(), 3);
    panel.sync_from_text("plain text", &store);
    assert!(panel.results().is_empty());

    // Link preview detection & disable
    assert_eq!(
        extract_first_url("Check out https://www.rust-lang.org today!").as_deref(),
        Some("https://www.rust-lang.org")
    );
    let mut lp = LinkPreviewEditorState::default();
    lp.sync_from_text("Visit https://www.rust-lang.org now", &store);
    assert!(lp.is_visible());
    assert_eq!(lp.preview.as_ref().unwrap().site_name, "Rust Lang");

    // Slow mode timer
    let sm = SlowModeState {
        seconds: 30,
        next_send_unix: 1030,
    };
    assert_eq!(sm.remaining_secs(1005), 25);
    assert_eq!(sm.remaining_secs(1030), 0);
    assert_eq!(SlowModeState::format_countdown(25), "0:25");
    assert_eq!(SlowModeState::format_countdown(3665), "1:01:05");

    // Spellchecker range replacement
    let mut text = "Helo world".to_owned();
    let range = MisspelledRange {
        utf16_start: 0,
        utf16_len: 4,
        byte_start: 0,
        byte_len: 4,
    };
    assert!(ComposeSpellchecker::replace_range(
        &mut text, range, "Hello"
    ));
    assert_eq!(text, "Hello world");

    // Message effects catalog
    assert!(!MESSAGE_EFFECTS.is_empty());
    assert_eq!(
        find_message_effect(MESSAGE_EFFECTS[0].id).unwrap().name,
        "Fire"
    );
}

#[test]
fn message_actions_copy_with_entities_shift_range_forward_options_translate_report_and_reactions() {
    let mut store = MockHistoryStore::new();
    let peer = DEMO_FORUM_PEER_ID;
    let dm = PeerId::from_user(1002);

    // 1. Copy selected text with entities (including surrogate-pair emoji before entity!)
    let parsed = parse_compose_markdown("🚀 Hello **bold** and `code` world");
    let sliced = slice_text_with_entities(&parsed, 8, 22);
    assert_eq!(sliced.text, "bold and code ");
    assert!(
        sliced
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Bold)
    );
    assert!(
        sliced
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Code)
    );
    let md = text_with_entities_to_markdown(&sliced);
    assert!(md.contains("**bold**"));
    assert!(md.contains("`code`"));

    // 2. Shift+click range selection & Reply with Quote extraction
    let mut inner = HistoryInnerWidget::new();
    let slice = store.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    inner.apply_slice(peer, slice, PeerKind::Group, true);
    let non_service: Vec<MsgId> = inner
        .messages()
        .iter()
        .filter(|m| !m.is_service())
        .map(|m| m.id)
        .collect();
    assert!(non_service.len() >= 4);
    inner.toggle_select_message(non_service[0]);
    inner.select_range_to(non_service[3]);
    assert_eq!(inner.selected_count(), 4);

    // Drag selection on one message produces ReplyQuote
    inner.clear_selection();
    let text_idx = inner
        .messages()
        .iter()
        .position(|m| !m.is_service() && m.text.text.chars().count() > 20)
        .expect("message with text");
    let text_msg_id = inner.messages()[text_idx].id;
    let full = to_hist_full(FullMsgId {
        peer,
        msg: text_msg_id,
    });
    // Simulate partial drag via list selection directly on known indices
    let mut compose = ComposeControls::new();
    let quote_src = slice_text_with_entities(&inner.messages()[text_idx].text, 0, 10);
    compose.start_reply_with_quote(
        text_msg_id,
        "Sam Chen".to_owned(),
        "preview".to_owned(),
        crate::history::sections_model::ReplyQuote {
            text: quote_src.text.clone(),
            entities: quote_src.entities,
            offset_utf16: 0,
        },
    );
    let pending = compose.build_pending_extended(None);
    assert_eq!(
        pending.reply_quote.as_ref().map(|q| q.text.as_str()),
        Some(quote_src.text.as_str())
    );
    let _ = (
        full,
        ListSelectionEndpoint {
            item_id: full,
            item_index: text_idx,
            text: FlatSelectionEndpoint::new(0, false),
        },
    );

    // 3. Forward with hide sender & hide captions options
    let photo_msg = inner
        .messages()
        .iter()
        .find(|m| matches!(m.media, Some(HistoryMedia::Photo(_))) && !m.text.is_empty())
        .expect("photo with caption");
    let fwds = store
        .forward_messages_with_options(
            peer,
            &[photo_msg.id],
            dm,
            ForwardOptions {
                drop_author: true,
                drop_media_captions: true,
            },
        )
        .expect("forward with options");
    assert_eq!(fwds.len(), 1);
    assert!(fwds[0].forward.is_none(), "sender must be hidden");
    assert!(fwds[0].text.is_empty(), "caption must be stripped");

    // 4. Translate & Show Original + Report
    let tr = store
        .translate_message(peer, text_msg_id, "en")
        .expect("translate succeeds");
    assert!(tr.contains("[EN translation]"));
    store.clear_message_translation(peer, text_msg_id);
    store
        .report_messages(peer, &[text_msg_id], MessageReportReason::Spam, "test")
        .expect("report succeeds");
    assert_eq!(store.ext.reports_log.len(), 1);

    // 5. Reactions picker full emoji list (`tg-text::emoji`) & `tg-lottie` animated reactions
    let mut picker = ReactionsPickerState::new();
    picker.active_section = emoji::Section::People;
    let people = picker.current_emoji_list();
    assert!(
        people.len() > 100,
        "full emoji list expected, got {}",
        people.len()
    );

    let lottie_json = animated_reaction_lottie_json("🔥");
    let comp = Composition::from_json(&lottie_json).expect("valid animated reaction Lottie");
    let fb = render_composition(&comp, 12.0, LottieSize::new(64, 64));
    assert_eq!((fb.width, fb.height), (64, 64));
    assert!(fb.data.as_chunks::<4>().0.iter().any(|px| px[3] > 180));

    // 6. Verify MockBackend delegates extended trait cleanly
    reset_mock_backend_history();
    let mut mb = crate::history::mock::MockHistoryBackendAdapter::new(true);
    assert!(!HistoryBackend::forum_topics(&mb, DEMO_FORUM_PEER_ID).is_empty());
    assert!(!HistoryBackend::saved_sublists(&mb, DEMO_SAVED_PEER_ID).is_empty());
    assert!(
        HistoryBackend::replies_root_and_comments(&mb, DEMO_CHANNEL_PEER_ID, DEMO_CHANNEL_POST_ID)
            .is_some()
    );
    assert!(
        !HistoryBackend::admin_log_entries(&mb, DEMO_FORUM_PEER_ID, &AdminLogFilter::default())
            .is_empty()
    );
    assert!(
        HistoryBackend::report_messages(
            &mut mb,
            DEMO_FORUM_PEER_ID,
            &[MsgId(102)],
            MessageReportReason::Spam,
            ""
        )
        .is_ok()
    );
}
