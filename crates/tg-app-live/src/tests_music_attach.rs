//! `FakeRpcClient` tests for `HistoryBackend::send_saved_music` (`MusicAttachBox`).
//!
//! Asserts the exact TL wire requests (`messages.sendMedia` for 1 track,
//! `messages.sendMultiMedia` for multiple tracks) carrying `InputMediaDocument` from
//! [`SavedMusicTrack`] (`document_id`, `access_hash`, `file_reference`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::SavedMusicTrack;
use tg_app_core::stand_in::types::PeerId;

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
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

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let (expected, result) = self
            .responses
            .lock()
            .expect("lock responses")
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected RPC call ctor 0x{ctor:08x}"));
        assert_eq!(
            ctor, expected,
            "RPC ctor mismatch: got 0x{ctor:08x}, expected 0x{expected:08x}"
        );
        let raw = result?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
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

fn synthetic_user(
    id: i64,
    access_hash: i64,
    first: &str,
    username: &str,
    is_self: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: !is_self,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: true,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn make_session() -> (LiveSession, FakeRpcClient, PeerId) {
    let fake = FakeRpcClient::new(2);
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(10_001),
            crate::ErasedRpcClient::new(fake.clone()),
            SessionRuntimeConfig {
                init_updates_state: false,
                run_get_difference_on_start: false,
                load_initial_dialogs: false,
                fetch_self_user_on_start: false,
                ..SessionRuntimeConfig::default()
            },
        )
        .expect("start SessionRuntime"),
    );

    let peer_uid = 42_002_i64;
    let peer_hash = 777_888_999_i64;
    let session_arc = runtime.session().clone();
    runtime.tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[
                tl_enums::User::User(synthetic_user(10_001, 111_222, "Taylor", "taylorkim", true)),
                tl_enums::User::User(synthetic_user(
                    peer_uid,
                    peer_hash,
                    "Mira",
                    "miravance",
                    false,
                )),
            ],
            &[],
        );
    });

    let live = LiveSession::new(runtime, 1);
    (live, fake, PeerId::from_user(peer_uid as u64))
}

fn sample_track(id: i64, hash: i64, fref: Vec<u8>, title: &str) -> SavedMusicTrack {
    SavedMusicTrack {
        document_id: id,
        access_hash: hash,
        file_reference: fref,
        title: title.to_owned(),
        performer: "Synth Collective".to_owned(),
        duration_secs: 215,
        size_bytes: 5_200_000,
        saved_on_profile: true,
    }
}

#[test]
fn send_saved_music_single_track_invokes_send_media_with_input_media_document() {
    let (mut live, fake, peer_id) = make_session();
    let track = sample_track(900_101, 555_001, vec![0xAA, 0xBB, 0xCC], "Midnight Pulse");

    fake.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_100,
            seq: 1,
        }),
    );

    let sent = live
        .send_saved_music(peer_id, std::slice::from_ref(&track))
        .expect("single saved music track send succeeds");
    assert_eq!(sent.len(), 1);
    assert!(sent[0].media.is_some());

    let calls = fake.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, tl_fns::messages::SendMedia::CONSTRUCTOR_ID);
    let req = tl_fns::messages::SendMedia::from_bytes(&calls[0].1).expect("decode SendMedia");
    assert!(
        req.message.is_empty(),
        "audio track must not send text body"
    );
    match req.peer {
        tl_enums::InputPeer::User(u) => {
            assert_eq!(u.user_id, 42_002);
            assert_eq!(u.access_hash, 777_888_999);
        }
        other => panic!("expected InputPeer::User, got {other:?}"),
    }
    match req.media {
        tl_enums::InputMedia::Document(doc) => match doc.id {
            tl_enums::InputDocument::InputDocument(id) => {
                assert_eq!(id.id, 900_101);
                assert_eq!(id.access_hash, 555_001);
                assert_eq!(id.file_reference, vec![0xAA, 0xBB, 0xCC]);
            }
            other => panic!("expected InputDocument::InputDocument, got {other:?}"),
        },
        other => panic!("expected InputMedia::Document, got {other:?}"),
    }
}

#[test]
fn send_saved_music_multiple_tracks_invokes_send_multi_media_with_input_media_documents() {
    let (mut live, fake, peer_id) = make_session();
    let tracks = vec![
        sample_track(900_201, 555_002, vec![1, 2, 3], "Track One"),
        sample_track(900_202, 555_003, vec![4, 5, 6, 7], "Track Two"),
    ];

    fake.enqueue_ok(
        tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_200,
            seq: 2,
        }),
    );

    let sent = live
        .send_saved_music(peer_id, &tracks)
        .expect("multi saved music send succeeds");
    assert_eq!(sent.len(), 2);

    let calls = fake.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID);
    let req =
        tl_fns::messages::SendMultiMedia::from_bytes(&calls[0].1).expect("decode SendMultiMedia");
    assert_eq!(req.multi_media.len(), 2);

    let mut seen_random_ids = Vec::new();
    for (item, expected) in req.multi_media.iter().zip(tracks.iter()) {
        let tl_enums::InputSingleMedia::InputSingleMedia(single) = item;
        assert!(single.message.is_empty());
        seen_random_ids.push(single.random_id);
        match &single.media {
            tl_enums::InputMedia::Document(doc) => match &doc.id {
                tl_enums::InputDocument::InputDocument(id) => {
                    assert_eq!(id.id, expected.document_id);
                    assert_eq!(id.access_hash, expected.access_hash);
                    assert_eq!(id.file_reference, expected.file_reference);
                }
                other => panic!("expected InputDocument::InputDocument, got {other:?}"),
            },
            other => panic!("expected InputMedia::Document, got {other:?}"),
        }
    }
    assert_ne!(
        seen_random_ids[0], seen_random_ids[1],
        "each InputSingleMedia must carry a distinct random_id"
    );
}

#[test]
fn send_saved_music_empty_tracks_returns_error_without_rpc() {
    let (mut live, fake, peer_id) = make_session();
    let res = live.send_saved_music(peer_id, &[]);
    assert!(res.is_err());
    assert!(fake.recorded_calls().is_empty());
}
