//! `FakeRpcClient` tests asserting that multi-page `messages.AffectedHistory` pagination
//! (`channels.deleteParticipantHistory`, `messages.deleteHistory` in `clear_history` and
//! `delete_dialog`) feeds every page's `(pts, pts_count)` into `SessionActor`, and that
//! channel `clear_history` (`channels.deleteHistory`) and `delete_dialog` (`channels.leaveChannel`)
//! route their returned `Updates` through `SessionActor`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
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

use crate::convert::from_data_peer;
use crate::session::LiveSession;
use tg_app_core::core_types::dto::moderate_messages::ModerateMessagesRequest;
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::info::backend::InfoBackend;

type QueuedResponses = Arc<Mutex<VecDeque<(u32, Result<Vec<u8>, InvocationError>)>>>;
type RecordedCalls = Arc<Mutex<Vec<u32>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    responses: QueuedResponses,
    recorded: RecordedCalls,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
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

    fn count_ctor(&self, ctor: u32) -> usize {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|&&c| c == ctor)
            .count()
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
        self.recorded.lock().expect("lock recorded").push(ctor);
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

fn make_live_with_peers() -> (
    tokio::runtime::Runtime,
    FakeRpcClient,
    LiveSession,
    tg_app_core::stand_in::types::PeerId,
    tg_app_core::stand_in::types::PeerId,
    ChannelId,
) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeRpcClient::new();
    let runtime = SessionRuntime::start_on(
        DataUserId(100),
        crate::ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            load_initial_dialogs: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 1);
    let user_id = DataUserId(301);
    let ch_id = ChannelId(601);
    let user_pid = from_data_peer(DataPeerId::from_user(user_id));
    let ch_pid = from_data_peer(DataPeerId::from_channel(ch_id));
    live.write_session(|s| {
        let mut u = User::new(user_id, false);
        u.first_name = "TestUser".to_owned();
        u.access_hash = 11_111;
        s.upsert_user(u, PeerUpdateFlags::empty());

        let mut ch = Channel::new(ch_id);
        ch.common.name = "TestGroup".to_owned();
        ch.access_hash = 22_222;
        ch.flags = ChannelFlags::MEGAGROUP | ChannelFlags::CREATOR;
        s.upsert_channel(ch, PeerUpdateFlags::empty());
    });
    (rt, rpc, live, user_pid, ch_pid, ch_id)
}

fn affected_history(pts: i32, pts_count: i32, offset: i32) -> tl_enums::messages::AffectedHistory {
    tl_enums::messages::AffectedHistory::AffectedHistory(tl_types::messages::AffectedHistory {
        pts,
        pts_count,
        offset,
    })
}

fn channel_delete_updates(channel_id: i64, pts: i32, pts_count: i32) -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DeleteChannelMessages(
            tl_types::UpdateDeleteChannelMessages {
                channel_id,
                messages: vec![1, 2],
                pts,
                pts_count,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_000,
        seq: 0,
    })
}

#[test]
fn delete_participant_history_loops_until_offset_zero_and_feeds_every_page_pts() {
    let (_rt, rpc, mut live, user_pid, ch_pid, ch_id) = make_live_with_peers();
    live.seed_updates_pts(100, Some((ch_id, 40)));

    // Two-page `channels.deleteParticipantHistory`: 40 -> 43 (offset=15) -> 45 (offset=0).
    rpc.enqueue_ok(
        tl_fns::channels::DeleteParticipantHistory::CONSTRUCTOR_ID,
        &affected_history(43, 3, 15),
    );
    rpc.enqueue_ok(
        tl_fns::channels::DeleteParticipantHistory::CONSTRUCTOR_ID,
        &affected_history(45, 2, 0),
    );

    let res = live
        .moderate_messages(ModerateMessagesRequest {
            peer_id: ch_pid,
            message_ids: Vec::new(),
            report_spam_peers: Vec::new(),
            delete_all_from_peers: vec![user_pid],
            delete_all_reactions_peers: Vec::new(),
            ban_peers: Vec::new(),
            ban_in_community_id: None,
            restrict_duration_secs: None,
        })
        .expect("moderate_messages multi-page deleteParticipantHistory");

    assert_eq!(res.deleted_all_peers, 1);
    assert_eq!(
        rpc.count_ctor(tl_fns::channels::DeleteParticipantHistory::CONSTRUCTOR_ID),
        2,
        "channels.deleteParticipantHistory must loop while AffectedHistory.offset > 0"
    );
    assert_eq!(live.updates_channel_pts(ch_id), Some(45));
}

#[test]
fn clear_history_and_delete_dialog_paginate_affected_history_and_apply_channel_updates() {
    let (_rt, rpc, mut live, user_pid, ch_pid, ch_id) = make_live_with_peers();
    live.seed_updates_pts(100, Some((ch_id, 60)));

    // 1. Non-channel `clear_history` loops `messages.deleteHistory` while `offset > 0`:
    //    100 -> 102 (offset=12) -> 105 (offset=0).
    rpc.enqueue_ok(
        tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID,
        &affected_history(102, 2, 12),
    );
    rpc.enqueue_ok(
        tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID,
        &affected_history(105, 3, 0),
    );
    DialogsBackend::clear_history(&mut live, user_pid);
    assert_eq!(
        rpc.count_ctor(tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID),
        2
    );
    assert_eq!(live.updates_common_pts(), 105);

    // 2. Non-channel `delete_dialog` also loops `messages.deleteHistory` while `offset > 0`:
    //    105 -> 106 (offset=5) -> 108 (offset=0).
    rpc.enqueue_ok(
        tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID,
        &affected_history(106, 1, 5),
    );
    rpc.enqueue_ok(
        tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID,
        &affected_history(108, 2, 0),
    );
    DialogsBackend::delete_dialog(&mut live, user_pid);
    assert_eq!(
        rpc.count_ctor(tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID),
        4
    );
    assert_eq!(live.updates_common_pts(), 108);

    // 3. Channel `clear_history` (`channels.deleteHistory`) feeds returned `Updates` into SessionActor.
    rpc.enqueue_ok(
        tl_fns::channels::DeleteHistory::CONSTRUCTOR_ID,
        &channel_delete_updates(ch_id.to_tl(), 62, 2),
    );
    DialogsBackend::clear_history(&mut live, ch_pid);
    assert_eq!(live.updates_channel_pts(ch_id), Some(62));

    // 4. Channel `delete_dialog` (`channels.leaveChannel`) feeds returned `Updates` into SessionActor.
    rpc.enqueue_ok(
        tl_fns::channels::LeaveChannel::CONSTRUCTOR_ID,
        &channel_delete_updates(ch_id.to_tl(), 64, 2),
    );
    DialogsBackend::delete_dialog(&mut live, ch_pid);
    assert_eq!(live.updates_channel_pts(ch_id), Some(64));
}
