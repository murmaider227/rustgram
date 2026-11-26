//! SEC3-A (security review #3) regression tests: A2 / A3 read receipts are sent only while the
//! user can see the chat and only for shown unread incoming messages (`MainWindow::markingAsRead`,
//! `readTill`); A4 a forward bar stays with its own chat. The fixture is the WP-79 live history
//! harness (`FakeRpcClient`, no network).

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
    // Read receipts and typing go out from the session's background queue.
    assert!(live.flush_background_rpcs(std::time::Duration::from_secs(10)));
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

fn short_message(id: i32, out: bool, pts: i32, text: &str) -> tl_enums::Updates {
    tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
        out,
        mentioned: false,
        media_unread: false,
        silent: false,
        id,
        user_id: 301,
        message: text.to_owned(),
        pts,
        pts_count: 1,
        date: 1_795_341_600 + pts,
        fwd_from: None,
        via_bot_id: None,
        reply_to: None,
        entities: None,
        ttl_period: None,
    })
}

fn push_and_wait(rpc: &FakeRpcClient, live: &LiveSession, update: tl_enums::Updates) {
    let gen_before = live.bridge_state().generation();
    rpc.push_update(update);
    wait_for_generation_above(live, gen_before);
}

fn enqueue_read_ok(rpc: &FakeRpcClient) {
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 200,
                pts_count: 0,
            },
        ),
    );
}

fn open_chat_with_one_read_message(rpc: &FakeRpcClient) {
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
}

fn read_requests(rpc: &FakeRpcClient) -> Vec<i32> {
    rpc.recorded_of::<tl_fns::messages::ReadHistory>()
        .iter()
        .map(|r| r.max_id)
        .collect()
}

#[test]
fn a2_covered_chat_sends_no_read_receipt_until_the_user_can_see_it() {
    let (_rt, rpc, mut live, peer_a, _channel_c) = start_live_fixture(100);
    open_chat_with_one_read_message(&rpc);
    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.05, true);

    // A box, layer or the media viewer covers the chat (the window sets the flag each frame).
    widget.inner_mut().set_marking_as_read(false);
    push_and_wait(
        &rpc,
        &live,
        short_message(11, false, 101, "Arrived under a box"),
    );
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.05, true);
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(11))
            .is_some(),
        "the message is painted behind the box"
    );
    assert!(
        read_requests(&rpc).is_empty(),
        "no read receipt while the chat is covered: {:?}",
        read_requests(&rpc)
    );

    // A live-host overlay (story viewer, Business, Payments, wallet) covers the chat.
    widget.inner_mut().set_marking_as_read(true);
    widget.inner_mut().set_host_overlay_open(true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.0, true);
    assert!(
        read_requests(&rpc).is_empty(),
        "no read receipt under a host overlay"
    );

    // The overlay closes: the shown message is marked read.
    enqueue_read_ok(&rpc);
    widget.inner_mut().set_host_overlay_open(false);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 4.0, true);
    assert!(
        read_requests(&rpc).iter().any(|&max_id| max_id >= 11),
        "uncovered and focused: the shown message is read"
    );
}

#[test]
fn a3_visible_outgoing_rows_do_not_mark_an_offscreen_incoming_message_read() {
    let (_rt, rpc, mut live, peer_a, _channel_c) = start_live_fixture(100);
    open_chat_with_one_read_message(&rpc);
    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.0, false);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.05, false);

    // Unread incoming #11, then outgoing #12..#25 sent from another device push it off-screen.
    push_and_wait(
        &rpc,
        &live,
        short_message(11, false, 101, "Unread incoming"),
    );
    for (i, id) in (12..=25).enumerate() {
        let pts = 102 + i32::try_from(i).expect("small");
        push_and_wait(
            &rpc,
            &live,
            short_message(
                id,
                true,
                pts,
                "Sent from another device\nsecond line\nthird line",
            ),
        );
    }
    widget.request_scroll_to_bottom(peer_a);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.0, false);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 2.05, false);
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(25))
            .is_some(),
        "the newest outgoing message is on screen"
    );
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(11))
            .is_none(),
        "the unread incoming message is off-screen above"
    );

    // Focus the window: only outgoing rows are shown, so nothing may be marked read.
    enqueue_read_ok(&rpc);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.0, true);
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.05, true);
    assert!(
        read_requests(&rpc).is_empty(),
        "visible outgoing ids must not advance the read watermark past an unseen incoming \
         message: {:?}",
        read_requests(&rpc)
    );
}

fn is_forward(widget: &mut HistoryWidget) -> bool {
    matches!(
        widget.compose_mut().mode,
        crate::history::compose::ComposeMode::Forward { .. }
    )
}

#[test]
fn a4_forward_bar_stays_with_its_chat_when_another_chat_opens() {
    let (_rt, rpc, mut live, peer_a, channel_c) = start_live_fixture(100);
    open_chat_with_one_read_message(&rpc);
    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.0, false);

    // The forward bar is set in chat A ("Forward to..." -> A).
    widget.compose_mut().start_forward(
        peer_a,
        vec![MsgId(10)],
        "1 message from Account A".to_owned(),
    );
    render_frame(&ctx, &mut widget, &mut live, peer_a, 1.05, false);
    assert!(is_forward(&mut widget));

    // A notification or the chat list opens chat C: the forward bar must not follow.
    render_frame(&ctx, &mut widget, &mut live, channel_c, 2.0, false);
    assert!(
        !is_forward(&mut widget),
        "the forward bar must not move to the next chat opened"
    );

    // Back in chat A the forward bar is there again.
    render_frame(&ctx, &mut widget, &mut live, peer_a, 3.0, false);
    assert!(
        is_forward(&mut widget),
        "the forward draft stays with chat A"
    );
}

fn live_text_message(peer: PeerId, id: i32) -> crate::history::backend::HistoryMessage {
    crate::history::backend::HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: peer,
        sender_name: "Account A".to_owned(),
        date_unix: 1_795_341_600 + id,
        outgoing: false,
        is_post: false,
        text: tg_text::TextWithEntities::simple(format!("Busy chat message {id}")),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: false,
        is_sending: false,
        is_pinned: false,
        extra: crate::history::backend::MessageExtra::default(),
    }
}

#[test]
fn a6_open_chat_at_bottom_unloads_the_oldest_page_past_the_cap() {
    use crate::history::inner::{
        HistoryInnerWidget, KEEP_LOADED_AFTER_UNLOAD, MAX_LOADED_AT_BOTTOM,
    };
    use tg_app_core::stand_in::types::PeerKind;

    let peer = PeerId(301);
    let mut inner = HistoryInnerWidget::new();
    let total = 2_000;
    for id in 1..=total {
        assert!(inner.apply_live_added_message(live_text_message(peer, id), PeerKind::User));
        assert!(
            inner.messages().len() <= MAX_LOADED_AT_BOTTOM,
            "loaded messages must stay bounded at the bottom ({} after #{id})",
            inner.messages().len()
        );
    }
    let ids: Vec<i32> = inner.messages().iter().map(|m| m.id.0).collect();
    assert_eq!(ids.last(), Some(&total), "the newest message stays loaded");
    assert!(ids.len() >= KEEP_LOADED_AFTER_UNLOAD);
    assert!(
        ids.windows(2).all(|w| w[1] == w[0] + 1),
        "the kept messages are the newest contiguous run"
    );
    assert!(
        !inner.list().loaded_at_top,
        "after unloading, scrolling up must load the older messages again"
    );
}
