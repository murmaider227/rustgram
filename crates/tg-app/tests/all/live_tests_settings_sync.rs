//! Integration tests for WP-06 live settings sync, profile photo uploads,
//! notification exceptions sync, local passcode / auto-lock, and `tdata`
//! settings persistence & restore.
//!
//! All tests run exclusively against synthetic keys/users and an in-process
//! `FakeRpcClient` (zero real network calls).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{ApiCredentials, DcId, InvocationError, RpcClient};
use tg_core::settings::{
    AutoplaySettings, InputSubmitSettings, NotifyView, PowerSavingFlags, ScreenCorner, WorkMode,
};
use tg_core::{CoreSettings, GlobalConfig};
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_session::{
    AccountManager, SessionRuntime, SessionRuntimeConfig, StoredSessionSettings,
    WorkingDirOpenOptions,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::dialogs::backend::DialogsBackend;
use tg_app::live::LiveSession;
use tg_app::live::settings_sync::{
    AutoDownloadPreset, UPLOAD_PART_SIZE, core_window_position_to_shell,
    persist_all_settings_to_tdata, shell_window_position_to_core,
};
use tg_app::notifications::AppNotifyTrayHost;
use tg_app::notifications::settings::{DefaultNotify, NotifySettingsStore, NotifySound};
use tg_app::session_api::AppSession;
use tg_app::settings::SettingsController;
use tg_app::settings::backend::{
    AutoDownloadSettings, AutoDownloadSource, AutoDownloadType, GlobalPrivacySettings,
    NotifyException, NotifyScope, ProfilePhotoUpload, ScopeNotifySettings, SettingsBackend,
};
use tg_app::settings::local_passcode::{AutoLockController, PasscodeVerifierFn};
use tg_app::stand_in::settings::{ShellSettings, WindowPosition as ShellWindowPosition};
use tg_app::stand_in::types::PeerId;
use tg_app::window::controller::{RootScreen, WindowController};

// =============================================================================
// Local FakeRpcClient & Synthetic TL Fixtures
// =============================================================================

type RecordedCall = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
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

    fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
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
            } else if crate::common::is_background_list_request(ctor) {
                // Never the answer a test queued for another request (TGB-2941).
                None
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
        access_hash: Some(id * 100 + 3),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Fixture".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("9996620001".to_owned()),
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

fn tl_auto_dl(
    disabled: bool,
    photo_mb: i32,
    video_mb: i64,
    file_mb: i64,
) -> tl_enums::AutoDownloadSettings {
    const MB: i64 = 1024 * 1024;
    tl_enums::AutoDownloadSettings::AutoDownloadSettings(tl_types::AutoDownloadSettings {
        disabled,
        video_preload_large: true,
        audio_preload_next: true,
        phonecalls_less_data: false,
        stories_preload: false,
        photo_size_max: photo_mb * (MB as i32),
        video_size_max: video_mb * MB,
        file_size_max: file_mb * MB,
        video_upload_maxbitrate: 0,
        small_queue_active_operations_max: 2,
        large_queue_active_operations_max: 2,
    })
}

struct SettingsSyncHarness {
    self_uid: i64,
    peer_uid: i64,
    peer_id: PeerId,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    live: LiveSession,
}

impl SettingsSyncHarness {
    fn start() -> Self {
        let self_uid = 610_001i64;
        let peer_uid = 610_002i64;
        let self_user = synthetic_tl_user(self_uid, "Self", "self_fixture", true);
        let peer_user = synthetic_tl_user(peer_uid, "Peer", "peer_fixture", false);

        let fake = FakeRpcClient::new(2);
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

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                tg_app::live::ErasedRpcClient::new(fake.clone()),
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
        runtime.seed_self_user(&self_user);
        {
            let sess = runtime.session();
            let handle = runtime.tokio_handle();
            handle.block_on(async {
                let mut guard = sess.write().await;
                tg_api::convert::apply_tl_users_and_chats(
                    &mut guard,
                    &[tl_enums::User::User(peer_user)],
                    &[],
                );
            });
        }
        let live = LiveSession::new(Arc::clone(&runtime), self_uid as u64);
        let peer_id = PeerId(DataPeerId::from_user(DataUserId::from_tl(peer_uid)).0);

        Self {
            self_uid,
            peer_uid,
            peer_id,
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

fn test_creds() -> ApiCredentials {
    ApiCredentials {
        api_id: 24_680,
        api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
    }
}

// =============================================================================
// 1. Server-synced Auto-Download & Presets
// =============================================================================

#[test]
fn auto_download_get_save_and_presets_sync() {
    let mut h = SettingsSyncHarness::start();
    const MB: i64 = 1024 * 1024;

    h.fake.enqueue_ok(
        tl_fns::account::GetAutoDownloadSettings::CONSTRUCTOR_ID,
        &tl_enums::account::AutoDownloadSettings::AutoDownloadSettings(
            tl_types::account::AutoDownloadSettings {
                low: tl_auto_dl(false, 1, 2, 1),
                medium: tl_auto_dl(false, 4, 10, 3),
                high: tl_auto_dl(false, 10, 50, 20),
            },
        ),
    );

    // First read fetches server presets and adopts Medium by default.
    let current = h.live.auto_download();
    assert_eq!(
        current.bytes_limit(AutoDownloadSource::Private, AutoDownloadType::Photo),
        4 * MB
    );
    assert_eq!(
        current.bytes_limit(AutoDownloadSource::Groups, AutoDownloadType::AutoPlayVideo),
        10 * MB
    );
    assert_eq!(
        current.bytes_limit(AutoDownloadSource::Channels, AutoDownloadType::File),
        3 * MB
    );

    // Switching to Low preset sends `account.saveAutoDownloadSettings(low=true, high=false)`.
    h.fake.clear_recorded();
    h.fake.enqueue_ok(
        tl_fns::account::SaveAutoDownloadSettings::CONSTRUCTOR_ID,
        &true,
    );
    let low = h
        .live
        .apply_auto_download_preset(AutoDownloadPreset::Low)
        .expect("apply low preset");
    assert_eq!(
        low.bytes_limit(AutoDownloadSource::Private, AutoDownloadType::AutoPlayVideo),
        2 * MB
    );
    assert!(h.live.take_session_settings_dirty());

    let calls = h.fake.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0,
        tl_fns::account::SaveAutoDownloadSettings::CONSTRUCTOR_ID
    );
    let saved_low =
        tl_fns::account::SaveAutoDownloadSettings::from_bytes(&calls[0].1).expect("decode low");
    assert!(saved_low.low);
    assert!(!saved_low.high);

    // Switching to High preset sends `account.saveAutoDownloadSettings(low=false, high=true)`.
    h.fake.clear_recorded();
    h.fake.enqueue_ok(
        tl_fns::account::SaveAutoDownloadSettings::CONSTRUCTOR_ID,
        &true,
    );
    let high = h
        .live
        .apply_auto_download_preset(AutoDownloadPreset::High)
        .expect("apply high preset");
    assert_eq!(
        high.bytes_limit(AutoDownloadSource::Private, AutoDownloadType::AutoPlayVideo),
        50 * MB
    );
    let calls = h.fake.recorded_calls();
    let saved_high =
        tl_fns::account::SaveAutoDownloadSettings::from_bytes(&calls[0].1).expect("decode high");
    assert!(!saved_high.low);
    assert!(saved_high.high);

    // Custom manual save via SettingsBackend::set_auto_download.
    let mut custom = AutoDownloadSettings::default();
    custom.set_bytes_limit(AutoDownloadSource::Private, AutoDownloadType::File, 7 * MB);
    custom.autoplay_gifs = false;
    h.fake.clear_recorded();
    h.fake.enqueue_ok(
        tl_fns::account::SaveAutoDownloadSettings::CONSTRUCTOR_ID,
        &true,
    );
    h.live.set_auto_download(custom.clone());
    assert_eq!(h.live.auto_download(), custom);
    assert!(h.live.take_session_settings_dirty());
    assert_eq!(
        h.fake.recorded_calls()[0].0,
        tl_fns::account::SaveAutoDownloadSettings::CONSTRUCTOR_ID
    );

    h.shutdown();
}

// =============================================================================
// 2. Global Privacy, Default Auto-Delete TTL, and Sensitive Content
// =============================================================================

#[test]
fn global_privacy_default_ttl_and_sensitive_content_sync() {
    let mut h = SettingsSyncHarness::start();

    h.fake.enqueue_ok(
        tl_fns::account::GetGlobalPrivacySettings::CONSTRUCTOR_ID,
        &tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(tl_types::GlobalPrivacySettings {
            archive_and_mute_new_noncontact_peers: true,
            keep_archived_unmuted: false,
            keep_archived_folders: true,
            hide_read_marks: true,
            new_noncontact_peers_require_premium: true,
            display_gifts_button: true,
            noncontact_peers_paid_stars: Some(25),
            disallowed_gifts: None,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::GetDefaultHistoryTTL::CONSTRUCTOR_ID,
        &tl_enums::DefaultHistoryTTL::DefaultHistoryTTL(tl_types::DefaultHistoryTTL {
            period: 86_400,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetContentSettings::CONSTRUCTOR_ID,
        &tl_enums::account::ContentSettings::ContentSettings(tl_types::account::ContentSettings {
            sensitive_enabled: false,
            sensitive_can_change: true,
        }),
    );

    let gp = h.live.global_privacy();
    assert!(gp.archive_and_mute_new);
    assert!(gp.keep_archived_folders);
    assert!(gp.hide_read_time);
    assert!(gp.new_noncontact_peers_require_premium);
    assert_eq!(gp.noncontact_peers_paid_stars, Some(25));
    assert_eq!(gp.global_ttl_seconds, 86_400);
    assert!(!gp.disable_sensitive_content_filtering);
    assert!(gp.sensitive_can_change);

    // Update all three server settings via set_global_privacy.
    let updated = GlobalPrivacySettings {
        archive_and_mute_new: false,
        keep_archived_unmuted: true,
        keep_archived_folders: true,
        hide_read_time: false,
        new_noncontact_peers_require_premium: false,
        noncontact_peers_paid_stars: Some(50),
        disable_sensitive_content_filtering: true,
        sensitive_can_change: true,
        global_ttl_seconds: 604_800,
    };

    h.fake.clear_recorded();
    h.fake.enqueue_ok(
        tl_fns::account::SetGlobalPrivacySettings::CONSTRUCTOR_ID,
        &tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(tl_types::GlobalPrivacySettings {
            archive_and_mute_new_noncontact_peers: false,
            keep_archived_unmuted: true,
            keep_archived_folders: true,
            hide_read_marks: false,
            new_noncontact_peers_require_premium: false,
            display_gifts_button: true,
            noncontact_peers_paid_stars: Some(50),
            disallowed_gifts: None,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::SetDefaultHistoryTTL::CONSTRUCTOR_ID,
        &true,
    );
    h.fake
        .enqueue_ok(tl_fns::account::SetContentSettings::CONSTRUCTOR_ID, &true);

    h.live.set_global_privacy(updated.clone());
    assert_eq!(h.live.global_privacy(), updated);

    let calls = h.fake.recorded_calls();
    assert_eq!(calls.len(), 3);
    let sent_gp =
        tl_fns::account::SetGlobalPrivacySettings::from_bytes(&calls[0].1).expect("decode set gp");
    let tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(inner_gp) = sent_gp.settings;
    assert!(!inner_gp.archive_and_mute_new_noncontact_peers);
    assert!(inner_gp.keep_archived_unmuted);
    assert_eq!(inner_gp.noncontact_peers_paid_stars, Some(50));
    // Auxiliary field returned by getGlobalPrivacySettings must be preserved on save.
    assert!(inner_gp.display_gifts_button);

    let sent_ttl =
        tl_fns::messages::SetDefaultHistoryTTL::from_bytes(&calls[1].1).expect("decode set ttl");
    assert_eq!(sent_ttl.period, 604_800);

    let sent_content =
        tl_fns::account::SetContentSettings::from_bytes(&calls[2].1).expect("decode set content");
    assert!(sent_content.sensitive_enabled);

    // Verify locked sensitive_can_change == false guard.
    h.live.invalidate_settings_caches();
    h.fake.enqueue_ok(
        tl_fns::account::GetGlobalPrivacySettings::CONSTRUCTOR_ID,
        &tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(tl_types::GlobalPrivacySettings {
            archive_and_mute_new_noncontact_peers: false,
            keep_archived_unmuted: false,
            keep_archived_folders: false,
            hide_read_marks: false,
            new_noncontact_peers_require_premium: false,
            display_gifts_button: false,
            noncontact_peers_paid_stars: None,
            disallowed_gifts: None,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::GetDefaultHistoryTTL::CONSTRUCTOR_ID,
        &tl_enums::DefaultHistoryTTL::DefaultHistoryTTL(tl_types::DefaultHistoryTTL { period: 0 }),
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetContentSettings::CONSTRUCTOR_ID,
        &tl_enums::account::ContentSettings::ContentSettings(tl_types::account::ContentSettings {
            sensitive_enabled: false,
            sensitive_can_change: false,
        }),
    );
    let locked = h.live.global_privacy();
    assert!(!locked.sensitive_can_change);
    assert!(h.live.set_content_settings(true).is_err());

    h.shutdown();
}

// =============================================================================
// 3. Notification Exceptions Sync with NotifySettingsStore
// =============================================================================

#[test]
fn notification_exceptions_sync_with_notify_settings_store() {
    let mut h = SettingsSyncHarness::start();
    let now = 1_750_000_000i64;

    h.fake.enqueue_ok(
        tl_fns::account::GetNotifySettings::CONSTRUCTOR_ID,
        &tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
            show_previews: Some(true),
            silent: Some(false),
            mute_until: Some(0),
            ios_sound: None,
            android_sound: None,
            other_sound: Some(tl_enums::NotificationSound::Default),
            stories_muted: None,
            stories_hide_sender: None,
            stories_ios_sound: None,
            stories_android_sound: None,
            stories_other_sound: None,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetNotifyExceptions::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NotifySettings(
                tl_types::UpdateNotifySettings {
                    peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                        peer: tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: h.peer_uid,
                        }),
                    }),
                    notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                        tl_types::PeerNotifySettings {
                            show_previews: Some(false),
                            silent: Some(true),
                            mute_until: Some(i32::MAX),
                            ios_sound: None,
                            android_sound: None,
                            other_sound: Some(tl_enums::NotificationSound::None),
                            stories_muted: None,
                            stories_hide_sender: None,
                            stories_ios_sound: None,
                            stories_android_sound: None,
                            stories_other_sound: None,
                        },
                    ),
                },
            )],
            users: vec![tl_enums::User::User(synthetic_tl_user(
                h.peer_uid,
                "Peer",
                "peer_fixture",
                false,
            ))],
            chats: Vec::new(),
            date: now as i32,
            seq: 2,
        }),
    );

    // Settings reads answer at once and load in the background (TGB-3070).
    let _ = h.live.scope_notifications(NotifyScope::PrivateChats);
    assert!(
        h.live
            .wait_settings_reads(std::time::Duration::from_secs(10))
    );
    let scope = h.live.scope_notifications(NotifyScope::PrivateChats);
    assert!(scope.enabled);
    assert_eq!(scope.exceptions.len(), 1);
    assert_eq!(scope.exceptions[0].peer.id, h.peer_id.0);
    assert!(scope.exceptions[0].muted);

    // Sync into NotifySettingsStore and verify effective mute & sound resolution.
    let mut store = NotifySettingsStore::default();
    h.live.sync_notify_settings_store(&mut store, now);
    assert!(!store.is_scope_muted(DefaultNotify::User, now));
    assert!(store.is_peer_muted(h.peer_id, now));
    assert_eq!(
        store.sound_for_peer(h.peer_id, false),
        NotifySound::silent()
    );

    // Remove the exception via set_scope_notifications -> sends scope update + peer reset RPC.
    h.fake.clear_recorded();
    h.fake
        .enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    h.fake
        .enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    h.live.set_scope_notifications(
        NotifyScope::PrivateChats,
        ScopeNotifySettings {
            enabled: true,
            show_preview: true,
            sound: "Default".to_owned(),
            exceptions: Vec::new(),
        },
    );
    // Applied at once, sent in the background in order (TGB-3104).
    assert!(
        h.live
            .flush_background_rpcs(std::time::Duration::from_secs(10))
    );
    let calls = h.fake.recorded_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].0,
        tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID
    );
    assert_eq!(
        calls[1].0,
        tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID
    );

    h.live.sync_notify_settings_store(&mut store, now);
    assert!(!store.is_peer_muted(h.peer_id, now));

    // Re-add an explicit muted exception and verify it propagates back.
    h.fake.clear_recorded();
    h.fake
        .enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    h.fake
        .enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    h.live.set_scope_notifications(
        NotifyScope::PrivateChats,
        ScopeNotifySettings {
            enabled: true,
            show_preview: true,
            sound: "Default".to_owned(),
            exceptions: vec![NotifyException {
                peer: scope.exceptions[0].peer.clone(),
                muted: true,
                sound: "None".to_owned(),
            }],
        },
    );
    h.live.sync_notify_settings_store(&mut store, now);
    assert!(store.is_peer_muted(h.peer_id, now));

    h.shutdown();
}

// =============================================================================
// 4. Profile Photo Upload (Static, Video Avatar, Fallback, File Path, Clear)
// =============================================================================

#[test]
fn profile_photo_static_video_fallback_and_clear() {
    let mut h = SettingsSyncHarness::start();

    let make_photo_resp = |photo_id: i64| {
        tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: photo_id }),
            users: vec![tl_enums::User::User(synthetic_tl_user(
                h.self_uid,
                "Self",
                "self_fixture",
                true,
            ))],
        })
    };

    // 1. Multi-part static photo (> 512 KiB -> 2 upload.saveFilePart calls + photos.uploadProfilePhoto).
    let static_bytes = vec![0xABu8; UPLOAD_PART_SIZE + 1024];
    let expected_md5 = tg_crypto::md5_hex(&static_bytes);
    h.fake.clear_recorded();
    h.fake
        .enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    h.fake
        .enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    h.fake.enqueue_ok(
        tl_fns::photos::UploadProfilePhoto::CONSTRUCTOR_ID,
        &make_photo_resp(900_111),
    );

    let photo_id = h
        .live
        .upload_profile_photo(ProfilePhotoUpload::static_photo(
            "avatar.jpg",
            static_bytes,
            false,
        ))
        .expect("upload static photo");
    assert_eq!(photo_id, 900_111);

    let calls = h.fake.recorded_calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID);
    assert_eq!(calls[1].0, tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID);
    let upload_req =
        tl_fns::photos::UploadProfilePhoto::from_bytes(&calls[2].1).expect("decode upload req");
    assert!(!upload_req.fallback);
    assert!(upload_req.video.is_none());
    match upload_req.file.expect("file present") {
        tl_enums::InputFile::InputFile(f) => {
            assert_eq!(f.parts, 2);
            assert_eq!(f.name, "avatar.jpg");
            assert_eq!(f.md5_checksum, expected_md5);
        }
        other => panic!("expected InputFile::InputFile, got {other:?}"),
    }

    // 2. Fallback video avatar with video_start_ts.
    h.fake.clear_recorded();
    h.fake
        .enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    h.fake.enqueue_ok(
        tl_fns::photos::UploadProfilePhoto::CONSTRUCTOR_ID,
        &make_photo_resp(900_222),
    );
    let vid_id = h
        .live
        .upload_profile_photo(ProfilePhotoUpload::video_avatar(
            "loop.mp4",
            vec![0xCDu8; 4096],
            Some(1.5),
            true,
        ))
        .expect("upload fallback video avatar");
    assert_eq!(vid_id, 900_222);

    let calls = h.fake.recorded_calls();
    assert_eq!(calls.len(), 2);
    let vid_req =
        tl_fns::photos::UploadProfilePhoto::from_bytes(&calls[1].1).expect("decode video req");
    assert!(vid_req.fallback);
    assert!(vid_req.file.is_none());
    assert!(vid_req.video.is_some());
    assert_eq!(vid_req.video_start_ts, Some(1.5));

    // 3. update_profile_photo with a real file path on disk triggers upload.
    let dir = TempDir::new().expect("tempdir");
    let img_path = dir.path().join("disk_avatar.png");
    std::fs::write(&img_path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
        .expect("write avatar file");
    h.fake.clear_recorded();
    h.fake
        .enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    h.fake.enqueue_ok(
        tl_fns::photos::UploadProfilePhoto::CONSTRUCTOR_ID,
        &make_photo_resp(900_333),
    );
    h.live
        .update_profile_photo(3, Some(img_path.to_string_lossy().into_owned()));
    let calls = h.fake.recorded_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID);
    assert_eq!(
        calls[1].0,
        tl_fns::photos::UploadProfilePhoto::CONSTRUCTOR_ID
    );

    // 4. Clearing profile photo sends photos.updateProfilePhoto(id: InputPhoto::Empty).
    h.fake.clear_recorded();
    h.fake.enqueue_ok(
        tl_fns::photos::UpdateProfilePhoto::CONSTRUCTOR_ID,
        &make_photo_resp(0),
    );
    h.live.clear_profile_photo(false).expect("clear photo");
    let calls = h.fake.recorded_calls();
    assert_eq!(calls.len(), 1);
    let clear_req =
        tl_fns::photos::UpdateProfilePhoto::from_bytes(&calls[0].1).expect("decode clear req");
    assert!(!clear_req.fallback);
    assert!(matches!(clear_req.id, tl_enums::InputPhoto::Empty));

    h.shutdown();
}

// =============================================================================
// 5. Local Passcode Auto-Lock + Live Lock Screen Verification
// =============================================================================

#[test]
fn local_passcode_auto_lock_and_live_lock_screen() {
    // Part A: AutoLockController idle timeout + sleep/wake late-deadline behavior.
    let mut auto_lock = AutoLockController::new();
    let t0 = Instant::now();
    // Disabled when no passcode or timeout == 0.
    assert!(!auto_lock.check_with_idle(false, false, 60, Some(Duration::from_secs(120)), t0));
    assert!(!auto_lock.check_with_idle(true, false, 0, Some(Duration::from_secs(120)), t0));
    // Idle below 60s threshold -> stays unlocked.
    assert!(!auto_lock.check_with_idle(true, false, 60, Some(Duration::from_secs(15)), t0));
    // Idle reaches 60s -> locks immediately.
    assert!(auto_lock.check_with_idle(
        true,
        false,
        60,
        Some(Duration::from_secs(60)),
        t0 + Duration::from_secs(1)
    ));
    // Already locked -> does not fire again and resets internal deadline.
    assert!(!auto_lock.check_with_idle(
        true,
        true,
        60,
        Some(Duration::from_secs(90)),
        t0 + Duration::from_secs(2)
    ));
    // Sleep/wake scenario: clock jumps well past scheduled deadline + 3s grace even with fresh 0s idle.
    let t_wake_base = t0 + Duration::from_secs(10);
    assert!(!auto_lock.check_with_idle(true, false, 60, Some(Duration::from_secs(5)), t_wake_base));
    assert!(auto_lock.check_with_idle(
        true,
        false,
        60,
        Some(Duration::from_secs(0)),
        t_wake_base + Duration::from_secs(70)
    ));

    // Part B: WindowController live passcode verifier backed by AccountManager.
    let dir = TempDir::new().expect("tempdir");
    let opts = WorkingDirOpenOptions::new(dir.path(), test_creds());
    let mut mgr = AccountManager::open_and_unlock(opts, b"").expect("open mgr");
    mgr.complete_login(0, &synthetic_tl_user(777_001, "Lock", "lock_user", true))
        .expect("complete login");
    let fast_kdf = tg_crypto::PasscodeKdf {
        kind: tg_crypto::PasscodeKdfKind::Argon2id,
        memory: 64,
        time: 1,
        parallel: 1,
    };
    mgr.set_local_passcode_with_kdf(None, b"live-secret", fast_kdf)
        .expect("set passcode");

    let mgr_arc = Arc::new(Mutex::new(mgr));
    let verifier: PasscodeVerifierFn = {
        let m = Arc::clone(&mgr_arc);
        Arc::new(move |cand: &str| {
            m.lock()
                .is_ok_and(|g| g.check_local_passcode(cand.as_bytes()))
        })
    };

    let mut shell = ShellSettings::default();
    assert!(shell.local_passcode.is_none());
    shell.local_passcode = None;
    let h = SettingsSyncHarness::start();
    let mut ctrl =
        WindowController::with_session(h.live.handle(), shell, dir.path().join("shell_state.json"));
    assert!(!ctrl.has_local_passcode());
    ctrl.set_external_passcode_verifier(Some(verifier));
    assert!(ctrl.has_local_passcode());

    ctrl.setup_passcode_lock();
    assert_eq!(ctrl.root_screen(), RootScreen::PasscodeLock);

    // Changing passcode in AccountManager immediately affects the live verifier.
    mgr_arc
        .lock()
        .expect("lock mgr")
        .set_local_passcode_with_kdf(Some(b"live-secret"), b"rotated-secret", fast_kdf)
        .expect("rotate passcode");
    assert!(
        !mgr_arc
            .lock()
            .expect("m")
            .check_local_passcode(b"live-secret")
    );
    assert!(
        mgr_arc
            .lock()
            .expect("m")
            .check_local_passcode(b"rotated-secret")
    );

    h.shutdown();
}

// =============================================================================
// 6. End-to-End `tdata` Settings Persistence & Restore Round-Trip
// =============================================================================

#[test]
fn tdata_settings_persistence_and_restore_roundtrip() {
    let dir = TempDir::new().expect("tempdir");
    let opts = WorkingDirOpenOptions::new(dir.path(), test_creds());
    const MB: i64 = 1024 * 1024;

    // 1. Configure CoreSettings, GlobalConfig, and LiveSession per-account settings, then persist.
    let mut core = CoreSettings::new();
    core.desktop_notify = false;
    core.sound_notify = false;
    core.notify_view = NotifyView::ShowName;
    core.notifications_count = 4;
    core.notifications_corner = ScreenCorner::TopRight;
    core.send_submit_way = InputSubmitSettings::CtrlEnter;
    core.spellchecker_enabled = false;
    core.work_mode = WorkMode::TrayOnly;
    core.large_emoji = false;
    core.replace_emoji = false;
    core.autoplay = AutoplaySettings {
        gifs: false,
        videos: true,
        round_videos: false,
        video_stickers: true,
    };
    core.power_saving = PowerSavingFlags::ANIMATIONS | PowerSavingFlags::CHAT_SPOILER;
    core.auto_lock = 300;
    let shell_pos = ShellWindowPosition {
        x: Some(140.0),
        y: Some(85.0),
        width: 1180.0,
        height: 760.0,
        maximized: false,
    };
    core.window_position = shell_window_position_to_core(shell_pos, 125);

    let global = GlobalConfig {
        config_scale: 125,
        auto_start: true,
        start_minimized: true,
        ..GlobalConfig::default()
    };

    let mut h1 = SettingsSyncHarness::start();
    h1.live.set_phone_hidden(true);
    h1.live.set_archive_collapsed(true);
    let mut auto_dl = AutoDownloadSettings::default();
    auto_dl.set_bytes_limit(AutoDownloadSource::Private, AutoDownloadType::Photo, 6 * MB);
    auto_dl.set_bytes_limit(AutoDownloadSource::Channels, AutoDownloadType::File, 0);
    auto_dl.autoplay_gifs = false;
    auto_dl.autoplay_round_videos = false;
    h1.fake.enqueue_ok(
        tl_fns::account::SaveAutoDownloadSettings::CONSTRUCTOR_ID,
        &true,
    );
    h1.live.set_auto_download(auto_dl);

    let base_ss = StoredSessionSettings::default();
    let collected_ss = h1.live.collect_stored_session_settings(&base_ss);
    assert!(collected_ss.phone_number_hidden);
    assert!(collected_ss.archive_collapsed);

    {
        let mut mgr = AccountManager::open_and_unlock(opts.clone(), b"").expect("open mgr 1");
        persist_all_settings_to_tdata(&mut mgr, 0, &core, &global, Some(&collected_ss))
            .expect("persist all to tdata");
    }
    h1.shutdown();

    // 2. Reopen tdata in a brand new AccountManager and restore into SettingsController,
    //    LiveSession, AppNotifyTrayHost, and ShellWindowPosition.
    let reopened = AccountManager::open_and_unlock(opts, b"").expect("reopen mgr");
    let loaded_core = reopened.core_settings().clone();
    let loaded_global = reopened.global_config().clone();
    let loaded_ss = reopened
        .load_session_settings(0)
        .expect("load session settings");

    assert!(!loaded_core.desktop_notify);
    assert!(!loaded_core.sound_notify);
    assert_eq!(loaded_core.notify_view, NotifyView::ShowName);
    assert_eq!(loaded_core.notifications_count, 4);
    assert_eq!(loaded_core.notifications_corner, ScreenCorner::TopRight);
    assert_eq!(loaded_core.send_submit_way, InputSubmitSettings::CtrlEnter);
    assert!(!loaded_core.spellchecker_enabled);
    assert_eq!(loaded_core.work_mode, WorkMode::TrayOnly);
    assert!(!loaded_core.large_emoji);
    assert!(!loaded_core.replace_emoji);
    assert_eq!(loaded_core.autoplay, core.autoplay);
    assert_eq!(loaded_core.power_saving, core.power_saving);
    assert_eq!(loaded_core.auto_lock, 300);
    assert_eq!(loaded_global.config_scale, 125);
    assert!(loaded_global.auto_start);
    assert!(loaded_global.start_minimized);

    let restored_shell_pos =
        core_window_position_to_shell(loaded_core.window_position).expect("valid window pos");
    assert_eq!(restored_shell_pos, shell_pos);

    let mut h2 = SettingsSyncHarness::start();
    h2.live.apply_stored_session_settings(
        &loaded_ss,
        (
            loaded_core.autoplay.videos,
            loaded_core.autoplay.gifs,
            loaded_core.autoplay.round_videos,
        ),
    );
    assert!(h2.live.profile().phone_hidden);
    // Even if account.getAutoDownloadSettings is called, tdata custom limits remain intact.
    let restored_dl = h2.live.auto_download();
    assert_eq!(
        restored_dl.bytes_limit(AutoDownloadSource::Private, AutoDownloadType::Photo),
        6 * MB
    );
    assert_eq!(
        restored_dl.bytes_limit(AutoDownloadSource::Channels, AutoDownloadType::File),
        0
    );
    assert!(!restored_dl.autoplay_gifs);
    assert!(!restored_dl.autoplay_round_videos);

    let mut ctrl = SettingsController::new(Box::new(h2.live.handle()));
    ctrl.seed_from_tdata(loaded_core.clone(), loaded_global, false, None);
    assert_eq!(
        ctrl.core_settings.send_submit_way,
        InputSubmitSettings::CtrlEnter
    );
    assert!(!ctrl.core_settings.spellchecker_enabled);
    assert!(ctrl.advanced.power_saving.animations);
    assert!(ctrl.advanced.power_saving.chat_spoiler);

    let mut notify_tray = AppNotifyTrayHost::with_backend(
        h2.live.create_notify_backend(),
        NotifySettingsStore::default(),
        Box::new(|_| {}),
    );
    notify_tray.apply_core_settings(&loaded_core);
    assert!(!notify_tray.preferences().desktop_notify);
    assert!(!notify_tray.preferences().sound_notify);
    assert_eq!(notify_tray.preferences().notifications_count, 4);

    h2.shutdown();
}
