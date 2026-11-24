//! `FakeRpcClient` tests for WP-68 live wiring gaps:
//! 1. `LivePlaylistProvider::mark_media_read` (`messages.readMessageContents` /
//!    `channels.readMessageContents`, `SessionRuntime` PTS feeding, `MEDIA_IS_UNREAD` clearing).
//! 2. `CallsBackend::group_invite_candidates` on `LiveSession` (live non-joined chat members,
//!    contacts, and loaded users; excluding self, already-joined participants, bots, deleted,
//!    support, and channels).
//! 3. `InfoBackend::{chat_star_gift_notifications_enabled, toggle_chat_star_gift_notifications}`
//!    on `LiveSession` (`payments.toggleChatStarGiftNotifications` + sync with
//!    `payments.getSavedStarGifts`).
//! 4. `account.getSavedMusicIds` (`CountHash`, `NotModified`, 30 s throttle, cache update on
//!    `account.saveMusic`).
//! 5. `messages.getAttachMenuBot` (user ingestion, `AttachMenuStore` upsert, bot ID validation).
//! 6. `help.saveAppLog` (non-personal events only with `peer = 0`, exact JSON payload).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::channels::count_hash_i64;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::calls::backend::CallsBackend;
use tg_app_core::info::backend::{InfoBackend, SavedMusicTrack};
use tg_app_core::media::backend::MediaBackend;
use tg_app_core::stand_in::types::PeerId;
use tg_bots::stand_in::UserId as BotUserId;
use tg_calls::types::CallId;
use tg_data::{
    BotInfo, Channel, ChannelFlags, ChannelId, Chat, ChatId, DocumentData, DocumentId,
    DocumentKind, FullMsgId as DataFullMsgId, Message, MessageContent, MessageFlags, MessageMedia,
    MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags, User, UserFlags,
    UserId as DataUserId,
};
use tg_payments::star_gifts::PeerGiftsFilter;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_voip::{AudioBackendKind, GroupMediaConfig, MediaEngineConfig};
use tokio::sync::broadcast;

use crate::convert::from_data_peer;
use crate::live_extras::{NonPersonalAppLogEvent, compute_saved_music_ids_hash};
use crate::session::LiveSession;

type QueuedResponses = Arc<Mutex<VecDeque<(u32, Result<Vec<u8>, InvocationError>)>>>;
type RecordedRequests = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    responses: QueuedResponses,
    recorded: RecordedRequests,
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

    fn take_requests<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        let recorded = self.recorded.lock().expect("lock recorded");
        recorded
            .iter()
            .filter(|(ctor, _)| *ctor == R::CONSTRUCTOR_ID)
            .map(|(_, bytes)| R::from_bytes(bytes).expect("decode recorded TL request"))
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

fn make_live() -> (tokio::runtime::Runtime, FakeRpcClient, LiveSession) {
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
    live.write_session(|s| {
        let mut me = User::new(DataUserId(100), true);
        me.first_name = "Self User".to_owned();
        me.flags |= UserFlags::SELF;
        s.upsert_user(me, PeerUpdateFlags::NAME);
    });
    (rt, rpc, live)
}

fn make_voice_message(
    peer: DataPeerId,
    msg_id: i64,
    doc_id: u64,
    kind: DocumentKind,
    flags: MessageFlags,
) -> Message {
    let doc = DocumentData {
        id: DocumentId(doc_id),
        kind,
        duration: Some(4.5),
        ..DocumentData::default()
    };
    let mut msg = Message::new_text(
        peer,
        DataMsgId(msg_id),
        Some(peer),
        1_750_000_000,
        tg_data::TextWithEntities::default(),
    );
    msg.content = MessageContent::Regular {
        text: tg_data::TextWithEntities::default(),
        media: MessageMedia::Document(Box::new(doc)),
    };
    msg.flags = flags;
    msg
}

fn make_tl_user(id: i64, first_name: &str, access_hash: i64, bot: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot,
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
        bot_attach_menu: bot,
        premium: false,
        attach_menu_enabled: bot,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
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
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(format!("user_{id}")),
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
    })
}

// =============================================================================
// 1. LivePlaylistProvider::mark_media_read
// =============================================================================

#[test]
fn mark_media_read_dm_sends_messages_read_message_contents_feeds_pts_and_clears_flag() {
    let (_rt, rpc, live) = make_live();
    let dm_peer = DataPeerId::from_user(DataUserId(201));
    let full_id = DataFullMsgId::new(dm_peer, DataMsgId(42));

    live.write_session(|s| {
        let mut u = User::new(DataUserId(201), false);
        u.first_name = "User Alpha".to_owned();
        u.access_hash = 0x101;
        s.upsert_user(u, PeerUpdateFlags::NAME);
        s.insert_message(make_voice_message(
            dm_peer,
            42,
            8801,
            DocumentKind::Voice,
            MessageFlags::MEDIA_IS_UNREAD,
        ));
    });
    live.seed_updates_pts(100, None);

    rpc.enqueue_ok(
        tl_fns::messages::ReadMessageContents::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 101,
                pts_count: 1,
            },
        ),
    );

    let provider = live.playlist_provider();
    provider.mark_media_read(DocumentId(8801), full_id);

    let reqs = rpc.take_requests::<tl_fns::messages::ReadMessageContents>();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].id, vec![42]);

    // Local MEDIA_IS_UNREAD must be cleared.
    let still_unread = live.read_session(|s| {
        s.message(full_id)
            .expect("dm voice msg")
            .flags
            .contains(MessageFlags::MEDIA_IS_UNREAD)
    });
    assert!(!still_unread, "MEDIA_IS_UNREAD should be cleared");

    // AffectedMessages (pts=101, pts_count=1) must have advanced SessionRuntime's pts.
    assert_eq!(live.updates_common_pts(), 101);

    // Repeating mark_media_read on an already-read message must not send another RPC.
    provider.mark_media_read(DocumentId(8801), full_id);
    assert_eq!(
        rpc.take_requests::<tl_fns::messages::ReadMessageContents>()
            .len(),
        1
    );
}

#[test]
fn mark_media_read_channel_sends_channels_read_message_contents_and_clears_flag() {
    let (_rt, rpc, live) = make_live();
    let ch_id = ChannelId(501);
    let ch_peer = DataPeerId::from_channel(ch_id);
    let full_id = DataFullMsgId::new(ch_peer, DataMsgId(77));

    live.write_session(|s| {
        let mut ch = Channel::new(ch_id);
        ch.common.name = "Channel News".to_owned();
        ch.access_hash = 0x1122_3344_5566_7788;
        ch.flags |= ChannelFlags::BROADCAST;
        s.upsert_channel(ch, PeerUpdateFlags::NAME);
        s.insert_message(make_voice_message(
            ch_peer,
            77,
            8802,
            DocumentKind::RoundVideo,
            MessageFlags::MEDIA_IS_UNREAD,
        ));
    });

    rpc.enqueue_ok(tl_fns::channels::ReadMessageContents::CONSTRUCTOR_ID, &true);

    let provider = live.playlist_provider();
    provider.mark_media_read(DocumentId(8802), full_id);

    let reqs = rpc.take_requests::<tl_fns::channels::ReadMessageContents>();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].id, vec![77]);
    assert_eq!(
        reqs[0].channel,
        tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
            channel_id: 501,
            access_hash: 0x1122_3344_5566_7788,
        })
    );
    assert!(
        rpc.take_requests::<tl_fns::messages::ReadMessageContents>()
            .is_empty()
    );

    let still_unread = live.read_session(|s| {
        s.message(full_id)
            .expect("channel round-video msg")
            .flags
            .contains(MessageFlags::MEDIA_IS_UNREAD)
    });
    assert!(!still_unread, "channel MEDIA_IS_UNREAD should be cleared");
}

#[test]
fn mark_media_read_skips_outgoing_dm_and_non_server_messages() {
    let (_rt, rpc, live) = make_live();
    let dm_peer = DataPeerId::from_user(DataUserId(202));
    let out_full_id = DataFullMsgId::new(dm_peer, DataMsgId(55));
    let local_full_id = DataFullMsgId::new(dm_peer, DataMsgId(-3));

    live.write_session(|s| {
        let mut u = User::new(DataUserId(202), false);
        u.first_name = "User Beta".to_owned();
        s.upsert_user(u, PeerUpdateFlags::NAME);
        s.insert_message(make_voice_message(
            dm_peer,
            55,
            8803,
            DocumentKind::Voice,
            MessageFlags::OUTGOING | MessageFlags::MEDIA_IS_UNREAD,
        ));
        s.insert_message(make_voice_message(
            dm_peer,
            -3,
            8804,
            DocumentKind::Voice,
            MessageFlags::MEDIA_IS_UNREAD,
        ));
    });

    let provider = live.playlist_provider();
    provider.mark_media_read(DocumentId(8803), out_full_id);
    provider.mark_media_read(DocumentId(8804), local_full_id);

    assert!(
        rpc.take_requests::<tl_fns::messages::ReadMessageContents>()
            .is_empty()
    );
    assert!(
        rpc.take_requests::<tl_fns::channels::ReadMessageContents>()
            .is_empty()
    );
    let out_still_unread = live.read_session(|s| {
        s.message(out_full_id)
            .expect("outgoing voice")
            .flags
            .contains(MessageFlags::MEDIA_IS_UNREAD)
    });
    assert!(
        out_still_unread,
        "playing own outgoing voice note must keep MEDIA_IS_UNREAD for recipient"
    );
}

// =============================================================================
// 2. CallsBackend::group_invite_candidates
// =============================================================================

#[test]
fn calls_backend_group_invite_candidates_returns_live_non_joined_members_and_contacts() {
    let (_rt, rpc, mut live) = make_live();
    let chat_id = ChatId(700);
    let chat_peer = DataPeerId::from_chat(chat_id);
    let self_pid = from_data_peer(DataPeerId::from_user(DataUserId(100)));
    let chat_app_pid = from_data_peer(chat_peer);

    live.write_session(|s| {
        // 201: chat member who is ALREADY joined in the group call -> excluded
        let mut u201 = User::new(DataUserId(201), false);
        u201.first_name = "Member Joined".to_owned();
        u201.flags |= UserFlags::CONTACT;
        s.upsert_user(u201, PeerUpdateFlags::NAME);

        // 202: chat member NOT yet joined -> must come FIRST
        let mut u202 = User::new(DataUserId(202), false);
        u202.first_name = "Member Waiting".to_owned();
        s.upsert_user(u202, PeerUpdateFlags::NAME);

        // 203: contact NOT in chat -> must come SECOND
        let mut u203 = User::new(DataUserId(203), false);
        u203.first_name = "Contact Gamma".to_owned();
        u203.flags |= UserFlags::CONTACT;
        s.upsert_user(u203, PeerUpdateFlags::NAME);

        // 204: bot (even if contact flag set) -> excluded
        let mut u204 = User::new(DataUserId(204), false);
        u204.first_name = "Helper Bot".to_owned();
        u204.flags |= UserFlags::CONTACT;
        u204.bot_info = Some(BotInfo::default());
        s.upsert_user(u204, PeerUpdateFlags::NAME);

        // 205: deleted/inaccessible user -> excluded
        let mut u205 = User::new(DataUserId(205), false);
        u205.first_name = "Deleted Account".to_owned();
        u205.flags |= UserFlags::DELETED;
        s.upsert_user(u205, PeerUpdateFlags::NAME);

        // 206: other known non-contact dialog user -> included after contacts
        let mut u206 = User::new(DataUserId(206), false);
        u206.first_name = "Known Peer".to_owned();
        s.upsert_user(u206, PeerUpdateFlags::NAME);
        let u206_peer = DataPeerId::from_user(DataUserId(206));
        s.insert_message(Message::new_text(
            u206_peer,
            DataMsgId(1),
            Some(u206_peer),
            1_750_000_000,
            tg_data::TextWithEntities::plain("hi"),
        ));

        // 501: channel -> excluded
        let mut ch = Channel::new(ChannelId(501));
        ch.common.name = "Broadcast Channel".to_owned();
        ch.flags |= ChannelFlags::BROADCAST;
        s.upsert_channel(ch, PeerUpdateFlags::NAME);

        // Basic chat 700 with members {100 (self), 201 (joined), 202 (waiting)}
        let mut chat = Chat::new(chat_id);
        chat.common.name = "Team Chat".to_owned();
        chat.participants.insert(DataUserId(100));
        chat.participants.insert(DataUserId(201));
        chat.participants.insert(DataUserId(202));
        s.upsert_chat(chat, PeerUpdateFlags::NAME);
    });

    // Enqueue phone.joinGroupCall and phone.getGroupCall responses containing user 201 as an active participant.
    rpc.enqueue_ok(
        tl_fns::phone::JoinGroupCall::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_000,
            seq: 0,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID,
        &tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
            call: tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
                join_muted: false,
                can_change_join_muted: true,
                join_date_asc: false,
                schedule_start_subscribed: false,
                can_start_video: true,
                record_video_active: false,
                rtmp_stream: false,
                listeners_hidden: false,
                conference: false,
                creator: false,
                messages_enabled: false,
                can_change_messages_enabled: false,
                min: false,
                id: 9001,
                access_hash: 0x5555,
                participants_count: 1,
                title: Some("Team Sync".to_owned()),
                stream_dc_id: None,
                record_start_date: None,
                schedule_date: None,
                unmuted_video_count: None,
                unmuted_video_limit: 0,
                version: 1,
                invite_link: None,
                send_paid_messages_stars: None,
                default_send_as: None,
            }),
            participants: vec![tl_enums::GroupCallParticipant::GroupCallParticipant(
                tl_types::GroupCallParticipant {
                    muted: false,
                    left: false,
                    can_self_unmute: true,
                    just_joined: false,
                    versioned: false,
                    min: false,
                    muted_by_you: false,
                    volume_by_admin: false,
                    self_: false,
                    video_joined: false,
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 }),
                    date: 1_750_000_000,
                    active_date: None,
                    source: 12345,
                    volume: None,
                    about: None,
                    raise_hand_rating: None,
                    video: None,
                    presentation: None,
                    paid_stars_total: None,
                },
            )],
            participants_next_offset: String::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    live.set_calls_media_config(
        MediaEngineConfig {
            audio_backend: AudioBackendKind::Virtual,
            ..MediaEngineConfig::default()
        },
        GroupMediaConfig {
            audio_backend: AudioBackendKind::Virtual,
            ..GroupMediaConfig::default()
        },
    );
    live.join_group_call_with_id(chat_app_pid, self_pid, CallId(9001), 0x5555, true, 100);
    // Let the spawned phone.getGroupCall task settle and tick the calls controller.
    for step in 1..=20 {
        std::thread::sleep(Duration::from_millis(15));
        CallsBackend::tick(&mut live, 100 + step * 15);
        if live
            .active_group_call()
            .is_some_and(|g| g.members.iter().any(|m| m.peer.id.bare() == 201))
        {
            break;
        }
    }

    let candidates = CallsBackend::group_invite_candidates(&live);
    let candidate_ids: Vec<u64> = candidates.iter().map(|c| c.id.bare()).collect();
    assert_eq!(
        candidate_ids,
        vec![202, 203, 206],
        "expected non-joined chat member 202, then contact 203, then known user 206; got {candidate_ids:?}"
    );
}

// =============================================================================
// 3. InfoBackend::chat_star_gift_notifications_*
// =============================================================================

#[test]
fn info_backend_chat_star_gift_notifications_toggles_and_syncs_with_saved_gifts() {
    let (_rt, rpc, mut live) = make_live();
    let ch_id = ChannelId(502);
    let ch_peer = DataPeerId::from_channel(ch_id);
    let ch_app_pid = from_data_peer(ch_peer);
    let user_app_pid = from_data_peer(DataPeerId::from_user(DataUserId(210)));

    live.write_session(|s| {
        let mut ch = Channel::new(ch_id);
        ch.common.name = "Gift Channel".to_owned();
        ch.access_hash = 0x7788_99AA_BBCC_DDEE;
        ch.flags |= ChannelFlags::BROADCAST;
        s.upsert_channel(ch, PeerUpdateFlags::NAME);

        let mut u = User::new(DataUserId(210), false);
        u.first_name = "User Delta".to_owned();
        s.upsert_user(u, PeerUpdateFlags::NAME);
    });

    // Before server state arrives: channel defaults to Some(false), user returns None.
    assert_eq!(
        InfoBackend::chat_star_gift_notifications_enabled(&live, ch_app_pid),
        Some(false)
    );
    assert_eq!(
        InfoBackend::chat_star_gift_notifications_enabled(&live, user_app_pid),
        None
    );

    // Loading saved gifts with `chat_notifications_enabled: Some(true)` populates InfoBackend state.
    rpc.enqueue_ok(
        tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 0,
            chat_notifications_enabled: Some(true),
            gifts: Vec::new(),
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::payments::GetStarGiftCollections::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftCollections::NotModified,
    );
    let _ = InfoBackend::peer_saved_gifts(&live, ch_app_pid, &PeerGiftsFilter::default())
        .expect("peer_saved_gifts");
    assert_eq!(
        InfoBackend::chat_star_gift_notifications_enabled(&live, ch_app_pid),
        Some(true)
    );

    // Toggling off sends `payments.toggleChatStarGiftNotifications(enabled=false, peer=channel)`.
    rpc.enqueue_ok(
        tl_fns::payments::ToggleChatStarGiftNotifications::CONSTRUCTOR_ID,
        &true,
    );
    let toggled = InfoBackend::toggle_chat_star_gift_notifications(&mut live, ch_app_pid, false)
        .expect("toggle off");
    assert!(!toggled);
    assert_eq!(
        InfoBackend::chat_star_gift_notifications_enabled(&live, ch_app_pid),
        Some(false)
    );

    // Toggling on sends `payments.toggleChatStarGiftNotifications(enabled=true, peer=channel)`.
    rpc.enqueue_ok(
        tl_fns::payments::ToggleChatStarGiftNotifications::CONSTRUCTOR_ID,
        &true,
    );
    let toggled_on = InfoBackend::toggle_chat_star_gift_notifications(&mut live, ch_app_pid, true)
        .expect("toggle on");
    assert!(toggled_on);
    assert_eq!(
        InfoBackend::chat_star_gift_notifications_enabled(&live, ch_app_pid),
        Some(true)
    );

    let reqs = rpc.take_requests::<tl_fns::payments::ToggleChatStarGiftNotifications>();
    assert_eq!(reqs.len(), 2);
    let expected_peer = tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
        channel_id: 502,
        access_hash: 0x7788_99AA_BBCC_DDEE,
    });
    assert!(!reqs[0].enabled);
    assert_eq!(reqs[0].peer, expected_peer);
    assert!(reqs[1].enabled);
    assert_eq!(reqs[1].peer, expected_peer);

    // Unknown peer returns an error without sending an RPC.
    let err = InfoBackend::toggle_chat_star_gift_notifications(&mut live, PeerId(999_999), true)
        .expect_err("unknown peer must fail");
    assert!(err.contains("not found"));
}

// =============================================================================
// 4. account.getSavedMusicIds
// =============================================================================

#[test]
fn account_get_saved_music_ids_sends_exact_hash_handles_not_modified_and_throttles() {
    let (_rt, rpc, mut live) = make_live();

    // First fetch sends hash = 0 (Api::CountHash of empty list).
    rpc.enqueue_ok(
        tl_fns::account::GetSavedMusicIds::CONSTRUCTOR_ID,
        &tl_enums::account::SavedMusicIds::SavedMusicIds(tl_types::account::SavedMusicIds {
            ids: vec![9001, 9002],
        }),
    );
    let first = live.load_saved_music_ids().expect("first load");
    assert_eq!(first.ids, vec![9001, 9002]);
    assert!(first.modified);
    assert!(!first.from_cache);
    assert!(live.has_saved_music_id(9001));
    assert!(!live.has_saved_music_id(9999));

    // Immediate second call hits 30 s throttle cache and sends no RPC.
    let cached = live.load_saved_music_ids().expect("cached load");
    assert_eq!(cached.ids, vec![9001, 9002]);
    assert!(!cached.modified);
    assert!(cached.from_cache);
    assert_eq!(
        rpc.take_requests::<tl_fns::account::GetSavedMusicIds>()
            .len(),
        1
    );

    // Forced refresh sends Api::CountHash([9001, 9002]) and handles NotModified.
    rpc.enqueue_ok(
        tl_fns::account::GetSavedMusicIds::CONSTRUCTOR_ID,
        &tl_enums::account::SavedMusicIds::NotModified,
    );
    let refreshed = live
        .refresh_saved_music_ids()
        .expect("not modified refresh");
    assert_eq!(refreshed.ids, vec![9001, 9002]);
    assert!(!refreshed.modified);
    assert!(!refreshed.from_cache);

    // Saving a new track via InfoBackend::save_profile_music updates the local ID list at index 0.
    rpc.enqueue_ok(tl_fns::account::SaveMusic::CONSTRUCTOR_ID, &true);
    let track = SavedMusicTrack {
        document_id: 9003,
        access_hash: 0x33,
        file_reference: vec![1, 2, 3],
        title: "Track Three".to_owned(),
        performer: "Artist".to_owned(),
        duration_secs: 180,
        size_bytes: 1_000_000,
        saved_on_profile: false,
    };
    InfoBackend::save_profile_music(&mut live, &track, false).expect("save track");
    assert_eq!(live.cached_saved_music_ids(), vec![9003, 9001, 9002]);

    // Next forced refresh must send Api::CountHash([9003, 9001, 9002]).
    rpc.enqueue_ok(
        tl_fns::account::GetSavedMusicIds::CONSTRUCTOR_ID,
        &tl_enums::account::SavedMusicIds::NotModified,
    );
    let _ = live.refresh_saved_music_ids().expect("refresh after save");

    let reqs = rpc.take_requests::<tl_fns::account::GetSavedMusicIds>();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].hash, 0);
    assert_eq!(reqs[1].hash, count_hash_i64([9001, 9002]));
    assert_eq!(reqs[1].hash, compute_saved_music_ids_hash(&[9001, 9002]));
    assert_eq!(reqs[2].hash, count_hash_i64([9003, 9001, 9002]));
}

// =============================================================================
// 5. messages.getAttachMenuBot
// =============================================================================

#[test]
fn messages_get_attach_menu_bot_fetches_ingests_users_and_upserts_store() {
    let (_rt, rpc, live) = make_live();
    let bot_uid = 777u64;

    live.write_session(|s| {
        let mut bot = User::new(DataUserId(bot_uid), false);
        bot.first_name = "Old Bot Name".to_owned();
        bot.access_hash = 0x2ABB_CCDD_1122_3344;
        bot.bot_info = Some(BotInfo::default());
        s.upsert_user(bot, PeerUpdateFlags::NAME);
    });

    rpc.enqueue_ok(
        tl_fns::messages::GetAttachMenuBot::CONSTRUCTOR_ID,
        &tl_enums::AttachMenuBotsBot::AttachMenuBotsBot(tl_types::AttachMenuBotsBot {
            bot: tl_enums::AttachMenuBot::AttachMenuBot(tl_types::AttachMenuBot {
                inactive: false,
                has_settings: true,
                request_write_access: true,
                show_in_attach_menu: true,
                show_in_side_menu: true,
                side_menu_disclaimer_needed: false,
                bot_id: bot_uid as i64,
                short_name: "MiniTool".to_owned(),
                peer_types: Some(vec![
                    tl_enums::AttachMenuPeerType::PM,
                    tl_enums::AttachMenuPeerType::Chat,
                ]),
                icons: Vec::new(),
            }),
            users: vec![make_tl_user(
                bot_uid as i64,
                "Updated Bot Name",
                0x2ABB_CCDD_1122_3344,
                true,
            )],
        }),
    );

    let bot = live
        .fetch_attach_menu_bot(BotUserId(bot_uid))
        .expect("getAttachMenuBot");
    assert_eq!(bot.bot_id, BotUserId(bot_uid));
    assert_eq!(bot.short_name, "MiniTool");
    assert!(bot.in_attach_menu);
    assert!(bot.in_side_menu);
    assert!(bot.request_write_access);
    assert!(!bot.inactive);

    // Exact TL request assertion.
    let reqs = rpc.take_requests::<tl_fns::messages::GetAttachMenuBot>();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].bot,
        tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: bot_uid as i64,
            access_hash: 0x2ABB_CCDD_1122_3344,
        })
    );

    // Returned `users` must be ingested into `Session`.
    let updated_name = live.read_session(|s| {
        s.user(DataUserId(bot_uid))
            .expect("bot in session")
            .first_name
            .clone()
    });
    assert_eq!(updated_name, "Updated Bot Name");

    // Bot entry must be upserted into the shared `AttachMenuStore`.
    let cached = crate::bots::cached_attach_menu_bot(live.runtime().session(), BotUserId(bot_uid))
        .expect("cached attach menu bot");
    assert_eq!(cached.short_name, "MiniTool");
    assert!(cached.in_attach_menu);

    // Mismatched bot_id from server must be rejected cleanly.
    rpc.enqueue_ok(
        tl_fns::messages::GetAttachMenuBot::CONSTRUCTOR_ID,
        &tl_enums::AttachMenuBotsBot::AttachMenuBotsBot(tl_types::AttachMenuBotsBot {
            bot: tl_enums::AttachMenuBot::AttachMenuBot(tl_types::AttachMenuBot {
                inactive: true,
                has_settings: false,
                request_write_access: false,
                show_in_attach_menu: false,
                show_in_side_menu: false,
                side_menu_disclaimer_needed: false,
                bot_id: 999,
                short_name: "WrongBot".to_owned(),
                peer_types: None,
                icons: Vec::new(),
            }),
            users: Vec::new(),
        }),
    );
    let err = live
        .fetch_attach_menu_bot(BotUserId(bot_uid))
        .expect_err("mismatched bot id must fail");
    assert!(err.contains("999"));
}

// =============================================================================
// 6. help.saveAppLog (non-personal events only)
// =============================================================================

#[test]
fn help_save_app_log_sends_only_non_personal_events_with_zero_peer() {
    let (_rt, rpc, live) = make_live();

    // Empty event list is a no-op with zero RPCs.
    let empty_ok = live
        .save_app_log_at(&[], 1_750_000_000.0)
        .expect("empty log");
    assert!(!empty_ok);
    assert!(rpc.take_requests::<tl_fns::help::SaveAppLog>().is_empty());

    rpc.enqueue_ok(tl_fns::help::SaveAppLog::CONSTRUCTOR_ID, &true);
    let events = vec![
        NonPersonalAppLogEvent::PremiumPromoScreenShow {
            order: vec!["stories".to_owned(), "double_limits".to_owned()],
            source: String::new(),
        },
        NonPersonalAppLogEvent::PremiumPromoScreenAccept,
    ];
    let saved = live
        .save_app_log_at(&events, 1_750_000_000.25)
        .expect("saveAppLog");
    assert!(saved);

    let reqs = rpc.take_requests::<tl_fns::help::SaveAppLog>();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].events.len(), 2);

    let tl_enums::InputAppEvent::InputAppEvent(ref show_ev) = reqs[0].events[0];
    assert_eq!(show_ev.time, 1_750_000_000.25);
    assert_eq!(show_ev.r#type, "premium.promo_screen_show");
    assert_eq!(show_ev.peer, 0);
    assert_eq!(
        show_ev.data,
        tl_enums::JSONValue::JsonObject(tl_types::JsonObject {
            value: vec![
                tl_enums::JSONObjectValue::JsonObjectValue(tl_types::JsonObjectValue {
                    key: "premium_promo_order".to_owned(),
                    value: tl_enums::JSONValue::JsonArray(tl_types::JsonArray {
                        value: vec![
                            tl_enums::JSONValue::JsonString(tl_types::JsonString {
                                value: "stories".to_owned(),
                            }),
                            tl_enums::JSONValue::JsonString(tl_types::JsonString {
                                value: "double_limits".to_owned(),
                            }),
                        ],
                    }),
                }),
                tl_enums::JSONObjectValue::JsonObjectValue(tl_types::JsonObjectValue {
                    key: "source".to_owned(),
                    value: tl_enums::JSONValue::JsonString(tl_types::JsonString {
                        value: "settings".to_owned(),
                    }),
                }),
            ],
        })
    );

    let tl_enums::InputAppEvent::InputAppEvent(ref accept_ev) = reqs[0].events[1];
    assert_eq!(accept_ev.time, 1_750_000_000.25);
    assert_eq!(accept_ev.r#type, "premium.promo_screen_accept");
    assert_eq!(accept_ev.peer, 0);
    assert_eq!(accept_ev.data, tl_enums::JSONValue::JsonNull);
}
