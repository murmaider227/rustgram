//! Integration & unit tests for WP-40:
//! - Reaction Notifications (`settings/notifications_reactions.rs` + `account.getReactionsNotifySettings`
//!   / `account.setReactionsNotifySettings` via `FakeRpcClient`)
//! - Hunspell Dictionaries Manager (`boxes/dictionaries.rs` + cloud channel `tdhbcfiles` via
//!   `FakeRpcClient` and fake zipped documents only)
//! - Windows Offline Translation Provider fallback (`tg-platform::translate_win` behind trait with
//!   `FakeWinTranslateBridge` + `messages.translateText` fallback)
//! - Session Window-Restore Offer & `RestoreShell` (`startup/restore_windows.rs`)
//! - Animated Dialog Video Userpic (`dialogs/video_userpic.rs`)
//! - `BotEarn` direct Affiliate button (`statistics/earn.rs` & `statistics/mod.rs`)

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_core::CoreSettings;
use tg_core::settings::WindowPosition;
use tg_data::{PeerUpdateFlags, User, UserId as DataUserId};
use tg_platform::translate_win::{FakeWinTranslateBridge, TranslateRequest, WinTranslateProvider};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::dialogs::video_userpic::{
    DialogVideoUserpicCache, VideoUserpicFrame, VideoUserpicSource, VideoUserpicState,
    apply_ellipse_mask,
};
use tg_app::live::LiveSession;
use tg_app::live::wp40_polish::{RpcDictionaryCloudSource, translate_text_with_platform_fallback};
use tg_app::settings::SettingsController;
use tg_app::settings::SettingsSection;
use tg_app::settings::backend::{ReactionsNotifyFrom, SettingsBackend};
use tg_app::settings::dictionaries::{
    DICTIONARIES_MANIFEST_CHANNEL, DICTIONARIES_MANIFEST_POST_ID, DICTIONARIES_REV,
    DictionaryState, HunspellDictionary, HunspellEngine, LANG_ID_SERBIAN, ManageDictionariesState,
    build_dictionary_zip_bytes, is_dictionary_downloaded, parse_dictionaries_manifest,
    parse_manifest_location, read_dictionaries_rev, unpack_dictionary_zip,
};
use tg_app::settings::main_page::search_settings;
use tg_app::settings::notifications_reactions::{ReactionNotifyTarget, ReactionsNotifyState};
use tg_app::stand_in::theme::ThemeMode;
use tg_app::stand_in::types::PeerId;
use tg_app::startup::restore_windows::{
    RestoreShell, RestoreShellStatus, RestoreWindowsAction, RestoreWindowsManager,
    RestoreWindowsPolicy, SavedWindowEntry, SavedWindowGeometry, SavedWindowsSnapshot,
    ScreenBounds, WINDOW_SHELL_MIN_HEIGHT, WINDOW_SHELL_MIN_WIDTH, count_initial_shell_geometry,
};
use tg_app::statistics::{StatisticsScreen, StatisticsScreenHost};

// =============================================================================
// FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
pub(super) struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    pub(super) fn push_response<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("responses lock")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    pub(super) fn recorded(&self) -> Vec<RequestCapture> {
        self.recorded.lock().expect("recorded lock").clone()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, request).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = request.to_bytes();
        let ctor = if bytes.len() >= 4 {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            0
        };
        self.recorded
            .lock()
            .expect("recorded lock")
            .push((ctor, bytes));
        let popped = self.responses.lock().expect("responses lock").pop_front();
        let Some((expected, res)) = popped else {
            return Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("UNMOCKED_RPC_{ctor:#010x}"),
                value: None,
            }));
        };
        assert_eq!(
            ctor, expected,
            "unexpected RPC constructor: got 0x{ctor:08x}, expected 0x{expected:08x}"
        );
        let raw = res?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("main_dc lock")
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

pub(super) fn make_live_session() -> (FakeRpcClient, LiveSession) {
    let rpc = FakeRpcClient::new(2);
    let self_uid = DataUserId(100);
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = SessionRuntime::start(
        self_uid,
        tg_app::live::ErasedRpcClient::new(rpc.clone()),
        cfg,
    )
    .expect("start runtime");
    let session_lock = runtime.session();
    runtime.tokio_handle().block_on(async {
        let mut s = session_lock.write().await;
        let mut self_user = User::new(self_uid, true);
        self_user.first_name = "Test".to_owned();
        self_user.last_name = "Account".to_owned();
        s.upsert_user(self_user, PeerUpdateFlags::empty());
    });
    let live = LiveSession::new(Arc::new(runtime), 1);
    (rpc, live)
}

// =============================================================================
// 1. Reaction Notifications (`settings_notifications_reactions`)
// =============================================================================

#[test]
fn wp40_reaction_notifications_live_rpc_roundtrip_and_state() {
    let (rpc, mut live) = make_live_session();

    // Fetch: account.getContactSignUpNotification -> false (meaning contact_joined = true)
    // and account.getReactionsNotifySettings -> messages=Contacts, stories=All, polls=None, show_previews=false.
    rpc.push_response(
        tl_fns::account::GetContactSignUpNotification::CONSTRUCTOR_ID,
        &false,
    );
    rpc.push_response(
        tl_fns::account::GetReactionsNotifySettings::CONSTRUCTOR_ID,
        &tl_enums::ReactionsNotifySettings::ReactionsNotifySettings(
            tl_types::ReactionsNotifySettings {
                messages_notify_from: Some(tl_enums::ReactionNotificationsFrom::Contacts),
                stories_notify_from: Some(tl_enums::ReactionNotificationsFrom::All),
                poll_votes_notify_from: None,
                sound: tl_enums::NotificationSound::Default,
                show_previews: false,
            },
        ),
    );

    let mut events = SettingsBackend::notification_events(&live);
    assert!(events.contact_joined);
    assert_eq!(events.message_reactions, ReactionsNotifyFrom::Contacts);
    assert_eq!(events.story_reactions, ReactionsNotifyFrom::All);
    assert_eq!(events.poll_votes, ReactionsNotifyFrom::None);
    assert!(!events.show_reaction_previews);
    assert!(events.reactions_enabled());

    // Edit through ReactionsNotifyState (ShowFromBox picker flow).
    let mut ui_state = ReactionsNotifyState::default();
    ui_state.open_picker(ReactionNotifyTarget::PollVotes, events.poll_votes);
    assert_eq!(ui_state.draft_from, ReactionsNotifyFrom::All);
    ui_state.draft_from = ReactionsNotifyFrom::Contacts;
    assert!(ui_state.apply_picker(&mut events));
    events.show_reaction_previews = true;

    // Save: account.setContactSignUpNotification + account.setReactionsNotifySettings.
    rpc.push_response(
        tl_fns::account::SetContactSignUpNotification::CONSTRUCTOR_ID,
        &true,
    );
    rpc.push_response(
        tl_fns::account::SetReactionsNotifySettings::CONSTRUCTOR_ID,
        &tl_enums::ReactionsNotifySettings::ReactionsNotifySettings(
            tl_types::ReactionsNotifySettings {
                messages_notify_from: Some(tl_enums::ReactionNotificationsFrom::Contacts),
                stories_notify_from: Some(tl_enums::ReactionNotificationsFrom::All),
                poll_votes_notify_from: Some(tl_enums::ReactionNotificationsFrom::Contacts),
                sound: tl_enums::NotificationSound::Default,
                show_previews: true,
            },
        ),
    );

    SettingsBackend::set_notification_events(&mut live, events.clone());
    let updated = SettingsBackend::notification_events(&live);
    assert_eq!(updated.poll_votes, ReactionsNotifyFrom::Contacts);
    assert!(updated.show_reaction_previews);

    // Verify exact TL payload sent in SetReactionsNotifySettings.
    let calls = rpc.recorded();
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls[3].0,
        tl_fns::account::SetReactionsNotifySettings::CONSTRUCTOR_ID
    );
    let sent = tl_fns::account::SetReactionsNotifySettings::from_bytes(&calls[3].1)
        .expect("decode setReactionsNotifySettings");
    let tl_enums::ReactionsNotifySettings::ReactionsNotifySettings(inner) = sent.settings;
    assert_eq!(
        inner.messages_notify_from,
        Some(tl_enums::ReactionNotificationsFrom::Contacts)
    );
    assert_eq!(
        inner.poll_votes_notify_from,
        Some(tl_enums::ReactionNotificationsFrom::Contacts)
    );
    assert!(inner.show_previews);

    // Search index points "reactions" to NotificationsReactions.
    let hits = search_settings("reactions poll");
    assert!(
        hits.iter()
            .any(|e| e.target == SettingsSection::NotificationsReactions)
    );

    // Verify SettingsController can push & render NotificationsReactions without panicking.
    let mut ctrl = SettingsController::default();
    ctrl.push_section(SettingsSection::NotificationsReactions);
    assert_eq!(
        ctrl.current_section(),
        SettingsSection::NotificationsReactions
    );
}

// =============================================================================
// 2. Hunspell Dictionaries Manager (`tdhbcfiles` fake documents only)
// =============================================================================

#[test]
fn wp40_hunspell_dictionaries_cloud_download_unpack_and_check() {
    assert_eq!(
        parse_manifest_location("tdhbcfiles#2950"),
        Some(("tdhbcfiles".to_owned(), 2950))
    );
    assert_eq!(parse_manifest_location("invalid"), None);
    assert_eq!(parse_manifest_location("chan#0"), None);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let rpc = FakeRpcClient::new(2);

    let manifest_json = r#"{
        "dictionaries": [
            { "id": 31, "size": 512, "name": "English (US)", "location": "tdhbcfiles#2950" },
            { "id": 100, "size": 400, "name": "Serbian", "location": "tdhbcfiles#2999" }
        ]
    }"#;
    let parsed = parse_dictionaries_manifest(manifest_json.as_bytes()).expect("parse manifest");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].locale_code, "en_US");
    assert_eq!(parsed[1].id, LANG_ID_SERBIAN);

    // Build fake zipped English Hunspell archive.
    let en_zip = build_dictionary_zip_bytes(
        "en_US",
        "3\nhello/S\ntelegram\nplanet\n",
        "SET UTF-8\nSFX S Y 1\nSFX S 0 s .\n",
    );

    // 1. contacts.resolveUsername("tdhbcfiles")
    rpc.push_response(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 777 }),
            chats: vec![tl_enums::Chat::Channel(tl_types::Channel {
                creator: false,
                left: false,
                broadcast: true,
                verified: false,
                megagroup: false,
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
                id: 777,
                access_hash: Some(987_654_321),
                title: "Dictionaries".to_owned(),
                username: Some(DICTIONARIES_MANIFEST_CHANNEL.to_owned()),
                photo: tl_enums::ChatPhoto::Empty,
                date: 1_700_000_000,
                restriction_reason: None,
                admin_rights: None,
                banned_rights: None,
                default_banned_rights: None,
                participants_count: None,
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
            })],
            users: Vec::new(),
        }),
    );

    // 2. channels.getMessages(post 2949 -> manifest JSON)
    rpc.push_response(
        tl_fns::channels::GetMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 1,
            count: 1,
            offset_id_offset: None,
            messages: vec![tl_enums::Message::Message(tl_types::Message {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                post: true,
                from_scheduled: false,
                legacy: false,
                edit_hide: false,
                pinned: true,
                noforwards: false,
                invert_media: false,
                offline: false,
                video_processing_pending: false,
                paid_suggested_post_stars: false,
                paid_suggested_post_ton: false,
                id: DICTIONARIES_MANIFEST_POST_ID,
                from_id: None,
                from_boosts_applied: None,
                from_rank: None,
                peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 777 }),
                saved_peer_id: None,
                fwd_from: None,
                via_bot_id: None,
                via_business_bot_id: None,
                guestchat_via_from: None,
                reply_to: None,
                date: 1_700_000_000,
                message: manifest_json.to_owned(),
                media: None,
                reply_markup: None,
                entities: None,
                views: None,
                forwards: None,
                replies: None,
                edit_date: None,
                post_author: None,
                grouped_id: None,
                reactions: None,
                restriction_reason: None,
                ttl_period: None,
                quick_reply_shortcut_id: None,
                effect: None,
                factcheck: None,
                report_delivery_until_date: None,
                paid_message_stars: None,
                suggested_post: None,
                schedule_repeat_period: None,
                summary_from_language: None,
                rich_message: None,
            })],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    // 3. channels.getMessages(post 2950 -> fake zip document)
    rpc.push_response(
        tl_fns::channels::GetMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 2,
            count: 1,
            offset_id_offset: None,
            messages: vec![tl_enums::Message::Message(tl_types::Message {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                post: true,
                from_scheduled: false,
                legacy: false,
                edit_hide: false,
                pinned: false,
                noforwards: false,
                invert_media: false,
                offline: false,
                video_processing_pending: false,
                paid_suggested_post_stars: false,
                paid_suggested_post_ton: false,
                id: 2950,
                from_id: None,
                from_boosts_applied: None,
                from_rank: None,
                peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 777 }),
                saved_peer_id: None,
                fwd_from: None,
                via_bot_id: None,
                via_business_bot_id: None,
                guestchat_via_from: None,
                reply_to: None,
                date: 1_700_000_000,
                message: String::new(),
                media: Some(tl_enums::MessageMedia::Document(
                    tl_types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: false,
                        document: Some(tl_enums::Document::Document(tl_types::Document {
                            id: 5001,
                            access_hash: 9999,
                            file_reference: vec![1, 2, 3],
                            date: 1_700_000_000,
                            mime_type: "application/zip".to_owned(),
                            size: en_zip.len() as i64,
                            thumbs: None,
                            video_thumbs: None,
                            dc_id: 2,
                            attributes: Vec::new(),
                        })),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                )),
                reply_markup: None,
                entities: None,
                views: None,
                forwards: None,
                replies: None,
                edit_date: None,
                post_author: None,
                grouped_id: None,
                reactions: None,
                restriction_reason: None,
                ttl_period: None,
                quick_reply_shortcut_id: None,
                effect: None,
                factcheck: None,
                report_delivery_until_date: None,
                paid_message_stars: None,
                suggested_post: None,
                schedule_repeat_period: None,
                summary_from_language: None,
                rich_message: None,
            })],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    // 4. upload.getFile -> returns fake zipped archive bytes
    rpc.push_response(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: 1_700_000_000,
            bytes: en_zip.clone(),
        }),
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let dicts_dir = tmp.path().join("tdata").join("dictionaries");
    let mut source = RpcDictionaryCloudSource::new(rpc, rt.handle().clone());
    let mut manager = ManageDictionariesState::new(dicts_dir.clone());

    manager
        .refresh(&["de-DE".to_owned()], &[], &mut source)
        .expect("refresh from fake cloud channel");
    assert!(manager.manifest_loaded);
    assert!(manager.entries[0].is_system);
    assert!(manager.entries.iter().any(|e| e.id == 31 && !e.is_system));

    // Toggle on English (31) -> downloads fake doc & unpacks en_US.{dic,aff}.
    manager
        .set_enabled(31, true, &mut source)
        .expect("download & unpack en_US");
    assert_eq!(manager.states.get(&31), Some(&DictionaryState::Active));
    assert!(is_dictionary_downloaded(&dicts_dir, 31, Some("en_US")));
    assert_eq!(read_dictionaries_rev(&dicts_dir), DICTIONARIES_REV);

    // Commit to CoreSettings & run pure-Rust HunspellEngine on unpacked dictionary.
    let mut core = CoreSettings::new();
    manager.save_to_core_settings(&mut core);
    assert_eq!(core.dictionaries_enabled, vec![31]);

    let mut engine = HunspellEngine::new();
    assert_eq!(
        engine.load_enabled(&dicts_dir, &core.dictionaries_enabled),
        1
    );
    assert!(engine.check_word("hello"));
    assert!(engine.check_word("hellos")); // SFX S rule!
    assert!(engine.check_word("Telegram"));
    assert!(!engine.check_word("helo"));
    assert!(engine.suggestions("helo", 3).contains(&"hello".to_owned()));

    // Verify Serbian archive rename quirk (`sr_Cyrl_RS` -> `sr_RS`).
    let sr_zip = build_dictionary_zip_bytes("sr_Cyrl_RS", "1\nздраво\n", "SET UTF-8\n");
    let sr_dir = unpack_dictionary_zip(&sr_zip, &dicts_dir, LANG_ID_SERBIAN, Some("sr_RS"))
        .expect("unpack serbian");
    assert!(sr_dir.join("sr_RS.dic").exists());
    assert!(sr_dir.join("sr_RS.aff").exists());
    let sr_dict = HunspellDictionary::load_from_dir(&sr_dir, "sr_RS").expect("load serbian");
    assert!(sr_dict.check_word("здраво"));

    // Verify safe removal of downloaded dictionary.
    assert!(manager.remove_downloaded(31).expect("remove en_US"));
    assert!(!is_dictionary_downloaded(&dicts_dir, 31, Some("en_US")));
}

// =============================================================================
// 3. WinRT Translation Fallback (`translate_provider_win` + MTProto fallback)
// =============================================================================

#[test]
fn wp40_winrt_translation_provider_and_mtproto_fallback() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let rpc = FakeRpcClient::new(2);

    let mut fake_bridge = FakeWinTranslateBridge::new(true, vec!["en-US".to_owned()]);
    fake_bridge.insert_translation("en", "Guten Tag", "Good day");
    let provider = WinTranslateProvider::with_bridge(fake_bridge.clone());

    // Offline hit: zero MTProto RPCs invoked.
    let offline = translate_text_with_platform_fallback(
        rt.handle(),
        &rpc,
        &TranslateRequest::new("Guten Tag", "en").with_from_lang("de"),
        true,
        Some(&provider),
    )
    .expect("offline translation");
    assert_eq!(offline.provider_name, "winrt-offline");
    assert_eq!(offline.translated_text, "Good day");
    assert!(rpc.recorded().is_empty());

    // Unsupported target language ("ja") -> transparently falls back to messages.translateText.
    rpc.push_response(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedText::TranslateResult(tl_types::messages::TranslateResult {
            result: vec![tl_enums::TextWithEntities::TextWithEntities(
                tl_types::TextWithEntities {
                    text: "こんにちは".to_owned(),
                    entities: Vec::new(),
                },
            )],
        }),
    );
    let fallback = translate_text_with_platform_fallback(
        rt.handle(),
        &rpc,
        &TranslateRequest::new("Hello", "ja"),
        true,
        Some(&provider),
    )
    .expect("mtproto fallback");
    assert_eq!(fallback.provider_name, "mtproto");
    assert_eq!(fallback.translated_text, "こんにちは");
    assert_eq!(rpc.recorded().len(), 1);
    assert_eq!(
        rpc.recorded()[0].0,
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID
    );
}

// =============================================================================
// 4. Window Restore Shell (`startup/restore_windows.rs`)
// =============================================================================

#[test]
fn wp40_window_restore_shell_offer_geometry_and_persistence() {
    let screen = ScreenBounds {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };

    // Invalid tiny geometry clamps to centered default bounds.
    let clamped = count_initial_shell_geometry(
        WindowPosition {
            w: 100,
            h: 100,
            ..WindowPosition::default()
        },
        screen,
    );
    assert!(clamped.2 >= WINDOW_SHELL_MIN_WIDTH);
    assert!(clamped.3 >= WINDOW_SHELL_MIN_HEIGHT);

    // Persistence roundtrip in temp working dir.
    let tmp = tempfile::tempdir().expect("tempdir");
    let snap = SavedWindowsSnapshot {
        policy: RestoreWindowsPolicy::Ask,
        windows: vec![
            SavedWindowEntry {
                id: 1,
                title: "Engineering Chat".to_owned(),
                account_index: 0,
                peer_id: Some(42),
                position: SavedWindowGeometry {
                    moncrc: 1234,
                    maximized: 0,
                    scale: 100,
                    x: 120,
                    y: 80,
                    w: 640,
                    h: 520,
                },
            },
            SavedWindowEntry {
                id: 2,
                title: String::new(),
                account_index: 0,
                peer_id: Some(99),
                position: SavedWindowGeometry {
                    moncrc: 1234,
                    maximized: 1,
                    scale: 100,
                    x: 200,
                    y: 100,
                    w: 700,
                    h: 600,
                },
            },
        ],
    };
    snap.save_to(tmp.path()).expect("save snapshot");
    let loaded = SavedWindowsSnapshot::load_from(tmp.path());
    assert_eq!(loaded, snap);

    let mut mgr = RestoreWindowsManager::from_snapshot(loaded, screen);
    assert!(mgr.pending_offer.is_some());
    assert!(mgr.shells.is_empty());

    // Choosing "Always" spawns both shells & returns enabled toast.
    let toast = mgr.apply_offer_action(RestoreWindowsAction::Always, screen);
    assert!(toast.is_some());
    assert_eq!(mgr.policy, RestoreWindowsPolicy::Always);
    assert_eq!(mgr.shells.len(), 2);
    assert_eq!(mgr.shells[0].title, "Engineering Chat");
    assert_eq!(mgr.shells[1].title, "Rustgram");
    assert!(mgr.shells[1].maximized);
    assert_eq!(mgr.shells[0].status, RestoreShellStatus::Loading);

    // Mark shell 2 unavailable -> moves to dead_shells.
    assert!(mgr.mark_shell_unavailable(2));
    assert_eq!(mgr.shells.len(), 1);
    assert_eq!(mgr.dead_shells.len(), 1);
    assert_eq!(mgr.dead_shells[0].status, RestoreShellStatus::Unavailable);

    // Save position roundtrip from RestoreShell.
    let saved_pos = mgr.shells[0].count_position_for_save(100, (0, 0), 1234);
    assert_eq!(saved_pos.x, 120);
    assert_eq!(saved_pos.y, 80);
    assert_eq!(saved_pos.w, 640);
    assert_eq!(saved_pos.h, 520);

    // Close dead shell cleanly.
    assert!(mgr.close_shell(2));
    assert!(mgr.dead_shells.is_empty());

    // Direct RestoreShell activate/ready lifecycle.
    let mut shell = RestoreShell::new(9, "Notes", None, saved_pos, screen);
    shell.activate();
    assert!(shell.active);
    shell.mark_ready();
    assert_eq!(shell.status, RestoreShellStatus::Ready);
}

// =============================================================================
// 5. Dialog Video Userpic (`dialogs/video_userpic.rs`)
// =============================================================================

struct FakeVideoUserpicSource {
    small_bytes: Option<Vec<u8>>,
    large_bytes: Option<Vec<u8>>,
    fail_decode: bool,
}

impl VideoUserpicSource for FakeVideoUserpicSource {
    fn video_bytes_small(&self, _peer_id: PeerId, _photo_id: i64) -> Option<Vec<u8>> {
        self.small_bytes.clone()
    }

    fn video_bytes_large(&self, _peer_id: PeerId, _photo_id: i64) -> Option<Vec<u8>> {
        self.large_bytes.clone()
    }

    fn decode_frames(&self, _bytes: &[u8]) -> Result<Vec<VideoUserpicFrame>, String> {
        if self.fail_decode {
            return Err("corrupt video".to_owned());
        }
        let f0 = VideoUserpicFrame::new(8, 8, vec![255; 8 * 8 * 4], 100).expect("f0");
        let f1 = VideoUserpicFrame::new(8, 8, vec![128; 8 * 8 * 4], 100).expect("f1");
        Ok(vec![f0, f1])
    }
}

#[test]
fn wp40_dialogs_video_userpic_lifecycle_and_ellipse_mask() {
    // Corner pixel of an 8x8 opaque square must become transparent after ellipse mask.
    let masked = apply_ellipse_mask(8, 8, &vec![255; 8 * 8 * 4]);
    assert_eq!(masked[3], 0, "top-left corner must be masked out");
    let center_alpha = masked[((4 * 8) + 4) * 4 + 3];
    assert_eq!(center_alpha, 255, "center pixel must remain opaque");

    let mut cache = DialogVideoUserpicCache::new();
    let peer = PeerId::from_user(501);
    let vu = cache.entry_mut(peer);
    assert_eq!(vu.state(), VideoUserpicState::Idle);

    // Sync photo ID -> Loading; fallback to Large bytes when Small is absent.
    vu.sync_photo_id(Some(9001));
    assert_eq!(vu.state(), VideoUserpicState::Loading);

    let src = FakeVideoUserpicSource {
        small_bytes: None,
        large_bytes: Some(vec![0xAA, 0xBB]),
        fail_decode: false,
    };
    vu.poll_source(&src);
    assert_eq!(vu.state(), VideoUserpicState::Ready);
    assert_eq!(vu.frame_count(), 2);

    // Paused freezes on frame 0; unpaused advances at 100ms boundary.
    assert_eq!(vu.current_frame_index(1_000, true), Some(0));
    assert_eq!(vu.current_frame_index(1_000, false), Some(0));
    assert_eq!(vu.current_frame_index(1_120, false), Some(1));
    assert_eq!(vu.current_frame_index(1_220, false), Some(0));

    // Photo ID change resets cache; corrupt decode marks Bad.
    vu.sync_photo_id(Some(9002));
    assert_eq!(vu.state(), VideoUserpicState::Loading);
    let bad_src = FakeVideoUserpicSource {
        small_bytes: Some(vec![0xFF]),
        large_bytes: None,
        fail_decode: true,
    };
    vu.poll_source(&bad_src);
    assert_eq!(vu.state(), VideoUserpicState::Bad);
    assert_eq!(vu.current_frame_index(2_000, false), None);
}

// =============================================================================
// 6. BotEarn Direct Affiliate Button (`statistics/earn.rs`)
// =============================================================================

fn wp40_test_harness(
    render: impl FnMut(&mut egui::Ui) + 'static,
) -> egui_kittest::Harness<'static> {
    egui_kittest::HarnessBuilder::default()
        .with_size(egui::Vec2::new(640.0, 720.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
        })
        .build_ui(render)
}

#[test]
fn wp40_bot_earn_direct_affiliate_button_triggers_navigation() {
    let mut host = StatisticsScreenHost::with_mock(ThemeMode::Day);
    host.select_top_screen(StatisticsScreen::BotEarn);
    let expected_peer_id = host
        .bot_earn_view()
        .expect("bot earn loaded")
        .bundle
        .peer
        .id;
    assert!(
        host.bot_earn_view()
            .expect("bot earn loaded")
            .bundle
            .affiliate_allowed
    );

    // Render BotEarn inside an egui_kittest headless harness to ensure zero panics.
    let mut harness = wp40_test_harness(move |ui| {
        let _ = host.show(ui, false);
    });
    harness.run();

    // Direct BotEarnView affiliate trigger & take_open_affiliate roundtrip.
    let mut host2 = StatisticsScreenHost::with_mock(ThemeMode::Night);
    host2.select_top_screen(StatisticsScreen::BotEarn);
    let backend = tg_app::statistics::mock::MockStatsBackend::new();
    let mut view = tg_app::statistics::earn::BotEarnView::new(
        tg_app::statistics::backend::StatsBackend::bot_earn(&backend).expect("bot earn"),
        host2.palette(),
    );
    assert_eq!(view.take_open_affiliate(), None);
    view.open_affiliate_requested = true;
    assert_eq!(view.take_open_affiliate(), Some(expected_peer_id));
    assert_eq!(view.take_open_affiliate(), None);
}

#[test]
fn wp40_ui_panels_render_cleanly_in_egui_kittest() {
    use tg_app::stand_in::types::ConnectionStatus;
    use tg_app::window::adaptive::{ChatLayout, ColumnLayout, WindowLayout};

    let layout = ColumnLayout {
        filters_width: 0,
        body_width: 960,
        dialogs_width: 300,
        chat_width: 660,
        third_width: 0,
        window_layout: WindowLayout::Normal,
        chat_layout: ChatLayout::Normal,
        dialogs_small_column: false,
    };

    // 1. Settings -> Notifications -> Reactions panel renders & opens cleanly.
    let mut ctrl = SettingsController::default();
    ctrl.push_section(SettingsSection::NotificationsReactions);
    ctrl.notifications_reactions
        .open_picker(ReactionNotifyTarget::Messages, ReactionsNotifyFrom::All);
    let mut h1 = wp40_test_harness(move |ui| {
        let _ = ctrl.show(ui, ConnectionStatus::Connected, layout);
    });
    h1.run();

    // 2. Settings -> Advanced -> Manage dictionaries panel renders cleanly.
    let mut ctrl2 = SettingsController::default();
    ctrl2.push_section(SettingsSection::Advanced);
    ctrl2.advanced.panel = tg_app::settings::advanced::AdvancedSubPanel::Dictionaries;
    let mut h2 = wp40_test_harness(move |ui| {
        let _ = ctrl2.show(ui, ConnectionStatus::Connected, layout);
    });
    h2.run();

    // 3. Startup RestoreWindowsManager offer banner & RestoreShell render cleanly.
    let snap = SavedWindowsSnapshot {
        policy: RestoreWindowsPolicy::Ask,
        windows: vec![SavedWindowEntry {
            id: 7,
            title: "Project Chat".to_owned(),
            account_index: 0,
            peer_id: Some(77),
            position: SavedWindowGeometry::default(),
        }],
    };
    let mgr = RestoreWindowsManager::from_snapshot(snap, ScreenBounds::default());
    let shell = RestoreShell::new(
        7,
        "Project Chat",
        Some(77),
        WindowPosition::default(),
        ScreenBounds::default(),
    );
    let mut h3 = wp40_test_harness(move |ui| {
        let _ = mgr.render_offer_banner(ui);
        shell.render_body(ui);
    });
    h3.run_steps(2);
}
