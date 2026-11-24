//! WP-38 integration & UI tests:
//! - Streamed bot drafts (`history_streamed_drafts`: text/rich drafts, stop action, adoption, expiry)
//! - Compose stash (`data_compose_stash`, `StashHintManager`, `Ctrl+S` / corner button UI)
//! - Read metrics (`ReadMetricsTracker`, `ReadMetricsBatcher`, `messages.reportReadMetrics`)
//! - Emoji interactions (`SendMessageEmojiInteraction` / `SendMessageEmojiInteractionSeen`, JSON, overlay)
//! - 1:1 No-Forwards request & toggle (`history_view_no_forwards_request`, `messages.toggleNoForwards`)
//! - Media toggling with `FILE_REFERENCE_*` refresh & retry (`api_toggling_media`)
//!
//! All tests run strictly against in-process `FakeRpcClient` and synthetic placeholder data.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Vec2};
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    DocumentData, DocumentId, DocumentKind, Message, MessageContent, MessageMedia,
    MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags, ServiceAction as DataServiceAction,
    StickerMeta, TextWithEntities as DataText, User, UserFlags, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::history::HistoryWidget;
use tg_app::history::backend::{HistoryBackend, ServiceAction as UiServiceAction};
use tg_app::history::compose_stash::{
    ComposeStash, ComposeStashKey, ComposeStashStore, STASH_HINT_MAX_SHOWS, StashHintManager,
    StashedFile, StashedForward,
};
use tg_app::history::emoji_interactions::{
    EMOJI_INTERACTION_ACCEPT_SEEN_MS, EMOJI_INTERACTION_ACCUMULATE_DELAY_MS,
    EmojiInteractionCoordinator, EmojiInteractionSingle, EmojiInteractionsBunch,
    HistoryViewEmojiInteractions, can_play_emoji_interaction_message, choose_interaction_emoji,
    interactions_to_json, parse_interactions_json,
};
use tg_app::history::no_forwards_request::{
    NO_FORWARDS_REQUEST_FEATURES, format_no_forwards_request_expired_text,
    format_no_forwards_request_title, format_no_forwards_toggle_text,
    has_no_forwards_request_buttons, measure_no_forwards_request_card,
    should_render_no_forwards_request_card,
};
use tg_app::history::rich_compose::RichThreadDraft;
use tg_app::history::sections_model::ForwardOptions;
use tg_app::history::streamed_drafts::{
    HistoryStreamedDrafts, STREAMED_DRAFT_CLEAR_TIMEOUT_MS, common_prefix_len,
};
use tg_app::live::LiveSession;
use tg_app::live::chat_l233::{
    apply_updates_for_live_session_at, flush_read_metrics_live_at, report_read_metrics_live_at,
    take_adopted_streamed_drafts,
};
use tg_app::live::read_metrics::{
    READ_METRICS_ACTIVITY_TIMEOUT_MS, READ_METRICS_GRACE_PERIOD_MS,
    READ_METRICS_MAX_TRACKING_DURATION_MS, READ_METRICS_SEND_TIMEOUT_MS, ReadMetricsBatcher,
    ReadMetricsTracker,
};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::{Palette, ThemeMode};
use tg_app::stand_in::types::{MsgId, PeerId, PeerKind};

// =============================================================================
// FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = request.to_bytes();
        let ctor = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, raw));

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, outcome)) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC ctor: got {ctor:#010x}, expected {expected:#010x}"
                );
                let bytes = outcome?;
                R::Return::from_bytes(&bytes)
                    .map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("NO_FAKE_RESPONSE_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }
}

fn make_live_session() -> (LiveSession, FakeRpcClient, PeerId, PeerId) {
    let fake = FakeRpcClient::new(2);
    let self_uid = DataUserId(10_001);
    let bot_uid = DataUserId(20_002);
    let friend_uid = DataUserId(30_003);

    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            self_uid,
            tg_app::live::ErasedRpcClient::new(fake.clone()),
            cfg,
        )
        .expect("start runtime"),
    );

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;

        let mut me = User::new(self_uid, true);
        me.first_name = "Tester".to_owned();
        me.flags.insert(UserFlags::SELF);
        s.upsert_user(me, PeerUpdateFlags::NAME);

        let mut bot = User::new(bot_uid, false);
        bot.first_name = "HelperBot".to_owned();
        bot.access_hash = 900_002;
        s.upsert_user(bot, PeerUpdateFlags::NAME);

        let mut friend = User::new(friend_uid, false);
        friend.first_name = "Nadia".to_owned();
        friend.access_hash = 900_003;
        s.upsert_user(friend, PeerUpdateFlags::NAME);

        // Mark histories as loaded at top & bottom so load_slice_around doesn't try GetHistory RPC.
        for uid in [bot_uid, friend_uid] {
            let dp = DataPeerId::from_user(uid);
            let h = s.ensure_history(dp);
            h.loaded_ranges.add_slice(
                std::iter::empty(),
                tg_data::MsgRange::new(DataMsgId(0), tg_data::SERVER_MAX_MSG_ID),
                Some(0),
            );
        }
    });

    let live = LiveSession::new(runtime, 1);
    (
        live,
        fake,
        PeerId::from_user(bot_uid.0),
        PeerId::from_user(friend_uid.0),
    )
}

fn make_tl_message(id: i32, peer_uid: i64, from_uid: i64, text: &str) -> tl_types::Message {
    tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        guestchat_via_from: None,
        id,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: from_uid,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: 1_735_000_100,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    }
}

fn wall_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn run_egui_frame(ctx: &egui::Context, raw: egui::RawInput, f: impl FnMut(&mut egui::Ui)) {
    let mut out = ctx.run_ui(raw, f);
    out.textures_delta.clear();
}

// =============================================================================
// 1. Streamed Bot Drafts (`history_streamed_drafts`)
// =============================================================================

#[test]
fn wp38_streamed_bot_drafts_live_stop_adopt_and_expiry() {
    let (mut live, fake, bot_peer, _) = make_live_session();
    let bot_uid = bot_peer.bare() as i64;
    let base_now = wall_now_ms();

    // 1. Feed incremental streamed text draft chunks (`sendMessageTextDraftAction`).
    let upd1 = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
            user_id: bot_uid,
            top_msg_id: None,
            action: tl_enums::SendMessageAction::SendMessageTextDraftAction(
                tl_types::SendMessageTextDraftAction {
                    can_stop: true,
                    keep_on_stop: false,
                    random_id: 777_001,
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Hello from".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                },
            ),
        }),
        date: 1_735_000_010,
    });
    assert!(apply_updates_for_live_session_at(&live, &upd1, base_now));
    assert!(live.has_stoppable_streamed_draft(bot_peer, None));

    let upd2 = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
            user_id: bot_uid,
            top_msg_id: None,
            action: tl_enums::SendMessageAction::SendMessageTextDraftAction(
                tl_types::SendMessageTextDraftAction {
                    can_stop: true,
                    keep_on_stop: false,
                    random_id: 777_001,
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Hello from bot!".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                },
            ),
        }),
        date: 1_735_000_011,
    });
    assert!(apply_updates_for_live_session_at(
        &live,
        &upd2,
        base_now + 500
    ));

    let drafts = live.streamed_draft_messages(bot_peer, None);
    assert_eq!(drafts.len(), 1);
    assert!(drafts[0].id.0 < 0);
    assert!(drafts[0].text.text.starts_with("Hello from bot!"));

    // 2. User clicks Stop -> sends `messages.setTyping(sendMessageStopDraftAction)`.
    fake.enqueue_ok(tl_fns::messages::SetTyping::CONSTRUCTOR_ID, &true);
    assert!(live.stop_streamed_draft(bot_peer, None).expect("stop ok"));
    assert!(!live.has_stoppable_streamed_draft(bot_peer, None));
    assert!(live.streamed_draft_messages(bot_peer, None).is_empty());

    let typing_reqs = fake.recorded_of(tl_fns::messages::SetTyping::CONSTRUCTOR_ID);
    assert_eq!(typing_reqs.len(), 1);
    let parsed = tl_fns::messages::SetTyping::from_bytes(&typing_reqs[0]).expect("deser setTyping");
    assert!(matches!(
        parsed.action,
        tl_enums::SendMessageAction::SendMessageStopDraftAction(ref a) if a.random_id == 777_001
    ));

    // Further updates for the stopped random_id are ignored.
    assert!(!apply_updates_for_live_session_at(
        &live,
        &upd2,
        base_now + 1_000
    ));

    // 3. New draft with different random_id is seamlessly adopted when final NewMessage arrives.
    let upd3 = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
            user_id: bot_uid,
            top_msg_id: None,
            action: tl_enums::SendMessageAction::SendMessageTextDraftAction(
                tl_types::SendMessageTextDraftAction {
                    can_stop: true,
                    keep_on_stop: true,
                    random_id: 777_002,
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Final answer is 42".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                },
            ),
        }),
        date: 1_735_000_020,
    });
    assert!(apply_updates_for_live_session_at(
        &live,
        &upd3,
        base_now + 1_500
    ));
    assert_eq!(live.streamed_draft_messages(bot_peer, None).len(), 1);

    let incoming_final = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: tl_enums::Message::Message(make_tl_message(
                501,
                10_001,
                bot_uid,
                "Final answer is 42!",
            )),
            pts: 10,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_735_000_025,
        seq: 1,
    });
    assert!(apply_updates_for_live_session_at(
        &live,
        &incoming_final,
        base_now + 2_000
    ));
    let adopted = take_adopted_streamed_drafts(&live);
    assert_eq!(adopted.len(), 1);
    assert_eq!(adopted[0].1.random_id, 777_002);
    assert_eq!(adopted[0].1.real_msg_id, MsgId(501));
    assert!(adopted[0].1.adopted_in_place);
    assert!(live.streamed_draft_messages(bot_peer, None).is_empty());

    // 4. Unit verification of 30s auto-expiration and prefix helper.
    assert_eq!(common_prefix_len("abcdef", "abcxyz"), 3);
    let mut local_drafts = HistoryStreamedDrafts::new(bot_peer);
    local_drafts.apply_text_draft(
        None,
        bot_peer,
        1_735_000_000,
        888,
        TextWithEntities::simple("ephemeral"),
        true,
        false,
        10_000,
    );
    assert!(
        local_drafts
            .expire_stale(10_000 + STREAMED_DRAFT_CLEAR_TIMEOUT_MS - 1)
            .is_empty()
    );
    let expired = local_drafts.expire_stale(10_000 + STREAMED_DRAFT_CLEAR_TIMEOUT_MS);
    assert_eq!(expired.len(), 1);
    assert!(local_drafts.is_empty());
}

#[test]
fn wp38_streamed_bot_draft_ui_stop_bar_renders() {
    let (mut live, _fake, bot_peer, _) = make_live_session();
    let bot_uid = bot_peer.bare() as i64;
    let upd = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
            user_id: bot_uid,
            top_msg_id: None,
            action: tl_enums::SendMessageAction::SendMessageTextDraftAction(
                tl_types::SendMessageTextDraftAction {
                    can_stop: true,
                    keep_on_stop: false,
                    random_id: 909_001,
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Streaming UI test...".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                },
            ),
        }),
        date: 1_735_000_050,
    });
    apply_updates_for_live_session_at(&live, &upd, wall_now_ms());

    let ctx = egui::Context::default();
    let pal = Palette::for_mode(ThemeMode::Day);
    let strings = EnglishStrings;
    let mut widget = HistoryWidget::new();
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 560.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            let _ = widget
                .show_with_backend(ui, bot_peer, &mut live, false, false, false, &pal, &strings);
        });
    });

    assert!(
        widget
            .inner()
            .messages()
            .iter()
            .any(|m| m.id.0 < 0 && m.text.text.contains("Streaming UI test")),
        "streamed bot draft bubble should be synchronized into HistoryInnerWidget"
    );
}

// =============================================================================
// 2. Compose Stash (`data_compose_stash` + `StashHintManager` + UI exchange)
// =============================================================================

#[test]
fn wp38_compose_stash_save_exchange_hint_and_ui() {
    let (mut live, fake, _, friend_peer) = make_live_session();
    let key = ComposeStashKey::main(friend_peer);

    // 1. Store & file/forward helpers.
    let mut store = ComposeStashStore::default();
    let rich_stash = ComposeStash {
        draft: RichThreadDraft {
            text: TextWithEntities::simple("Important stashed draft"),
            reply_to: Some(MsgId(42)),
            reply_quote: None,
            no_webpage: false,
        },
        reply_author: Some("Nadia".to_owned()),
        reply_preview: Some("Original message".to_owned()),
        forward: Some(StashedForward {
            from_peer: friend_peer,
            msg_ids: vec![MsgId(10), MsgId(11)],
            options: ForwardOptions::default(),
            summary: "2 messages from Nadia".to_owned(),
        }),
        files: vec![StashedFile {
            name: "spec.pdf".to_owned(),
            path: None,
            size_bytes: 12_345,
            mime_type: "application/pdf".to_owned(),
        }],
    };
    assert!(rich_stash.has_files());
    assert!(rich_stash.has_text());
    assert!(rich_stash.has_content());
    store.set(key, Some(rich_stash.clone()));
    assert!(store.has(key));
    let swapped = store.exchange(
        key,
        Some(ComposeStash {
            draft: RichThreadDraft {
                text: TextWithEntities::simple("Second draft"),
                ..RichThreadDraft::default()
            },
            ..ComposeStash::default()
        }),
    );
    assert_eq!(swapped, Some(rich_stash));
    assert_eq!(
        store.get(key).map(|s| s.draft.text.text.as_str()),
        Some("Second draft")
    );

    // 2. StashHintManager: cut >= 20 chars -> paste same text -> triggers hint up to STASH_HINT_MAX_SHOWS.
    let mut hint = StashHintManager::new();
    let long_text = "This is a twenty-five char message.";
    for show_idx in 0..STASH_HINT_MAX_SHOWS {
        let base_t = u64::from(show_idx) * 10_000;
        assert!(hint.track_change(long_text, true, true, base_t).is_none());
        assert!(hint.track_change("", true, true, base_t + 500).is_none());
        let tip = hint.track_change(long_text, true, true, base_t + 1_500);
        assert!(
            tip.is_some(),
            "hint #{show_idx} should trigger on cut-then-paste"
        );
        assert!(hint.track_change("", false, true, base_t + 2_000).is_none());
    }
    assert!(!hint.should_show_hint());

    // 3. Live + UI exchange flow via `HistoryWidget::exchange_compose_stash`.
    let mut widget = HistoryWidget::new();
    widget.compose_mut().text = "First draft text".to_owned();
    widget
        .compose_mut()
        .rich_edit
        .set_text_with_entities(TextWithEntities::simple("First draft text"));

    // save_rich_draft triggers `messages.saveDraft` RPC when live peer exists.
    fake.enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    assert!(widget.exchange_compose_stash(friend_peer, &mut live, 1.0));
    assert!(widget.compose_mut().text.is_empty());
    assert_eq!(
        live.compose_stash(key).map(|s| s.draft.text.text),
        Some("First draft text".to_owned())
    );

    // Type a second draft and swap back.
    widget.compose_mut().text = "Second draft text".to_owned();
    widget
        .compose_mut()
        .rich_edit
        .set_text_with_entities(TextWithEntities::simple("Second draft text"));
    fake.enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    assert!(widget.exchange_compose_stash(friend_peer, &mut live, 2.0));
    assert_eq!(widget.compose_mut().text, "First draft text");
    assert_eq!(
        live.compose_stash(key).map(|s| s.draft.text.text),
        Some("Second draft text".to_owned())
    );

    // Render HistoryWidget with an active stash to verify the floating corner stash button paints cleanly.
    let ctx = egui::Context::default();
    let pal = Palette::for_mode(ThemeMode::Day);
    let strings = EnglishStrings;
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 560.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            let _ = widget.show_with_backend(
                ui,
                friend_peer,
                &mut live,
                false,
                false,
                false,
                &pal,
                &strings,
            );
        });
    });
}

// =============================================================================
// 3. Read Metrics (`ReadMetricsTracker` + `ReadMetricsBatcher` + `messages.reportReadMetrics`)
// =============================================================================

#[test]
fn wp38_read_metrics_tracker_and_batched_rpc() {
    let (live, fake, _, friend_peer) = make_live_session();
    let mut tracker = ReadMetricsTracker::new(friend_peer);

    // Frame 0 (t=1000): msg 101 (with views) enters viewport [0, 500], height 250 (half visible [0, 125]),
    // msg 102 (no views) ignored, msg 103 (with views) enters but will scroll out before 300ms grace.
    tracker.register_activity(1_000);
    tracker.start_batch(0, 500, 1_000);
    tracker.push(MsgId(101), true, true, -125, 250);
    tracker.push(MsgId(102), true, false, 150, 100);
    tracker.push(MsgId(103), true, true, 300, 100);
    assert!(tracker.end_batch().is_empty());
    assert_eq!(tracker.tracked_count(), 2);

    // Frame 1 (t=1150): msg 103 leaves before 300ms entry grace -> dropped immediately.
    tracker.start_batch(0, 500, 1_150);
    tracker.push(MsgId(101), true, true, 0, 250);
    assert!(tracker.end_batch().is_empty());
    assert_eq!(tracker.tracked_count(), 1);

    // Frame 2 (t=1400): past 300ms entry grace -> msg 101 gets non-zero view_id and accumulates 400ms.
    tracker.start_batch(0, 500, 1_400);
    tracker.push(MsgId(101), true, true, 0, 250);
    assert!(tracker.end_batch().is_empty());

    // Pause app from t=2000 to t=5000 (3000ms paused must NOT be counted).
    tracker.set_app_active(false, 2_000);
    tracker.set_app_active(true, 5_000);

    // Finalize at t=6000 -> total_in_view should be 1000ms (1000..2000) + 1000ms (5000..6000) = 2000ms.
    let finalized = tracker.finalize_all(6_000);
    assert_eq!(finalized.len(), 1);
    let m = finalized[0];
    assert_eq!(m.msg_id, MsgId(101));
    assert_ne!(m.view_id, 0);
    assert_eq!(m.time_in_view_ms, 2_000);
    assert_eq!(m.active_time_in_view_ms, 2_000);
    assert_eq!(m.height_to_viewport_ratio_permille, 500);
    assert_eq!(m.seen_range_ratio_permille, 1_000);

    // Verify 5-minute cap + activity timeout math in a second tracker.
    let mut cap_tracker = ReadMetricsTracker::new(friend_peer);
    cap_tracker.register_activity(10_000);
    cap_tracker.start_batch(0, 500, 10_000);
    cap_tracker.push(MsgId(202), true, true, 0, 200);
    let _ = cap_tracker.end_batch();
    let _ = cap_tracker.tick(10_000 + READ_METRICS_GRACE_PERIOD_MS);
    let capped = cap_tracker.tick(10_000 + READ_METRICS_MAX_TRACKING_DURATION_MS + 1_000);
    assert_eq!(capped.len(), 1);
    assert_eq!(
        capped[0].time_in_view_ms as u64,
        READ_METRICS_MAX_TRACKING_DURATION_MS
    );
    assert_eq!(
        capped[0].active_time_in_view_ms as u64,
        READ_METRICS_ACTIVITY_TIMEOUT_MS
    );

    // Batcher respects 5s delay and invokes `messages.reportReadMetrics` with exact TL payload.
    let mut batcher = ReadMetricsBatcher::new();
    batcher.add(friend_peer, finalized.clone(), 6_000);
    assert!(
        batcher
            .drain_ready(6_000 + READ_METRICS_SEND_TIMEOUT_MS - 1, false)
            .is_empty()
    );
    assert_eq!(
        batcher
            .drain_ready(6_000 + READ_METRICS_SEND_TIMEOUT_MS, false)
            .len(),
        1
    );

    report_read_metrics_live_at(&live, friend_peer, finalized, 10_000);
    fake.enqueue_ok(tl_fns::messages::ReportReadMetrics::CONSTRUCTOR_ID, &true);
    let sent = flush_read_metrics_live_at(&live, 10_000 + READ_METRICS_SEND_TIMEOUT_MS, false);
    assert_eq!(sent, 1);

    let reqs = fake.recorded_of(tl_fns::messages::ReportReadMetrics::CONSTRUCTOR_ID);
    assert_eq!(reqs.len(), 1);
    let parsed =
        tl_fns::messages::ReportReadMetrics::from_bytes(&reqs[0]).expect("deser reportReadMetrics");
    assert_eq!(parsed.metrics.len(), 1);
    let tl_enums::InputMessageReadMetric::InputMessageReadMetric(ref im) = parsed.metrics[0];
    assert_eq!(im.msg_id, 101);
    assert_eq!(im.time_in_view_ms, 2_000);
    assert_eq!(im.height_to_viewport_ratio_permille, 500);
    assert_eq!(im.seen_range_ratio_permille, 1_000);
}

// =============================================================================
// 4. Emoji Interactions (`history_view_emoji_interactions` + JSON + live sync)
// =============================================================================

#[test]
fn wp38_emoji_interactions_json_sync_and_overlay() {
    let (mut live, fake, _, friend_peer) = make_live_session();
    let friend_uid = friend_peer.bare() as i64;

    // 1. JSON serialization & strict tdesktop validation.
    let bunch = EmojiInteractionsBunch {
        interactions: vec![
            EmojiInteractionSingle {
                index: 1,
                time: 0.0,
            },
            EmojiInteractionSingle {
                index: 3,
                time: 0.35,
            },
        ],
    };
    let json = interactions_to_json(&bunch);
    assert_eq!(parse_interactions_json(&json), bunch);
    assert!(
        parse_interactions_json(r#"{"v":2,"a":[{"i":1,"t":0.0}]}"#)
            .interactions
            .is_empty()
    );
    assert!(
        parse_interactions_json(r#"{"v":1,"a":[{"i":11,"t":0.0}]}"#)
            .interactions
            .is_empty()
    );
    let mono =
        parse_interactions_json(r#"{"v":1,"a":[{"i":1,"t":0.4},{"i":2,"t":0.2},{"i":3,"t":0.8}]}"#);
    assert_eq!(mono.interactions.len(), 2);
    assert_eq!(mono.interactions[1].index, 3);
    assert_eq!(choose_interaction_emoji("❤️"), Some("❤"));
    assert_eq!(choose_interaction_emoji("🎆"), Some("🎆"));
    assert_eq!(choose_interaction_emoji("not emoji"), None);

    // 2. Outgoing taps accumulate for 1000ms then send `messages.setTyping(SendMessageEmojiInteraction)`.
    let mut coord = EmojiInteractionCoordinator::new();
    let p1 = coord.start_outgoing(friend_peer, MsgId(77), "🎆", 1_000);
    assert_eq!(p1.len(), 1);
    let p2 = coord.start_outgoing(friend_peer, MsgId(77), "🎆", 1_300);
    assert_eq!(p2.len(), 1);
    assert_ne!(
        p1[0].variant_index, p2[0].variant_index,
        "consecutive taps should avoid repeating the same variant"
    );
    assert!(coord.poll_outgoing_batches(1_900).is_empty());
    let batches = coord.poll_outgoing_batches(1_000 + EMOJI_INTERACTION_ACCUMULATE_DELAY_MS);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].bunch.interactions.len(), 2);

    fake.enqueue_ok(tl_fns::messages::SetTyping::CONSTRUCTOR_ID, &true);
    live.send_emoji_interaction(
        batches[0].peer_id,
        batches[0].msg_id,
        &batches[0].emoticon,
        &batches[0].bunch,
    )
    .expect("send emoji interaction");

    // 3. Incoming `SendMessageEmojiInteraction` & `SendMessageEmojiInteractionSeen` updates.
    let incoming_upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
                user_id: friend_uid,
                top_msg_id: None,
                action: tl_enums::SendMessageAction::SendMessageEmojiInteraction(
                    tl_types::SendMessageEmojiInteraction {
                        emoticon: "🎆".to_owned(),
                        msg_id: 77,
                        interaction: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                            data: json.clone(),
                        }),
                    },
                ),
            }),
            tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
                user_id: friend_uid,
                top_msg_id: None,
                action: tl_enums::SendMessageAction::SendMessageEmojiInteractionSeen(
                    tl_types::SendMessageEmojiInteractionSeen {
                        emoticon: "🎆".to_owned(),
                    },
                ),
            }),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_735_000_200,
        seq: 2,
    });
    assert!(apply_updates_for_live_session_at(
        &live,
        &incoming_upd,
        2_200
    ));

    let drained = live.drain_incoming_emoji_interactions(friend_peer);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].msg_id, MsgId(77));
    assert_eq!(drained[0].bunch.interactions.len(), 2);

    let seen_list = live.drain_emoji_interaction_seen(friend_peer);
    assert_eq!(seen_list, vec!["🎆".to_owned()]);
    assert!(coord.accept_seen_outgoing(friend_peer, "🎆", 2_500));
    assert!(!coord.accept_seen_outgoing(
        friend_peer,
        "🎆",
        2_000 + EMOJI_INTERACTION_ACCEPT_SEEN_MS + 100
    ));

    // Send seen ack RPC.
    fake.enqueue_ok(tl_fns::messages::SetTyping::CONSTRUCTOR_ID, &true);
    live.send_emoji_interaction_seen(friend_peer, "🎆")
        .expect("send emoji seen");
    let typing_reqs = fake.recorded_of(tl_fns::messages::SetTyping::CONSTRUCTOR_ID);
    assert_eq!(typing_reqs.len(), 2);
    let second = tl_fns::messages::SetTyping::from_bytes(&typing_reqs[1]).expect("deser seen");
    assert!(matches!(
        second.action,
        tl_enums::SendMessageAction::SendMessageEmojiInteractionSeen(ref a) if a.emoticon == "🎆"
    ));

    // 4. Overlay playback & painting test.
    let mut overlay = HistoryViewEmojiInteractions::new();
    overlay.play(
        MsgId(77),
        "🎆".to_owned(),
        Pos2::new(200.0, 200.0),
        true,
        0,
        1_000,
    );
    assert_eq!(overlay.active_count(), 1);
    let ctx = egui::Context::default();
    let colors =
        tg_app::history::theme::HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        overlay.paint_overlays(
            ui,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)),
            1_300,
            &colors,
        );
    });
    assert_eq!(overlay.active_count(), 1);

    // Eligibility check for HistoryMessage.
    let sample_msg = tg_app::history::backend::HistoryMessage {
        id: MsgId(77),
        peer_id: friend_peer,
        from_id: friend_peer,
        sender_name: "Nadia".to_owned(),
        date_unix: 1_735_000_000,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple("🎆"),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: Default::default(),
    };
    assert_eq!(
        can_play_emoji_interaction_message(&sample_msg, PeerKind::User),
        Some("🎆")
    );
    assert_eq!(
        can_play_emoji_interaction_message(&sample_msg, PeerKind::Group),
        None
    );
}

// =============================================================================
// 5. 1:1 No-Forwards Request (`history_view_no_forwards_request`)
// =============================================================================

#[test]
fn wp38_no_forwards_request_card_and_toggle_rpc() {
    let (mut live, fake, _, friend_peer) = make_live_session();
    let friend_dp = DataPeerId::from_user(DataUserId(friend_peer.bare()));

    // Insert an incoming `NoForwardsRequest` service message into session.
    live.runtime().tokio_handle().block_on(async {
        let mut s = live.runtime().session().write().await;
        let msg = Message::new_service(
            friend_dp,
            DataMsgId(601),
            Some(friend_dp),
            1_735_000_300,
            DataServiceAction::NoForwardsRequest {
                expired: false,
                prev_value: true,
                new_value: false,
            },
        );
        s.insert_message(msg);
    });

    // Verify conversion from `tg_data::ServiceAction::NoForwardsRequest` -> `UiServiceAction::NoForwardsRequest`.
    let slice = live.load_slice_around(friend_peer, MsgId::SHOW_AT_END, 20, 20);
    let svc_msg = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(601))
        .expect("no-forwards request message present");
    assert!(matches!(
        svc_msg.service,
        Some(UiServiceAction::NoForwardsRequest {
            expired: false,
            prev_value: true,
            new_value: false,
        })
    ));

    // Verify card formatting & dimensions.
    assert_eq!(NO_FORWARDS_REQUEST_FEATURES.len(), 3);
    assert!(should_render_no_forwards_request_card(false, false));
    assert!(!should_render_no_forwards_request_card(true, false));
    assert!(should_render_no_forwards_request_card(true, true));
    assert!(has_no_forwards_request_buttons(false, false, false));
    assert!(!has_no_forwards_request_buttons(false, true, false));
    assert_eq!(
        measure_no_forwards_request_card(false, false, false),
        (276, 154)
    );
    assert!(format_no_forwards_request_title("Nadia", false).contains("Nadia"));
    assert!(format_no_forwards_request_expired_text().contains("expired"));
    assert!(format_no_forwards_toggle_text("Nadia", true, true).contains("disabled sharing"));

    // Render HistoryWidget with the NoForwardsRequest card to verify layout + painting.
    let ctx = egui::Context::default();
    let pal = Palette::for_mode(ThemeMode::Day);
    let strings = EnglishStrings;
    let mut widget = HistoryWidget::new();
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 560.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            let _ = widget.show_with_backend(
                ui,
                friend_peer,
                &mut live,
                false,
                false,
                false,
                &pal,
                &strings,
            );
        });
    });

    // Accept the request -> sends `messages.toggleNoForwards { enabled: false, request_msg_id: Some(601) }`.
    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_735_000_310,
        seq: 3,
    });
    fake.enqueue_ok(
        tl_fns::messages::ToggleNoForwards::CONSTRUCTOR_ID,
        &empty_updates,
    );
    live.toggle_peer_no_forwards(friend_peer, false, Some(MsgId(601)))
        .expect("accept no-forwards request");
    assert!(live.is_no_forwards_request_acted(friend_peer, MsgId(601)));
    assert!(!live.peer_no_forwards_enabled(friend_peer));

    // Toggle protection ON at peer level (`request_msg_id: None`).
    fake.enqueue_ok(
        tl_fns::messages::ToggleNoForwards::CONSTRUCTOR_ID,
        &empty_updates,
    );
    live.toggle_peer_no_forwards(friend_peer, true, None)
        .expect("enable no-forwards");
    assert!(live.peer_no_forwards_enabled(friend_peer));

    let reqs = fake.recorded_of(tl_fns::messages::ToggleNoForwards::CONSTRUCTOR_ID);
    assert_eq!(reqs.len(), 2);
    let req_accept =
        tl_fns::messages::ToggleNoForwards::from_bytes(&reqs[0]).expect("deser accept");
    assert!(!req_accept.enabled);
    assert_eq!(req_accept.request_msg_id, Some(601));
    let req_toggle =
        tl_fns::messages::ToggleNoForwards::from_bytes(&reqs[1]).expect("deser toggle");
    assert!(req_toggle.enabled);
    assert_eq!(req_toggle.request_msg_id, None);
}

// =============================================================================
// 6. Media Toggling with `FILE_REFERENCE_*` Auto-Refresh (`api_toggling_media`)
// =============================================================================

#[test]
fn wp38_api_toggling_media_file_reference_refresh_and_retry() {
    let (mut live, fake, _, friend_peer) = make_live_session();
    let friend_dp = DataPeerId::from_user(DataUserId(friend_peer.bare()));
    let friend_uid = friend_peer.bare() as i64;

    // Seed three messages in session: GIF (801), Sticker (802), Audio ringtone (803), all with stale file_reference.
    live.runtime().tokio_handle().block_on(async {
        let mut s = live.runtime().session().write().await;

        let gif_doc = DocumentData {
            id: DocumentId(91_001),
            access_hash: 55_001,
            file_reference: vec![0x01, 0x02],
            mime_type: "video/mp4".to_owned(),
            kind: DocumentKind::Gif,
            ..DocumentData::default()
        };
        let mut m_gif = Message::new_text(
            friend_dp,
            DataMsgId(801),
            Some(friend_dp),
            1_735_000_400,
            DataText::default(),
        );
        m_gif.content = MessageContent::Regular {
            text: DataText::default(),
            media: MessageMedia::Document(Box::new(gif_doc)),
        };
        s.insert_message(m_gif);

        let st_doc = DocumentData {
            id: DocumentId(91_002),
            access_hash: 55_002,
            file_reference: vec![0x03, 0x04],
            mime_type: "application/x-tgsticker".to_owned(),
            kind: DocumentKind::Sticker,
            sticker: Some(StickerMeta {
                alt: "✨".to_owned(),
                set_id: 7_001,
                set_access_hash: 8_001,
                is_mask: false,
            }),
            ..DocumentData::default()
        };
        let mut m_st = Message::new_text(
            friend_dp,
            DataMsgId(802),
            Some(friend_dp),
            1_735_000_401,
            DataText::default(),
        );
        m_st.content = MessageContent::Regular {
            text: DataText::default(),
            media: MessageMedia::Document(Box::new(st_doc)),
        };
        s.insert_message(m_st);

        let aud_doc = DocumentData {
            id: DocumentId(91_003),
            access_hash: 55_003,
            file_reference: vec![0x05, 0x06],
            mime_type: "audio/ogg".to_owned(),
            kind: DocumentKind::Audio,
            ..DocumentData::default()
        };
        let mut m_aud = Message::new_text(
            friend_dp,
            DataMsgId(803),
            Some(friend_dp),
            1_735_000_402,
            DataText::default(),
        );
        m_aud.content = MessageContent::Regular {
            text: DataText::default(),
            media: MessageMedia::Document(Box::new(aud_doc)),
        };
        s.insert_message(m_aud);
    });

    let make_refreshed_doc_msg = |msg_id: i32, doc_id: i64, hash: i64, new_ref: Vec<u8>| {
        let mut tl_m = make_tl_message(msg_id, 10_001, friend_uid, "");
        tl_m.media = Some(tl_enums::MessageMedia::Document(
            tl_types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: false,
                document: Some(tl_enums::Document::Document(tl_types::Document {
                    id: doc_id,
                    access_hash: hash,
                    file_reference: new_ref,
                    date: 1_735_000_400,
                    mime_type: "video/mp4".to_owned(),
                    size: 1024,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: Vec::new(),
                })),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
            },
        ));
        tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![tl_enums::Message::Message(tl_m)],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        })
    };

    // 1. Save GIF: fails with FILE_REFERENCE_EXPIRED -> refreshes via messages.getMessages -> retries with fresh ref.
    fake.enqueue_err(
        tl_fns::messages::SaveGif::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "FILE_REFERENCE_EXPIRED".to_owned(),
            value: None,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetMessages::CONSTRUCTOR_ID,
        &make_refreshed_doc_msg(801, 91_001, 55_001, vec![0xAA, 0xBB]),
    );
    fake.enqueue_ok(tl_fns::messages::SaveGif::CONSTRUCTOR_ID, &true);

    assert!(
        live.toggle_saved_gif_from_message(friend_peer, MsgId(801), true)
            .expect("save gif after file_ref refresh")
    );
    let gif_reqs = fake.recorded_of(tl_fns::messages::SaveGif::CONSTRUCTOR_ID);
    assert_eq!(gif_reqs.len(), 2);
    let retry_gif = tl_fns::messages::SaveGif::from_bytes(&gif_reqs[1]).expect("deser retry gif");
    assert!(!retry_gif.unsave);
    assert!(matches!(
        retry_gif.id,
        tl_enums::InputDocument::InputDocument(ref d) if d.file_reference == vec![0xAA, 0xBB]
    ));

    // 2. Fave Sticker: stale ref -> refresh -> retry succeeds.
    fake.enqueue_err(
        tl_fns::messages::FaveSticker::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "FILE_REFERENCE_0_EXPIRED".to_owned(),
            value: None,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetMessages::CONSTRUCTOR_ID,
        &make_refreshed_doc_msg(802, 91_002, 55_002, vec![0xCC, 0xDD]),
    );
    fake.enqueue_ok(tl_fns::messages::FaveSticker::CONSTRUCTOR_ID, &true);

    assert!(
        live.toggle_faved_sticker_from_message(friend_peer, MsgId(802), true)
            .expect("fave sticker after refresh")
    );
    let fave_reqs = fake.recorded_of(tl_fns::messages::FaveSticker::CONSTRUCTOR_ID);
    assert_eq!(fave_reqs.len(), 2);
    let retry_fave =
        tl_fns::messages::FaveSticker::from_bytes(&fave_reqs[1]).expect("deser retry fave");
    assert!(!retry_fave.unfave);
    assert!(matches!(
        retry_fave.id,
        tl_enums::InputDocument::InputDocument(ref d) if d.file_reference == vec![0xCC, 0xDD]
    ));

    // 3. Recent Sticker: direct unsave call (`unsave: true`).
    fake.enqueue_ok(tl_fns::messages::SaveRecentSticker::CONSTRUCTOR_ID, &true);
    assert!(
        live.toggle_recent_sticker_from_message(friend_peer, MsgId(802), false)
            .expect("unsave recent sticker")
    );
    let recent_reqs = fake.recorded_of(tl_fns::messages::SaveRecentSticker::CONSTRUCTOR_ID);
    assert_eq!(recent_reqs.len(), 1);
    let parsed_recent =
        tl_fns::messages::SaveRecentSticker::from_bytes(&recent_reqs[0]).expect("deser recent");
    assert!(parsed_recent.unsave);

    // 4. Save Ringtone: stale ref -> refresh -> retry succeeds.
    fake.enqueue_err(
        tl_fns::account::SaveRingtone::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "FILE_REFERENCE_EXPIRED".to_owned(),
            value: None,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetMessages::CONSTRUCTOR_ID,
        &make_refreshed_doc_msg(803, 91_003, 55_003, vec![0xEE, 0xFF]),
    );
    fake.enqueue_ok(
        tl_fns::account::SaveRingtone::CONSTRUCTOR_ID,
        &tl_enums::account::SavedRingtone::SavedRingtone,
    );
    assert!(
        live.toggle_saved_ringtone_from_message(friend_peer, MsgId(803), true)
            .expect("save ringtone after refresh")
    );
    let ring_reqs = fake.recorded_of(tl_fns::account::SaveRingtone::CONSTRUCTOR_ID);
    assert_eq!(ring_reqs.len(), 2);
    let retry_ring =
        tl_fns::account::SaveRingtone::from_bytes(&ring_reqs[1]).expect("deser retry ringtone");
    assert!(!retry_ring.unsave);
    assert!(matches!(
        retry_ring.id,
        tl_enums::InputDocument::InputDocument(ref d) if d.file_reference == vec![0xEE, 0xFF]
    ));
}
