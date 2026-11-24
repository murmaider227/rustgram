//! Headless `FakeRpcClient` tests for Connected Websites (`account.getWebAuthorizations`,
//! `account.resetWebAuthorization`, `account.resetWebAuthorizations`) and Custom Ringtones
//! (`account.getSavedRingtones`, `account.saveRingtone`, `upload.saveFilePart` +
//! `account.uploadRingtone`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::settings::backend::{DEFAULT_RINGTONE_MAX_SIZE, SettingsBackend};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

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

    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .map(|(ctor, _)| *ctor)
            .collect()
    }

    fn recorded_requests_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
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

fn synthetic_tl_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    bot: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !bot,
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
        access_hash: Some(id * 1000 + 7),
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

fn start_live_session() -> (FakeRpcClient, LiveSession) {
    let self_uid = 600_001i64;
    let self_user = synthetic_tl_user(self_uid, "Taylor", "taylor_self", true, false);

    let fake = FakeRpcClient::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user)],
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
            chats: Vec::new(),
            users: Vec::new(),
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

    fake.clear_recorded();
    let live = LiveSession::new(runtime, 1);
    (fake, live)
}

fn synthetic_ringtone_doc(
    id: i64,
    file_name: &str,
    title: Option<&str>,
    duration: i32,
) -> tl_enums::Document {
    let mut attributes = vec![tl_enums::DocumentAttribute::Filename(
        tl_types::DocumentAttributeFilename {
            file_name: file_name.to_owned(),
        },
    )];
    attributes.push(tl_enums::DocumentAttribute::Audio(
        tl_types::DocumentAttributeAudio {
            voice: false,
            duration,
            title: title.map(str::to_owned),
            performer: None,
            waveform: None,
        },
    ));
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 17 + 3,
        file_reference: vec![0x11, 0x22, 0x33],
        date: 1_750_010_000,
        mime_type: "audio/mpeg".to_owned(),
        size: 16_384,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes,
    })
}

#[test]
fn settings_extra_web_authorizations_load_disconnect_and_block() {
    let (fake, mut live) = start_live_session();

    let bot_a = synthetic_tl_user(900_001, "Atlas Docs Bot", "atlas_docs_bot", false, true);
    let bot_b = synthetic_tl_user(900_002, "Relay Status Bot", "relay_status_bot", false, true);

    fake.enqueue_ok(
        tl_fns::account::GetWebAuthorizations::CONSTRUCTOR_ID,
        &tl_enums::account::WebAuthorizations::WebAuthorizations(
            tl_types::account::WebAuthorizations {
                authorizations: vec![
                    tl_enums::WebAuthorization::WebAuthorization(tl_types::WebAuthorization {
                        hash: 101,
                        bot_id: 900_002,
                        domain: "status.relay.example".to_owned(),
                        browser: "Edge 131".to_owned(),
                        platform: "Windows".to_owned(),
                        date_created: 1_749_000_000,
                        date_active: 1_749_100_000,
                        ip: "203.0.113.71".to_owned(),
                        region: "Geneva, Switzerland".to_owned(),
                    }),
                    tl_enums::WebAuthorization::WebAuthorization(tl_types::WebAuthorization {
                        hash: 202,
                        bot_id: 900_001,
                        domain: "docs.atlas.example".to_owned(),
                        browser: "Firefox 132".to_owned(),
                        platform: "Windows".to_owned(),
                        date_created: 1_749_500_000,
                        date_active: 1_750_000_000,
                        ip: "198.51.100.40".to_owned(),
                        region: "Zurich, Switzerland".to_owned(),
                    }),
                ],
                users: vec![tl_enums::User::User(bot_a), tl_enums::User::User(bot_b)],
            },
        ),
    );

    let list = live.web_authorizations();
    assert_eq!(list.len(), 2);
    // Sorted newest date_active first.
    assert_eq!(list[0].hash, 202);
    assert_eq!(list[0].bot_name, "Atlas Docs Bot");
    assert_eq!(list[0].domain, "docs.atlas.example");
    assert_eq!(list[1].hash, 101);
    assert_eq!(list[1].bot_name, "Relay Status Bot");

    // Second read hits cache (no extra GetWebAuthorizations RPC).
    let cached = live.web_authorizations();
    assert_eq!(cached.len(), 2);
    assert_eq!(
        fake.recorded_requests_for(tl_fns::account::GetWebAuthorizations::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Disconnect one website AND block its bot.
    fake.enqueue_ok(
        tl_fns::account::ResetWebAuthorization::CONSTRUCTOR_ID,
        &true,
    );
    fake.enqueue_ok(tl_fns::contacts::Block::CONSTRUCTOR_ID, &true);
    let disconnected = live
        .disconnect_website(202, true)
        .expect("disconnect_website succeeds");
    assert!(disconnected);
    assert!(
        fake.recorded_ctors()
            .contains(&tl_fns::account::ResetWebAuthorization::CONSTRUCTOR_ID)
    );
    assert!(
        fake.recorded_ctors()
            .contains(&tl_fns::contacts::Block::CONSTRUCTOR_ID)
    );
    assert_eq!(live.web_authorizations().len(), 1);
    assert_eq!(live.web_authorizations()[0].hash, 101);

    // Disconnect all remaining websites.
    fake.enqueue_ok(
        tl_fns::account::ResetWebAuthorizations::CONSTRUCTOR_ID,
        &true,
    );
    let removed = live
        .disconnect_all_websites()
        .expect("disconnect_all_websites succeeds");
    assert_eq!(removed, 1);
    assert!(live.web_authorizations().is_empty());
    assert!(
        fake.recorded_ctors()
            .contains(&tl_fns::account::ResetWebAuthorizations::CONSTRUCTOR_ID)
    );
}

#[test]
fn settings_extra_saved_ringtones_not_modified_save_and_upload() {
    let (fake, mut live) = start_live_session();

    // 1. Initial getSavedRingtones
    fake.enqueue_ok(
        tl_fns::account::GetSavedRingtones::CONSTRUCTOR_ID,
        &tl_enums::account::SavedRingtones::SavedRingtones(tl_types::account::SavedRingtones {
            hash: 4242,
            ringtones: vec![
                synthetic_ringtone_doc(700_001, "soft_chime.mp3", Some("Soft Chime"), 2),
                synthetic_ringtone_doc(700_002, "glass_bell.ogg", None, 3),
            ],
        }),
    );

    let ringtones = live.saved_ringtones();
    assert_eq!(ringtones.len(), 2);
    assert_eq!(ringtones[0].id, 700_001);
    assert_eq!(ringtones[0].title, "Soft Chime");
    assert_eq!(ringtones[1].id, 700_002);
    assert_eq!(ringtones[1].title, "glass_bell");

    // 2. Cache invalidation -> sends stored hash and handles NotModified
    live.invalidate_settings_caches();
    fake.enqueue_ok(
        tl_fns::account::GetSavedRingtones::CONSTRUCTOR_ID,
        &tl_enums::account::SavedRingtones::NotModified,
    );
    let after_not_mod = live.saved_ringtones();
    assert_eq!(after_not_mod.len(), 2);

    // 3. Unsave ringtone removes it from cache
    fake.enqueue_ok(
        tl_fns::account::SaveRingtone::CONSTRUCTOR_ID,
        &tl_enums::account::SavedRingtone::SavedRingtone,
    );
    live.save_ringtone(700_001, true)
        .expect("unsave ringtone succeeds");
    let remaining = live.saved_ringtones();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, 700_002);

    // 4. Save ringtone with SavedRingtoneConverted replaces/inserts converted doc
    fake.enqueue_ok(
        tl_fns::account::SaveRingtone::CONSTRUCTOR_ID,
        &tl_enums::account::SavedRingtone::Converted(tl_types::account::SavedRingtoneConverted {
            document: synthetic_ringtone_doc(700_009, "converted.mp3", Some("Converted Tone"), 4),
        }),
    );
    live.save_ringtone(700_009, false)
        .expect("save converted ringtone succeeds");
    assert_eq!(live.saved_ringtones()[0].id, 700_009);
    assert_eq!(live.saved_ringtones()[0].title, "Converted Tone");

    // 5. Upload ringtone validates empty & oversize BEFORE touching RPC
    fake.clear_recorded();
    assert!(
        live.upload_ringtone("empty.mp3", "audio/mpeg", &[])
            .is_err()
    );
    let oversized = vec![0u8; DEFAULT_RINGTONE_MAX_SIZE + 1];
    assert!(
        live.upload_ringtone("too_big.mp3", "audio/mpeg", &oversized)
            .is_err()
    );
    assert!(
        fake.recorded_ctors().is_empty(),
        "invalid ringtone uploads must fail locally before RPC"
    );

    // 6. Valid ringtone upload invokes upload.saveFilePart + account.uploadRingtone
    fake.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::account::UploadRingtone::CONSTRUCTOR_ID,
        &synthetic_ringtone_doc(700_010, "marimba.mp3", Some("Marimba"), 2),
    );
    let payload = vec![0x49, 0x44, 0x33, 0x04, 0x00, 0x00, 0x00, 0x00];
    let uploaded = live
        .upload_ringtone("marimba.mp3", "audio/mpeg", &payload)
        .expect("upload_ringtone succeeds");
    assert_eq!(uploaded.id, 700_010);
    assert_eq!(uploaded.title, "Marimba");
    assert_eq!(
        fake.recorded_ctors(),
        vec![
            tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID,
            tl_fns::account::UploadRingtone::CONSTRUCTOR_ID,
        ]
    );
    assert_eq!(live.saved_ringtones()[0].id, 700_010);

    // 7. Server error on resetWebAuthorization propagates cleanly
    fake.enqueue_err(
        tl_fns::account::ResetWebAuthorization::CONSTRUCTOR_ID,
        InvocationError::Dropped,
    );
    assert!(live.disconnect_website(999, false).is_err());
}
