//! `tg-tgw`: Sandboxed HTML widget messages engine, bridge protocol, canvas/cursor/emoji
//! backend, Win32 process sandbox, and WebView2 widget host.
//!
//! Port origin: `Telegram/SourceFiles/tgw/*`, `Telegram/SourceFiles/platform/win/widget_*`,
//! `Telegram/SourceFiles/_other/widget_helper*`, and
//! `Telegram/SourceFiles/history/view/media/history_view_html_widget.*`.

pub mod backend;
pub mod bridge;
pub mod canvas;
pub mod cursor;
pub mod emoji;
pub mod error;
pub mod filters;
pub mod meter;
pub mod model;
pub mod sandbox;
pub mod state_machine;
pub mod storage;
pub mod storage_disk;
pub mod wire;

#[cfg(test)]
mod tests_wp47;

pub use backend::{
    BackendFont, BackendImage, CanvasSpellings, DataImageFormat, FontKind, FontMetrics,
    MAX_BACKEND_IMAGE_BYTES, MAX_BACKEND_IMAGES, MAX_CANVAS_FONT_PARSES,
    MAX_CANVAS_FONT_SPELLING_SIZE, MAX_CANVAS_FONT_SPELLINGS, MAX_CANVAS_FONTS, MAX_FAMILIES_BYTES,
    MAX_FONT_PIXEL_SIZE, MAX_FONT_SIZE, MAX_IMAGE_PIXELS, MAX_IMAGE_SIDE, MAX_IMAGE_URL_SIZE,
    MAX_LIVE_FONTS, MAX_MEASURED_TEXT, MAX_NEW_CANVAS_FONTS, ParsedDataUrl, WidgetBackend,
    normalize_font_weight, parse_data_url, resolve_font_families, valid_image_size,
};
pub use bridge::{
    BotQueryTracker, COPIED_TOAST_LINES, CopyTextDecision, LinkPolicyDecision, PendingBotQuery,
    REQUEST_TIMEOUT_MS, WidgetRequestAnswer, WidgetRequestFailure, WidgetRequestOutcome,
    evaluate_copy_text, evaluate_link_open, is_passport_or_oauth_link, map_rpc_error_to_outcome,
};
pub use canvas::{
    CANVAS_LIMIT_DIAG_KIND, CANVAS_PIXEL_BYTES, CanvasDiagnostic, CanvasOp, CanvasStore,
    CompositeMode, DEFAULT_MITER_LIMIT, LineCap, LineJoin, MAX_CANVAS_BACKEND_BYTES,
    MAX_CANVAS_PIXEL_WORK, MAX_CANVAS_READ_BYTES, MAX_CANVAS_SIDE, MAX_CANVAS_TOTAL_BYTES,
    MAX_CANVASES, MAX_OP_CLIPS, MAX_OP_DASHES, MAX_OP_STOPS, MAX_OP_TEXT, MAX_SHADOW_BLUR,
    Matrix2D, PathOp, Rgba, blend_hsl_pixel, blend_hsl_rgb, composite_hsl_pixmap, stack_radius,
    stroke_reach, valid_canvas_size,
};
pub use cursor::{
    CursorKind, CursorStyles, ElementCursorRule, GestureClaim, HoverAction, HoverTargets,
    MAX_DEVICE_RATIO, MIN_DEVICE_RATIO, TOUCH_SLOP_PX, TouchAction, TouchOwner, clamp_device_ratio,
    outside_touch_slop, pointer_targets, shadow_chain,
};
pub use emoji::{
    EMOJI_ADVANCE, EMOJI_PRESENTATION_RANGES, EMOJI_RISE, EmojiAccount, EmojiCell, EmojiRect,
    EmojiSides, EmojiWaits, color_glyph_candidates, emoji_advance, emoji_box, find_emoji_cells,
    is_emoji_presentation, is_joining, is_regional_indicator, is_rtl_char, walk_text_with_emoji,
};
pub use error::{Result, TgwError};
pub use filters::{
    BLUR_SLACK, CLIP_HISTORY_ENTRY_BYTES, CLIP_ROW_BYTES, CLIP_STRUCT_BYTES, ClipRect, ClipShape,
    ClipStack, FILTER_PIXEL_SCRATCH_BYTES, FilterCost, FilterMargins, FilterOp, MAX_FILTER_CHAIN,
    MAX_FILTER_FACTOR, MAX_FILTER_LENGTH, MAX_FILTER_PIXELS, MIN_BLUR_SIGMA, OFFSET_SLACK,
    REDUCED_BLUR_SIGMA, RESAMPLE_PIXEL_WORK, apply_filters, clip_base_bytes, filter_reach,
    measure_filters, usable_filter, usable_filters,
};
pub use meter::{
    DEFAULT_MAX_CONSECUTIVE_SLOW_FRAMES, DEFAULT_MEMORY_HARD_LIMIT_BYTES,
    DEFAULT_MEMORY_PRESSURE_HOLD_MS, DEFAULT_MEMORY_SOFT_LIMIT_BYTES,
    DEFAULT_SINGLE_FRAME_SUSPEND_SECS, DEFAULT_SUSPEND_HOLD_MS, METER_HEAVY_PASS_SECS,
    METER_INPUT_IMMEDIATE_SECS, METER_LOAD_GRACE_SECS, METER_SUSTAINED_FACTOR,
    METER_SUSTAINED_LIMIT_RATIO, METER_SUSTAINED_WINDOW_SECS, METER_THROTTLE_FACTOR,
    METER_THROTTLE_FREE_SECS, MeterConfig, MeterSnapshot, MeterVerdict, WatchdogReason,
    WidgetMeter,
};
pub use model::{
    ANSWER_DEADLINE_MS, Anchors, BubbleThemeColors, ChatType, ContentSafeArea,
    DEFAULT_PUBLIC_DATA_POLL_MS, HeightChange, HeightLayout, MAX_PUBLIC_DATA_POLL_SECS,
    MAX_SAFE_AREA_CORNER, MAX_SAFE_AREA_INSET, MAX_UNPACKED_PAYLOAD_BYTES,
    MIN_PUBLIC_DATA_POLL_SECS, MessageContext, PublicDataPollResult, PublicDataPoller,
    ResizeAnchor, TGW_MAX_HEIGHT, TGW_MIN_HEIGHT, TGW_MIN_WIDTH, TGW_PLACEHOLDER_HEIGHT,
    WidgetButtonTokens, WidgetFields, WidgetFlags, WidgetMedia, WidgetState, WidgetStateEdit,
    WidgetTextTokens, WidgetTheme, build_bubble_theme, clamp_poll_period_ms,
    extract_html_text_preview, extract_html_title, flatten_color, is_dark_color, pack_payload_zlib,
    same_theme, unpack_payload,
};
pub use sandbox::{
    ALL_APPLICATION_PACKAGES_OPT_OUT, APP_CONTAINER_NAME, AppContainerLevel,
    CHANNEL_PIPE_BUFFER_BYTES, CHILD_PROCESS_RESTRICTED, DEFAULT_JOB_LIMIT_FLAGS,
    DEFAULT_JOB_UI_RESTRICTIONS, JobLimitsPolicy, LESS_PRIVILEGED_ATTR, MitigationExpectation,
    RestrictedTokenPolicy, SE_CHANGE_NOTIFY_LUID_LOW, SandboxPolicy, WIDGET_CUSTOM_SCHEME,
    WIDGET_ENTRY_DOC_ID, WebViewWidgetHost, WidgetCspPolicy, is_allowed_widget_navigation,
    quote_win32_arg, wrap_widget_html,
};
pub use state_machine::{
    EnginePlace, HELPER_FAILURE_RUN_AGE_MS, HELPER_IDLE_TIMEOUT_MS, HelperCircuitBreaker,
    KEPT_PICTURE_LIMIT_BYTES, KeptPictureEntry, KeptPictureTrim, MAX_ALIVE_WIDGETS,
    MAX_HELPER_FAILURES, MAX_LOG_LINE, MAX_LOG_NOTES, MAX_WIDGET_CRASHES, MessageMemory,
    RuntimeState, WidgetStateMachine, plan_kept_picture_trim,
};
pub use storage::{
    HeightRecord, LEGACY_MAX_STORED_BYTES, STORAGE_INDEX_VERSION, STORAGE_QUOTA_BYTES,
    STORAGE_RECORD_VERSION, StorageArea, StorageMeta, WidgetMsgKey, WidgetStorageStore,
    decode_storage_record, encode_storage_record, storage_area_bytes, storage_area_fits,
};
pub use storage_disk::{
    DEFAULT_TOTAL_DISK_QUOTA_BYTES, DISK_INDEX_VERSION, DiskStorageConfig, DiskWriteOutcome,
    WidgetStorageDisk,
};
pub use wire::{
    ControlKind, ControlMessage, EmbeddingCaps, HostToRendererMsg, LOAD_ACK_QUERY_ID,
    LOCAL_QUERY_ID, LoadChange, LoadTracker, MAX_CANVAS_RUN_OPS, MAX_CURSOR_BYTES,
    MAX_DECODE_BYTES, MAX_FRAME_BYTES, MAX_LIST_COMMANDS, MAX_MESSAGE_BYTES,
    MAX_MIRRORED_TEXT_BYTES, MAX_REPORTS_BYTES, MAX_STOP_REASON_BYTES, MIRRORED_OVERHEAD_BYTES,
    MirroredWay, READY_JSON_MESSAGE, RendererToHostMsg, WIRE_HEADER_BYTES, WIRE_LENGTH_BYTES,
    WIRE_NUMBER_BYTES, WIRE_TAG, control_kind_of, decode_host_message, decode_press,
    decode_renderer_message, encode_host_message, encode_renderer_message, is_control_frame,
    is_exactly,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    // =========================================================================
    // 1. Bridge protocol & hosted binary wire encode/decode tests
    // =========================================================================

    #[test]
    fn wire_control_round_trip_all_variants() {
        let cases = vec![
            ControlMessage::Open(2.0),
            ControlMessage::MenuDeliver(42),
            ControlMessage::FingerHold(99),
            ControlMessage::FingerRelease(101),
            ControlMessage::Mirror,
            ControlMessage::MirrorWork,
            ControlMessage::CanvasRefill,
            ControlMessage::Content,
            ControlMessage::Loaded,
            ControlMessage::Stopped("exceeded cpu quota".to_owned()),
            ControlMessage::Mirrored {
                way: MirroredWay::Host,
                text: r#"{"type":"ready"}"#.to_owned(),
            },
            ControlMessage::Cursor {
                pointers: 7,
                cursor: "pointer".to_owned(),
            },
            ControlMessage::CanvasRefused,
            ControlMessage::Clicked(123456),
        ];

        for msg in cases {
            let encoded = msg.encode().expect("encode control message");
            assert!(is_control_frame(&encoded));
            assert_eq!(control_kind_of(&encoded), Some(msg.kind()));
            let decoded = ControlMessage::decode(&encoded).expect("decode control message");
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn wire_rejects_non_finite_open_and_corrupt_frames() {
        assert!(ControlMessage::Open(f64::NAN).encode().is_err());
        assert!(ControlMessage::Open(f64::INFINITY).encode().is_err());

        // Inject NaN bits into Open frame directly.
        let mut nan_frame = vec![WIRE_TAG, ControlKind::Open as u8];
        nan_frame.extend_from_slice(&f64::NAN.to_bits().to_le_bytes());
        assert!(ControlMessage::decode(&nan_frame).is_err());

        // Trailing bytes on empty-body control frame.
        assert!(ControlMessage::decode(&[WIRE_TAG, ControlKind::Loaded as u8, 0]).is_err());
        // Unknown kind byte.
        assert!(ControlMessage::decode(&[WIRE_TAG, 200]).is_err());
        // Truncated stopped frame.
        assert!(
            ControlMessage::decode(&[WIRE_TAG, ControlKind::Stopped as u8, 10, 0, 0, 0]).is_err()
        );
    }

    #[test]
    fn wire_stops_reason_and_cursor_truncate_safely() {
        let long_reason = "a".repeat(MAX_STOP_REASON_BYTES + 128);
        let bytes = ControlMessage::Stopped(long_reason)
            .encode()
            .expect("encode long stopped");
        let decoded = ControlMessage::decode(&bytes).expect("decode truncated stopped");
        match decoded {
            ControlMessage::Stopped(s) => assert_eq!(s.len(), MAX_STOP_REASON_BYTES),
            other => panic!("unexpected: {other:?}"),
        }

        let long_cursor = "p".repeat(MAX_CURSOR_BYTES + 50);
        let cbytes = ControlMessage::Cursor {
            pointers: 3,
            cursor: long_cursor,
        }
        .encode()
        .expect("encode long cursor");
        match ControlMessage::decode(&cbytes).expect("decode truncated cursor") {
            ControlMessage::Cursor { pointers, cursor } => {
                assert_eq!(pointers, 3);
                assert_eq!(cursor.len(), MAX_CURSOR_BYTES);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn json_bridge_protocol_round_trip_and_validation() {
        let theme = build_bubble_theme(&BubbleThemeColors::dark());
        let host_msgs = vec![
            HostToRendererMsg::Ready,
            HostToRendererMsg::Theme {
                dark: true,
                theme: Box::new(theme),
            },
            HostToRendererMsg::Data {
                chat_data: Some("{\"votes\":5}".to_owned()),
                public_data: Some("{\"total\":42}".to_owned()),
            },
            HostToRendererMsg::SafeArea {
                area: ContentSafeArea {
                    bottom: 24,
                    corner_left: 0,
                    corner_right: 68,
                },
            },
            HostToRendererMsg::Visibility { visible: true },
            HostToRendererMsg::Resize {
                width: 320.0,
                max_height: 600.0,
                dpr: 1.5,
            },
            HostToRendererMsg::QueryAnswer {
                request_id: 9,
                outcome: WidgetRequestOutcome::ok(Some("{\"ok\":true}".to_owned())),
            },
            HostToRendererMsg::Frame { time: 16.667 },
            HostToRendererMsg::Frozen,
            HostToRendererMsg::Query {
                qid: LOAD_ACK_QUERY_ID,
                q: "viewport".to_owned(),
            },
        ];
        for msg in host_msgs {
            let encoded = encode_host_message(&msg).expect("encode host msg");
            let decoded = decode_host_message(&encoded).expect("decode host msg");
            assert_eq!(decoded, msg);
        }

        let mut kv = BTreeMap::new();
        kv.insert("counter".to_owned(), "7".to_owned());
        let renderer_msgs = vec![
            RendererToHostMsg::Ready,
            RendererToHostMsg::Size { height: 240.0 },
            RendererToHostMsg::Ops { count: 14 },
            RendererToHostMsg::OpenLink {
                url: "https://example.org".to_owned(),
                telegram: false,
            },
            RendererToHostMsg::CopyText {
                text: "HELLO".to_owned(),
            },
            RendererToHostMsg::BotQuery {
                request_id: 9,
                data: "action=vote".to_owned(),
                wallet: false,
            },
            RendererToHostMsg::StorageCommit {
                area: "local".to_owned(),
                values: kv,
            },
            RendererToHostMsg::WatchChange { on: true },
            RendererToHostMsg::CursorChange {
                cursor: "pointer".to_owned(),
            },
            RendererToHostMsg::SelectionChange {
                text: "selected".to_owned(),
            },
            RendererToHostMsg::Diagnostic {
                kind: "canvas-limit".to_owned(),
                detail: "skipped".to_owned(),
            },
            RendererToHostMsg::Stopped {
                reason: "timeout".to_owned(),
            },
        ];
        for msg in renderer_msgs {
            let encoded = encode_renderer_message(&msg).expect("encode renderer msg");
            let decoded = decode_renderer_message(&encoded).expect("decode renderer msg");
            assert_eq!(decoded, msg);
        }

        // Negative height must be rejected.
        assert!(decode_renderer_message(r#"{"type":"size","height":-5.0}"#).is_err());

        // Load tracker requires both non-empty draw ops and load ack (-1).
        let mut tracker = LoadTracker::default();
        let c1 = tracker.on_draw_ops(3);
        assert!(c1.content && !c1.loaded);
        let c2 = tracker.on_query(LOCAL_QUERY_ID);
        assert!(!c2.loaded);
        let c3 = tracker.on_query(LOAD_ACK_QUERY_ID);
        assert!(c3.loaded && tracker.loaded);
    }

    #[test]
    fn bridge_link_copy_and_rpc_error_policies() {
        assert_eq!(
            map_rpc_error_to_outcome("FLOOD_WAIT_30", true),
            WidgetRequestOutcome::err(WidgetRequestFailure::RateLimited)
        );
        assert_eq!(
            map_rpc_error_to_outcome("WIDGET_RESPONSE_TIMEOUT", false),
            WidgetRequestOutcome::err(WidgetRequestFailure::Timeout)
        );
        assert_eq!(
            map_rpc_error_to_outcome("BOT_RESPONSE_TIMEOUT", false),
            WidgetRequestOutcome::err(WidgetRequestFailure::Failed)
        );

        assert!(matches!(
            evaluate_link_open("tg://passport?bot_id=1", true, true, false),
            LinkPolicyDecision::Refused(_)
        ));
        assert!(matches!(
            evaluate_link_open("https://t.me/login?token=abc", true, true, false),
            LinkPolicyDecision::Refused(_)
        ));
        assert!(matches!(
            evaluate_link_open("https://example.org", false, true, true),
            LinkPolicyDecision::Dropped(_)
        ));
        assert!(matches!(
            evaluate_link_open("https://example.org", false, true, false),
            LinkPolicyDecision::ConfirmExternal { .. }
        ));
        assert!(matches!(
            evaluate_link_open("https://t.me/telegram", true, true, false),
            LinkPolicyDecision::OpenTelegram { .. }
        ));

        match evaluate_copy_text("hi 🚀", true) {
            CopyTextDecision::Copy { utf16_units, .. } => assert_eq!(utf16_units, 5),
            other => panic!("expected Copy, got {other:?}"),
        }

        let mut tracker = BotQueryTracker::new();
        tracker.register(1, false, 1_000);
        tracker.register(2, true, 5_000);
        assert_eq!(tracker.expire(30_999), Vec::<u64>::new());
        assert_eq!(tracker.expire(31_000), vec![1]);
        assert!(tracker.complete(2));
        assert!(tracker.is_empty());
    }

    // =========================================================================
    // 2. Widget state machine, meter, kept budget & payload tests
    // =========================================================================

    #[test]
    fn widget_state_machine_lifecycle_crashes_and_a_b_a_edit() {
        let mut sm = WidgetStateMachine::new(b"<div>v1</div>");
        assert_eq!(sm.state(), RuntimeState::Idle);
        assert!(!sm.wants_start());

        sm.set_visible(true);
        sm.mark_painted();
        assert!(sm.wants_start());

        let gen1 = sm.request_start().expect("request start");
        assert_eq!(sm.state(), RuntimeState::Waiting);
        sm.begin_construct(gen1).expect("begin construct");
        assert_eq!(sm.state(), RuntimeState::Constructing);
        sm.mark_live(gen1).expect("mark live");
        assert!(sm.is_interactive());

        // Record auto-height.
        sm.record_height(220, HeightChange::Own, ResizeAnchor::Bottom);
        assert_eq!(sm.height_layout().map(|h| h.height), Some(220));
        assert_eq!(sm.height_layout().map(|h| h.serial), Some(1));

        // Release keeping still frame, then restart.
        sm.release(true);
        assert_eq!(sm.state(), RuntimeState::Released);
        assert!(sm.is_heavy());

        let gen2 = sm.request_start().expect("restart after release");
        sm.begin_construct(gen2).expect("construct 2");
        sm.mark_live(gen2).expect("live 2");

        // Crash #1: recoverable -> returns to Idle so it can try once more.
        sm.handle_stop("oom", false);
        assert_eq!(sm.state(), RuntimeState::Idle);
        assert_eq!(sm.crash_count(), 1);
        assert!(!sm.crashed_out());

        // Crash #2: reaches MAX_WIDGET_CRASHES -> permanently Stopped.
        let gen3 = sm.request_start().expect("restart after 1st crash");
        sm.begin_construct(gen3).expect("construct 3");
        sm.handle_stop("oom again", false);
        assert_eq!(sm.state(), RuntimeState::Stopped);
        assert!(sm.crashed_out());
        assert!(sm.stop_fatal());
        assert!(sm.request_start().is_err());

        // Edit A -> B -> A resets crash counter and advances content serial.
        let serial_v1 = sm.content_serial();
        assert!(sm.update_html(b"<div>v2</div>"));
        assert_eq!(sm.crash_count(), 0);
        assert_eq!(sm.state(), RuntimeState::Idle);
        assert!(sm.update_html(b"<div>v1</div>"));
        assert!(sm.content_serial() > serial_v1 + 1);
    }

    #[test]
    fn widget_meter_and_kept_budget_eviction() {
        let mut meter = WidgetMeter::new();
        // Inside 1s load grace -> no hold.
        meter.charge(10.0, 0.08);
        assert_eq!(meter.held_for_ms(10.0), 0);

        // Past load grace, single 40ms pass -> 3x throttle (120ms hold).
        meter.charge(11.5, 0.04);
        assert_eq!(meter.held_for_ms(11.5), 120);

        // User input grants 250ms exemption.
        meter.note_input(11.5);
        assert_eq!(meter.held_for_ms(11.6), 0);

        // Kept picture trim evicts oldest unloaded entries first while never evicting visible ones.
        let stills = vec![
            KeptPictureEntry {
                pixels: 30 * 1024 * 1024,
                unloaded_serial: 0, // visible
                work_id: 1,
            },
            KeptPictureEntry {
                pixels: 25 * 1024 * 1024,
                unloaded_serial: 10,
                work_id: 2,
            },
            KeptPictureEntry {
                pixels: 30 * 1024 * 1024,
                unloaded_serial: 5, // oldest unloaded
                work_id: 3,
            },
        ];
        let trim = plan_kept_picture_trim(&stills);
        assert_eq!(trim.visible_bytes, 30 * 1024 * 1024);
        assert_eq!(trim.unloaded_bytes, 55 * 1024 * 1024);
        assert_eq!(trim.drop_indices, vec![2]); // drops 30 MiB oldest, leaving 25 MiB <= 48 MiB
    }

    #[test]
    fn payload_unpack_supports_zlib_and_gzip_and_rejects_corrupt() {
        let html = b"<title>Weather</title><div>Sunny 22C</div>";
        let zlib_bytes = pack_payload_zlib(html);
        assert_eq!(unpack_payload(&zlib_bytes).expect("unpack zlib"), html);

        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(html).expect("write gz");
        let gz_bytes = gz.finish().expect("finish gz");
        assert_eq!(unpack_payload(&gz_bytes).expect("unpack gzip"), html);

        assert!(unpack_payload(&[]).is_err());
        assert!(unpack_payload(b"not compressed data").is_err());
        assert_eq!(
            extract_html_title("<title>Demo Card</title><p>Hello</p>").as_deref(),
            Some("Demo Card")
        );
    }

    // =========================================================================
    // 3. Sandbox policy construction & Windows Win32 verification tests
    // =========================================================================

    #[test]
    fn sandbox_policy_construction_and_validation() {
        let policy64 = SandboxPolicy::build(true, true);
        policy64.validate().expect("valid 64-bit CET policy");
        assert_eq!(policy64.creation_size, 16);
        assert_ne!(
            policy64.creation_flags[0] & sandbox::MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON,
            0
        );
        assert_ne!(
            policy64.creation_flags[0] & sandbox::MITIGATION_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON,
            0
        );
        assert_ne!(
            policy64.creation_flags[0] & sandbox::MITIGATION_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON,
            0
        );
        assert_ne!(
            policy64.creation_flags[1] & sandbox::MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON,
            0
        );

        let policy32 = SandboxPolicy::build(false, false);
        policy32.validate().expect("valid 32-bit policy");
        assert_eq!(policy32.creation_size, 8);
        assert_ne!(
            policy32.creation_flags[0] & sandbox::MITIGATION_DEP_ENABLE,
            0
        );

        // Tampering with job limit fails validation.
        let mut weakened = policy64.clone();
        weakened.job.active_process_limit = 4;
        assert!(weakened.validate().is_err());

        // Navigation policy blocks external network and file URLs.
        assert!(is_allowed_widget_navigation(
            "tgw-widget://widget/index.html"
        ));
        assert!(is_allowed_widget_navigation("about:blank"));
        assert!(!is_allowed_widget_navigation("https://example.com"));
        assert!(!is_allowed_widget_navigation("file:///C:/Windows/win.ini"));
        assert!(!is_allowed_widget_navigation("javascript:alert(1)"));

        // Win32 argument quoting matches CommandLineToArgvW.
        assert_eq!(quote_win32_arg("plain"), "plain");
        assert_eq!(quote_win32_arg("a b"), "\"a b\"");
        assert_eq!(quote_win32_arg(r#"a"b\"#), r#""a\"b\\""#);
    }

    #[cfg(windows)]
    #[test]
    fn win32_sandbox_job_token_and_pipe_enforcement() {
        let policy = SandboxPolicy::for_current_target();
        let job = sandbox::win::create_sandbox_job(&policy.job).expect("create sandbox job");
        sandbox::win::verify_job_limits(&job.handle, &policy.job).expect("verify job limits");

        let token = sandbox::win::create_restricted_lockdown_token()
            .expect("create restricted lockdown token");
        sandbox::win::verify_lockdown_token(&token).expect("verify lockdown token");

        let pipe_name = format!(
            r"\\.\pipe\rustgram-tgw-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let (_server, _client) =
            sandbox::win::create_sandbox_pipe_pair(&pipe_name).expect("create owner-DACL pipe");
    }

    // =========================================================================
    // 4. Canvas, cursor, emoji, backend & storage tests
    // =========================================================================

    #[test]
    fn canvas_draw_read_and_composite_diagnostics() {
        let mut backend = WidgetBackend::new();
        let ops = vec![
            CanvasOp::FillRect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
                color: Rgba::new(255, 64, 32, 255),
                alpha: 1.0,
                composite: CompositeMode::SourceOver,
                matrix: Matrix2D::default(),
            },
            CanvasOp::FillRect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                color: Rgba::new(0, 255, 0, 255),
                alpha: 1.0,
                composite: CompositeMode::Hue,
                matrix: Matrix2D::default(),
            },
        ];
        backend.canvas_run(1, 4, 4, &ops).expect("run canvas ops");
        let untouched = backend.canvas_read(1, 1, 1, 1, 1).expect("read pixel");
        assert_eq!(untouched, vec![255, 64, 32, 255]);
        let blended = backend.canvas_read(1, 0, 0, 1, 1).expect("read hue pixel");
        let expected = blend_hsl_pixel(
            Rgba::new(255, 64, 32, 255),
            Rgba::new(0, 255, 0, 255),
            CompositeMode::Hue,
        );
        assert_eq!(
            blended,
            vec![expected.r, expected.g, expected.b, expected.a]
        );
        let diags = backend.canvases_mut().take_diagnostics();
        assert!(
            diags.is_empty(),
            "HSL modes must emit 0 unsupported diagnostics, got: {diags:?}"
        );
    }

    #[test]
    fn cursor_shadow_chain_and_inheritance() {
        let html = r#"<div data-twid="1"><style>.a{cursor:pointer}</style><br><button data-twid="2"><span data-twid="3">Go</span></button></div>"#;
        assert_eq!(shadow_chain(html, 3), vec![3, 2, 1]);

        let mut styles = CursorStyles::new();
        styles.set_html(html);
        styles.set_rule(
            2,
            ElementCursorRule {
                base: Some(CursorKind::Auto),
                hover: Some(CursorKind::Pointer),
                active: Some(CursorKind::Grabbing),
            },
        );
        assert_eq!(styles.resolve(3, 0, 0), CursorKind::Auto);
        assert!(styles.is_pointer(3, 3, 0));
        assert_eq!(styles.resolve(3, 3, 3), CursorKind::Grabbing);
    }

    #[test]
    fn emoji_cells_and_storage_record_round_trip() {
        let cells = find_emoji_cells("Hi 🚀 and 🇺🇳!");
        assert_eq!(cells.len(), 2);
        assert!(cells.iter().all(|c| c.resolved));
        assert!(find_emoji_cells("plain ascii").is_empty());
        assert!(color_glyph_candidates("🚀⭐") >= 2);

        let mut area = StorageArea::new();
        area.insert("theme".to_owned(), "dark".to_owned());
        area.insert("empty".to_owned(), String::new());
        let encoded = encode_storage_record(&area).expect("encode storage");
        let decoded = decode_storage_record(&encoded).expect("decode storage");
        assert_eq!(decoded, area);

        let mut store = WidgetStorageStore::new();
        let key = WidgetMsgKey {
            peer_id: 100,
            msg_id: 55,
        };
        store
            .write(key, StorageMeta::default(), area.clone())
            .expect("write store");
        assert_eq!(store.read(key), Some(&area));
        assert!(store.set_height(
            key,
            StorageMeta::default(),
            HeightRecord {
                height: 240.0,
                bottom: true,
            }
        ));
        assert_eq!(store.height(key).map(|h| h.height), Some(240.0));
    }
}

#[cfg(test)]
mod fuzz_untrusted;
