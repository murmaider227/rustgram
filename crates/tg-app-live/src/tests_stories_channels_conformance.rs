//! Protocol conformance tests for live channels/admin (`admin.rs`) and stories (`stories.rs`)
//! fixes in `tg-app-live`, using an in-process `FakeRpcClient` (synthetic data only).
//!
//! Covers:
//! 1. `admin.rs` `update_admin_rights` (`channels.editAdmin`): sends `rank: Some("")` when
//!    clearing rank or demoting (`boxes/peers/edit_participants_box.cpp:63-78, 165-183`).
//! 2. `admin.rs` `remove_member` & `banned_rights_to_tl` (`channels.editBanned`): uses
//!    `kicked_restrictions_for_peer` and `tg_api::convert::banned_rights_to_tl` (no legacy
//!    `send_messages`/`send_media` bits; non-user peers get `VIEW_MESSAGES` only,
//!    `data/data_channel.cpp:495-514`, `data/data_chat_participant_status.cpp:147-177`).
//! 3. `admin.rs` `get_exported_chat_invites` & `get_chat_invite_importers`: cursor pagination
//!    with `(offset_date, offset_link)` and `(offset_date, offset_user)` and tdesktop page sizes
//!    (`10` first page -> `50` subsequent pages, `api/api_invite_links.cpp:24-26, 415-456, 598-618, 772-793`).
//! 4. `stories.rs` + `bridge.rs`: `stories.sendReaction` with `add_to_recent: false`
//!    (`data/data_stories.cpp:1117-1130`); `fail_album_stories` / `fail_albums` on RPC failure
//!    (`data/data_stories.cpp:1882-1891, 1958-1962`); consuming `SessionEvent::StoryUpdate` from
//!    `updates.getDifference` and live updates without double-applying (`api/api_updates.cpp:2947-2958`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::channels::{
    EXPORTED_INVITES_FIRST_PAGE, EXPORTED_INVITES_PER_PAGE, INVITE_IMPORTERS_FIRST_PAGE,
    INVITE_IMPORTERS_PER_PAGE,
};
use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::rights::{ChatAdminRights, ChatBannedRights};
use tg_data::{FullStoryId, PeerId as DataPeerId, ReactionId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_stories::{STORIES_ALBUM_ID_SAVED, StoriesEvent, StoriesStore, StoryAlbumIdsKey};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::info::backend::InfoBackend;
use tg_app_core::stand_in::types::PeerId;
use tg_app_core::stories::backend::StoriesBackend;

type RecordedRpc = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpc>>>,
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

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    fn recorded_calls(&self) -> Vec<RecordedRpc> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes(bytes[..4].try_into().unwrap_or([0; 4]));
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, res)) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC constructor: got 0x{ctor:08x}, expected 0x{expected:08x}"
                );
                let raw = res?;
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Dropped),
        }
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
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

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
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
        access_hash: Some(id * 100 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Fixture".to_owned()),
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

fn synthetic_tl_channel(id: i64, title: &str, broadcast: bool) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified: false,
        megagroup: !broadcast,
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
        access_hash: Some(id * 31 + 11),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
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
    }
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_000,
        seq: 0,
    })
}

fn build_live_session(
    self_uid: i64,
) -> (
    FakeRpcClient,
    Arc<SessionRuntime<crate::ErasedRpcClient>>,
    LiveSession,
) {
    let rpc = FakeRpcClient::new();
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(self_uid),
            crate::ErasedRpcClient::new(rpc.clone()),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    let live = LiveSession::new(Arc::clone(&runtime), 1);
    live.write_session(|s| {
        apply_tl_users_and_chats(
            s,
            &[tl_enums::User::User(synthetic_tl_user(
                self_uid,
                "Self",
                "self_fixture",
                true,
            ))],
            &[],
        );
    });
    (rpc, runtime, live)
}

fn shutdown_live(live: LiveSession, runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>) {
    drop(live);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

#[test]
fn edit_admin_sends_explicit_empty_rank_when_clearing_or_demoting() {
    let self_uid = 800_001i64;
    let member_uid = 800_002i64;
    let channel_id = 900_001i64;
    let (rpc, runtime, mut live) = build_live_session(self_uid);

    live.write_session(|s| {
        apply_tl_users_and_chats(
            s,
            &[tl_enums::User::User(synthetic_tl_user(
                member_uid,
                "Helper",
                "helper_user",
                false,
            ))],
            &[tl_enums::Chat::Channel(synthetic_tl_channel(
                channel_id,
                "Supergroup Fixture",
                false,
            ))],
        );
    });

    let ch_peer = PeerId::from_channel(channel_id as u64);
    let member_peer = PeerId::from_user(member_uid as u64);

    // 1. Promote with custom title -> sends trimmed Some("Moderator").
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::channels::EditAdmin::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.update_admin_rights(
        ch_peer,
        member_peer,
        ChatAdminRights::DELETE_MESSAGES | ChatAdminRights::BAN_USERS,
        "  Moderator  ".to_owned(),
    )
    .expect("promote admin");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req_promote =
        tl_fns::channels::EditAdmin::from_bytes(&calls[0].1).expect("decode EditAdmin promote");
    assert_eq!(req_promote.rank.as_deref(), Some("Moderator"));
    let tl_enums::ChatAdminRights::ChatAdminRights(r) = req_promote.admin_rights;
    assert!(r.delete_messages && r.ban_users && !r.add_admins);

    // 2. Clear custom title (`""` / whitespace) -> MUST send `rank: Some("")`, NOT `None`
    //    (`SaveChannelAdmin` in `boxes/peers/edit_participants_box.cpp:165-183`).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::channels::EditAdmin::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.update_admin_rights(
        ch_peer,
        member_peer,
        ChatAdminRights::DELETE_MESSAGES,
        "   ".to_owned(),
    )
    .expect("clear admin rank");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req_clear =
        tl_fns::channels::EditAdmin::from_bytes(&calls[0].1).expect("decode EditAdmin clear");
    assert_eq!(
        req_clear.rank.as_deref(),
        Some(""),
        "clearing rank must set flags.0 with empty string"
    );

    // 3. Demote (`ChatAdminRights::empty()`) -> MUST send empty admin_rights AND `rank: Some("")`
    //    even if a stale non-empty custom_title was passed
    //    (`RemoveAdmin` in `boxes/peers/edit_participants_box.cpp:63-78`).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::channels::EditAdmin::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.update_admin_rights(
        ch_peer,
        member_peer,
        ChatAdminRights::empty(),
        "Stale Title".to_owned(),
    )
    .expect("demote admin");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req_demote =
        tl_fns::channels::EditAdmin::from_bytes(&calls[0].1).expect("decode EditAdmin demote");
    assert_eq!(
        req_demote.rank.as_deref(),
        Some(""),
        "demoting admin must send rank: Some(\"\")"
    );
    assert_eq!(
        req_demote.admin_rights,
        tg_api::convert::admin_rights_to_tl(ChatAdminRights::empty())
    );

    shutdown_live(live, runtime);
}

#[test]
fn remove_member_uses_kicked_restrictions_without_legacy_bits_and_view_only_for_non_users() {
    let self_uid = 800_011i64;
    let member_uid = 800_012i64;
    let group_id = 900_011i64;
    let sender_channel_id = 900_012i64;
    let (rpc, runtime, mut live) = build_live_session(self_uid);

    live.write_session(|s| {
        apply_tl_users_and_chats(
            s,
            &[tl_enums::User::User(synthetic_tl_user(
                member_uid,
                "Member",
                "member_user",
                false,
            ))],
            &[
                tl_enums::Chat::Channel(synthetic_tl_channel(group_id, "Supergroup", false)),
                tl_enums::Chat::Channel(synthetic_tl_channel(
                    sender_channel_id,
                    "Posting Channel",
                    true,
                )),
            ],
        );
    });

    let group_peer = PeerId::from_channel(group_id as u64);
    let member_peer = PeerId::from_user(member_uid as u64);
    let sender_channel_peer = PeerId::from_channel(sender_channel_id as u64);

    // 1. Kick a user peer -> granular KICKED_USER bits, NO legacy `send_messages`/`send_media`,
    //    `until_date == 0` (`ChannelData::KickedRestrictedRights` in `data/data_channel.cpp:495-514`,
    //    `RestrictionsToMTP` in `data/data_chat_participant_status.cpp:147-177`).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::channels::EditBanned::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.remove_member(group_peer, member_peer)
        .expect("kick user member");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req_user =
        tl_fns::channels::EditBanned::from_bytes(&calls[0].1).expect("decode EditBanned user");
    let tl_enums::ChatBannedRights::ChatBannedRights(br_user) = req_user.banned_rights;
    assert!(br_user.view_messages);
    assert!(
        !br_user.send_messages,
        "legacy send_messages bit must not be set"
    );
    assert!(!br_user.send_media, "legacy send_media bit must not be set");
    assert!(br_user.send_stickers);
    assert!(br_user.send_gifs);
    assert!(br_user.send_games);
    assert!(br_user.send_inline);
    assert!(br_user.embed_links);
    assert!(br_user.send_photos);
    assert!(br_user.send_videos);
    assert!(br_user.send_roundvideos);
    assert!(br_user.send_audios);
    assert!(br_user.send_voices);
    assert!(br_user.send_docs);
    assert!(br_user.send_plain);
    assert_eq!(br_user.until_date, 0);

    // 2. Kick a non-user peer (channel) -> VIEW_MESSAGES only (`data/data_channel.cpp:512`).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::channels::EditBanned::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.remove_member(group_peer, sender_channel_peer)
        .expect("kick channel peer");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req_chan =
        tl_fns::channels::EditBanned::from_bytes(&calls[0].1).expect("decode EditBanned channel");
    let tl_enums::ChatBannedRights::ChatBannedRights(br_chan) = req_chan.banned_rights;
    assert!(br_chan.view_messages);
    assert!(!br_chan.send_messages);
    assert!(!br_chan.send_media);
    assert!(!br_chan.send_stickers);
    assert!(!br_chan.send_gifs);
    assert!(!br_chan.send_games);
    assert!(!br_chan.send_inline);
    assert!(!br_chan.embed_links);
    assert!(!br_chan.send_polls);
    assert!(!br_chan.send_photos);
    assert!(!br_chan.send_videos);
    assert!(!br_chan.send_roundvideos);
    assert!(!br_chan.send_audios);
    assert!(!br_chan.send_voices);
    assert!(!br_chan.send_docs);
    assert!(!br_chan.send_plain);
    assert_eq!(br_chan.until_date, 0);

    // 3. `update_default_permissions` also drops legacy `send_messages`/`send_media`.
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::messages::EditChatDefaultBannedRights::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.update_default_permissions(
        group_peer,
        ChatBannedRights::SEND_MESSAGES
            | ChatBannedRights::SEND_MEDIA
            | ChatBannedRights::SEND_POLLS,
        0,
    )
    .expect("update default permissions");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req_def = tl_fns::messages::EditChatDefaultBannedRights::from_bytes(&calls[0].1)
        .expect("decode EditChatDefaultBannedRights");
    let tl_enums::ChatBannedRights::ChatBannedRights(br_def) = req_def.banned_rights;
    assert!(!br_def.send_messages);
    assert!(!br_def.send_media);
    assert!(br_def.send_polls);
    assert_eq!(br_def.until_date, 0);

    shutdown_live(live, runtime);
}

fn synthetic_tl_exported_invite(
    link: &str,
    admin_id: i64,
    date: i32,
    revoked: bool,
) -> tl_enums::ExportedChatInvite {
    tl_enums::ExportedChatInvite::ChatInviteExported(tl_types::ChatInviteExported {
        revoked,
        permanent: false,
        request_needed: false,
        link: link.to_owned(),
        admin_id,
        date,
        start_date: None,
        expire_date: None,
        usage_limit: None,
        usage: Some(1),
        requested: None,
        subscription_expired: None,
        title: None,
        subscription_pricing: None,
    })
}

#[test]
fn get_exported_chat_invites_paginates_with_offset_date_and_offset_link_cursors() {
    let self_uid = 800_021i64;
    let channel_id = 900_021i64;
    let (rpc, runtime, live) = build_live_session(self_uid);

    live.write_session(|s| {
        apply_tl_users_and_chats(
            s,
            &[],
            &[tl_enums::Chat::Channel(synthetic_tl_channel(
                channel_id,
                "Invites Channel",
                true,
            ))],
        );
    });
    let ch_peer = PeerId::from_channel(channel_id as u64);

    // Enqueue 2 pages (`count = 3`: 2 links on page 1, 1 link on page 2).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::messages::GetExportedChatInvites::CONSTRUCTOR_ID,
        &tl_enums::messages::ExportedChatInvites::ExportedChatInvites(
            tl_types::messages::ExportedChatInvites {
                count: 3,
                invites: vec![
                    synthetic_tl_exported_invite(
                        "https://t.me/+inv1",
                        self_uid,
                        1_750_000_300,
                        true,
                    ),
                    synthetic_tl_exported_invite(
                        "https://t.me/+inv2",
                        self_uid,
                        1_750_000_200,
                        true,
                    ),
                ],
                users: Vec::new(),
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetExportedChatInvites::CONSTRUCTOR_ID,
        &tl_enums::messages::ExportedChatInvites::ExportedChatInvites(
            tl_types::messages::ExportedChatInvites {
                count: 3,
                invites: vec![synthetic_tl_exported_invite(
                    "https://t.me/+inv3",
                    self_uid,
                    1_750_000_100,
                    true,
                )],
                users: Vec::new(),
            },
        ),
    );

    let list = live
        .exported_chat_invites(ch_peer, true)
        .expect("paginated exported_chat_invites");
    assert_eq!(list.count, 3);
    assert_eq!(
        list.links
            .iter()
            .map(|l| l.link.as_str())
            .collect::<Vec<_>>(),
        vec![
            "https://t.me/+inv1",
            "https://t.me/+inv2",
            "https://t.me/+inv3",
        ]
    );

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2, "must issue exactly 2 paginated RPCs");

    let req0 = tl_fns::messages::GetExportedChatInvites::from_bytes(&calls[0].1)
        .expect("decode page 1 GetExportedChatInvites");
    assert!(req0.revoked);
    assert_eq!(req0.admin_id, tl_enums::InputUser::Self_);
    assert_eq!(req0.offset_date, None);
    assert_eq!(req0.offset_link, None);
    assert_eq!(req0.limit, EXPORTED_INVITES_FIRST_PAGE);

    let req1 = tl_fns::messages::GetExportedChatInvites::from_bytes(&calls[1].1)
        .expect("decode page 2 GetExportedChatInvites");
    assert!(req1.revoked);
    assert_eq!(req1.admin_id, tl_enums::InputUser::Self_);
    assert_eq!(req1.offset_date, Some(1_750_000_200));
    assert_eq!(req1.offset_link.as_deref(), Some("https://t.me/+inv2"));
    assert_eq!(req1.limit, EXPORTED_INVITES_PER_PAGE);

    shutdown_live(live, runtime);
}

#[test]
fn get_chat_invite_importers_paginates_with_offset_date_and_offset_user_cursors() {
    let self_uid = 800_031i64;
    let channel_id = 900_031i64;
    let u1 = 800_101i64;
    let u2 = 800_102i64;
    let u3 = 800_103i64;
    let (rpc, runtime, live) = build_live_session(self_uid);

    live.write_session(|s| {
        apply_tl_users_and_chats(
            s,
            &[],
            &[tl_enums::Chat::Channel(synthetic_tl_channel(
                channel_id,
                "Importers Group",
                false,
            ))],
        );
    });
    let ch_peer = PeerId::from_channel(channel_id as u64);

    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::messages::GetChatInviteImporters::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatInviteImporters::ChatInviteImporters(
            tl_types::messages::ChatInviteImporters {
                count: 3,
                importers: vec![
                    tl_enums::ChatInviteImporter::ChatInviteImporter(
                        tl_types::ChatInviteImporter {
                            requested: true,
                            via_chatlist: false,
                            user_id: u1,
                            date: 1_750_001_300,
                            about: None,
                            approved_by: None,
                        },
                    ),
                    tl_enums::ChatInviteImporter::ChatInviteImporter(
                        tl_types::ChatInviteImporter {
                            requested: true,
                            via_chatlist: false,
                            user_id: u2,
                            date: 1_750_001_200,
                            about: Some("Hi".to_owned()),
                            approved_by: None,
                        },
                    ),
                ],
                users: vec![
                    tl_enums::User::User(synthetic_tl_user(u1, "First", "first_imp", false)),
                    tl_enums::User::User(synthetic_tl_user(u2, "Second", "second_imp", false)),
                ],
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetChatInviteImporters::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatInviteImporters::ChatInviteImporters(
            tl_types::messages::ChatInviteImporters {
                count: 3,
                importers: vec![tl_enums::ChatInviteImporter::ChatInviteImporter(
                    tl_types::ChatInviteImporter {
                        requested: true,
                        via_chatlist: false,
                        user_id: u3,
                        date: 1_750_001_100,
                        about: None,
                        approved_by: None,
                    },
                )],
                users: vec![tl_enums::User::User(synthetic_tl_user(
                    u3,
                    "Third",
                    "third_imp",
                    false,
                ))],
            },
        ),
    );

    let list = live
        .chat_invite_importers(ch_peer, Some("https://t.me/+inv2"), true, "")
        .expect("paginated chat_invite_importers");
    assert_eq!(list.count, 3);
    assert_eq!(
        list.importers.iter().map(|i| i.user_id).collect::<Vec<_>>(),
        vec![
            PeerId::from_user(u1 as u64),
            PeerId::from_user(u2 as u64),
            PeerId::from_user(u3 as u64),
        ]
    );

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2, "must issue exactly 2 paginated RPCs");

    let req0 = tl_fns::messages::GetChatInviteImporters::from_bytes(&calls[0].1)
        .expect("decode page 1 GetChatInviteImporters");
    assert!(req0.requested);
    assert_eq!(req0.link.as_deref(), Some("https://t.me/+inv2"));
    assert_eq!(req0.q, None);
    assert_eq!(req0.offset_date, 0);
    assert_eq!(req0.offset_user, tl_enums::InputUser::Empty);
    assert_eq!(req0.limit, INVITE_IMPORTERS_FIRST_PAGE);

    let req1 = tl_fns::messages::GetChatInviteImporters::from_bytes(&calls[1].1)
        .expect("decode page 2 GetChatInviteImporters");
    assert!(req1.requested);
    assert_eq!(req1.link.as_deref(), Some("https://t.me/+inv2"));
    assert_eq!(req1.q, None);
    assert_eq!(req1.offset_date, 1_750_001_200);
    assert_eq!(
        req1.offset_user,
        tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: u2,
            access_hash: u2 * 100 + 7,
        })
    );
    assert_eq!(req1.limit, INVITE_IMPORTERS_PER_PAGE);

    shutdown_live(live, runtime);
}

fn synthetic_photo_story_item(id: i32, date: i32, expire_date: i32) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: true,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out: false,
        id,
        date,
        from_id: None,
        fwd_from: None,
        expire_date,
        caption: Some(format!("Story {id}")),
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler: false,
            live_photo: false,
            photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                has_stickers: false,
                id: 9_000 + i64::from(id),
                access_hash: 42_000 + i64::from(id),
                file_reference: vec![1, 2, 3],
                date,
                sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                    r#type: "m".to_owned(),
                    w: 320,
                    h: 568,
                    size: 4096,
                })],
                video_sizes: None,
                dc_id: 2,
            })),
            ttl_seconds: None,
            video: None,
        })),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    })
}

#[test]
fn stories_send_reaction_sets_add_to_recent_false() {
    const NOW_SECS: i32 = 1_750_000_500;
    let self_uid = 800_041i64;
    let bob_uid = 800_042i64;
    let self_peer = DataPeerId::from_user(DataUserId::from_tl(self_uid));
    let bob_peer = DataPeerId::from_user(DataUserId::from_tl(bob_uid));
    let (rpc, runtime, mut live) = build_live_session(self_uid);

    let mut store = StoriesStore::new(self_peer);
    live.apply_users_chats_and_sync_store(
        &mut store,
        &[tl_enums::User::User(synthetic_tl_user(
            bob_uid,
            "Bob",
            "bob_story",
            false,
        ))],
        &[],
        false,
    );
    store.apply_single_tl(
        bob_peer,
        &synthetic_photo_story_item(77, NOW_SECS - 60, NOW_SECS + 3600),
        NOW_SECS,
    );

    // 1. Send emoji reaction -> `add_to_recent` MUST be `false`
    //    (`Stories::sendReaction` in `data/data_stories.cpp:1117-1130`).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::stories::SendReaction::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    store.send_reaction(
        FullStoryId::new(bob_peer, 77),
        Some(ReactionId::Emoji("\u{1F525}".to_owned())),
    );
    let actions = store.take_actions();
    live.execute_actions(&mut store, actions, NOW_SECS);

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req = tl_fns::stories::SendReaction::from_bytes(&calls[0].1)
        .expect("decode stories.sendReaction");
    assert!(
        !req.add_to_recent,
        "stories.sendReaction must send add_to_recent = false (MTP_flags(0))"
    );
    assert_eq!(req.story_id, 77);
    assert_eq!(
        req.reaction,
        tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: "\u{1F525}".to_owned(),
        })
    );

    // 2. Clear reaction -> `add_to_recent == false`, `reaction == Reaction::Empty`.
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::stories::SendReaction::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    store.send_reaction(FullStoryId::new(bob_peer, 77), None);
    let actions = store.take_actions();
    live.execute_actions(&mut store, actions, NOW_SECS);

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req_clear = tl_fns::stories::SendReaction::from_bytes(&calls[0].1)
        .expect("decode clear stories.sendReaction");
    assert!(!req_clear.add_to_recent);
    assert_eq!(req_clear.reaction, tl_enums::Reaction::Empty);

    shutdown_live(live, runtime);
}

#[test]
fn stories_fail_album_stories_and_fail_albums_recover_on_rpc_error() {
    const NOW_SECS: i32 = 1_750_000_500;
    let self_uid = 800_051i64;
    let bob_uid = 800_052i64;
    let self_peer = DataPeerId::from_user(DataUserId::from_tl(self_uid));
    let bob_peer = DataPeerId::from_user(DataUserId::from_tl(bob_uid));
    let (rpc, runtime, mut live) = build_live_session(self_uid);

    let mut store = StoriesStore::new(self_peer);
    live.apply_users_chats_and_sync_store(
        &mut store,
        &[tl_enums::User::User(synthetic_tl_user(
            bob_uid,
            "AlbumPeer",
            "album_peer",
            false,
        ))],
        &[],
        false,
    );
    let _ = store.take_events();

    // 1. `LoadAlbumStories` RPC fails -> `fail_album_stories` must clear in-flight,
    //    set total = 0 (known), emit `AlbumIdsChanged`, and allow subsequent reload
    //    (`Stories::albumIdsLoadMore` `.fail` in `data/data_stories.cpp:1882-1891`).
    rpc.clear_recorded();
    rpc.enqueue_rpc_err(
        tl_fns::stories::GetPinnedStories::CONSTRUCTOR_ID,
        400,
        "STORY_NOT_FOUND",
    );
    store.album_ids_load_more(bob_peer, STORIES_ALBUM_ID_SAVED);
    let acts = store.take_actions();
    live.execute_actions(&mut store, acts, NOW_SECS);

    assert!(store.album_ids_count_known(bob_peer, STORIES_ALBUM_ID_SAVED));
    assert_eq!(store.album_ids_count(bob_peer, STORIES_ALBUM_ID_SAVED), 0);
    let events = store.take_events();
    assert!(
        events.contains(&StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
            bob_peer,
            STORIES_ALBUM_ID_SAVED,
        ))),
        "fail_album_stories must emit AlbumIdsChanged"
    );

    // Reload must now emit a fresh LoadAlbumStories action (proving request_in_flight was cleared).
    store.album_ids_reload(bob_peer, STORIES_ALBUM_ID_SAVED);
    let retry_acts = store.take_actions();
    assert_eq!(retry_acts.len(), 1);
    rpc.enqueue_ok(
        tl_fns::stories::GetPinnedStories::CONSTRUCTOR_ID,
        &tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
            count: 0,
            stories: Vec::new(),
            pinned_to_top: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    live.execute_actions(&mut store, retry_acts, NOW_SECS);

    // 2. `GetAlbums` succeeds once with hash = 0x1234, then fails -> `fail_albums`
    //    must clear in-flight and reset hash to 0 so the next request sends `hash = 0`
    //    (`Stories::loadAlbums` `.fail` in `data/data_stories.cpp:1958-1962`).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::stories::GetAlbums::CONSTRUCTOR_ID,
        &tl_enums::stories::Albums::Albums(tl_types::stories::Albums {
            hash: 0x1234,
            albums: Vec::new(),
        }),
    );
    store.request_albums(bob_peer);
    let acts = store.take_actions();
    live.execute_actions(&mut store, acts, NOW_SECS);

    // Second request sends hash 0x1234 and fails.
    rpc.enqueue_rpc_err(tl_fns::stories::GetAlbums::CONSTRUCTOR_ID, 500, "INTERNAL");
    store.request_albums(bob_peer);
    let acts = store.take_actions();
    live.execute_actions(&mut store, acts, NOW_SECS);

    // Third request must NOT be blocked by stale `request_in_flight` and MUST send `hash = 0`.
    rpc.enqueue_ok(
        tl_fns::stories::GetAlbums::CONSTRUCTOR_ID,
        &tl_enums::stories::Albums::NotModified,
    );
    store.request_albums(bob_peer);
    let acts = store.take_actions();
    assert_eq!(
        acts.len(),
        1,
        "fail_albums must clear request_in_flight so retry emits GetAlbums"
    );
    live.execute_actions(&mut store, acts, NOW_SECS);

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 3);
    let req0 = tl_fns::stories::GetAlbums::from_bytes(&calls[0].1).expect("decode GetAlbums #1");
    let req1 = tl_fns::stories::GetAlbums::from_bytes(&calls[1].1).expect("decode GetAlbums #2");
    let req2 = tl_fns::stories::GetAlbums::from_bytes(&calls[2].1).expect("decode GetAlbums #3");
    assert_eq!(req0.hash, 0);
    assert_eq!(req1.hash, 0x1234);
    assert_eq!(req2.hash, 0, "fail_albums must reset albums hash to 0");

    shutdown_live(live, runtime);
}

#[test]
fn stories_consumes_session_event_story_update_from_get_difference_without_double_applying() {
    const NOW_SECS: i32 = 1_750_000_500;
    let self_uid = 800_061i64;
    let bob_uid = 800_062i64;
    let self_peer = DataPeerId::from_user(DataUserId::from_tl(self_uid));
    let bob_peer = DataPeerId::from_user(DataUserId::from_tl(bob_uid));

    let rpc = FakeRpcClient::new();
    // Startup `updates.getState` -> pts=10, seq=1
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: NOW_SECS,
            seq: 1,
            unread_count: 0,
        }),
    );
    // Startup `updates.getDifference` -> delivers story updates (`updateStory`,
    // `updateReadStories`, `updateStoriesStealthMode`) via `SessionEvent::StoryUpdate`
    // (`Updates::applyUpdate` in `api/api_updates.cpp:2947-2958`).
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: Vec::new(),
            new_encrypted_messages: Vec::new(),
            other_updates: vec![
                tl_enums::Update::Story(tl_types::UpdateStory {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                    story: synthetic_photo_story_item(501, NOW_SECS - 10, NOW_SECS + 3600),
                }),
                tl_enums::Update::ReadStories(tl_types::UpdateReadStories {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                    max_id: 501,
                }),
                tl_enums::Update::StoriesStealthMode(tl_types::UpdateStoriesStealthMode {
                    stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                        tl_types::StoriesStealthMode {
                            active_until_date: Some(NOW_SECS + 900),
                            cooldown_until_date: Some(NOW_SECS + 3600),
                        },
                    ),
                }),
            ],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(synthetic_tl_user(
                bob_uid,
                "DiffUser",
                "diff_user",
                false,
            ))],
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: NOW_SECS + 1,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: true,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(self_uid),
            crate::ErasedRpcClient::new(rpc.clone()),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    // Wait for startup `getDifference` to finish BEFORE constructing `LiveSession` to verify
    // `LiveRuntimeBridge` receives startup `SessionEvent::StoryUpdate` via `initial_events_rx`.
    runtime
        .tokio_handle()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), runtime.wait_ready()).await
        })
        .expect("wait_ready timeout")
        .expect("wait_ready ok");

    let live = LiveSession::new(Arc::clone(&runtime), 1);
    let mut store = StoriesStore::new(self_peer);

    // 1. All 3 story updates from startup `getDifference` are drained and applied.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut applied = 0usize;
    while Instant::now() < deadline && applied < 3 {
        applied += live.drain_story_updates(&mut store, NOW_SECS + 1);
        if applied < 3 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert_eq!(applied, 3);
    assert!(store.lookup(FullStoryId::new(bob_peer, 501)).is_ok());
    assert_eq!(store.read_till(bob_peer), 501);
    assert_eq!(store.stealth_mode().enabled_till, NOW_SECS + 900);

    // 2. Live pushed `Updates::Updates` is applied ONCE (no double-applying from raw listener).
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::Story(tl_types::UpdateStory {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
            story: synthetic_photo_story_item(502, NOW_SECS + 2, NOW_SECS + 3600),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: NOW_SECS + 2,
        seq: 2,
    }));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut live_applied = 0usize;
    while Instant::now() < deadline && live_applied < 1 {
        live_applied += live.drain_story_updates(&mut store, NOW_SECS + 2);
        if live_applied < 1 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    std::thread::sleep(Duration::from_millis(50));
    live_applied += live.drain_story_updates(&mut store, NOW_SECS + 2);
    assert_eq!(
        live_applied, 1,
        "live updateStory must be applied exactly once without double-applying"
    );
    assert!(store.lookup(FullStoryId::new(bob_peer, 502)).is_ok());

    // 3. Mid-session `trigger_difference()` delivering another `Update::Story` is also applied.
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: Vec::new(),
            new_encrypted_messages: Vec::new(),
            other_updates: vec![tl_enums::Update::Story(tl_types::UpdateStory {
                peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                story: synthetic_photo_story_item(503, NOW_SECS + 5, NOW_SECS + 3600),
            })],
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: NOW_SECS + 5,
                seq: 3,
                unread_count: 0,
            }),
        }),
    );
    runtime
        .tokio_handle()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), runtime.trigger_difference()).await
        })
        .expect("trigger_difference timeout")
        .expect("trigger_difference ok");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut diff_applied = 0usize;
    while Instant::now() < deadline && diff_applied < 1 {
        diff_applied += live.drain_story_updates(&mut store, NOW_SECS + 5);
        if diff_applied < 1 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert_eq!(diff_applied, 1);
    assert!(store.lookup(FullStoryId::new(bob_peer, 503)).is_ok());

    shutdown_live(live, runtime);
}
