//! Live Layer-233 RPC tests for WP-27 (`live/communities.rs`):
//! - Communities (`communities.create`, `getJoinedCommunities`, `togglePeerLink`,
//!   `toggleCommunityCollapsedInDialogs` + rollback, `getPeerLinkRequests`,
//!   `togglePeerLinkRequestApproval`, `toggleAllPeerLinkRequestApproval`,
//!   `toggleParticipantBanned`, `getParticipantJoinedChats`, `channels.getFullChannel`
//!   returning `CommunityFull`, `ManageCommunityBox` settings & delete)
//! - Managed Bots (`bots.checkUsername`, `bots.createBot`, `bots.getBotInfo`,
//!   `bots.setBotInfo`, `bots.getAdminedBots`, `messages.translateText`)
//! - Group Moderation (`ModerateMessagesBox`: `channels.deleteMessages`, `channels.reportSpam`,
//!   `channels.deleteParticipantHistory`, `channels.editBanned`,
//!   `communities.toggleParticipantBanned`)
//! - Welcome Messages (`ephemeral.getWelcomeMessages` + `NotModified`, `ephemeral.sendMessage`,
//!   `ephemeral.editMessage`, `ephemeral.deleteWelcomeMessage`,
//!   `ephemeral.deleteAllWelcomeMessages`)
//! - Channel Anti-Spam (`channels.toggleAntiSpam`, `channels.reportAntiSpamFalsePositive`)
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs/users.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{ChannelFlags, ChannelId, Peer, PeerId as DataPeerId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::core_types::dto::community::{
    COMMUNITY_REQUEST_CREATED, CommunityPeerLinkAction, CreateCommunityRequest,
};
use tg_app_core::core_types::dto::managed_bot::BotInfoChange;
use tg_app_core::core_types::dto::moderate_messages::ModerateMessagesRequest;
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::info::backend::InfoBackend;
use tg_app_core::stand_in::types::PeerId;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

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

fn synthetic_user(id: i64, first_name: &str, username: &str, is_bot: bool) -> tl_types::User {
    tl_types::User {
        self_: id == 1000,
        contact: id != 1000 && !is_bot,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
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
        bot_can_edit: is_bot,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: is_bot,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 100 + 7),
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
    }
}

fn synthetic_channel(id: i64, title: &str, megagroup: bool) -> tl_types::Channel {
    tl_types::Channel {
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
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 50 + 3),
        title: title.to_owned(),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_795_348_800,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(250),
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
    }
}

fn synthetic_community(id: i64, title: &str, collapsed: bool) -> tl_types::Community {
    tl_types::Community {
        creator: true,
        left: false,
        min: false,
        collapsed_in_dialogs: collapsed,
        id,
        access_hash: Some(id * 77 + 11),
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_795_348_800,
        admin_rights: None,
        default_banned_rights: None,
    }
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_348_800,
        seq: 1,
    })
}

fn build_live_session() -> (
    Arc<SessionRuntime<crate::ErasedRpcClient>>,
    FakeRpcClient,
    LiveSession,
) {
    let rpc = FakeRpcClient::new(2);
    let self_user = synthetic_user(1000, "Taylor", "taylor_self", false);
    rpc.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user)],
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_795_348_800,
            seq: 1,
            unread_count: 0,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_795_348_801,
            seq: 1,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_795_348_801,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(1000),
            crate::ErasedRpcClient::new(rpc.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start SessionRuntime"),
    );
    runtime
        .tokio_handle()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("startup ready");

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let users = vec![
            tl_enums::User::User(synthetic_user(1000, "Taylor", "taylor_self", false)),
            tl_enums::User::User(synthetic_user(1001, "Mira", "mira_demo", false)),
            tl_enums::User::User(synthetic_user(5001, "ManagerBot", "manager_demo_bot", true)),
            tl_enums::User::User(synthetic_user(5002, "WorkerBot", "worker_demo_bot", true)),
        ];
        let chats = vec![
            tl_enums::Chat::Channel(synthetic_channel(3001, "Dev Supergroup", true)),
            tl_enums::Chat::Channel(synthetic_channel(3002, "Release Feed", false)),
            tl_enums::Chat::Community(synthetic_community(4001, "Core Hub", false)),
        ];
        apply_tl_users_and_chats(&mut s, &users, &chats);
    });
    let live = LiveSession::new(Arc::clone(&runtime), 27);
    (runtime, rpc, live)
}

#[test]
fn live_communities_create_list_link_collapse_and_rollback() {
    let (_rt, rpc, mut live) = build_live_session();
    let group_id = PeerId::from_channel(3001);
    let channel_id = PeerId::from_channel(3002);
    let comm_id = PeerId::from_channel(4001);

    // 1. communities.create -> returns Updates containing Chat::Community(4002)
    let created_comm = synthetic_community(4002, "New Contributors Hub", false);
    rpc.enqueue_ok(
        tl_fns::communities::Create::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: vec![tl_enums::Chat::Community(created_comm)],
            date: 1_795_348_800,
            seq: 2,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
            full_chat: tl_enums::ChatFull::CommunityFull(tl_types::CommunityFull {
                id: 4002,
                about: "Testing hub".to_owned(),
                chat_photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
                linked_peers: vec![tl_enums::CommunityPeer::CommunityPeer(
                    tl_types::CommunityPeer {
                        can_view_history: true,
                        visible: Some(true),
                        peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 3001 }),
                    },
                )],
                admins_count: Some(1),
                kicked_count: Some(0),
                peer_link_requests_pending: Some(0),
            }),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let new_cid = live
        .create_community(CreateCommunityRequest {
            title: "New Contributors Hub".to_owned(),
            about: "Testing hub".to_owned(),
            initial_peer_id: group_id,
            hidden: false,
        })
        .expect("create_community");
    assert_eq!(new_cid, PeerId::from_channel(4002));

    // 2. communities.getJoinedCommunities
    rpc.enqueue_ok(
        tl_fns::communities::GetJoinedCommunities::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
            chats: vec![tl_enums::Chat::Community(synthetic_community(
                4001, "Core Hub", false,
            ))],
        }),
    );
    let joined = live.joined_communities().expect("joined_communities");
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].id, comm_id);

    // 3. communities.togglePeerLink (Visible, Hidden, Deleted, plus COMMUNITY_REQUEST_CREATED error)
    rpc.enqueue_ok(tl_fns::communities::TogglePeerLink::CONSTRUCTOR_ID, &true);
    live.toggle_community_peer_link(comm_id, channel_id, CommunityPeerLinkAction::Visible)
        .expect("toggle visible");

    rpc.enqueue_rpc_err(
        tl_fns::communities::TogglePeerLink::CONSTRUCTOR_ID,
        400,
        COMMUNITY_REQUEST_CREATED,
    );
    let req_err = live
        .toggle_community_peer_link(comm_id, group_id, CommunityPeerLinkAction::Hidden)
        .expect_err("expected COMMUNITY_REQUEST_CREATED");
    assert!(req_err.contains(COMMUNITY_REQUEST_CREATED));

    // 4. communities.toggleCommunityCollapsedInDialogs: success then failure rollback
    rpc.enqueue_ok(
        tl_fns::communities::ToggleCommunityCollapsedInDialogs::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.toggle_community_collapsed_in_dialogs(comm_id, true)
        .expect("collapse community");
    let groups = live.community_groups();
    assert!(
        groups
            .iter()
            .any(|g| g.community_id == comm_id && g.collapsed_in_dialogs)
    );

    rpc.enqueue_rpc_err(
        tl_fns::communities::ToggleCommunityCollapsedInDialogs::CONSTRUCTOR_ID,
        500,
        "INTERNAL",
    );
    assert!(
        live.toggle_community_collapsed_in_dialogs(comm_id, false)
            .is_err()
    );
    // Must stay collapsed (`true`) after rollback!
    let collapsed_after_fail = live
        .community_groups()
        .into_iter()
        .find(|g| g.community_id == comm_id)
        .expect("community group")
        .collapsed_in_dialogs;
    assert!(collapsed_after_fail);

    let ctors: Vec<u32> = rpc.recorded_calls().into_iter().map(|(c, _)| c).collect();
    assert!(ctors.contains(&tl_fns::communities::Create::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::communities::GetJoinedCommunities::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::communities::TogglePeerLink::CONSTRUCTOR_ID));
    assert!(
        ctors.contains(&tl_fns::communities::ToggleCommunityCollapsedInDialogs::CONSTRUCTOR_ID)
    );
}

#[test]
fn live_communities_requests_bans_settings_and_delete() {
    let (_rt, rpc, mut live) = build_live_session();
    let comm_id = PeerId::from_channel(4001);
    let group_id = PeerId::from_channel(3001);
    let user_id = PeerId::from_user(1001);

    // 1. communities.getPeerLinkRequests
    rpc.enqueue_ok(
        tl_fns::communities::GetPeerLinkRequests::CONSTRUCTOR_ID,
        &tl_enums::communities::PeerLinkRequests::PeerLinkRequests(
            tl_types::communities::PeerLinkRequests {
                total_count: 1,
                requests: vec![tl_enums::CommunityPeerRequest::CommunityPeerRequest(
                    tl_types::CommunityPeerRequest {
                        visible: true,
                        peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 3001 }),
                        requested_by: 1001,
                        date: 1_795_348_700,
                    },
                )],
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    let slice = live
        .community_peer_link_requests(comm_id, "", 20)
        .expect("getPeerLinkRequests");
    assert_eq!(slice.total_count, 1);
    assert_eq!(slice.list[0].peer_id, group_id);
    assert_eq!(slice.list[0].requested_by, Some(user_id));
    assert!(slice.list[0].visible);

    // 2. Approve single + reject all
    rpc.enqueue_ok(
        tl_fns::communities::TogglePeerLinkRequestApproval::CONSTRUCTOR_ID,
        &true,
    );
    live.toggle_community_peer_link_request_approval(comm_id, group_id, false)
        .expect("approve single");

    rpc.enqueue_ok(
        tl_fns::communities::ToggleAllPeerLinkRequestApproval::CONSTRUCTOR_ID,
        &true,
    );
    live.toggle_all_community_peer_link_request_approval(comm_id, true)
        .expect("reject all");

    // 3. Ban participant + getParticipantJoinedChats
    rpc.enqueue_ok(
        tl_fns::communities::ToggleParticipantBanned::CONSTRUCTOR_ID,
        &true,
    );
    live.toggle_community_participant_banned(comm_id, user_id, false)
        .expect("ban participant");

    rpc.enqueue_ok(
        tl_fns::communities::GetParticipantJoinedChats::CONSTRUCTOR_ID,
        &tl_enums::communities::ParticipantJoinedChats::ParticipantJoinedChats(
            tl_types::communities::ParticipantJoinedChats {
                creator_chat_ids: vec![3001],
                joined_chat_ids: vec![3001, 3002],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    let p_chats = live
        .community_participant_joined_chats(comm_id, user_id)
        .expect("participant joined chats");
    assert_eq!(p_chats.creator_chat_ids, vec![PeerId::from_channel(3001)]);
    assert_eq!(p_chats.joined_chat_ids.len(), 2);

    // 4. ManageCommunityBox save settings (editTitle + editChatDefaultBannedRights) + deleteChannel
    rpc.enqueue_ok(
        tl_fns::channels::EditTitle::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    rpc.enqueue_ok(
        tl_fns::messages::EditChatDefaultBannedRights::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.update_community_settings(comm_id, "Renamed Hub", true)
        .expect("update community settings");

    rpc.enqueue_ok(
        tl_fns::channels::DeleteChannel::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.delete_community(comm_id).expect("delete community");

    let calls = rpc.recorded_calls();
    // Verify editChatDefaultBannedRights actually set manage_linked_peers = true
    let (_, rights_bytes) = calls
        .iter()
        .find(|(c, _)| *c == tl_fns::messages::EditChatDefaultBannedRights::CONSTRUCTOR_ID)
        .expect("editChatDefaultBannedRights recorded");
    let decoded = tl_fns::messages::EditChatDefaultBannedRights::from_bytes(rights_bytes)
        .expect("decode EditChatDefaultBannedRights");
    let tl_enums::ChatBannedRights::ChatBannedRights(br) = decoded.banned_rights;
    assert!(br.manage_linked_peers);
}

#[test]
fn live_managed_bots_create_info_translate_and_admined() {
    let (_rt, rpc, mut live) = build_live_session();
    let manager_id = PeerId::from_user(5001);
    let worker_id = PeerId::from_user(5002);

    // 1. bots.checkUsername
    rpc.enqueue_ok(tl_fns::bots::CheckUsername::CONSTRUCTOR_ID, &true);
    assert!(
        live.check_bot_username("my_demo_bot")
            .expect("check username")
    );

    // 2. bots.createBot (with via_deeplink = true)
    rpc.enqueue_ok(
        tl_fns::bots::CreateBot::CONSTRUCTOR_ID,
        &tl_enums::User::User(synthetic_user(5009, "My Demo Bot", "my_demo_bot", true)),
    );
    let created_id = live
        .create_managed_bot(manager_id, "My Demo Bot", "my_demo_bot", true)
        .expect("create managed bot");
    assert_eq!(created_id, PeerId::from_user(5009));

    let calls = rpc.recorded_calls();
    let (_, create_bytes) = calls
        .iter()
        .find(|(c, _)| *c == tl_fns::bots::CreateBot::CONSTRUCTOR_ID)
        .expect("createBot recorded");
    let decoded_create =
        tl_fns::bots::CreateBot::from_bytes(create_bytes).expect("decode CreateBot");
    assert!(decoded_create.via_deeplink);
    assert_eq!(decoded_create.username, "my_demo_bot");

    // 3. bots.getBotInfo + bots.setBotInfo
    rpc.enqueue_ok(
        tl_fns::bots::GetBotInfo::CONSTRUCTOR_ID,
        &tl_enums::bots::BotInfo::BotInfo(tl_types::bots::BotInfo {
            name: "Worker Bot".to_owned(),
            about: "Automates QA".to_owned(),
            description: "Runs synthetic tests.".to_owned(),
        }),
    );
    let info = live.get_bot_info(worker_id, "").expect("getBotInfo");
    assert_eq!(info.name, "Worker Bot");
    assert_eq!(info.about, "Automates QA");

    rpc.enqueue_ok(tl_fns::bots::SetBotInfo::CONSTRUCTOR_ID, &true);
    live.set_bot_info(
        worker_id,
        "es",
        &BotInfoChange {
            name: Some("Bot Trabajador".to_owned()),
            about: Some("Automatiza QA".to_owned()),
            description: None,
        },
    )
    .expect("setBotInfo");

    // 4. messages.translateText for bot info localization
    rpc.enqueue_ok(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedText::TranslateResult(tl_types::messages::TranslateResult {
            result: vec![tl_enums::TextWithEntities::TextWithEntities(
                tl_types::TextWithEntities {
                    text: "Bot de Trabajo".to_owned(),
                    entities: Vec::new(),
                },
            )],
        }),
    );
    let translated = live
        .translate_bot_info_texts(&["Worker Bot".to_owned()], "es")
        .expect("translate_bot_info_texts");
    assert_eq!(translated, vec!["Bot de Trabajo".to_owned()]);

    // 5. bots.getAdminedBots
    rpc.enqueue_ok(
        tl_fns::bots::GetAdminedBots::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_user(
            5002,
            "WorkerBot",
            "worker_demo_bot",
            true,
        ))],
    );
    let admined = live.admined_bots().expect("admined_bots");
    assert_eq!(admined, vec![worker_id]);
}

#[test]
fn live_moderate_messages_executes_full_rpc_sequence() {
    let (_rt, rpc, mut live) = build_live_session();
    let group_id = PeerId::from_channel(3001);
    let comm_id = PeerId::from_channel(4001);
    let offender = PeerId::from_user(1001);

    rpc.enqueue_ok(
        tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 10,
                pts_count: 2,
            },
        ),
    );
    rpc.enqueue_ok(tl_fns::channels::ReportSpam::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::channels::DeleteParticipantHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 11,
                pts_count: 1,
                offset: 0,
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::channels::EditBanned::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    rpc.enqueue_ok(
        tl_fns::communities::ToggleParticipantBanned::CONSTRUCTOR_ID,
        &true,
    );

    let res = live
        .moderate_messages(ModerateMessagesRequest {
            peer_id: group_id,
            message_ids: vec![41, 42],
            report_spam_peers: vec![offender],
            delete_all_from_peers: vec![offender],
            delete_all_reactions_peers: Vec::new(),
            ban_peers: vec![offender],
            restrict_duration_secs: None,
            ban_in_community_id: Some(comm_id),
        })
        .expect("moderate_messages");

    assert_eq!(res.deleted_selected, 2);
    assert_eq!(res.reported_peers, 1);
    assert_eq!(res.deleted_all_peers, 1);
    assert_eq!(res.banned_peers, 1);
    assert_eq!(res.community_banned_peers, 1);

    let ctors: Vec<u32> = rpc.recorded_calls().into_iter().map(|(c, _)| c).collect();
    assert!(ctors.contains(&tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::channels::ReportSpam::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::channels::DeleteParticipantHistory::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::channels::EditBanned::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::communities::ToggleParticipantBanned::CONSTRUCTOR_ID));
}

#[test]
fn live_welcome_messages_crud_and_not_modified_cache() {
    let (_rt, rpc, mut live) = build_live_session();
    let group_id = PeerId::from_channel(3001);

    // 1. Initial ephemeral.getWelcomeMessages
    rpc.enqueue_ok(
        tl_fns::ephemeral::GetWelcomeMessages::CONSTRUCTOR_ID,
        &tl_enums::ephemeral::WelcomeMessages::WelcomeMessages(
            tl_types::ephemeral::WelcomeMessages {
                hash: 998877,
                messages: vec![tl_enums::EphemeralMessage::EphemeralMessage(
                    tl_types::EphemeralMessage {
                        out: true,
                        welcome_template: true,
                        invert_media: false,
                        noforwards: false,
                        id: 1,
                        from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 1000 }),
                        peer_id: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: 3001,
                        })),
                        receiver_id: 0,
                        top_msg_id: None,
                        date: 1_795_348_000,
                        message: "Welcome aboard!".to_owned(),
                        entities: None,
                        media: None,
                        reply_markup: None,
                        reply_to: None,
                        rich_message: None,
                        chat_instance: None,
                        anchor_msg_id: None,
                    },
                )],
            },
        ),
    );
    let initial = live.welcome_messages(group_id).expect("getWelcomeMessages");
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].text, "Welcome aboard!");

    // 2. Second fetch returns NotModified and serves cached messages with hash=998877
    rpc.enqueue_ok(
        tl_fns::ephemeral::GetWelcomeMessages::CONSTRUCTOR_ID,
        &tl_enums::ephemeral::WelcomeMessages::NotModified,
    );
    let cached = live.welcome_messages(group_id).expect("not modified cache");
    assert_eq!(cached.len(), 1);
    assert_eq!(cached[0].text, "Welcome aboard!");

    // 3. send / edit / delete / delete_all
    rpc.enqueue_ok(
        tl_fns::ephemeral::SendMessage::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let added = live
        .send_welcome_message(group_id, "Check pinned docs")
        .expect("send welcome");
    assert_eq!(added.text, "Check pinned docs");

    rpc.enqueue_ok(
        tl_fns::ephemeral::EditMessage::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let edited = live
        .edit_welcome_message(group_id, added.id, "Check pinned architecture docs")
        .expect("edit welcome");
    assert_eq!(edited.text, "Check pinned architecture docs");

    rpc.enqueue_ok(
        tl_fns::ephemeral::DeleteWelcomeMessage::CONSTRUCTOR_ID,
        &true,
    );
    live.delete_welcome_message(group_id, added.id)
        .expect("delete welcome");

    rpc.enqueue_ok(
        tl_fns::ephemeral::DeleteAllWelcomeMessages::CONSTRUCTOR_ID,
        &true,
    );
    live.delete_all_welcome_messages(group_id)
        .expect("delete all welcome");

    let calls = rpc.recorded_calls();
    // Confirm ephemeral.sendMessage set `welcome = true`
    let (_, send_bytes) = calls
        .iter()
        .find(|(c, _)| *c == tl_fns::ephemeral::SendMessage::CONSTRUCTOR_ID)
        .expect("sendMessage recorded");
    let decoded_send =
        tl_fns::ephemeral::SendMessage::from_bytes(send_bytes).expect("decode SendMessage");
    assert!(decoded_send.welcome);
    assert_eq!(decoded_send.message, "Check pinned docs");
}

#[test]
fn live_channel_antispam_toggle_and_false_positive_report() {
    let (_rt, rpc, mut live) = build_live_session();
    let group_id = PeerId::from_channel(3001);
    assert!(!live.channel_anti_spam_enabled(group_id));

    rpc.enqueue_ok(
        tl_fns::channels::ToggleAntiSpam::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.toggle_channel_anti_spam(group_id, true)
        .expect("enable anti-spam");
    assert!(live.channel_anti_spam_enabled(group_id));

    // Verify ChannelFlags::ANTI_SPAM was persisted in tg_data::Session
    let has_flag = {
        let s = live.runtime().session().try_read().expect("read session");
        let dp = DataPeerId::from_channel(ChannelId::from_tl(3001));
        matches!(
            s.peer(dp),
            Some(Peer::Channel(ch)) if ch.flags.contains(ChannelFlags::ANTI_SPAM)
        )
    };
    assert!(has_flag);

    rpc.enqueue_ok(
        tl_fns::channels::ReportAntiSpamFalsePositive::CONSTRUCTOR_ID,
        &true,
    );
    live.report_antispam_false_positive(group_id, 77)
        .expect("report anti-spam false positive");

    let ctors: Vec<u32> = rpc.recorded_calls().into_iter().map(|(c, _)| c).collect();
    assert!(ctors.contains(&tl_fns::channels::ToggleAntiSpam::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::channels::ReportAntiSpamFalsePositive::CONSTRUCTOR_ID));
    let _ = TEST_TIMEOUT;
}
