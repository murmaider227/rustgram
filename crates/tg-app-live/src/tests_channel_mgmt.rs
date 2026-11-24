//! Live `InfoBackend` / `FakeRpcClient` tests for WP-65:
//! all 15 channel/supergroup/basic-group management RPCs plus the `InfoBackend::check_username`
//! dispatch fix (`channels.checkUsername` for channels/groups vs `account.checkUsername` for users).
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs and zero network access.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatFlags, ChatId, LoadedStatus, Peer,
    PeerId as DataPeerId, PeerUpdateFlags, User, UserId as DataUserId, Username,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Deserializable, Identifiable, RemoteCall, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::from_data_peer;
use tg_app_core::info::backend::{ChannelStickerSetRef, InfoBackend};
use tg_app_core::info::values::UsernameAvailability;
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
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
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

    fn enqueue_rpc_err(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value: None,
            })),
        ));
    }

    fn calls_of<F: Identifiable>(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == F::CONSTRUCTOR_ID)
            .cloned()
            .collect()
    }

    fn calls_matching(&self, ctors: &[u32]) -> Vec<(u32, Vec<u8>)> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| ctors.contains(c))
            .cloned()
            .collect()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
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
        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
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

struct TestFixture {
    _rt: tokio::runtime::Runtime,
    rpc: FakeRpcClient,
    live: LiveSession,
    self_pid: PeerId,
    megagroup_cid: ChannelId,
    megagroup_pid: PeerId,
    broadcast_pid: PeerId,
    basic_chat_id: ChatId,
    basic_chat_pid: PeerId,
}

fn make_fixture() -> TestFixture {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeRpcClient::new();
    let self_uid = DataUserId(100);
    let runtime = SessionRuntime::start_on(
        self_uid,
        crate::ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            load_initial_dialogs: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 1);

    let megagroup_cid = ChannelId(5001);
    let broadcast_cid = ChannelId(5002);
    let basic_chat_id = ChatId(7001);

    live.write_session(|s| {
        let mut me = User::new(self_uid, true);
        me.set_name("Account", "Owner", "Account Owner", "selfhandle");
        me.access_hash = 1001;
        s.upsert_user(me, PeerUpdateFlags::empty());

        let mut mg = Channel::new(megagroup_cid);
        mg.set_name("Demo Supergroup", "demosupergroup");
        mg.set_usernames(&[
            Username {
                username: "demosupergroup".to_owned(),
                active: true,
                editable: true,
            },
            Username {
                username: "collectible_one".to_owned(),
                active: true,
                editable: false,
            },
        ]);
        mg.access_hash = 0x1122_3344_5566_7788;
        mg.common.loaded_status = LoadedStatus::Normal;
        mg.flags = ChannelFlags::MEGAGROUP
            | ChannelFlags::CREATOR
            | ChannelFlags::USERNAME
            | ChannelFlags::CAN_SET_STICKERS;
        s.set_peer_access_hash(
            DataPeerId::from_channel(megagroup_cid),
            mg.access_hash as i64,
        );
        s.upsert_channel(mg, PeerUpdateFlags::empty());

        let mut bc = Channel::new(broadcast_cid);
        bc.set_name("Demo Broadcast", "demobroadcast");
        bc.access_hash = 0x2233_4455_6677_8899;
        bc.common.loaded_status = LoadedStatus::Normal;
        bc.flags = ChannelFlags::BROADCAST | ChannelFlags::CREATOR | ChannelFlags::USERNAME;
        s.set_peer_access_hash(
            DataPeerId::from_channel(broadcast_cid),
            bc.access_hash as i64,
        );
        s.upsert_channel(bc, PeerUpdateFlags::empty());

        let mut chat = Chat::new(basic_chat_id);
        chat.set_name("Demo Basic Group");
        chat.flags = ChatFlags::CREATOR;
        chat.count = 4;
        s.upsert_chat(chat, PeerUpdateFlags::empty());
    });

    TestFixture {
        _rt: rt,
        rpc,
        live,
        self_pid: from_data_peer(DataPeerId::from_user(self_uid)),
        megagroup_cid,
        megagroup_pid: from_data_peer(DataPeerId::from_channel(megagroup_cid)),
        broadcast_pid: from_data_peer(DataPeerId::from_channel(broadcast_cid)),
        basic_chat_id,
        basic_chat_pid: from_data_peer(DataPeerId::from_chat(basic_chat_id)),
    }
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_000,
        seq: 0,
    })
}

fn synthetic_tl_channel(id: i64, access_hash: i64, title: &str, megagroup: bool) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: !megagroup,
        verified: false,
        megagroup,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        forum_sidebar: false,
        in_news: false,
        id,
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: Some(format!("group_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(42),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    })
}

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
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
        access_hash: Some(id * 37),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
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

#[test]
fn check_username_dispatches_channels_check_username_for_channels_and_handles_errors() {
    let f = make_fixture();

    // 1. Channel peer -> MUST invoke `channels.checkUsername(InputChannel)` (NEVER `account.checkUsername`).
    f.rpc
        .enqueue_ok(tl_fns::channels::CheckUsername::CONSTRUCTOR_ID, &true);
    let res = f
        .live
        .check_username("@fresh_handle", Some(f.megagroup_pid));
    assert_eq!(res, UsernameAvailability::Available);
    assert!(
        f.rpc
            .calls_of::<tl_fns::account::CheckUsername>()
            .is_empty(),
        "channel check_username must never invoke account.checkUsername"
    );
    let calls = f.rpc.calls_of::<tl_fns::channels::CheckUsername>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0x10e6_bd2c);
    let decoded = tl_fns::channels::CheckUsername::from_bytes(&calls[0].1).expect("decode");
    assert_eq!(
        decoded.channel,
        tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
            channel_id: f.megagroup_cid.0 as i64,
            access_hash: 0x1122_3344_5566_7788,
        })
    );
    assert_eq!(decoded.username, "fresh_handle");
    f.rpc.clear_recorded();

    // 2. Current editable username of the same channel returns Available locally without RPC.
    assert_eq!(
        f.live
            .check_username("demosupergroup", Some(f.megagroup_pid)),
        UsernameAvailability::Available
    );
    assert!(
        f.rpc
            .calls_of::<tl_fns::channels::CheckUsername>()
            .is_empty()
    );
    assert!(
        f.rpc
            .calls_of::<tl_fns::account::CheckUsername>()
            .is_empty()
    );

    // 3. `USERNAME_OCCUPIED`, `USERNAME_INVALID`, `CHANNELS_ADMIN_PUBLIC_TOO_MUCH`,
    //    `USERNAME_PURCHASE_AVAILABLE`, `CHANNEL_PUBLIC_GROUP_NA`.
    let cases = [
        ("USERNAME_OCCUPIED", UsernameAvailability::Occupied),
        ("USERNAME_INVALID", UsernameAvailability::InvalidFormat),
        (
            "CHANNELS_ADMIN_PUBLIC_TOO_MUCH",
            UsernameAvailability::TooManyPublicChannels,
        ),
        (
            "USERNAME_PURCHASE_AVAILABLE",
            UsernameAvailability::PurchaseAvailable,
        ),
        (
            "CHANNEL_PUBLIC_GROUP_NA",
            UsernameAvailability::PublicGroupNotAvailable,
        ),
    ];
    for (err_name, expected) in cases {
        f.rpc.enqueue_rpc_err(
            tl_fns::channels::CheckUsername::CONSTRUCTOR_ID,
            400,
            err_name,
        );
        let got = f.live.check_username("other_handle", Some(f.megagroup_pid));
        assert_eq!(got, expected, "error mapping for {err_name}");
    }
    assert_eq!(f.rpc.calls_of::<tl_fns::channels::CheckUsername>().len(), 5);
    assert!(
        f.rpc
            .calls_of::<tl_fns::account::CheckUsername>()
            .is_empty()
    );
    f.rpc.clear_recorded();

    // 4. `exclude_peer: None` (creating a new channel) sends `channels.checkUsername(InputChannel::Empty)`.
    f.rpc
        .enqueue_ok(tl_fns::channels::CheckUsername::CONSTRUCTOR_ID, &true);
    assert_eq!(
        f.live.check_username("brand_new_chan", None),
        UsernameAvailability::Available
    );
    let calls = f.rpc.calls_of::<tl_fns::channels::CheckUsername>();
    assert_eq!(calls.len(), 1);
    let decoded = tl_fns::channels::CheckUsername::from_bytes(&calls[0].1).expect("decode");
    assert_eq!(decoded.channel, tl_enums::InputChannel::Empty);
    assert_eq!(decoded.username, "brand_new_chan");
    f.rpc.clear_recorded();

    // 5. User peer (`Some(self_pid)`) still invokes `account.checkUsername`.
    f.rpc
        .enqueue_ok(tl_fns::account::CheckUsername::CONSTRUCTOR_ID, &true);
    assert_eq!(
        f.live.check_username("new_user_handle", Some(f.self_pid)),
        UsernameAvailability::Available
    );
    let calls = f.rpc.calls_of::<tl_fns::account::CheckUsername>();
    assert_eq!(calls.len(), 1);
    assert!(
        f.rpc
            .calls_of::<tl_fns::channels::CheckUsername>()
            .is_empty()
    );
}

#[test]
fn channel_toggles_and_boosts_wire_bytes_and_session_state_updates() {
    let mut f = make_fixture();
    f.live.seed_updates_pts(100, Some((f.megagroup_cid, 20)));

    // 1. `channels.togglePreHistoryHidden` (success + Updates applied through SessionRuntime + CHAT_NOT_MODIFIED).
    let pts_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewChannelMessage(
            tl_types::UpdateNewChannelMessage {
                message: tl_enums::Message::Empty(tl_types::MessageEmpty {
                    id: 99,
                    peer_id: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: f.megagroup_cid.0 as i64,
                    })),
                }),
                pts: 21,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_000,
        seq: 0,
    });
    f.rpc.enqueue_ok(
        tl_fns::channels::TogglePreHistoryHidden::CONSTRUCTOR_ID,
        &pts_updates,
    );
    f.live
        .toggle_pre_history_hidden(f.megagroup_pid, true)
        .expect("toggle_pre_history_hidden true");
    let calls = f.rpc.calls_of::<tl_fns::channels::TogglePreHistoryHidden>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0xeabb_b94c);
    let req = tl_fns::channels::TogglePreHistoryHidden::from_bytes(&calls[0].1).expect("decode");
    assert!(req.enabled);
    assert_eq!(
        u32::from_le_bytes(calls[0].1[calls[0].1.len() - 4..].try_into().unwrap()),
        BOOL_TRUE_ID
    );
    assert_eq!(
        f.live.updates_channel_pts(f.megagroup_cid),
        Some(21),
        "returned Updates must advance channel pts via SessionRuntime::apply_updates"
    );
    assert!(
        f.live
            .channel_management_state(f.megagroup_pid)
            .expect("state")
            .pre_history_hidden
    );
    f.rpc.clear_recorded();

    // `CHAT_NOT_MODIFIED` is non-fatal and updates local cached flag.
    f.rpc.enqueue_rpc_err(
        tl_fns::channels::TogglePreHistoryHidden::CONSTRUCTOR_ID,
        400,
        "CHAT_NOT_MODIFIED",
    );
    f.live
        .toggle_pre_history_hidden(f.megagroup_pid, false)
        .expect("toggle_pre_history_hidden CHAT_NOT_MODIFIED");
    assert!(
        !f.live
            .channel_management_state(f.megagroup_pid)
            .expect("state")
            .pre_history_hidden
    );
    f.rpc.clear_recorded();

    // 2. `channels.toggleForum` (enable with tabs, then disable forcing tabs=false).
    f.rpc.enqueue_ok(
        tl_fns::channels::ToggleForum::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .toggle_channel_forum(f.megagroup_pid, true, true)
        .expect("toggle_channel_forum enable");
    let calls = f.rpc.calls_of::<tl_fns::channels::ToggleForum>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0xa295_2b54);
    let req = tl_fns::channels::ToggleForum::from_bytes(&calls[0].1).expect("decode");
    assert!(req.enabled);
    assert!(req.tabs);
    assert!(!req.sidebar);
    let st = f
        .live
        .channel_management_state(f.megagroup_pid)
        .expect("state");
    assert!(st.is_forum);
    assert!(st.forum_tabs);
    f.rpc.clear_recorded();

    f.rpc.enqueue_rpc_err(
        tl_fns::channels::ToggleForum::CONSTRUCTOR_ID,
        400,
        "CHAT_NOT_MODIFIED",
    );
    f.live
        .toggle_channel_forum(f.megagroup_pid, false, true)
        .expect("toggle_channel_forum disable");
    let calls = f.rpc.calls_of::<tl_fns::channels::ToggleForum>();
    assert_eq!(calls.len(), 1);
    let req = tl_fns::channels::ToggleForum::from_bytes(&calls[0].1).expect("decode");
    assert!(!req.enabled);
    assert!(!req.tabs, "tabs must be forced false when enabled=false");
    assert!(!req.sidebar);
    let st = f
        .live
        .channel_management_state(f.megagroup_pid)
        .expect("state");
    assert!(!st.is_forum);
    assert!(!st.forum_tabs);
    f.rpc.clear_recorded();

    // 3. `channels.toggleParticipantsHidden`.
    f.rpc.enqueue_ok(
        tl_fns::channels::ToggleParticipantsHidden::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .toggle_participants_hidden(f.megagroup_pid, true)
        .expect("toggle_participants_hidden");
    let calls = f
        .rpc
        .calls_of::<tl_fns::channels::ToggleParticipantsHidden>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0x6a6e_7854);
    assert!(
        f.live
            .channel_management_state(f.megagroup_pid)
            .expect("state")
            .participants_hidden
    );
    f.rpc.clear_recorded();

    // 4. `channels.toggleViewForumAsMessages`.
    f.rpc.enqueue_ok(
        tl_fns::channels::ToggleViewForumAsMessages::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .toggle_view_forum_as_messages(f.megagroup_pid, true)
        .expect("toggle_view_forum_as_messages");
    let calls = f
        .rpc
        .calls_of::<tl_fns::channels::ToggleViewForumAsMessages>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0x9738_bb15);
    assert!(
        f.live
            .channel_management_state(f.megagroup_pid)
            .expect("state")
            .view_forum_as_messages
    );
    f.rpc.clear_recorded();

    // 5. `channels.setBoostsToUnblockRestrictions`.
    f.rpc.enqueue_ok(
        tl_fns::channels::SetBoostsToUnblockRestrictions::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .set_boosts_to_unblock_restrictions(f.megagroup_pid, 4)
        .expect("set_boosts_to_unblock_restrictions");
    let calls = f
        .rpc
        .calls_of::<tl_fns::channels::SetBoostsToUnblockRestrictions>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0xad39_9cee);
    let req =
        tl_fns::channels::SetBoostsToUnblockRestrictions::from_bytes(&calls[0].1).expect("decode");
    assert_eq!(req.boosts, 4);
    assert_eq!(
        f.live
            .channel_management_state(f.megagroup_pid)
            .expect("state")
            .boosts_unrestrict,
        4
    );
    f.rpc.clear_recorded();

    // 6. `channels.convertToGigagroup`.
    f.rpc.enqueue_ok(
        tl_fns::channels::ConvertToGigagroup::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .convert_to_gigagroup(f.megagroup_pid)
        .expect("convert_to_gigagroup");
    let calls = f.rpc.calls_of::<tl_fns::channels::ConvertToGigagroup>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0x0b29_0c69);
    assert!(
        f.live
            .channel_management_state(f.megagroup_pid)
            .expect("state")
            .is_gigagroup
    );
    f.rpc.clear_recorded();

    // 7. `channels.toggleAutotranslation`.
    f.rpc.enqueue_ok(
        tl_fns::channels::ToggleAutotranslation::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .toggle_channel_autotranslation(f.broadcast_pid, true)
        .expect("toggle_channel_autotranslation");
    let calls = f.rpc.calls_of::<tl_fns::channels::ToggleAutotranslation>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0x167f_c0a1);
    let req = tl_fns::channels::ToggleAutotranslation::from_bytes(&calls[0].1).expect("decode");
    assert!(req.enabled);
    assert!(
        f.live
            .channel_management_state(f.broadcast_pid)
            .expect("state")
            .autotranslation
    );
}

#[test]
fn deactivate_usernames_stickers_paid_price_discussion_delete_chat_and_admins_with_invites() {
    let mut f = make_fixture();

    // 1. `channels.deactivateAllUsernames` keeps editable username and drops collectible ones.
    f.rpc.enqueue_ok(
        tl_fns::channels::DeactivateAllUsernames::CONSTRUCTOR_ID,
        &true,
    );
    f.live
        .deactivate_all_channel_usernames(f.megagroup_pid)
        .expect("deactivate_all_channel_usernames");
    let calls = f.rpc.calls_of::<tl_fns::channels::DeactivateAllUsernames>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 0x0a24_5dd3);
    f.live.read_session(|s| {
        let ch = s.channel(f.megagroup_cid).expect("channel");
        assert_eq!(ch.usernames.usernames(), &["demosupergroup".to_owned()]);
        assert!(ch.flags.contains(ChannelFlags::USERNAME));
    });
    f.rpc.clear_recorded();

    // 2. `channels.setStickers` (ID) and `channels.setEmojiStickers` (ShortName).
    f.rpc
        .enqueue_ok(tl_fns::channels::SetStickers::CONSTRUCTOR_ID, &true);
    f.live
        .set_channel_sticker_set(
            f.megagroup_pid,
            ChannelStickerSetRef::Id {
                id: 901,
                access_hash: 902,
            },
        )
        .expect("set_channel_sticker_set");
    f.rpc
        .enqueue_ok(tl_fns::channels::SetEmojiStickers::CONSTRUCTOR_ID, &true);
    f.live
        .set_channel_emoji_sticker_set(
            f.megagroup_pid,
            ChannelStickerSetRef::ShortName("demo_emojis".to_owned()),
        )
        .expect("set_channel_emoji_sticker_set");
    let calls = f.rpc.calls_matching(&[
        tl_fns::channels::SetStickers::CONSTRUCTOR_ID,
        tl_fns::channels::SetEmojiStickers::CONSTRUCTOR_ID,
    ]);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, 0xea8c_a4f9);
    let set_st = tl_fns::channels::SetStickers::from_bytes(&calls[0].1).expect("decode");
    assert_eq!(
        set_st.stickerset,
        tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
            id: 901,
            access_hash: 902,
        })
    );
    assert_eq!(calls[1].0, 0x3cd9_30b7);
    let set_em = tl_fns::channels::SetEmojiStickers::from_bytes(&calls[1].1).expect("decode");
    assert_eq!(
        set_em.stickerset,
        tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
            short_name: "demo_emojis".to_owned(),
        })
    );
    let st = f
        .live
        .channel_management_state(f.megagroup_pid)
        .expect("state");
    assert_eq!(
        st.sticker_set,
        Some(ChannelStickerSetRef::Id {
            id: 901,
            access_hash: 902
        })
    );
    assert_eq!(
        st.emoji_set,
        Some(ChannelStickerSetRef::ShortName("demo_emojis".to_owned()))
    );
    f.rpc.clear_recorded();

    // 3. `channels.updatePaidMessagesPrice` for broadcast (flag set) vs megagroup (flag forced clear).
    f.rpc.enqueue_ok(
        tl_fns::channels::UpdatePaidMessagesPrice::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .update_channel_paid_messages_price(f.broadcast_pid, true, 25)
        .expect("broadcast paid price");
    f.rpc.enqueue_ok(
        tl_fns::channels::UpdatePaidMessagesPrice::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .update_channel_paid_messages_price(f.megagroup_pid, true, 10)
        .expect("megagroup paid price");
    let calls = f
        .rpc
        .calls_of::<tl_fns::channels::UpdatePaidMessagesPrice>();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, 0x4b12_327b);
    let bc_req =
        tl_fns::channels::UpdatePaidMessagesPrice::from_bytes(&calls[0].1).expect("decode");
    assert!(bc_req.broadcast_messages_allowed);
    assert_eq!(bc_req.send_paid_messages_stars, 25);
    let mg_req =
        tl_fns::channels::UpdatePaidMessagesPrice::from_bytes(&calls[1].1).expect("decode");
    assert!(
        !mg_req.broadcast_messages_allowed,
        "megagroup must never set broadcast_messages_allowed"
    );
    assert_eq!(mg_req.send_paid_messages_stars, 10);
    let bc_state = f
        .live
        .channel_management_state(f.broadcast_pid)
        .expect("state");
    assert!(bc_state.broadcast_messages_allowed);
    assert_eq!(bc_state.stars_per_message, 25);
    f.rpc.clear_recorded();

    // 4. `channels.getGroupsForDiscussion` ingests chats into `Session`.
    f.rpc.enqueue_ok(
        tl_fns::channels::GetGroupsForDiscussion::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
            chats: vec![synthetic_tl_channel(8801, 4455, "Discussion Hub", true)],
        }),
    );
    let groups = f
        .live
        .groups_for_discussion()
        .expect("groups_for_discussion");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].title, "Discussion Hub");
    assert!(groups[0].is_megagroup);
    assert_eq!(groups[0].members_count, 42);
    assert_eq!(
        f.rpc.calls_of::<tl_fns::channels::GetGroupsForDiscussion>()[0].0,
        0xf5da_d378
    );
    f.live.read_session(|s| {
        assert!(s.channel(ChannelId(8801)).is_some());
    });
    f.rpc.clear_recorded();

    // 5. `messages.getAdminsWithInvites` ingests users and returns counts.
    f.rpc.enqueue_ok(
        tl_fns::messages::GetAdminsWithInvites::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatAdminsWithInvites::ChatAdminsWithInvites(
            tl_types::messages::ChatAdminsWithInvites {
                admins: vec![
                    tl_enums::ChatAdminWithInvites::ChatAdminWithInvites(
                        tl_types::ChatAdminWithInvites {
                            admin_id: 100,
                            invites_count: 3,
                            revoked_invites_count: 1,
                        },
                    ),
                    tl_enums::ChatAdminWithInvites::ChatAdminWithInvites(
                        tl_types::ChatAdminWithInvites {
                            admin_id: 205,
                            invites_count: 5,
                            revoked_invites_count: 2,
                        },
                    ),
                ],
                users: vec![synthetic_tl_user(205, "Admin Alpha", "admin_alpha", false)],
            },
        ),
    );
    let admins = f
        .live
        .admins_with_invites(f.megagroup_pid)
        .expect("admins_with_invites");
    assert_eq!(admins.len(), 2);
    assert!(admins[0].is_self);
    assert_eq!(admins[0].invites_count, 3);
    assert_eq!(admins[0].revoked_invites_count, 1);
    assert!(!admins[1].is_self);
    assert_eq!(admins[1].name, "Admin Alpha");
    assert_eq!(admins[1].username.as_deref(), Some("admin_alpha"));
    assert_eq!(admins[1].invites_count, 5);
    assert_eq!(admins[1].revoked_invites_count, 2);
    assert_eq!(
        f.rpc.calls_of::<tl_fns::messages::GetAdminsWithInvites>()[0].0,
        0x3920_e6ef
    );
    f.rpc.clear_recorded();

    // 6. `messages.deleteChat` with `PEER_ID_INVALID` auto-join (`messages.addChatUser(self, 0)`) + retry.
    f.rpc.enqueue_rpc_err(
        tl_fns::messages::DeleteChat::CONSTRUCTOR_ID,
        400,
        "PEER_ID_INVALID",
    );
    f.rpc.enqueue_ok(
        tl_fns::messages::AddChatUser::CONSTRUCTOR_ID,
        &tl_enums::messages::InvitedUsers::InvitedUsers(tl_types::messages::InvitedUsers {
            updates: empty_updates(),
            missing_invitees: Vec::new(),
        }),
    );
    f.rpc
        .enqueue_ok(tl_fns::messages::DeleteChat::CONSTRUCTOR_ID, &true);
    f.live
        .delete_basic_chat(f.basic_chat_pid)
        .expect("delete_basic_chat with PEER_ID_INVALID retry");
    let calls = f.rpc.calls_matching(&[
        tl_fns::messages::DeleteChat::CONSTRUCTOR_ID,
        tl_fns::messages::AddChatUser::CONSTRUCTOR_ID,
    ]);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, 0x5bd0_ee50);
    assert_eq!(calls[1].0, tl_fns::messages::AddChatUser::CONSTRUCTOR_ID);
    let join_req = tl_fns::messages::AddChatUser::from_bytes(&calls[1].1).expect("decode");
    assert_eq!(join_req.chat_id, f.basic_chat_id.0 as i64);
    assert_eq!(join_req.user_id, tl_enums::InputUser::Self_);
    assert_eq!(join_req.fwd_limit, 0);
    assert_eq!(calls[2].0, 0x5bd0_ee50);
    f.live.read_session(|s| {
        let Some(Peer::Chat(chat)) = s.peer(DataPeerId::from_chat(f.basic_chat_id)) else {
            panic!("basic chat missing");
        };
        assert!(chat.flags.contains(ChatFlags::DEACTIVATED));
        assert!(chat.flags.contains(ChatFlags::LEFT));
    });

    // Verify `Bool::False` wire encoding on `togglePreHistoryHidden(false)` as well.
    f.rpc.clear_recorded();
    f.rpc.enqueue_ok(
        tl_fns::channels::TogglePreHistoryHidden::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    f.live
        .toggle_pre_history_hidden(f.megagroup_pid, false)
        .expect("toggle false");
    let bytes = &f.rpc.calls_of::<tl_fns::channels::TogglePreHistoryHidden>()[0].1;
    assert_eq!(
        u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap()),
        BOOL_FALSE_ID
    );
}
