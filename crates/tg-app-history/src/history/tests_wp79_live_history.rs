//! WP-79: Live MTProto updates (`updateShortMessage`, `updateNewMessage`,
//! `updateNewChannelMessage`, `updateEditMessage`, `updateEditChannelMessage`,
//! `updateDeleteMessages`, `updateDeleteChannelMessages`, `updateReadHistoryOutbox`)
//! pushed over `FakeRpcClient` while a chat is ALREADY OPEN must append/edit/delete
//! bubbles in `HistoryWidget` without re-opening the chat, and mark incoming messages
//! read when visible and focused (matching tdesktop's `History::addNewMessage` /
//! `HistoryWidget::newItemAdded`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{CentralPanel, Context, Frame, RawInput, Rect, pos2, vec2};
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_live::convert::from_data_peer;
use tg_app_live::session::LiveSession;
use tg_data::{
    Channel, ChannelFlags, ChannelId, PeerId as DataPeerId, PeerUpdateFlags, User,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::history::HistoryWidget;

type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    recorded: RecordedCalls,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            recorded: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    fn recorded_of<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(ctor, _)| *ctor == R::CONSTRUCTOR_ID)
            .filter_map(|(_, bytes)| R::from_bytes(bytes).ok())
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
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let popped = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
        };
        match popped {
            Some(Ok(raw)) => {
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            Some(Err(err)) => Err(err),
            None => Err(InvocationError::Dropped),
        }
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn tl_text_message(
    id: i32,
    peer_id: tl_enums::Peer,
    from_id: Option<tl_enums::Peer>,
    out: bool,
    date: i32,
    text: &str,
    edit_date: Option<i32>,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out,
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
        from_id,
        from_boosts_applied: None,
        from_rank: None,
        peer_id,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date,
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
    })
}

fn render_frame(
    ctx: &Context,
    widget: &mut HistoryWidget,
    live: &mut LiveSession,
    peer: PeerId,
    time: f64,
    focused: bool,
) {
    let palette = Palette::day();
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(720.0, 520.0))),
        time: Some(time),
        focused,
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let _ = widget.show_with_backend(
                ui,
                peer,
                live,
                true,
                false,
                false,
                &palette,
                &EnglishStrings,
            );
        });
    });
    out.textures_delta.clear();
}

fn wait_for_generation_above(live: &LiveSession, baseline: u64) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while live.bridge_state().generation() <= baseline {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for LiveRuntimeBridge generation to advance past {baseline}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn start_live_fixture(
    initial_pts: i32,
) -> (
    tokio::runtime::Runtime,
    FakeRpcClient,
    LiveSession,
    PeerId,
    PeerId,
) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeRpcClient::new();
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: initial_pts,
            qts: 0,
            date: 1_795_341_600,
            seq: 1,
            unread_count: 0,
        }),
    );

    let runtime = SessionRuntime::start_on(
        DataUserId(100),
        ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            fetch_self_user_on_start: false,
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    rt.block_on(runtime.wait_ready())
        .expect("SessionRuntime ready");

    let live = LiveSession::new(Arc::new(runtime), 1);
    let user_a_id = DataUserId(301);
    let channel_c_id = ChannelId(601);
    let user_a_data = DataPeerId::from_user(user_a_id);
    let channel_c_data = DataPeerId::from_channel(channel_c_id);

    live.write_session(|s| {
        let mut me = User::new(DataUserId(100), true);
        me.first_name = "Account B".to_owned();
        me.common.name = "Account B".to_owned();
        me.common.loaded_status = tg_data::LoadedStatus::Full;
        s.upsert_user(me, PeerUpdateFlags::empty());

        let mut peer_a = User::new(user_a_id, false);
        peer_a.first_name = "Account A".to_owned();
        peer_a.common.name = "Account A".to_owned();
        peer_a.common.loaded_status = tg_data::LoadedStatus::Full;
        peer_a.access_hash = 11_111;
        s.upsert_user(peer_a, PeerUpdateFlags::empty());

        let mut ch = Channel::new(channel_c_id);
        ch.common.name = "Test Megagroup".to_owned();
        ch.common.loaded_status = tg_data::LoadedStatus::Full;
        ch.access_hash = 22_222;
        ch.flags = ChannelFlags::MEGAGROUP | ChannelFlags::CREATOR;
        s.upsert_channel(ch, PeerUpdateFlags::empty());

        s.set_history_unread_count(user_a_data, 0);
        s.set_history_unread_count(channel_c_data, 0);
    });

    (
        rt,
        rpc,
        live,
        from_data_peer(user_a_data),
        from_data_peer(channel_c_data),
    )
}

#[test]
fn wp79_open_chat_appends_live_short_and_new_messages_edits_deletes_and_marks_read() {
    let (_rt, rpc, mut live, peer_a, _channel_c) = start_live_fixture(100);
    let peer_a_tl = tl_enums::Peer::User(tl_types::PeerUser { user_id: 301 });
    let self_tl = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });

    // Initial history returned when B opens A's chat: outgoing #9 (unread by A) + incoming #10.
    rpc.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                tl_text_message(
                    10,
                    peer_a_tl.clone(),
                    Some(peer_a_tl.clone()),
                    false,
                    1_795_341_610,
                    "Initial incoming message",
                    None,
                ),
                tl_text_message(
                    9,
                    peer_a_tl.clone(),
                    Some(self_tl),
                    true,
                    1_795_341_605,
                    "Initial outgoing message",
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let ctx = Context::default();
    let mut widget = HistoryWidget::new();

    // Open A's chat and paint two frames so layout and scroll settle.
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.05, true);

    let initial_ids: Vec<MsgId> = widget.inner_mut().messages().iter().map(|m| m.id).collect();
    assert_eq!(initial_ids, vec![MsgId(9), MsgId(10)]);
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(10))
            .is_some(),
        "initial message #10 should be painted"
    );

    // 1. Push live `updateShortMessage` (#11) while A's chat is ALREADY OPEN.
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 101,
                pts_count: 0,
            },
        ),
    );
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 11,
            user_id: 301,
            message: "Live short message from A".to_owned(),
            pts: 101,
            pts_count: 1,
            date: 1_795_341_620,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));
    wait_for_generation_above(&live, gen_before);

    // Render without closing or switching the open chat.
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.05, true);

    assert!(
        widget
            .inner_mut()
            .messages()
            .iter()
            .any(|m| m.id == MsgId(11) && m.text.text == "Live short message from A"),
        "open history must append incoming updateShortMessage #11 without re-opening"
    );
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(11))
            .is_some(),
        "incoming message #11 bubble must be painted in the open chat"
    );
    let read_calls = rpc.recorded_of::<tl_fns::messages::ReadHistory>();
    assert!(
        read_calls.iter().any(|r| r.max_id >= 11),
        "visible incoming message #11 must trigger messages.readHistory(max_id >= 11), got {read_calls:?}"
    );
    assert_eq!(
        tg_app_core::history::backend::HistoryBackend::peer_info(&live, peer_a)
            .map(|p| p.unread_count),
        Some(0),
        "unread_count must be cleared after marking visible message #11 read"
    );

    // 2. Push live `updateNewMessage` (#12) while A's chat is STILL OPEN.
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 102,
                pts_count: 0,
            },
        ),
    );
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: tl_text_message(
                12,
                peer_a_tl.clone(),
                Some(peer_a_tl.clone()),
                false,
                1_795_341_625,
                "Second live message via NewMessage",
                None,
            ),
            pts: 102,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_625,
        seq: 2,
    }));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.05, true);

    assert!(
        widget
            .inner_mut()
            .messages()
            .iter()
            .any(|m| m.id == MsgId(12) && m.text.text == "Second live message via NewMessage"),
        "open history must append incoming updateNewMessage #12 without re-opening"
    );
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(12))
            .is_some(),
        "incoming message #12 bubble must be painted"
    );
    assert!(
        rpc.recorded_of::<tl_fns::messages::ReadHistory>()
            .iter()
            .any(|r| r.max_id >= 12),
        "visible incoming message #12 must advance messages.readHistory(max_id >= 12)"
    );

    // 3. Push live `updateEditMessage` on #11 while chat is open.
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
            message: tl_text_message(
                11,
                peer_a_tl.clone(),
                Some(peer_a_tl.clone()),
                false,
                1_795_341_620,
                "Edited live message",
                Some(1_795_341_630),
            ),
            pts: 103,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_630,
        seq: 3,
    }));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 4.0, true);

    let edited_11 = widget
        .inner_mut()
        .messages()
        .iter()
        .find(|m| m.id == MsgId(11))
        .expect("message #11 still present after edit");
    assert_eq!(edited_11.text.text, "Edited live message");
    assert!(edited_11.edited, "edited flag must reflect live edit");

    // 4. Push live `updateDeleteMessages` removing #10 while chat is open.
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DeleteMessages(
            tl_types::UpdateDeleteMessages {
                messages: vec![10],
                pts: 104,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_635,
        seq: 4,
    }));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 5.0, true);

    let remaining_ids: Vec<MsgId> = widget.inner_mut().messages().iter().map(|m| m.id).collect();
    assert_eq!(
        remaining_ids,
        vec![MsgId(9), MsgId(11), MsgId(12)],
        "live updateDeleteMessages must remove #10 from open history"
    );

    // 5. Push live `updateReadHistoryOutbox` marking outgoing #9 read by A.
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::ReadHistoryOutbox(
            tl_types::UpdateReadHistoryOutbox {
                peer: peer_a_tl,
                max_id: 9,
                pts: 105,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_640,
        seq: 5,
    }));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 6.0, true);

    let msg_9 = widget
        .inner_mut()
        .messages()
        .iter()
        .find(|m| m.id == MsgId(9))
        .expect("outgoing message #9 present");
    assert!(
        msg_9.is_read,
        "live updateReadHistoryOutbox must mark outgoing #9 read in open history"
    );
}

#[test]
fn wp79_open_channel_appends_new_channel_message_edit_and_delete() {
    let (_rt, rpc, mut live, _peer_a, channel_c) = start_live_fixture(100);
    let ch_tl = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 601 });
    let sender_tl = tl_enums::Peer::User(tl_types::PeerUser { user_id: 301 });

    rpc.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 50,
            offset_id_offset: None,
            count: 1,
            messages: vec![tl_text_message(
                20,
                ch_tl.clone(),
                Some(sender_tl.clone()),
                false,
                1_795_341_600,
                "Channel initial",
                None,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, channel_c, 1.0, true);
    render_frame(&ctx, &mut widget, &mut live, channel_c, 1.05, true);
    assert_eq!(widget.inner_mut().messages().len(), 1);

    // Push `updateNewChannelMessage` (#21).
    rpc.enqueue_ok(tl_fns::channels::ReadHistory::CONSTRUCTOR_ID, &true);
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewChannelMessage(
            tl_types::UpdateNewChannelMessage {
                message: tl_text_message(
                    21,
                    ch_tl.clone(),
                    Some(sender_tl.clone()),
                    false,
                    1_795_341_610,
                    "Channel live message",
                    None,
                ),
                pts: 51,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_610,
        seq: 2,
    }));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, channel_c, 2.0, true);
    render_frame(&ctx, &mut widget, &mut live, channel_c, 2.05, true);

    assert!(
        widget
            .inner_mut()
            .messages()
            .iter()
            .any(|m| m.id == MsgId(21) && m.text.text == "Channel live message"),
        "open channel history must append updateNewChannelMessage #21"
    );
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(21))
            .is_some(),
        "channel message #21 bubble must be painted"
    );
    assert!(
        rpc.recorded_of::<tl_fns::channels::ReadHistory>()
            .iter()
            .any(|r| r.max_id >= 21),
        "visible channel message #21 must trigger channels.readHistory"
    );

    // Edit #21 via `updateEditChannelMessage`.
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::EditChannelMessage(
            tl_types::UpdateEditChannelMessage {
                message: tl_text_message(
                    21,
                    ch_tl,
                    Some(sender_tl),
                    false,
                    1_795_341_610,
                    "Channel live message edited",
                    Some(1_795_341_620),
                ),
                pts: 52,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_620,
        seq: 3,
    }));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, channel_c, 3.0, true);
    assert_eq!(
        widget
            .inner_mut()
            .messages()
            .iter()
            .find(|m| m.id == MsgId(21))
            .map(|m| m.text.text.as_str()),
        Some("Channel live message edited")
    );

    // Delete #20 via `updateDeleteChannelMessages`.
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DeleteChannelMessages(
            tl_types::UpdateDeleteChannelMessages {
                channel_id: 601,
                messages: vec![20],
                pts: 53,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_625,
        seq: 4,
    }));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, channel_c, 4.0, true);
    let ids: Vec<MsgId> = widget.inner_mut().messages().iter().map(|m| m.id).collect();
    assert_eq!(ids, vec![MsgId(21)]);
}

#[test]
fn wp79_unfocused_open_chat_appends_without_marking_read_until_focused() {
    let (_rt, rpc, mut live, peer_a, _channel_c) = start_live_fixture(100);
    let peer_a_tl = tl_enums::Peer::User(tl_types::PeerUser { user_id: 301 });

    rpc.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![tl_text_message(
                10,
                peer_a_tl.clone(),
                Some(peer_a_tl),
                false,
                1_795_341_610,
                "Old message",
                None,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    // Open chat while unfocused.
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.0, false);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.05, false);

    // Push incoming message #11 while window is unfocused.
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 11,
            user_id: 301,
            message: "Arrived while unfocused".to_owned(),
            pts: 101,
            pts_count: 1,
            date: 1_795_341_620,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.0, false);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.05, false);

    assert!(
        widget
            .inner_mut()
            .messages()
            .iter()
            .any(|m| m.id == MsgId(11) && m.text.text == "Arrived while unfocused"),
        "incoming message must still append & paint while unfocused"
    );
    assert!(
        rpc.recorded_of::<tl_fns::messages::ReadHistory>()
            .is_empty(),
        "unfocused window must NOT mark newly arrived messages read"
    );

    // Now focus the window -> message #11 is visible and immediately marked read.
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 101,
                pts_count: 0,
            },
        ),
    );
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.0, true);
    assert!(
        rpc.recorded_of::<tl_fns::messages::ReadHistory>()
            .iter()
            .any(|r| r.max_id >= 11),
        "focusing window with visible unread message #11 must mark it read"
    );
}

#[test]
fn wp79_scrolled_up_open_chat_appends_without_yanking_scroll_or_marking_offscreen_read() {
    let (_rt, rpc, mut live, peer_a, _channel_c) = start_live_fixture(100);
    let peer_a_tl = tl_enums::Peer::User(tl_types::PeerUser { user_id: 301 });

    // Seed 35 messages so the list overflows the 520px viewport and can scroll up.
    let initial_msgs: Vec<tl_enums::Message> = (1..=35)
        .rev()
        .map(|id| {
            tl_text_message(
                id,
                peer_a_tl.clone(),
                Some(peer_a_tl.clone()),
                false,
                1_795_341_000 + id * 10,
                &format!("Historical message #{id} with enough text to occupy bubble height"),
                None,
            )
        })
        .collect();
    rpc.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: initial_msgs,
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.05, true);

    // Scroll all the way to the top (away from the bottom).
    widget.inner_mut().set_scroll_offset(0.0);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.1, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.15, true);
    assert!(
        widget.inner_mut().show_scroll_down_button(),
        "chat should be scrolled up away from bottom"
    );
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(35))
            .is_none(),
        "bottom message #35 should be offscreen when scrolled to top"
    );

    // Push incoming #36 while user is reading near the top.
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 36,
            user_id: 301,
            message: "New message while scrolled up".to_owned(),
            pts: 101,
            pts_count: 1,
            date: 1_795_342_000,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.05, true);

    // Message #36 is appended to the model, scroll stays up, and #36 is NOT marked read yet.
    assert!(
        widget
            .inner_mut()
            .messages()
            .iter()
            .any(|m| m.id == MsgId(36) && m.text.text == "New message while scrolled up"),
        "incoming message #36 must be appended to model while scrolled up"
    );
    assert!(
        widget.inner_mut().show_scroll_down_button(),
        "incoming message must NOT yank scrolled-up viewport down to the bottom"
    );
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(36))
            .is_none(),
        "incoming message #36 must remain offscreen until user scrolls down"
    );
    assert!(
        rpc.recorded_of::<tl_fns::messages::ReadHistory>()
            .iter()
            .all(|r| r.max_id < 36),
        "offscreen incoming message #36 must NOT be marked read while scrolled up"
    );
    assert_eq!(
        widget.inner_mut().unread_count(),
        1,
        "unread count badge should reflect 1 unread message while scrolled up"
    );

    // Now scroll back to the bottom -> #36 becomes visible and is marked read!
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 101,
                pts_count: 0,
            },
        ),
    );
    widget.request_scroll_to_bottom(peer_a);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.05, true);

    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(36))
            .is_some(),
        "message #36 must be painted after scrolling to bottom"
    );
    assert!(
        rpc.recorded_of::<tl_fns::messages::ReadHistory>()
            .iter()
            .any(|r| r.max_id >= 36),
        "scrolling to bottom must mark visible message #36 read"
    );
    assert_eq!(widget.inner_mut().unread_count(), 0);
}

#[test]
fn wp79_unread_bar_persists_on_read_and_clears_on_new_bottom_message_or_send() {
    let (_rt, rpc, mut live, peer_a, _channel_c) = start_live_fixture(100);
    let peer_a_tl = tl_enums::Peer::User(tl_types::PeerUser { user_id: 301 });
    let self_tl = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let peer_a_data = DataPeerId::from_user(DataUserId(301));

    // Seed an unread inbox state: message #9 already read, #10 incoming unread.
    live.write_session(|s| {
        s.history_mut(peer_a_data)
            .expect("peer_a history")
            .inbox_read(tg_data::MsgId(9), Some(1));
    });

    rpc.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                tl_text_message(
                    10,
                    peer_a_tl.clone(),
                    Some(peer_a_tl.clone()),
                    false,
                    1_795_341_610,
                    "Unread incoming message",
                    None,
                ),
                tl_text_message(
                    9,
                    peer_a_tl.clone(),
                    Some(self_tl),
                    true,
                    1_795_341_605,
                    "Earlier outgoing message",
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 100,
                pts_count: 0,
            },
        ),
    );

    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.05, true);

    // Visible unread #10 is marked read, yet the "Unread messages" bar stays visible while
    // reading the chat (matching tdesktop `HistoryInner::paintEvent` + `History::inboxRead`).
    assert_eq!(widget.inner().unread_count(), 0);
    assert_eq!(
        widget.inner().list().unread_bar_index,
        Some(1),
        "unread divider bar must stay visible above #10 while the chat stays open"
    );
    assert!(
        rpc.recorded_of::<tl_fns::messages::ReadHistory>()
            .iter()
            .any(|r| r.max_id >= 10),
        "opening focused chat with visible unread #10 must still mark it read"
    );

    // A new incoming message arriving while at bottom destroys the old unread bar
    // (matching tdesktop `HistoryWidget::newItemAdded`, `history_widget.cpp:4726`).
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 101,
                pts_count: 0,
            },
        ),
    );
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 11,
            user_id: 301,
            message: "Fresh message at bottom".to_owned(),
            pts: 101,
            pts_count: 1,
            date: 1_795_341_620,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));
    wait_for_generation_above(&live, gen_before);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.05, true);

    assert_eq!(
        widget.inner().list().unread_bar_index,
        None,
        "new message arriving at bottom must clear the old unread divider bar"
    );
}
