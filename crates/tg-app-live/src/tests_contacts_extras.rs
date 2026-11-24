//! `FakeRpcClient` unit tests for WP-66:
//! - `contacts.deleteContacts` (clears local contact status & applies server Updates)
//! - `contacts.acceptContact`, `messages.reportSpam`, `messages.hidePeerSettingsBar`
//!   (clear `PeerBarSettings` & apply Updates)
//! - `contacts.exportContactToken`, `contacts.importContactToken`,
//!   `contacts.exportNoPaidMessagesToken`, `contacts.importNoPaidMessagesToken`
//! - `contacts.updateContactNote` (set with entities + clear)
//! - `contacts.getBirthdays` (filters self/deleted/blocked, updates `User::birthday`, daily cache)
//! - `contacts.blockFromReplies` (exact bitflags + Updates through `SessionRuntime`)
//! - `contacts.setBlocked` (`my_stories_from = true` & `false`, limit calculation)
//! - `contacts.toggleTopPeers` (exact Bool wire bytes & `search_and_info` disabled state sync)
//! - `contacts.getStatuses` (updates `User::lastseen`, preserves local pseudo-online on `Recently`)
//! - `contacts.getSponsoredPeers` (empty & populated, hydrates users/chats into `Session`)
//! - `photos.deletePhotos` (clears self `userpic_photo_id` when active photo deleted)
//! - `photos.uploadContactProfilePhoto` (`save`, `suggest`, `video` + `video_start_ts`,
//!   `video_emoji_markup`, and `clearPersonal`)
//! - `help.hidePromoData` (exact `InputPeer` encoding & clears `LivePromoState::promo_peer`)
//! - Synthetic-only test for `contacts.importContacts` (never real phone numbers)
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs/users.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::rpc::RpcError;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    BlockStatus, Channel, ChannelFlags, ChannelId, ContactStatus, LastseenStatus, PeerBarSettings,
    PeerId as DataPeerId, PeerUpdateFlags, PhotoId, UserFlags, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::{EntityInText, EntityType, TextWithEntities as UiText};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::{from_data_peer, now_unix_i32, to_data_peer};
use tg_app_core::core_types::dto::contacts_extras::{
    BlockFromRepliesRequest, ContactProfilePhotoMode, ContactProfilePhotoUpload,
};
use tg_app_core::core_types::misc::PromoDataSnapshot;
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::{EmojiUserpicRequest, InfoBackend, ProfilePhotoRef};
use tg_app_core::settings::backend::SettingsBackend;
use tg_app_core::stand_in::types::MsgId;
use tg_app_core::stories::backend::StoriesBackend;

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

    fn enqueue_rpc_error(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError::new(code, name))),
        ));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
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
        _dc: DcId,
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
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
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

fn synthetic_tl_user(
    id: i64,
    access_hash: i64,
    first: &str,
    username: Option<&str>,
    is_self: bool,
    contact: bool,
    stars_per_msg: Option<i64>,
) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: is_self,
        contact,
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
        apply_min_photo: true,
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
        access_hash: Some(access_hash),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: username.map(str::to_owned),
        phone: None,
        photo: None,
        status: Some(tl_enums::UserStatus::Offline(tl_types::UserStatusOffline {
            was_online: 1_700_000_000,
        })),
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
        send_paid_messages_stars: stars_per_msg,
        linked_community_id: None,
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_100,
        seq: 0,
    })
}

fn make_live_session() -> (tokio::runtime::Runtime, FakeRpcClient, LiveSession) {
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
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    runtime
        .restore_updates_state_blocking(tg_api::PersistedUpdatesState {
            pts: 10,
            qts: 0,
            date: 1_700_000_000,
            seq: 1,
            ..tg_api::PersistedUpdatesState::default()
        })
        .expect("restore updates state");

    let live = LiveSession::new(Arc::new(runtime), 1);
    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[
                synthetic_tl_user(
                    100,
                    1001,
                    "SelfAccount",
                    Some("self_test"),
                    true,
                    false,
                    None,
                ),
                synthetic_tl_user(
                    201,
                    2001,
                    "Alpha",
                    Some("alpha_test"),
                    false,
                    true,
                    Some(50),
                ),
                synthetic_tl_user(202, 2002, "Beta", Some("beta_test"), false, true, None),
                synthetic_tl_user(203, 2003, "Gamma", Some("gamma_test"), false, false, None),
            ],
            &[],
        );
        // Set active profile photo on self user for delete_profile_photos test.
        if let Some(u) = s.user_mut(DataUserId(100)) {
            u.common.userpic_photo_id = PhotoId::from_tl(9_001);
            u.common.userpic_has_video = true;
        }
        // Seed PeerBarSettings on user 203 for acceptContact/reportSpam/hidePeerSettingsBar tests.
        if let Some(p) = s.peer_mut(DataPeerId::from_user(DataUserId(203))) {
            p.common_mut().bar_settings = PeerBarSettings::REPORT_SPAM
                | PeerBarSettings::ADD_CONTACT
                | PeerBarSettings::SHARE_CONTACT;
        }
        // Seed a synthetic channel 301 for reportSpam / hidePromoData tests.
        let mut ch = Channel::new(ChannelId(301));
        ch.access_hash = 3001;
        ch.common.name = "PromoChannel".to_owned();
        ch.flags = ChannelFlags::BROADCAST;
        ch.common.bar_settings = PeerBarSettings::REPORT_SPAM;
        s.upsert_channel(ch, PeerUpdateFlags::NAME);
    });
    (rt, rpc, live)
}

#[test]
fn wp66_delete_contacts_clears_local_contact_status_and_encodes_tl() {
    let (rt, rpc, mut live) = make_live_session();
    let u201 = from_data_peer(DataPeerId::from_user(DataUserId(201)));
    let u202 = from_data_peer(DataPeerId::from_user(DataUserId(202)));

    // Empty slice rejected locally.
    assert!(InfoBackend::delete_contacts(&mut live, &[]).is_err());

    rpc.enqueue_ok(
        tl_fns::contacts::DeleteContacts::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    InfoBackend::delete_contacts(&mut live, &[u201, u202]).expect("deleteContacts ok");

    let calls = rpc.recorded_of(tl_fns::contacts::DeleteContacts::CONSTRUCTOR_ID);
    assert_eq!(calls.len(), 1);
    let expected = tl_fns::contacts::DeleteContacts {
        id: vec![
            tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 201,
                access_hash: 2001,
            }),
            tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 202,
                access_hash: 2002,
            }),
        ],
    };
    assert_eq!(calls[0], expected.to_bytes());

    rt.block_on(async {
        let s = live.runtime().session().read().await;
        for uid in [DataUserId(201), DataUserId(202)] {
            let u = s.user(uid).expect("user present");
            assert!(!u.is_contact());
            assert_eq!(u.contact_status, ContactStatus::NotContact);
            assert!(!u.flags.contains(UserFlags::CONTACT));
            assert!(!u.flags.contains(UserFlags::MUTUAL_CONTACT));
        }
    });
}

#[test]
fn wp66_accept_contact_report_spam_and_hide_bar_clear_peer_bar_settings() {
    let (rt, rpc, mut live) = make_live_session();
    let u203 = from_data_peer(DataPeerId::from_user(DataUserId(203)));
    let ch301 = from_data_peer(DataPeerId::from_channel(ChannelId(301)));

    // 1. contacts.acceptContact
    rpc.enqueue_ok(
        tl_fns::contacts::AcceptContact::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    HistoryBackend::accept_contact(&mut live, u203).expect("acceptContact ok");

    let accept_calls = rpc.recorded_of(tl_fns::contacts::AcceptContact::CONSTRUCTOR_ID);
    assert_eq!(accept_calls.len(), 1);
    assert_eq!(
        accept_calls[0],
        tl_fns::contacts::AcceptContact {
            id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 203,
                access_hash: 2003,
            }),
        }
        .to_bytes()
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let p = s.peer(to_data_peer(u203)).expect("u203");
        assert!(p.common().bar_settings.is_empty());
    });

    // 2. messages.reportSpam on channel 301
    rpc.enqueue_ok(tl_fns::messages::ReportSpam::CONSTRUCTOR_ID, &true);
    assert!(HistoryBackend::report_spam(&mut live, ch301).expect("reportSpam ok"));
    let spam_calls = rpc.recorded_of(tl_fns::messages::ReportSpam::CONSTRUCTOR_ID);
    assert_eq!(spam_calls.len(), 1);
    assert_eq!(
        spam_calls[0],
        tl_fns::messages::ReportSpam {
            peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                channel_id: 301,
                access_hash: 3001,
            }),
        }
        .to_bytes()
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let p = s.peer(to_data_peer(ch301)).expect("ch301");
        assert!(p.common().bar_settings.is_empty());
    });

    // 3. Re-seed bar settings on u203 and call messages.hidePeerSettingsBar
    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        s.peer_mut(to_data_peer(u203))
            .expect("u203")
            .common_mut()
            .bar_settings = PeerBarSettings::ADD_CONTACT | PeerBarSettings::BLOCK_CONTACT;
    });
    rpc.enqueue_ok(tl_fns::messages::HidePeerSettingsBar::CONSTRUCTOR_ID, &true);
    assert!(HistoryBackend::hide_peer_settings_bar(&mut live, u203).expect("hideBar ok"));
    let hide_calls = rpc.recorded_of(tl_fns::messages::HidePeerSettingsBar::CONSTRUCTOR_ID);
    assert_eq!(hide_calls.len(), 1);
    assert_eq!(
        hide_calls[0],
        tl_fns::messages::HidePeerSettingsBar {
            peer: tl_enums::InputPeer::User(tl_types::InputPeerUser {
                user_id: 203,
                access_hash: 2003,
            }),
        }
        .to_bytes()
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        assert!(
            s.peer(to_data_peer(u203))
                .expect("u203")
                .common()
                .bar_settings
                .is_empty()
        );
    });
}

#[test]
fn wp66_contact_tokens_and_no_paid_messages_tokens_round_trip() {
    let (rt, rpc, mut live) = make_live_session();
    let u201 = from_data_peer(DataPeerId::from_user(DataUserId(201)));

    // 1. contacts.exportContactToken
    rpc.enqueue_ok(
        tl_fns::contacts::ExportContactToken::CONSTRUCTOR_ID,
        &tl_enums::ExportedContactToken::ExportedContactToken(tl_types::ExportedContactToken {
            url: "https://t.me/contact/tok_demo_123".to_owned(),
            expires: 1_800_000_000,
        }),
    );
    let exported = InfoBackend::export_contact_token(&live).expect("exportContactToken ok");
    assert_eq!(exported.url, "https://t.me/contact/tok_demo_123");
    assert_eq!(exported.expires, 1_800_000_000);
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::ExportContactToken::CONSTRUCTOR_ID)[0],
        tl_fns::contacts::ExportContactToken {}.to_bytes()
    );

    // 2. contacts.importContactToken (rejects empty token & deleted user, hydrates valid user)
    assert!(InfoBackend::import_contact_token(&mut live, "   ").is_err());

    rpc.enqueue_ok(
        tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID,
        &synthetic_tl_user(
            209,
            2009,
            "TokenUser",
            Some("token_user"),
            false,
            false,
            None,
        ),
    );
    let imported = InfoBackend::import_contact_token(&mut live, "tok_demo_123")
        .expect("importContactToken ok");
    assert_eq!(
        imported,
        from_data_peer(DataPeerId::from_user(DataUserId(209)))
    );
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID)[0],
        tl_fns::contacts::ImportContactToken {
            token: "tok_demo_123".to_owned(),
        }
        .to_bytes()
    );

    // Deleted / empty user returned by server is rejected like tdesktop `isInaccessible()`.
    rpc.enqueue_ok(
        tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID,
        &tl_enums::User::Empty(tl_types::UserEmpty { id: 210 }),
    );
    assert!(InfoBackend::import_contact_token(&mut live, "expired_tok").is_err());

    // 3. contacts.exportNoPaidMessagesToken (exact revoke + temporary flags)
    rpc.enqueue_ok(
        tl_fns::contacts::ExportNoPaidMessagesToken::CONSTRUCTOR_ID,
        &tl_enums::ExportedNoPaidMessagesToken::ExportedNoPaidMessagesToken(
            tl_types::ExportedNoPaidMessagesToken {
                token: "fee_waiver_xyz".to_owned(),
                url: "https://t.me/self_test?free=fee_waiver_xyz".to_owned(),
                expires: Some(1_800_003_600),
            },
        ),
    );
    let waiver = InfoBackend::export_no_paid_messages_token(&live, true, true)
        .expect("exportNoPaidMessagesToken ok");
    assert_eq!(waiver.token, "fee_waiver_xyz");
    assert_eq!(waiver.url, "https://t.me/self_test?free=fee_waiver_xyz");
    assert_eq!(waiver.expires, Some(1_800_003_600));
    let exp_fee_bytes =
        &rpc.recorded_of(tl_fns::contacts::ExportNoPaidMessagesToken::CONSTRUCTOR_ID)[0];
    assert_eq!(
        *exp_fee_bytes,
        tl_fns::contacts::ExportNoPaidMessagesToken {
            revoke: true,
            temporary: true,
        }
        .to_bytes()
    );

    // 4. contacts.importNoPaidMessagesToken removes Stars-per-message on user 201
    rpc.enqueue_ok(
        tl_fns::contacts::ImportNoPaidMessagesToken::CONSTRUCTOR_ID,
        &synthetic_tl_user(201, 2001, "Alpha", Some("alpha_test"), false, true, None),
    );
    let res = InfoBackend::import_no_paid_messages_token(&mut live, u201, "fee_waiver_xyz")
        .expect("importNoPaidMessagesToken ok");
    assert_eq!(res.user_id, u201);
    assert!(res.price_removed);
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::ImportNoPaidMessagesToken::CONSTRUCTOR_ID)[0],
        tl_fns::contacts::ImportNoPaidMessagesToken {
            user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 201,
                access_hash: 2001,
            }),
            token: "fee_waiver_xyz".to_owned(),
        }
        .to_bytes()
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let u = s.user(DataUserId(201)).expect("u201");
        assert_eq!(u.stars_per_message, 0);
        assert!(!u.flags.contains(UserFlags::HAS_STARS_PER_MESSAGE));
    });
}

#[test]
fn wp66_update_contact_note_set_and_clear() {
    let (rt, rpc, mut live) = make_live_session();
    let u201 = from_data_peer(DataPeerId::from_user(DataUserId(201)));

    let note = UiText::with_entities(
        "Met at Rust conf",
        vec![EntityInText::new(EntityType::Bold, 7, 4)],
    );
    rpc.enqueue_ok(tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID, &true);
    assert!(InfoBackend::update_contact_note(&mut live, u201, note.clone()).expect("set note ok"));

    let calls = rpc.recorded_of(tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        tl_fns::contacts::UpdateContactNote {
            id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 201,
                access_hash: 2001,
            }),
            note: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Met at Rust conf".to_owned(),
                entities: vec![tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                    offset: 7,
                    length: 4,
                })],
            }),
        }
        .to_bytes()
    );
    assert_eq!(InfoBackend::contact_note(&live, u201), Some(note));

    // Clear note (empty text sends empty TextWithEntities per `DeleteContactNote`).
    rpc.enqueue_ok(tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID, &true);
    assert!(
        InfoBackend::update_contact_note(&mut live, u201, UiText::simple(""))
            .expect("clear note ok")
    );
    let calls = rpc.recorded_of(tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        tl_fns::contacts::UpdateContactNote {
            id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 201,
                access_hash: 2001,
            }),
            note: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: String::new(),
                entities: Vec::new(),
            }),
        }
        .to_bytes()
    );
    assert_eq!(InfoBackend::contact_note(&live, u201), None);
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        assert!(s.user(DataUserId(201)).expect("u201").note.text.is_empty());
    });
}

#[test]
fn wp66_get_birthdays_filters_self_and_blocked_and_caches_per_day() {
    let (rt, rpc, live) = make_live_session();
    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        s.set_peer_blocked(DataPeerId::from_user(DataUserId(202)), true);
    });

    rpc.enqueue_ok(
        tl_fns::contacts::GetBirthdays::CONSTRUCTOR_ID,
        &tl_enums::contacts::ContactBirthdays::ContactBirthdays(
            tl_types::contacts::ContactBirthdays {
                contacts: vec![
                    tl_enums::ContactBirthday::ContactBirthday(tl_types::ContactBirthday {
                        contact_id: 100, // self -> excluded
                        birthday: tl_enums::Birthday::Birthday(tl_types::Birthday {
                            day: 1,
                            month: 1,
                            year: Some(1990),
                        }),
                    }),
                    tl_enums::ContactBirthday::ContactBirthday(tl_types::ContactBirthday {
                        contact_id: 202, // blocked -> excluded
                        birthday: tl_enums::Birthday::Birthday(tl_types::Birthday {
                            day: 2,
                            month: 2,
                            year: None,
                        }),
                    }),
                    tl_enums::ContactBirthday::ContactBirthday(tl_types::ContactBirthday {
                        contact_id: 201, // active contact -> included
                        birthday: tl_enums::Birthday::Birthday(tl_types::Birthday {
                            day: 15,
                            month: 8,
                            year: Some(1995),
                        }),
                    }),
                ],
                users: vec![synthetic_tl_user(
                    201,
                    2001,
                    "Alpha",
                    Some("alpha_test"),
                    false,
                    true,
                    None,
                )],
            },
        ),
    );

    let list = DialogsBackend::contact_birthdays(&live, false).expect("getBirthdays ok");
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].peer_id,
        from_data_peer(DataPeerId::from_user(DataUserId(201)))
    );
    assert_eq!(list[0].birthday.day, 15);
    assert_eq!(list[0].birthday.month, 8);
    assert_eq!(list[0].birthday.year, Some(1995));
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetBirthdays::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Second call without `force` serves from daily cache without hitting RPC.
    let cached = DialogsBackend::contact_birthdays(&live, false).expect("cached birthdays");
    assert_eq!(cached, list);
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetBirthdays::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Verify `User::birthday` was updated in `tg_data::Session`.
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let bday = s
            .user(DataUserId(201))
            .expect("u201")
            .birthday
            .expect("birthday");
        assert_eq!(bday.day, 15);
        assert_eq!(bday.month, 8);
        assert_eq!(bday.year, 1995);
    });
}

#[test]
fn wp66_block_from_replies_and_set_blocked_encode_exact_flags() {
    let (rt, rpc, mut live) = make_live_session();
    let u201_data = DataPeerId::from_user(DataUserId(201));
    let u202_data = DataPeerId::from_user(DataUserId(202));

    // 1. contacts.blockFromReplies with delete_message + report_spam, !delete_history
    let upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::PeerBlocked(tl_types::UpdatePeerBlocked {
            blocked: true,
            blocked_my_stories_from: false,
            peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 203 }),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_200,
        seq: 0,
    });
    rpc.enqueue_ok(tl_fns::contacts::BlockFromReplies::CONSTRUCTOR_ID, &upd);
    HistoryBackend::block_from_replies(
        &mut live,
        BlockFromRepliesRequest {
            msg_id: MsgId(777),
            delete_message: true,
            delete_history: false,
            report_spam: true,
        },
    )
    .expect("blockFromReplies ok");

    let calls = rpc.recorded_of(tl_fns::contacts::BlockFromReplies::CONSTRUCTOR_ID);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        tl_fns::contacts::BlockFromReplies {
            delete_message: true,
            delete_history: false,
            report_spam: true,
            msg_id: 777,
        }
        .to_bytes()
    );
    // `UpdatePeerBlocked` flowed through `SessionRuntime::apply_updates` into `Session`!
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        assert_eq!(
            s.peer(DataPeerId::from_user(DataUserId(203)))
                .expect("u203")
                .common()
                .block_status,
            BlockStatus::Blocked
        );
    });

    // 2. contacts.setBlocked (my_stories_from = true) via StoriesBackend::save_stories_blocklist
    rpc.enqueue_ok(tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID, &true);
    assert!(
        StoriesBackend::save_stories_blocklist(&mut live, &[u201_data, u202_data])
            .expect("save_stories_blocklist ok")
    );
    assert!(StoriesBackend::is_viewer_stories_blocked(&live, u201_data));
    assert!(StoriesBackend::is_viewer_stories_blocked(&live, u202_data));

    // Shrink blocklist to [u201]: limit must be max(prev=2, new=1) = 2 (matching `api_stories.cpp:604`).
    rpc.enqueue_ok(tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID, &true);
    assert!(
        StoriesBackend::save_stories_blocklist(&mut live, &[u201_data])
            .expect("shrink stories blocklist ok")
    );
    assert!(StoriesBackend::is_viewer_stories_blocked(&live, u201_data));
    assert!(!StoriesBackend::is_viewer_stories_blocked(&live, u202_data));

    let set_calls = rpc.recorded_of(tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID);
    assert_eq!(set_calls.len(), 2);
    assert_eq!(
        set_calls[1],
        tl_fns::contacts::SetBlocked {
            my_stories_from: true,
            id: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                user_id: 201,
                access_hash: 2001,
            })],
            limit: 2,
        }
        .to_bytes()
    );

    // 3. contacts.setBlocked (my_stories_from = false) via SettingsBackend::set_blocked_peers
    rpc.enqueue_ok(tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID, &true);
    let raw_202 = from_data_peer(u202_data).0;
    assert!(
        SettingsBackend::set_blocked_peers(&mut live, &[raw_202], false).expect("setBlocked ok")
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        assert_eq!(
            s.peer(u202_data).expect("u202").common().block_status,
            BlockStatus::Blocked
        );
    });
}

#[test]
fn wp66_toggle_top_peers_get_statuses_get_sponsored_peers_and_hide_promo() {
    let (rt, rpc, mut live) = make_live_session();
    let ch301 = from_data_peer(DataPeerId::from_channel(ChannelId(301)));

    // 1. contacts.toggleTopPeers(false) then (true)
    rpc.enqueue_ok(tl_fns::contacts::ToggleTopPeers::CONSTRUCTOR_ID, &true);
    assert!(DialogsBackend::toggle_top_peers(&mut live, false).expect("disable top peers"));
    assert!(!DialogsBackend::top_peers_enabled(&live));
    assert!(DialogsBackend::top_peers(&live).is_empty());

    rpc.enqueue_ok(tl_fns::contacts::ToggleTopPeers::CONSTRUCTOR_ID, &true);
    assert!(DialogsBackend::toggle_top_peers(&mut live, true).expect("enable top peers"));
    assert!(DialogsBackend::top_peers_enabled(&live));

    let toggle_calls = rpc.recorded_of(tl_fns::contacts::ToggleTopPeers::CONSTRUCTOR_ID);
    assert_eq!(toggle_calls.len(), 2);
    assert_eq!(
        toggle_calls[0],
        tl_fns::contacts::ToggleTopPeers { enabled: false }.to_bytes()
    );
    assert_eq!(
        toggle_calls[1],
        tl_fns::contacts::ToggleTopPeers { enabled: true }.to_bytes()
    );

    // 2. contacts.getStatuses updates User::lastseen (and preserves local pseudo-online on Recently)
    let now = now_unix_i32();
    let online_exp = now + 300;
    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        if let Some(u) = s.user_mut(DataUserId(202)) {
            u.lastseen = LastseenStatus::online_till_status(now + 25, true, false);
        }
    });
    rpc.enqueue_ok(
        tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID,
        &vec![
            tl_enums::ContactStatus::ContactStatus(tl_types::ContactStatus {
                user_id: 201,
                status: tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
                    expires: online_exp,
                }),
            }),
            tl_enums::ContactStatus::ContactStatus(tl_types::ContactStatus {
                user_id: 202,
                status: tl_enums::UserStatus::Recently(tl_types::UserStatusRecently {
                    by_me: false,
                }),
            }),
        ],
    );
    let statuses = DialogsBackend::refresh_contact_statuses(&mut live).expect("getStatuses ok");
    assert_eq!(statuses.len(), 2);
    assert!(statuses[0].is_online);
    assert_eq!(statuses[0].online_till, online_exp);
    // User 202 had local pseudo-online value, preserved by `LastseenStatus::from_api`!
    assert!(statuses[1].is_online);

    // 3. contacts.getSponsoredPeers (empty then populated)
    rpc.enqueue_ok(
        tl_fns::contacts::GetSponsoredPeers::CONSTRUCTOR_ID,
        &tl_enums::contacts::SponsoredPeers::Empty,
    );
    assert!(
        DialogsBackend::sponsored_peers(&live, "empty_q")
            .expect("empty sponsored")
            .is_empty()
    );

    rpc.enqueue_ok(
        tl_fns::contacts::GetSponsoredPeers::CONSTRUCTOR_ID,
        &tl_enums::contacts::SponsoredPeers::SponsoredPeers(tl_types::contacts::SponsoredPeers {
            peers: vec![tl_enums::SponsoredPeer::SponsoredPeer(
                tl_types::SponsoredPeer {
                    random_id: vec![0xDE, 0xAD, 0xBE, 0xEF],
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 215 }),
                    sponsor_info: Some("Demo Sponsor".to_owned()),
                    additional_info: Some("Ad disclosure".to_owned()),
                },
            )],
            chats: Vec::new(),
            users: vec![synthetic_tl_user(
                215,
                2015,
                "SponsoredBot",
                Some("sponsored_demo"),
                false,
                false,
                None,
            )],
        }),
    );
    let sponsored = DialogsBackend::sponsored_peers(&live, "rust").expect("sponsored ok");
    assert_eq!(sponsored.len(), 1);
    assert_eq!(
        sponsored[0].peer_id,
        from_data_peer(DataPeerId::from_user(DataUserId(215)))
    );
    assert_eq!(sponsored[0].random_id, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(sponsored[0].sponsor_info.as_deref(), Some("Demo Sponsor"));
    assert_eq!(
        sponsored[0].additional_info.as_deref(),
        Some("Ad disclosure")
    );
    assert_eq!(sponsored[0].username.as_deref(), Some("sponsored_demo"));
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetSponsoredPeers::CONSTRUCTOR_ID)[1],
        tl_fns::contacts::GetSponsoredPeers {
            q: "rust".to_owned(),
        }
        .to_bytes()
    );

    // 4. help.hidePromoData clears LivePromoState::promo_peer / psa_type / psa_message
    {
        let mut guard = live.promo_state.lock().expect("lock promo_state");
        guard.seed_snapshot_for_test(PromoDataSnapshot {
            expires: now + 3600,
            promo_peer: Some(ch301),
            psa_type: Some("PSA_DEMO".to_owned()),
            psa_message: Some("Demo PSA".to_owned()),
            ..PromoDataSnapshot::default()
        });
    }
    rpc.enqueue_ok(tl_fns::help::HidePromoData::CONSTRUCTOR_ID, &true);
    assert!(DialogsBackend::hide_promo_data(&mut live, ch301).expect("hidePromoData ok"));
    {
        let guard = live.promo_state.lock().expect("lock promo_state");
        let snap = guard.snapshot_for_test().expect("promo snapshot");
        assert_eq!(snap.promo_peer, None);
        assert_eq!(snap.psa_type, None);
        assert_eq!(snap.psa_message, None);
    }
    assert_eq!(
        rpc.recorded_of(tl_fns::help::HidePromoData::CONSTRUCTOR_ID)[0],
        tl_fns::help::HidePromoData {
            peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                channel_id: 301,
                access_hash: 3001,
            }),
        }
        .to_bytes()
    );
}

#[test]
fn wp66_photos_delete_and_upload_contact_profile_photo_modes() {
    let (rt, rpc, mut live) = make_live_session();
    let u201 = from_data_peer(DataPeerId::from_user(DataUserId(201)));

    // 1. photos.deletePhotos clears self `userpic_photo_id` when matching
    rpc.enqueue_ok(
        tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID,
        &vec![9_001i64],
    );
    let deleted = InfoBackend::delete_profile_photos(
        &mut live,
        &[ProfilePhotoRef {
            id: 9_001,
            access_hash: 99_001,
            file_reference: vec![1, 2, 3],
        }],
    )
    .expect("deletePhotos ok");
    assert_eq!(deleted, vec![9_001]);
    assert_eq!(
        rpc.recorded_of(tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID)[0],
        tl_fns::photos::DeletePhotos {
            id: vec![tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                id: 9_001,
                access_hash: 99_001,
                file_reference: vec![1, 2, 3],
            })],
        }
        .to_bytes()
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let self_u = s.user(DataUserId(100)).expect("self");
        assert_eq!(self_u.common.userpic_photo_id, PhotoId::ZERO);
        assert!(!self_u.common.userpic_has_video);
    });

    // 2. photos.uploadContactProfilePhoto (Save personal static photo)
    rpc.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID,
        &tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 77_001 }),
            users: Vec::new(),
        }),
    );
    let saved_id = InfoBackend::upload_contact_profile_photo(
        &mut live,
        u201,
        ContactProfilePhotoUpload::static_photo(
            ContactProfilePhotoMode::Save,
            "avatar.jpg",
            vec![0xFF, 0xD8, 0xFF, 0xD9],
        ),
    )
    .expect("save contact photo ok");
    assert_eq!(saved_id, 77_001);
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let u = s.user(DataUserId(201)).expect("u201");
        assert!(u.flags.contains(UserFlags::PERSONAL_PHOTO));
        assert_eq!(u.common.userpic_photo_id, PhotoId::from_tl(77_001));
    });

    // 3. photos.uploadContactProfilePhoto (Suggest with emoji markup -> suggest=true, save=false)
    rpc.enqueue_ok(
        tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID,
        &tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 77_002 }),
            users: Vec::new(),
        }),
    );
    let suggested_id = InfoBackend::upload_contact_profile_photo(
        &mut live,
        u201,
        ContactProfilePhotoUpload::emoji_markup(
            ContactProfilePhotoMode::Suggest,
            EmojiUserpicRequest {
                emoji_id: 555_001,
                emoji_glyph: "⭐".to_owned(),
                background_colors: vec![0x336699, 0x6699CC],
            },
        ),
    )
    .expect("suggest emoji photo ok");
    assert_eq!(suggested_id, 77_002);

    // 4. PeerPhoto::clearPersonal (`save = true, suggest = false`, no media)
    rpc.enqueue_ok(
        tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID,
        &tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
            users: Vec::new(),
        }),
    );
    let cleared_id =
        InfoBackend::clear_contact_profile_photo(&mut live, u201).expect("clearPersonal ok");
    assert_eq!(cleared_id, 0);
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let u = s.user(DataUserId(201)).expect("u201");
        assert!(!u.flags.contains(UserFlags::PERSONAL_PHOTO));
    });

    let up_calls = rpc.recorded_of(tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID);
    assert_eq!(up_calls.len(), 3);
    let req_save =
        tl_fns::photos::UploadContactProfilePhoto::from_bytes(&up_calls[0]).expect("decode save");
    assert!(req_save.save && !req_save.suggest && req_save.file.is_some());
    assert!(req_save.video.is_none() && req_save.video_start_ts.is_none());

    let req_suggest = tl_fns::photos::UploadContactProfilePhoto::from_bytes(&up_calls[1])
        .expect("decode suggest");
    assert!(req_suggest.suggest && !req_suggest.save && req_suggest.video_emoji_markup.is_some());

    let req_clear =
        tl_fns::photos::UploadContactProfilePhoto::from_bytes(&up_calls[2]).expect("decode clear");
    assert_eq!(
        up_calls[2],
        tl_fns::photos::UploadContactProfilePhoto {
            suggest: false,
            save: true,
            user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 201,
                access_hash: 2001,
            }),
            file: None,
            video: None,
            video_start_ts: None,
            video_emoji_markup: None,
        }
        .to_bytes()
    );
    assert!(req_clear.save && !req_clear.suggest && req_clear.file.is_none());
}

#[test]
fn wp66_synthetic_import_contacts_and_error_handling() {
    let (_rt, rpc, mut live) = make_live_session();
    let u201 = from_data_peer(DataPeerId::from_user(DataUserId(201)));

    // Synthetic-only `contacts.importContacts` test (explicitly non-real RFC 2606 / 555-style test digits).
    rpc.enqueue_ok(
        tl_fns::contacts::ImportContacts::CONSTRUCTOR_ID,
        &tl_enums::contacts::ImportedContacts::ImportedContacts(
            tl_types::contacts::ImportedContacts {
                imported: vec![tl_enums::ImportedContact::ImportedContact(
                    tl_types::ImportedContact {
                        user_id: 250,
                        client_id: 1,
                    },
                )],
                popular_invites: Vec::new(),
                retry_contacts: Vec::new(),
                users: vec![synthetic_tl_user(
                    250,
                    2050,
                    "Synthetic",
                    Some("synth_contact"),
                    false,
                    true,
                    None,
                )],
            },
        ),
    );
    let imported = InfoBackend::add_contact(&mut live, "Synthetic", "Contact", "+0000000000")
        .expect("synthetic importContacts ok");
    assert_eq!(
        imported,
        from_data_peer(DataPeerId::from_user(DataUserId(250)))
    );

    // Server RPC error surfaces cleanly without panicking.
    rpc.enqueue_rpc_error(
        tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID,
        400,
        "IMPORT_TOKEN_INVALID",
    );
    let err = InfoBackend::import_contact_token(&mut live, "bad_token").unwrap_err();
    assert!(err.contains("IMPORT_TOKEN_INVALID"), "got: {err}");

    // Suggesting a contact photo without media is rejected locally.
    assert!(
        InfoBackend::upload_contact_profile_photo(
            &mut live,
            u201,
            ContactProfilePhotoUpload {
                mode: ContactProfilePhotoMode::Suggest,
                image: None,
                video: None,
                video_start_ts: None,
                emoji_markup: None,
            },
        )
        .is_err()
    );
}
