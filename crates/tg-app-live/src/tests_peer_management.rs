//! Live RPC tests for WP-10 peer management gaps (`live/admin.rs`):
//! - Invite links (`getExportedChatInvites`, `exportChatInvite`, `editExportedChatInvite`,
//!   revoke + replacement, `deleteExportedChatInvite`, `deleteRevokedExportedChatInvites`,
//!   `getChatInviteImporters`)
//! - Join requests (`getChatInviteImporters(requested)`, `hideChatJoinRequest`,
//!   `hideAllChatJoinRequests`, and `HistoryBackend` join-requests bar)
//! - Peer colors (`account.updateColor`, `channels.updateColor`)
//! - Ownership transfer (`messages.editChatCreator` pre-check + 2FA SRP check)
//! - Similar channels (`channels.getChannelRecommendations`) and live Downloads Manager
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs/users.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use num_bigint::BigUint;
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
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::{
    DownloadEntry, DownloadStatus, InfoBackend, InviteLinkRequest, OwnershipTransferCheck,
    OwnershipTransferError, UpdatePeerColorRequest,
};
use tg_app_core::stand_in::types::PeerId;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    dynamic: Arc<Mutex<Option<DynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
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

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
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
            .push((ctor, bytes.clone()));

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(ctor, &bytes)
        {
            let raw = res?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }

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
        premium: true,
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
        access_hash: Some(id * 100 + 9),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Demo".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
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

fn synthetic_tl_channel(
    id: i64,
    title: &str,
    username: Option<&str>,
    broadcast: bool,
    verified: bool,
    participants_count: i32,
) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified,
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
        join_request: true,
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
        access_hash: Some(id * 17 + 5),
        title: title.to_owned(),
        username: username.map(str::to_owned),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(participants_count),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: Some(4),
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn synthetic_tl_invite(
    link: &str,
    admin_id: i64,
    permanent: bool,
    revoked: bool,
    request_needed: bool,
    title: Option<&str>,
    expire_date: Option<i32>,
    usage_limit: Option<i32>,
    usage: Option<i32>,
    requested: Option<i32>,
) -> tl_enums::ExportedChatInvite {
    tl_enums::ExportedChatInvite::ChatInviteExported(tl_types::ChatInviteExported {
        revoked,
        permanent,
        request_needed,
        link: link.to_owned(),
        admin_id,
        date: 1_750_000_000,
        start_date: expire_date.map(|_| 1_750_000_000),
        expire_date,
        usage_limit,
        usage,
        requested,
        subscription_expired: None,
        title: title.map(str::to_owned),
        subscription_pricing: None,
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_100,
        seq: 10,
    })
}

struct PeerMgmtTestHarness {
    self_uid: i64,
    self_peer: PeerId,
    channel_id: i64,
    channel_peer: PeerId,
    admin_peer: PeerId,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>,
    live: LiveSession,
}

impl PeerMgmtTestHarness {
    fn start() -> Self {
        let self_uid = 610_001i64;
        let admin_uid = 610_002i64;
        let channel_id = 710_001i64;
        let self_user = synthetic_tl_user(self_uid, "Taylor", "taylor_self", true);
        let admin_user = synthetic_tl_user(admin_uid, "Mira", "mira_admin", false);
        let chan = synthetic_tl_channel(
            channel_id,
            "Rustgram Engineering",
            Some("rustgram_eng"),
            true,
            true,
            1_250,
        );

        let fake = FakeRpcClient::new(2);
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user.clone())],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 0,
            }),
        );
        fake.enqueue_ok(
            tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
            &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                date: 1_750_000_001,
                seq: 1,
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
            &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: vec![tl_enums::Chat::Channel(chan)],
                users: vec![tl_enums::User::User(admin_user)],
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
            &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
                state: tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 10,
                    qts: 0,
                    date: 1_750_000_001,
                    seq: 1,
                    unread_count: 0,
                }),
            }),
        );

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                crate::ErasedRpcClient::new(fake.clone()),
                SessionRuntimeConfig::default(),
            )
            .expect("start SessionRuntime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("startup ready");

        // Ensure channel and admin user are definitely loaded in tg-data::Session.
        runtime.tokio_handle().block_on(async {
            let mut s = runtime.session().write().await;
            apply_tl_users_and_chats(
                &mut s,
                &[tl_enums::User::User(synthetic_tl_user(
                    admin_uid,
                    "Mira",
                    "mira_admin",
                    false,
                ))],
                &[tl_enums::Chat::Channel(synthetic_tl_channel(
                    channel_id,
                    "Rustgram Engineering",
                    Some("rustgram_eng"),
                    true,
                    true,
                    1_250,
                ))],
            );
        });

        let live = LiveSession::new(Arc::clone(&runtime), 1);

        Self {
            self_uid,
            self_peer: PeerId::from_user(self_uid as u64),
            channel_id,
            channel_peer: PeerId::from_channel(channel_id as u64),
            admin_peer: PeerId::from_user(admin_uid as u64),
            fake,
            runtime,
            live,
        }
    }

    fn shutdown(self) {
        drop(self.live);
        if let Ok(rt) = Arc::try_unwrap(self.runtime) {
            rt.shutdown();
        }
    }
}

fn synthetic_srp_password(
    has_password: bool,
    hint: &str,
    password: &str,
) -> tl_enums::account::Password {
    let algo = tg_crypto::srp::PasswordKdfAlgoModPow {
        salt1: b"test_salt_1_0123456789ab".to_vec(),
        salt2: b"test_salt_2_fedcba987654".to_vec(),
        g: 3,
        p: tg_crypto::dh::GOOD_PRIME.to_vec(),
    };
    let tl_algo = tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
        tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
            salt1: algo.salt1.clone(),
            salt2: algo.salt2.clone(),
            g: algo.g,
            p: algo.p.clone(),
        },
    );
    let (srp_b, srp_id, current_algo) = if has_password {
        let v_bytes = tg_crypto::srp::compute_digest(&algo, password.as_bytes())
            .expect("compute srp verifier");
        let p = BigUint::from_bytes_be(&algo.p);
        let g = BigUint::from(algo.g as u32);
        let v = BigUint::from_bytes_be(&v_bytes);
        let k_bytes = tg_crypto::sha256(&[&algo.p, &{
            let mut gb = [0u8; 256];
            gb[255] = algo.g as u8;
            gb
        }]);
        let k = BigUint::from_bytes_be(&k_bytes);
        let b_secret = BigUint::from(123_456_789u64);
        let b_pub = ((&k * &v) + g.modpow(&b_secret, &p)) % &p;
        let b_raw = b_pub.to_bytes_be();
        let mut srp_b = vec![0u8; 256];
        let offset = 256usize.saturating_sub(b_raw.len());
        srp_b[offset..].copy_from_slice(&b_raw[..b_raw.len().min(256)]);
        (Some(srp_b), Some(424_242i64), Some(tl_algo.clone()))
    } else {
        (None, None, None)
    };
    tl_enums::account::Password::Password(tl_types::account::Password {
        has_recovery: false,
        has_secure_values: false,
        has_password,
        current_algo,
        srp_b,
        srp_id,
        hint: (!hint.is_empty()).then(|| hint.to_owned()),
        email_unconfirmed_pattern: None,
        new_algo: tl_algo,
        new_secure_algo: tl_enums::SecurePasswordKdfAlgo::Unknown,
        secure_random: vec![1, 2, 3, 4],
        pending_reset_date: None,
        login_email_pattern: None,
    })
}

#[test]
fn live_invite_links_crud_and_importers_rpcs() {
    let mut h = PeerMgmtTestHarness::start();

    // 1. messages.getExportedChatInvites (active)
    h.fake.enqueue_ok(
        tl_fns::messages::GetExportedChatInvites::CONSTRUCTOR_ID,
        &tl_enums::messages::ExportedChatInvites::ExportedChatInvites(
            tl_types::messages::ExportedChatInvites {
                count: 2,
                invites: vec![
                    synthetic_tl_invite(
                        "https://t.me/+PermLive01",
                        h.self_uid,
                        true,
                        false,
                        false,
                        None,
                        None,
                        None,
                        Some(12),
                        None,
                    ),
                    synthetic_tl_invite(
                        "https://t.me/+TempLive02",
                        h.self_uid,
                        false,
                        false,
                        true,
                        Some("QA Cohort"),
                        Some(1_760_000_000),
                        None,
                        Some(3),
                        Some(2),
                    ),
                ],
                users: Vec::new(),
            },
        ),
    );
    let active = h
        .live
        .exported_chat_invites(h.channel_peer, false)
        .expect("get active invites");
    assert_eq!(active.count, 2);
    assert_eq!(active.links.len(), 2);
    assert!(active.links[0].permanent);
    assert_eq!(active.links[1].title, "QA Cohort");
    assert!(active.links[1].request_needed);
    assert_eq!(active.links[1].requested, 2);

    // 2. messages.exportChatInvite (expiry + usage_limit + title)
    h.fake.enqueue_ok(
        tl_fns::messages::ExportChatInvite::CONSTRUCTOR_ID,
        &synthetic_tl_invite(
            "https://t.me/+Created03",
            h.self_uid,
            false,
            false,
            false,
            Some("Launch Event"),
            Some(1_765_000_000),
            Some(50),
            Some(0),
            None,
        ),
    );
    let created = h
        .live
        .export_chat_invite(
            h.channel_peer,
            InviteLinkRequest {
                title: "Launch Event".to_owned(),
                expire_date: Some(1_765_000_000),
                usage_limit: Some(50),
                request_needed: false,
                legacy_revoke_permanent: false,
            },
        )
        .expect("export invite");
    assert_eq!(created.link, "https://t.me/+Created03");
    assert_eq!(created.usage_limit, Some(50));
    assert_eq!(created.expire_date, Some(1_765_000_000));

    // 3. messages.editExportedChatInvite (update label & request_needed)
    h.fake.enqueue_ok(
        tl_fns::messages::EditExportedChatInvite::CONSTRUCTOR_ID,
        &tl_enums::messages::ExportedChatInvite::ExportedChatInvite(
            tl_types::messages::ExportedChatInvite {
                invite: synthetic_tl_invite(
                    "https://t.me/+Created03",
                    h.self_uid,
                    false,
                    false,
                    true,
                    Some("Launch Event (Gated)"),
                    Some(1_765_000_000),
                    None,
                    Some(0),
                    Some(0),
                ),
                users: Vec::new(),
            },
        ),
    );
    let edited = h
        .live
        .edit_exported_chat_invite(
            h.channel_peer,
            "https://t.me/+Created03",
            InviteLinkRequest {
                title: "Launch Event (Gated)".to_owned(),
                expire_date: Some(1_765_000_000),
                usage_limit: None,
                request_needed: true,
                legacy_revoke_permanent: false,
            },
        )
        .expect("edit invite");
    assert!(edited.request_needed);
    assert_eq!(edited.title, "Launch Event (Gated)");

    // 4. messages.editExportedChatInvite(revoked: true) -> ExportedChatInviteReplaced
    h.fake.enqueue_ok(
        tl_fns::messages::EditExportedChatInvite::CONSTRUCTOR_ID,
        &tl_enums::messages::ExportedChatInvite::Replaced(
            tl_types::messages::ExportedChatInviteReplaced {
                invite: synthetic_tl_invite(
                    "https://t.me/+PermLive01",
                    h.self_uid,
                    true,
                    true,
                    false,
                    None,
                    None,
                    None,
                    Some(12),
                    None,
                ),
                new_invite: synthetic_tl_invite(
                    "https://t.me/+PermLiveNew99",
                    h.self_uid,
                    true,
                    false,
                    false,
                    None,
                    None,
                    None,
                    Some(0),
                    None,
                ),
                users: Vec::new(),
            },
        ),
    );
    let (revoked, replacement) = h
        .live
        .revoke_exported_chat_invite(h.channel_peer, "https://t.me/+PermLive01")
        .expect("revoke permanent invite");
    assert!(revoked.revoked);
    let fresh = replacement.expect("permanent replacement link");
    assert_eq!(fresh.link, "https://t.me/+PermLiveNew99");
    let cached_link = h.runtime.tokio_handle().block_on(async {
        let s = h.runtime.session().read().await;
        s.peer(DataPeerId::from_channel(ChannelId::from_tl(h.channel_id)))
            .and_then(Peer::as_channel)
            .map(|c| c.invite_link.clone())
    });
    assert_eq!(cached_link.as_deref(), Some("https://t.me/+PermLiveNew99"));

    // 5. messages.deleteExportedChatInvite & deleteRevokedExportedChatInvites
    h.fake.enqueue_ok(
        tl_fns::messages::DeleteExportedChatInvite::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .delete_exported_chat_invite(h.channel_peer, "https://t.me/+PermLive01")
        .expect("delete single revoked invite");

    h.fake.enqueue_ok(
        tl_fns::messages::DeleteRevokedExportedChatInvites::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .delete_revoked_exported_chat_invites(h.channel_peer)
        .expect("delete all revoked invites");

    // 6. messages.getChatInviteImporters (joined users)
    let joiner_uid = 610_055i64;
    h.fake.enqueue_ok(
        tl_fns::messages::GetChatInviteImporters::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatInviteImporters::ChatInviteImporters(
            tl_types::messages::ChatInviteImporters {
                count: 1,
                importers: vec![tl_enums::ChatInviteImporter::ChatInviteImporter(
                    tl_types::ChatInviteImporter {
                        requested: false,
                        via_chatlist: true,
                        user_id: joiner_uid,
                        date: 1_750_005_000,
                        about: Some("Joined via folder".to_owned()),
                        approved_by: None,
                    },
                )],
                users: vec![tl_enums::User::User(synthetic_tl_user(
                    joiner_uid,
                    "Jordan",
                    "jordan_join",
                    false,
                ))],
            },
        ),
    );
    let importers = h
        .live
        .chat_invite_importers(h.channel_peer, Some("https://t.me/+Created03"), false, "")
        .expect("get importers");
    assert_eq!(importers.count, 1);
    assert_eq!(
        importers.importers[0].user_id,
        PeerId::from_user(joiner_uid as u64)
    );
    assert!(importers.importers[0].via_chatlist);
    assert_eq!(
        importers.importers[0].username.as_deref(),
        Some("jordan_join")
    );

    // Verify wire request flags for exportChatInvite & deleteRevokedExportedChatInvites
    let calls = h.fake.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::GetExportedChatInvites::CONSTRUCTOR_ID)
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::ExportChatInvite::CONSTRUCTOR_ID)
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::EditExportedChatInvite::CONSTRUCTOR_ID)
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::DeleteExportedChatInvite::CONSTRUCTOR_ID)
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::DeleteRevokedExportedChatInvites::CONSTRUCTOR_ID)
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::GetChatInviteImporters::CONSTRUCTOR_ID)
    );

    h.shutdown();
}

#[test]
fn live_join_requests_approve_dismiss_and_history_bar_rpcs() {
    let mut h = PeerMgmtTestHarness::start();
    let req_uid_1 = 610_081i64;
    let req_uid_2 = 610_082i64;

    // Seed pending_requests_count & recent_requesters in tg-data::Session (as received from ChannelFull)
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[
                tl_enums::User::User(synthetic_tl_user(req_uid_1, "Avery", "avery_req", false)),
                tl_enums::User::User(synthetic_tl_user(req_uid_2, "Devon", "devon_req", false)),
            ],
            &[],
        );
        if let Some(Peer::Channel(ch)) =
            s.peer_mut(DataPeerId::from_channel(ChannelId::from_tl(h.channel_id)))
        {
            ch.pending_requests_count = 2;
            ch.recent_requesters = vec![
                DataUserId::from_tl(req_uid_1),
                DataUserId::from_tl(req_uid_2),
            ];
        }
    });

    let bar = HistoryBackend::join_requests_bar(&h.live, h.channel_peer)
        .expect("join requests bar should be visible when count > 0");
    assert_eq!(bar.count, 2);
    assert_eq!(bar.recent_requesters.len(), 2);
    assert_eq!(bar.recent_requesters[0].1, "Avery");

    // 1. messages.getChatInviteImporters(requested: true, q: "avery")
    h.fake.enqueue_ok(
        tl_fns::messages::GetChatInviteImporters::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatInviteImporters::ChatInviteImporters(
            tl_types::messages::ChatInviteImporters {
                count: 1,
                importers: vec![tl_enums::ChatInviteImporter::ChatInviteImporter(
                    tl_types::ChatInviteImporter {
                        requested: true,
                        via_chatlist: false,
                        user_id: req_uid_1,
                        date: 1_750_009_000,
                        about: Some("Would love to help test".to_owned()),
                        approved_by: None,
                    },
                )],
                users: vec![tl_enums::User::User(synthetic_tl_user(
                    req_uid_1,
                    "Avery",
                    "avery_req",
                    false,
                ))],
            },
        ),
    );
    let reqs = HistoryBackend::join_requests_entries(&h.live, h.channel_peer, "avery");
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].requested);
    assert_eq!(reqs[0].about.as_deref(), Some("Would love to help test"));

    // 2. messages.hideChatJoinRequest(approved: true)
    h.fake.enqueue_ok(
        tl_fns::messages::HideChatJoinRequest::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    HistoryBackend::decide_join_request(
        &mut h.live,
        h.channel_peer,
        PeerId::from_user(req_uid_1 as u64),
        true,
    )
    .expect("approve single join request");

    let bar_after_one = HistoryBackend::join_requests_bar(&h.live, h.channel_peer)
        .expect("1 request should remain");
    assert_eq!(bar_after_one.count, 1);
    assert_eq!(bar_after_one.recent_requesters.len(), 1);
    assert_eq!(bar_after_one.recent_requesters[0].1, "Devon");

    // 3. messages.hideAllChatJoinRequests(approved: false)
    h.fake.enqueue_ok(
        tl_fns::messages::HideAllChatJoinRequests::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let dismissed = HistoryBackend::decide_all_join_requests(&mut h.live, h.channel_peer, false)
        .expect("dismiss all remaining requests");
    assert_eq!(dismissed, 1);
    assert!(
        HistoryBackend::join_requests_bar(&h.live, h.channel_peer).is_none(),
        "bar must hide once pending_requests_count reaches 0"
    );

    // Inspect decoded TL request for HideChatJoinRequest to confirm approved=true
    let hide_bytes = h
        .fake
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::messages::HideChatJoinRequest::CONSTRUCTOR_ID)
        .expect("HideChatJoinRequest recorded")
        .1;
    let decoded =
        tl_fns::messages::HideChatJoinRequest::from_bytes(&hide_bytes).expect("decode hide req");
    assert!(decoded.approved);

    h.shutdown();
}

#[test]
fn live_peer_color_account_and_channel_rpcs() {
    let mut h = PeerMgmtTestHarness::start();

    // 1. Self name color -> account.updateColor(for_profile: false)
    h.fake
        .enqueue_ok(tl_fns::account::UpdateColor::CONSTRUCTOR_ID, &true);
    h.live
        .update_peer_color(
            h.self_peer,
            UpdatePeerColorRequest {
                for_profile: false,
                color_index: Some(11),
                background_emoji_id: 5368324170671202286,
            },
        )
        .expect("account.updateColor name");

    // 2. Self profile color -> account.updateColor(for_profile: true)
    h.fake
        .enqueue_ok(tl_fns::account::UpdateColor::CONSTRUCTOR_ID, &true);
    h.live
        .update_peer_color(
            h.self_peer,
            UpdatePeerColorRequest {
                for_profile: true,
                color_index: Some(4),
                background_emoji_id: 5368324170671202287,
            },
        )
        .expect("account.updateColor profile");

    let self_colors = h.live.peer_color_settings(h.self_peer);
    assert_eq!(self_colors.color_index, Some(11));
    assert_eq!(self_colors.background_emoji_id, 5368324170671202286);
    assert_eq!(self_colors.profile_color_index, Some(4));
    assert_eq!(self_colors.profile_background_emoji_id, 5368324170671202287);

    // 3. Channel color -> channels.updateColor(for_profile: true)
    h.fake.enqueue_ok(
        tl_fns::channels::UpdateColor::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .update_peer_color(
            h.channel_peer,
            UpdatePeerColorRequest {
                for_profile: true,
                color_index: Some(16),
                background_emoji_id: 5368324170671202288,
            },
        )
        .expect("channels.updateColor profile");

    let ch_colors = h.live.peer_color_settings(h.channel_peer);
    assert_eq!(ch_colors.profile_color_index, Some(16));
    assert_eq!(ch_colors.profile_background_emoji_id, 5368324170671202288);

    // Verify decoded TL args for channels.updateColor
    let ch_color_raw = h
        .fake
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::channels::UpdateColor::CONSTRUCTOR_ID)
        .expect("channels.updateColor recorded")
        .1;
    let decoded =
        tl_fns::channels::UpdateColor::from_bytes(&ch_color_raw).expect("decode updateColor");
    assert!(decoded.for_profile);
    assert_eq!(decoded.color, Some(16));
    assert_eq!(decoded.background_emoji_id, Some(5368324170671202288));

    h.shutdown();
}

#[test]
fn live_ownership_transfer_precheck_and_srp_edit_chat_creator() {
    let mut h = PeerMgmtTestHarness::start();

    // 1. Precheck blocked by PASSWORD_TOO_FRESH_604800
    h.fake.enqueue_rpc_err(
        tl_fns::messages::EditChatCreator::CONSTRUCTOR_ID,
        400,
        "PASSWORD_TOO_FRESH_604800",
    );
    let check_fresh = h
        .live
        .check_ownership_transfer(h.channel_peer)
        .expect("precheck rpc");
    assert!(matches!(
        check_fresh,
        OwnershipTransferCheck::Blocked {
            reason: OwnershipTransferError::PasswordTooFresh {
                retry_after_secs: 604_800
            },
            ..
        }
    ));

    // 2. Precheck ready with password hint
    h.fake.enqueue_rpc_err(
        tl_fns::messages::EditChatCreator::CONSTRUCTOR_ID,
        400,
        "USER_ID_INVALID",
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_srp_password(true, "favorite-crate", "correct-horse-2fa"),
    );
    let check_ready = h
        .live
        .check_ownership_transfer(h.channel_peer)
        .expect("precheck ready");
    assert_eq!(
        check_ready,
        OwnershipTransferCheck::Ready {
            password_hint: "favorite-crate".to_owned(),
        }
    );

    // 3. Wrong password returns OwnershipTransferError::WrongPassword
    h.fake.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_srp_password(true, "favorite-crate", "correct-horse-2fa"),
    );
    h.fake.enqueue_rpc_err(
        tl_fns::messages::EditChatCreator::CONSTRUCTOR_ID,
        400,
        "PASSWORD_HASH_INVALID",
    );
    let bad = h
        .live
        .transfer_ownership(h.channel_peer, h.admin_peer, "wrong-pwd");
    assert_eq!(bad, Err(OwnershipTransferError::WrongPassword));

    // 4. Valid 2FA SRP transfer succeeds and sends real SRP check to messages.editChatCreator
    let captured_srp: Arc<Mutex<Option<tl_types::InputCheckPasswordSRP>>> =
        Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured_srp);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::messages::EditChatCreator::CONSTRUCTOR_ID {
            let req =
                tl_fns::messages::EditChatCreator::from_bytes(raw).expect("decode EditChatCreator");
            if let tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(srp) = req.password {
                *captured_clone.lock().expect("lock srp") = Some(srp);
                return Some(Ok(empty_updates().to_bytes()));
            }
        }
        None
    }));
    h.fake.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_srp_password(true, "favorite-crate", "correct-horse-2fa"),
    );
    h.live
        .transfer_ownership(h.channel_peer, h.admin_peer, "correct-horse-2fa")
        .expect("transfer ownership with valid SRP check");

    let srp = captured_srp
        .lock()
        .expect("lock srp")
        .clone()
        .expect("EditChatCreator must carry InputCheckPasswordSRP");
    assert_eq!(srp.srp_id, 424_242);
    assert_eq!(srp.a.len(), 256);
    assert_eq!(srp.m1.len(), 32);

    // Local channel CREATOR flag should be cleared after ownership transfer
    let still_creator = h.runtime.tokio_handle().block_on(async {
        let s = h.runtime.session().read().await;
        s.peer(DataPeerId::from_channel(ChannelId::from_tl(h.channel_id)))
            .and_then(Peer::as_channel)
            .is_some_and(|ch| ch.flags.contains(ChannelFlags::CREATOR))
    });
    assert!(
        !still_creator,
        "CREATOR flag must be cleared locally after transfer"
    );

    h.shutdown();
}

#[test]
fn live_similar_channels_and_downloads_manager() {
    let mut h = PeerMgmtTestHarness::start();

    // 1. channels.getChannelRecommendations returning messages.chatsSlice with more for Premium
    let rec_a = synthetic_tl_channel(
        720_001,
        "Rust GUI News",
        Some("rust_gui_news"),
        true,
        true,
        9_500,
    );
    let rec_b = synthetic_tl_channel(
        720_002,
        "Systems Weekly",
        Some("systems_weekly"),
        true,
        false,
        4_200,
    );
    h.fake.enqueue_ok(
        tl_fns::channels::GetChannelRecommendations::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Slice(tl_types::messages::ChatsSlice {
            count: 18,
            chats: vec![
                tl_enums::Chat::Channel(rec_a),
                tl_enums::Chat::Channel(rec_b),
            ],
        }),
    );

    let similar = h
        .live
        .similar_channels(h.channel_peer)
        .expect("get channel recommendations");
    assert_eq!(similar.total_count, 18);
    assert!(similar.has_more_for_premium);
    assert_eq!(similar.channels.len(), 2);
    assert_eq!(similar.channels[0].title, "Rust GUI News");
    assert!(similar.channels[0].verified);
    assert_eq!(similar.channels[0].subscribers_count, 9_500);

    // 2. Live Downloads Manager tracking, search, pause/resume, clear completed, delete all
    let id1 = h.live.track_download(DownloadEntry {
        id: 0,
        peer_id: h.channel_peer,
        peer_title: "Rustgram Engineering".to_owned(),
        msg_id: 101,
        file_name: "design_spec.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        total_bytes: 2_000_000,
        local_path: None,
        started_at: 1_750_000_010,
        date_group: "Today".to_owned(),
        status: DownloadStatus::Downloading {
            received_bytes: 500_000,
            total_bytes: 2_000_000,
        },
    });
    let id2 = h.live.track_download(DownloadEntry {
        id: 0,
        peer_id: h.channel_peer,
        peer_title: "Rustgram Engineering".to_owned(),
        msg_id: 102,
        file_name: "release_archive.zip".to_owned(),
        mime_type: "application/zip".to_owned(),
        total_bytes: 5_000_000,
        local_path: None,
        started_at: 1_750_000_020,
        date_group: "Today".to_owned(),
        status: DownloadStatus::Completed,
    });

    assert_eq!(h.live.downloads_list("").len(), 2);
    assert_eq!(h.live.downloads_list("design").len(), 1);

    h.live.toggle_download_pause(id1).expect("pause download");
    let after_pause = h.live.downloads_list("design");
    assert!(matches!(
        after_pause[0].status,
        DownloadStatus::Paused { .. }
    ));

    h.live.toggle_download_pause(id1).expect("resume download");
    let after_resume = h.live.downloads_list("design");
    assert!(matches!(
        after_resume[0].status,
        DownloadStatus::Downloading { .. }
    ));

    let cleared = h.live.clear_completed_downloads().expect("clear completed");
    assert_eq!(cleared, 1);
    assert_eq!(h.live.downloads_list("").len(), 1);
    assert!(h.live.cancel_or_delete_download(id2, false).is_err());

    let deleted_all = h.live.delete_all_downloads().expect("delete all");
    assert_eq!(deleted_all, 1);
    assert!(h.live.downloads_list("").is_empty());

    h.shutdown();
}
