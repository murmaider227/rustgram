//! Tests asserting that every remaining `messages.AffectedHistory`, `messages.AffectedMessages`,
//! and `messages.Messages::ChannelMessages` caller in `tg-app-live` feeds `(pts, pts_count)` into
//! `SessionActor`'s single updates handler (`LiveSession::apply_affected_history` /
//! `apply_channel_pts`).

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
use tg_app_core::history::backend::{HistoryBackend, UnreadThingsKind, UnreadThingsScope};
use tg_app_core::info::backend::InfoBackend;
use tg_app_core::stand_in::types::MsgId;

type QueuedResponses = Arc<Mutex<VecDeque<(u32, Result<Vec<u8>, InvocationError>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    responses: QueuedResponses,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
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
    let user_id = DataUserId(201);
    let ch_id = ChannelId(501);
    let user_pid = from_data_peer(DataPeerId::from_user(user_id));
    let ch_pid = from_data_peer(DataPeerId::from_channel(ch_id));
    live.write_session(|s| {
        let mut u = User::new(user_id, false);
        u.first_name = "Alice".to_owned();
        u.access_hash = 12345;
        s.upsert_user(u, PeerUpdateFlags::empty());

        let mut ch = Channel::new(ch_id);
        ch.common.name = "Forum Group".to_owned();
        ch.access_hash = 67890;
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

fn affected_messages(pts: i32, pts_count: i32) -> tl_enums::messages::AffectedMessages {
    tl_enums::messages::AffectedMessages::AffectedMessages(tl_types::messages::AffectedMessages {
        pts,
        pts_count,
    })
}

#[test]
fn unpin_all_and_read_unread_things_feed_affected_history_pts_into_session_runtime() {
    let (_rt, rpc, mut live, user_pid, ch_pid, ch_id) = make_live_with_peers();
    live.seed_updates_pts(100, Some((ch_id, 50)));

    // 1. `messages.unpinAllMessages` (2-page pagination on user peer: 100 -> 102 -> 105).
    rpc.enqueue_ok(
        tl_fns::messages::UnpinAllMessages::CONSTRUCTOR_ID,
        &affected_history(102, 2, 10),
    );
    rpc.enqueue_ok(
        tl_fns::messages::UnpinAllMessages::CONSTRUCTOR_ID,
        &affected_history(105, 3, 0),
    );
    live.unpin_all_messages_in_thread(user_pid, None, None)
        .expect("unpin_all_messages_in_thread user");
    assert_eq!(live.updates_common_pts(), 105);

    // 2. `messages.unpinAllMessages` on channel peer advances channel pts (50 -> 52).
    rpc.enqueue_ok(
        tl_fns::messages::UnpinAllMessages::CONSTRUCTOR_ID,
        &affected_history(52, 2, 0),
    );
    live.unpin_all_messages_in_thread(ch_pid, None, None)
        .expect("unpin_all_messages_in_thread channel");
    assert_eq!(live.updates_channel_pts(ch_id), Some(52));

    // 3. `messages.readMentions` (2 pages: common 105 -> 106 -> 108).
    rpc.enqueue_ok(
        tl_fns::messages::ReadMentions::CONSTRUCTOR_ID,
        &affected_history(106, 1, 5),
    );
    rpc.enqueue_ok(
        tl_fns::messages::ReadMentions::CONSTRUCTOR_ID,
        &affected_history(108, 2, 0),
    );
    let r = live
        .read_unread_mentions(user_pid, None)
        .expect("readMentions");
    assert_eq!(r.pages, 2);
    assert_eq!(live.updates_common_pts(), 108);

    // 4. `messages.readReactions` on channel peer (channel 52 -> 55).
    rpc.enqueue_ok(
        tl_fns::messages::ReadReactions::CONSTRUCTOR_ID,
        &affected_history(55, 3, 0),
    );
    live.read_unread_reactions(ch_pid, None, None)
        .expect("readReactions");
    assert_eq!(live.updates_channel_pts(ch_id), Some(55));

    // 5. `messages.readPollVotes` on channel peer (channel 55 -> 57).
    rpc.enqueue_ok(
        tl_fns::messages::ReadPollVotes::CONSTRUCTOR_ID,
        &affected_history(57, 2, 0),
    );
    live.read_unread_poll_votes(ch_pid, None)
        .expect("readPollVotes");
    assert_eq!(live.updates_channel_pts(ch_id), Some(57));
}

#[test]
fn channel_messages_and_moderation_and_delete_rpcs_feed_pts_into_session_runtime() {
    let (_rt, rpc, mut live, user_pid, ch_pid, ch_id) = make_live_with_peers();
    live.seed_updates_pts(200, None);

    // 1. `messages.getUnreadMentions` returning `ChannelMessages(pts=82)` initializes channel pts.
    rpc.enqueue_ok(
        tl_fns::messages::GetUnreadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 82,
            count: 0,
            offset_id_offset: None,
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let scope = UnreadThingsScope {
        peer_id: ch_pid,
        top_msg_id: None,
        saved_peer_id: None,
    };
    let _ = live
        .load_unread_things_page(scope, UnreadThingsKind::Mentions)
        .expect("load_unread_things_page");
    assert_eq!(live.updates_channel_pts(ch_id), Some(82));

    // 2. `communities::moderate_messages`: `channels.deleteMessages` (82 -> 84) +
    //    `channels.deleteParticipantHistory` (84 -> 87).
    rpc.enqueue_ok(
        tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID,
        &affected_messages(84, 2),
    );
    rpc.enqueue_ok(
        tl_fns::channels::DeleteParticipantHistory::CONSTRUCTOR_ID,
        &affected_history(87, 3, 0),
    );
    let mod_res = live
        .moderate_messages(ModerateMessagesRequest {
            peer_id: ch_pid,
            message_ids: vec![10, 11],
            report_spam_peers: Vec::new(),
            delete_all_from_peers: vec![user_pid],
            delete_all_reactions_peers: Vec::new(),
            ban_peers: Vec::new(),
            ban_in_community_id: None,
            restrict_duration_secs: None,
        })
        .expect("moderate_messages");
    assert_eq!(mod_res.deleted_selected, 2);
    assert_eq!(mod_res.deleted_all_peers, 1);
    assert_eq!(live.updates_channel_pts(ch_id), Some(87));

    // 3. `HistoryBackend::delete_messages` on user peer (common 200 -> 202) and channel peer (87 -> 89).
    rpc.enqueue_ok(
        tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID,
        &affected_messages(202, 2),
    );
    HistoryBackend::delete_messages(&mut live, user_pid, &[MsgId(1), MsgId(2)], true)
        .expect("delete_messages user");
    assert_eq!(live.updates_common_pts(), 202);

    rpc.enqueue_ok(
        tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID,
        &affected_messages(89, 2),
    );
    HistoryBackend::delete_messages(&mut live, ch_pid, &[MsgId(3), MsgId(4)], true)
        .expect("delete_messages channel");
    assert_eq!(live.updates_channel_pts(ch_id), Some(89));
}
