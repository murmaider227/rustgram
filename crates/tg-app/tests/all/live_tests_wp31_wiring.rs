//! WP-31 live wiring tests against `FakeRpcClient` and headless `egui_kittest::Harness`.
//!
//! Verifies:
//! 1. Promo suggestions (`help.getPromoData` + `help.dismissSuggestion` + real suggestion
//!    activation to Settings / Payments / Link instead of toast-only placeholders).
//! 2. Music listen tracker (`AudioPlayerBar::poll_listen_reports` polled from `MainWidget` into
//!    `MediaBackend::report_music_listen` -> `messages.reportMusicListen` on track end and 60s pause).
//! 3. Custom keyboard shortcut rebinding via `SettingsController.shortcuts.manager` dispatching
//!    `WindowController` actions and persisting/reloading via `tdata/shortcuts-custom.json`.
//! 4. `LiveLangPackHandle::attach_tdata_dir` wired in `MultiAccountWorkspaceHost::set_tdata_dir`.
//! 5. `IntroAction::OpenSettings` opening pre-login Proxy Settings / `AddProxyBox` before login.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tempfile::TempDir;
use tg_api::{ApiCredentials, DcId, InvocationError, RpcClient};
use tg_audio::{AudioMsgId, AudioType, DocumentId, FullMsgId, MediaKind};
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType, SettingsProxy,
};
use tg_data::{
    DocumentData, DocumentKind, Message, MessageContent, MessageMedia, MsgId as DataMsgId,
    PeerId as DataPeerId, TextWithEntities as DataText, UserId as DataUserId,
};
use tg_lang::{Instance as LangInstance, LangPackDifference, Language};
use tg_session::{AccountManager, SessionRuntime, SessionRuntimeConfig, WorkingDirOpenOptions};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tg_ui::theme::ThemeManager;
use tokio::sync::broadcast;

use tg_app::dialogs::suggestions::{PromoSuggestion, PromoSuggestionKind, SuggestionPriority};
use tg_app::info::InfoSection;
use tg_app::intro::{IntroAction, IntroWidget};
use tg_app::live::LiveSession;
use tg_app::live::bridge::RuntimeBridgedRpc;
use tg_app::live::host::LiveActiveWorkspace;
use tg_app::live::multi_account::MultiAccountWorkspaceHost;
use tg_app::live::shortcuts_wiring::{IntroProxyModal, drive_intro_proxy_modal};
use tg_app::media::backend::MediaBackend;
use tg_app::notifications::manager::NotificationSystem;
use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use tg_app::payments::PaymentsScreen;
use tg_app::settings::SettingsSection;
use tg_app::settings::shortcuts::{CUSTOM_SHORTCUTS_FILENAME, KeyChord, ShortcutCommand};
use tg_app::stand_in::backend::Backend;
use tg_app::stand_in::settings::{SavedWindowColumns, ShellSettings};
use tg_app::stand_in::strings::TgLangStrings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{
    AuthUser, CodeSentInfo, CodeVerifyOutcome, ConnectionStatus, DialogEntry, FilterId,
    FolderFilter, MessageItem, PeerId, PeerSummary,
};
use tg_app::tray::MockTrayPlatform;
use tg_app::window::section::SectionMemento;

/// Polls `cond` every 10 ms (stepping nothing) until it holds, failing after 10 s.
/// Live promo / listen RPCs run on the session runtime, so tests wait for the fake RPC.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Steps `harness` until `cond` holds, failing after 10 s.
fn step_until<S>(harness: &mut Harness<'_, S>, what: &str, mut cond: impl FnMut(&mut S) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond(harness.state_mut()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
        harness.step();
    }
}

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

#[derive(Clone)]
struct CapturedRpc {
    ctor: u32,
    bytes: Vec<u8>,
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct Wp31FakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    captured: Arc<Mutex<Vec<CapturedRpc>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp31FakeRpc {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            captured: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    fn captured_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.captured
            .lock()
            .expect("lock captured")
            .iter()
            .filter(|c| c.ctor == ctor)
            .map(|c| c.bytes.clone())
            .collect()
    }
}

impl RpcClient for Wp31FakeRpc {
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
        self.captured
            .lock()
            .expect("lock captured")
            .push(CapturedRpc { ctor, bytes });

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

fn synthetic_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
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
        access_hash: Some(id * 37 + 11),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: Some(tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
            expires: now_unix_i32() + 3600,
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
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn build_live_runtime(
    rpc: Wp31FakeRpc,
    self_id: u64,
) -> (
    tokio::runtime::Runtime,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId(self_id),
            tg_app::live::ErasedRpcClient::new(rpc),
            rt.handle().clone(),
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
    runtime.seed_self_user(&synthetic_user(self_id as i64, "Alex", "alex_dev", true));
    (rt, runtime)
}

fn make_tl_text(text: &str) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: text.to_owned(),
        entities: Vec::new(),
    })
}

/// Generates a minimal valid 48kHz mono 16-bit PCM WAV file of `duration_secs` seconds.
fn generate_test_wav(duration_secs: u32) -> Vec<u8> {
    let sample_rate: u32 = 48_000;
    let num_samples = sample_rate * duration_secs;
    let data_bytes = num_samples * 2;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for i in 0..num_samples {
        let sample = ((i as f32 * 0.05).sin() * 4000.0) as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

// =============================================================================
// 1. Live Promo Suggestions: help.getPromoData + help.dismissSuggestion + real activation
// =============================================================================

#[test]
fn wp31_live_promo_suggestions_fetch_activate_and_dismiss_via_fake_rpc() {
    let rpc = Wp31FakeRpc::new(2);
    let promo_tl = tl_enums::help::PromoData::PromoData(tl_types::help::PromoData {
        proxy: false,
        expires: now_unix_i32() + 3600,
        peer: None,
        psa_type: None,
        psa_message: None,
        pending_suggestions: vec!["BIRTHDAY_SETUP".to_owned(), "PREMIUM_ANNUAL".to_owned()],
        dismissed_suggestions: vec![],
        custom_pending_suggestion: Some(tl_enums::PendingSuggestion::PendingSuggestion(
            tl_types::PendingSuggestion {
                suggestion: "CUSTOM_RELEASE".to_owned(),
                title: make_tl_text("Rustgram Changelog"),
                description: make_tl_text("Read what is new"),
                url: "https://t.me/telegram".to_owned(),
            },
        )),
        chats: vec![],
        users: vec![],
    });
    rpc.enqueue_ok(tl_fns::help::GetPromoData::CONSTRUCTOR_ID, &promo_tl);
    rpc.enqueue_ok(tl_fns::help::DismissSuggestion::CONSTRUCTOR_ID, &true);

    let (rt, runtime) = build_live_runtime(rpc.clone(), 501);
    let live = LiveSession::new(Arc::clone(&runtime), 501);
    let tmp = TempDir::new().expect("tempdir");
    let workspace = LiveActiveWorkspace::new_headless(
        live,
        ShellSettings::default(),
        tmp.path().join("shell.json"),
        false,
    );

    let mut harness: Harness<LiveActiveWorkspace> = HarnessBuilder::default()
        .with_size(Vec2::new(1080.0, 720.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(register_tdesktop_fonts)
        .build_ui_state(
            |ui, ws: &mut LiveActiveWorkspace| {
                let ctx = ui.ctx().clone();
                let _ = ws.show(&ctx, ui);
            },
            workspace,
        );
    harness.step();
    step_until(&mut harness, "promo suggestions to load", |ws| {
        ws.controller_mut()
            .main_widget_mut()
            .dialogs_mut()
            .promo_suggestions
            .active()
            .is_some()
    });
    harness.step();

    // 1. Live DialogsPanel fetched help.getPromoData and selected the highest-priority suggestion
    //    ("PREMIUM_ANNUAL" has priority 2 < "BIRTHDAY_SETUP" priority 3).
    assert_eq!(
        rpc.captured_for(tl_fns::help::GetPromoData::CONSTRUCTOR_ID)
            .len(),
        1,
        "DialogsPanel must fetch help.getPromoData once from LiveSession"
    );
    let active_key = harness
        .state_mut()
        .controller_mut()
        .main_widget_mut()
        .dialogs_mut()
        .promo_suggestions
        .active()
        .expect("active promo suggestion")
        .key
        .clone();
    assert_eq!(active_key, "PREMIUM_ANNUAL");

    // 2. Clicking the PREMIUM_ANNUAL banner body opens PaymentsScreen::Premium (not a toast).
    harness.get_by_label("Telegram Premium").click();
    harness.step();
    harness.step();

    assert!(
        harness.state().payments_open(),
        "activating PREMIUM_ANNUAL must open the live Payments overlay (not a placeholder toast)"
    );
    // Close the Payments overlay so the dialogs column is clickable again, then dismiss
    // PREMIUM_ANNUAL by clicking its close (×) button -> invokes help.dismissSuggestion.
    harness.state_mut().close_payments();
    harness.step();

    harness.get_by_label("Dismiss Telegram Premium").click();
    harness.step();
    harness.step();

    wait_for("help.dismissSuggestion", || {
        !rpc.captured_for(tl_fns::help::DismissSuggestion::CONSTRUCTOR_ID)
            .is_empty()
    });
    let dismiss_calls = rpc.captured_for(tl_fns::help::DismissSuggestion::CONSTRUCTOR_ID);
    assert_eq!(
        dismiss_calls.len(),
        1,
        "clicking promo dismiss button must invoke help.dismissSuggestion"
    );
    let decoded_dismiss =
        tl_fns::help::DismissSuggestion::from_bytes(&dismiss_calls[0]).expect("decode dismiss");
    assert_eq!(decoded_dismiss.suggestion, "PREMIUM_ANNUAL");

    // Next highest priority suggestion ("BIRTHDAY_SETUP") is now active.
    let next_key = harness
        .state_mut()
        .controller_mut()
        .main_widget_mut()
        .dialogs_mut()
        .promo_suggestions
        .active()
        .expect("next promo suggestion")
        .key
        .clone();
    assert_eq!(next_key, "BIRTHDAY_SETUP");

    // 3. Clicking BIRTHDAY_SETUP opens Settings -> Information (where Birthday setup lives).
    harness.get_by_label("Add Your Birthday").click();
    harness.step();
    harness.step();

    let section = harness
        .state()
        .controller()
        .main_widget()
        .settings()
        .map(|s| s.current_section());
    assert_eq!(
        section,
        Some(SettingsSection::Information),
        "activating BIRTHDAY_SETUP must open SettingsSection::Information"
    );

    // 4. Verify CustomPromo activation routes URL to OpenLink (not a toast).
    let custom = PromoSuggestion {
        key: "CUSTOM_RELEASE".to_owned(),
        priority: SuggestionPriority::CustomPromo,
        kind: PromoSuggestionKind::CustomPromo {
            url: "https://t.me/telegram".to_owned(),
        },
        title: "Rustgram Changelog".to_owned(),
        description: "Read what is new".to_owned(),
        peer_id: None,
        dismissible: true,
    };
    let mut actions = Vec::new();
    {
        let ctrl = harness.state_mut().controller_mut();
        // Activate directly via helper and verify real action emitted.
        let mut session_ctrl = tg_app::window::session_controller::SessionController::new(
            SavedWindowColumns::default(),
            true,
        );
        let mut mock = tg_app::stand_in::mock_session::MockSession::new(true);
        tg_app::live::shortcuts_wiring::activate_promo_suggestion(
            ctrl.main_widget_mut(),
            custom,
            &mut session_ctrl,
            &mut mock,
            tg_theme::EmbeddedType::DayBlue,
            None,
            &mut actions,
        );
    }
    assert!(
        actions.iter().any(|a| matches!(
            a,
            tg_app::window::main_widget::MainWidgetAction::OpenLink(u) if u == "https://t.me/telegram"
        )),
        "CustomPromo must emit MainWidgetAction::OpenLink, got {actions:?}"
    );
    // Also verify PremiumOffer emits OpenPayments { screen: Premium }.
    let mut prem_actions = Vec::new();
    {
        let ctrl = harness.state_mut().controller_mut();
        let mut session_ctrl = tg_app::window::session_controller::SessionController::new(
            SavedWindowColumns::default(),
            true,
        );
        let mut mock = tg_app::stand_in::mock_session::MockSession::new(true);
        tg_app::live::shortcuts_wiring::activate_promo_suggestion(
            ctrl.main_widget_mut(),
            PromoSuggestion {
                key: "PREMIUM_ANNUAL".to_owned(),
                priority: SuggestionPriority::PremiumOffer,
                kind: PromoSuggestionKind::PremiumOffer,
                title: "Telegram Premium".to_owned(),
                description: "Upgrade".to_owned(),
                peer_id: None,
                dismissible: true,
            },
            &mut session_ctrl,
            &mut mock,
            tg_theme::EmbeddedType::DayBlue,
            None,
            &mut prem_actions,
        );
    }
    assert!(
        prem_actions.iter().any(|a| matches!(
            a,
            tg_app::window::main_widget::MainWidgetAction::OpenPayments {
                screen: PaymentsScreen::Premium,
                ..
            }
        )),
        "PremiumOffer must emit OpenPayments(Premium), got {prem_actions:?}"
    );

    drop(harness);
    drop(runtime);
    rt.shutdown_timeout(Duration::from_secs(2));
}

// =============================================================================
// 2. Music Listen Tracker -> MainWidget -> MediaBackend::report_music_listen
// =============================================================================

#[test]
fn wp31_music_listen_tracker_reports_on_track_end_and_60s_pause_via_fake_rpc() {
    let rpc = Wp31FakeRpc::new(2);
    let wav_bytes = generate_test_wav(1);
    let peer_id = DataPeerId::from_user(DataUserId(777));
    let doc1 = DocumentData {
        id: DocumentId(9001),
        access_hash: 42_001,
        file_reference: vec![1, 2, 3, 4],
        date: now_unix_i32(),
        mime_type: "audio/wav".to_owned(),
        size: wav_bytes.len() as i64,
        dc_id: 2,
        file_name: "track_one.wav".to_owned(),
        kind: DocumentKind::Audio,
        duration: Some(6.0),
        ..DocumentData::default()
    };
    let doc2 = DocumentData {
        id: DocumentId(9002),
        access_hash: 42_002,
        file_reference: vec![5, 6, 7, 8],
        date: now_unix_i32(),
        mime_type: "audio/wav".to_owned(),
        size: wav_bytes.len() as i64,
        dc_id: 2,
        file_name: "track_two.wav".to_owned(),
        kind: DocumentKind::Audio,
        duration: Some(6.0),
        ..DocumentData::default()
    };

    // Enqueue upload.getFile responses for doc1 and doc2 (single chunk each) + two reportMusicListen OKs.
    let file_resp = tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FileUnknown,
        mtime: now_unix_i32(),
        bytes: wav_bytes.clone(),
    });
    rpc.enqueue_ok(tl_fns::upload::GetFile::CONSTRUCTOR_ID, &file_resp);
    rpc.enqueue_ok(tl_fns::upload::GetFile::CONSTRUCTOR_ID, &file_resp);
    rpc.enqueue_ok(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID, &true);

    let (rt, runtime) = build_live_runtime(rpc.clone(), 502);
    rt.block_on(async {
        let mut s = runtime.session().write().await;
        let mut msg1 = Message::new_text(
            peer_id,
            DataMsgId(10),
            Some(peer_id),
            now_unix_i32(),
            DataText::plain(String::new()),
        );
        msg1.content = MessageContent::Regular {
            text: DataText::plain(String::new()),
            media: MessageMedia::Document(Box::new(doc1.clone())),
        };
        let mut msg2 = Message::new_text(
            peer_id,
            DataMsgId(11),
            Some(peer_id),
            now_unix_i32(),
            DataText::plain(String::new()),
        );
        msg2.content = MessageContent::Regular {
            text: DataText::plain(String::new()),
            media: MessageMedia::Document(Box::new(doc2.clone())),
        };
        s.insert_message(msg1);
        s.insert_message(msg2);
    });

    let live = LiveSession::new(Arc::clone(&runtime), 502);
    let mut main_widget = tg_app::window::main_widget::MainWidget::with_hardware_audio(false);

    let track1 = AudioMsgId::new(
        doc1.id,
        FullMsgId::new(peer_id, DataMsgId(10)),
        MediaKind::AudioFile,
        6_000,
        0,
    );
    main_widget
        .play_audio_track(&live, track1, false)
        .expect("start track1");

    // Start tracking at t = 1_000ms while playing.
    main_widget.poll_music_listen_reports(&live, 1_000);
    assert!(
        rpc.captured_for(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID)
            .is_empty(),
        "should not report while track is still playing"
    );

    // Stop track1 at t = 5_200ms (4.2s listened -> 4s report) -> immediately invokes reportMusicListen.
    main_widget.audio_player_bar_mut().stop_and_close();
    main_widget.poll_music_listen_reports(&live, 5_200);

    wait_for("first messages.reportMusicListen", || {
        !rpc.captured_for(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID)
            .is_empty()
    });
    let reports = rpc.captured_for(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID);
    assert_eq!(
        reports.len(),
        1,
        "stopping track after >=3s must invoke messages.reportMusicListen"
    );
    let req1 = tl_fns::messages::ReportMusicListen::from_bytes(&reports[0]).expect("decode req1");
    assert_eq!(req1.listened_duration, 4);
    match req1.id {
        tl_enums::InputDocument::InputDocument(d) => {
            assert_eq!(d.id, 9001);
            assert_eq!(d.access_hash, 42_001);
            assert_eq!(d.file_reference, vec![1, 2, 3, 4]);
        }
        other => panic!("expected InputDocument, got {other:?}"),
    }

    // Track 2: play for 5s, pause, verify no report at +59s, and report after 60s pause timeout.
    let track2 = AudioMsgId::new(
        doc2.id,
        FullMsgId::new(peer_id, DataMsgId(11)),
        MediaKind::AudioFile,
        6_000,
        0,
    );
    main_widget
        .play_audio_track(&live, track2, false)
        .expect("start track2");
    main_widget.poll_music_listen_reports(&live, 10_000);
    main_widget
        .audio_player_bar_mut()
        .player_mut()
        .pause(AudioType::Song);
    main_widget.poll_music_listen_reports(&live, 15_000);

    // At +59_999ms after pause (t = 74_999ms): still not flushed.
    main_widget.poll_music_listen_reports(&live, 74_999);
    assert_eq!(
        rpc.captured_for(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // At +60_000ms after pause (t = 75_000ms): flushes 5s report via messages.reportMusicListen.
    main_widget.poll_music_listen_reports(&live, 75_000);
    wait_for("second messages.reportMusicListen", || {
        rpc.captured_for(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID)
            .len()
            >= 2
    });
    let reports = rpc.captured_for(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID);
    assert_eq!(
        reports.len(),
        2,
        "60s pause timeout must flush messages.reportMusicListen"
    );
    let req2 = tl_fns::messages::ReportMusicListen::from_bytes(&reports[1]).expect("decode req2");
    assert_eq!(req2.listened_duration, 5);
    match req2.id {
        tl_enums::InputDocument::InputDocument(d) => {
            assert_eq!(d.id, 9002);
            assert_eq!(d.access_hash, 42_002);
            assert_eq!(d.file_reference, vec![5, 6, 7, 8]);
        }
        other => panic!("expected InputDocument, got {other:?}"),
    }

    // Also verify FILE_REFERENCE_EXPIRED error surfaces gracefully when unrefreshable and
    // short listens (< 3s) make zero RPC calls.
    assert_eq!(live.report_music_listen(track2, 2), Ok(false));
    assert_eq!(
        rpc.captured_for(tl_fns::messages::ReportMusicListen::CONSTRUCTOR_ID)
            .len(),
        2
    );

    drop(main_widget);
    drop(live);
    drop(runtime);
    rt.shutdown_timeout(Duration::from_secs(2));
}

// =============================================================================
// 3. Custom Shortcuts Dispatch + tdata Reload
// =============================================================================

#[test]
fn wp31_custom_shortcut_rebind_dispatches_window_actions_and_reloads_from_tdata() {
    let tdata_dir = TempDir::new().expect("tdata tempdir");
    let rpc = Wp31FakeRpc::new(2);
    let (rt, runtime) = build_live_runtime(rpc, 503);

    // Host 1: attach tdata, rebind ShowDownloads from Ctrl+J to Ctrl+Shift+D, persist to tdata.
    let mut host1 = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        tdata_dir.path().join("shell.json"),
        NotificationSystem::headless(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
            tdata_dir.path().join("notify_cache"),
        ),
        Box::new(Arc::new(Mutex::new(MockTrayPlatform::default()))),
    );
    host1.set_tdata_dir(tdata_dir.path().to_path_buf());
    host1.add_account(
        0,
        DataUserId(503),
        false,
        true,
        Arc::clone(&runtime),
        None,
        true,
        false,
    );

    {
        let ctrl = host1
            .active_workspace_mut()
            .expect("active workspace")
            .controller_mut();
        let settings_backend = ctrl.session_backend().create_settings_backend();
        let settings = ctrl.main_widget_mut().ensure_settings(settings_backend);
        assert_eq!(
            settings.shortcuts.manager.tdata_dir(),
            Some(tdata_dir.path()),
            "MultiAccountWorkspaceHost::set_tdata_dir must attach tdata to ShortcutManager"
        );
        let old_chord = KeyChord::parse("ctrl+j").expect("ctrl+j");
        let new_chord = KeyChord::parse("ctrl+shift+d").expect("ctrl+shift+d");
        settings.shortcuts.manager.change_binding(
            ShortcutCommand::ShowDownloads,
            Some(&old_chord),
            Some(new_chord),
        );
        settings
            .shortcuts
            .manager
            .persist_if_attached()
            .expect("persist shortcuts-custom.json");
    }

    assert!(
        tdata_dir.path().join(CUSTOM_SHORTCUTS_FILENAME).exists(),
        "shortcuts-custom.json must be written to tdata"
    );

    // Drive host1 via egui_kittest: old Ctrl+J must NOT open Downloads; rebound Ctrl+Shift+D MUST!
    let mut harness1 = HarnessBuilder::default()
        .with_size(Vec2::new(1080.0, 720.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(register_tdesktop_fonts)
        .build_ui_state(
            |ui, host: &mut MultiAccountWorkspaceHost| {
                let ctx = ui.ctx().clone();
                let _ = host.show(&ctx, ui);
            },
            host1,
        );
    harness1.step();

    // Old default Ctrl+J was unbound -> must NOT open Downloads.
    harness1.key_press_modifiers(Modifiers::CTRL, Key::J);
    harness1.step();
    let after_old = harness1
        .state()
        .active_workspace()
        .expect("ws")
        .controller()
        .session()
        .expect("session")
        .main_section()
        .cloned();
    assert!(
        !matches!(
            after_old,
            Some(SectionMemento::PeerInfo {
                tab: tg_app::window::section::InfoTab::Downloads,
                ..
            })
        ),
        "unbound Ctrl+J must not open Downloads anymore, got {after_old:?}"
    );

    // While recording a shortcut, pressing Ctrl+Shift+D must be suppressed from global dispatch.
    harness1
        .state_mut()
        .active_workspace_mut()
        .expect("ws")
        .controller_mut()
        .main_widget_mut()
        .settings_mut()
        .expect("settings")
        .shortcuts
        .recording = Some((ShortcutCommand::Search, None));
    harness1.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::D);
    harness1.step();
    assert!(
        !matches!(
            harness1
                .state()
                .active_workspace()
                .expect("ws")
                .controller()
                .session()
                .expect("session")
                .main_section(),
            Some(SectionMemento::PeerInfo {
                tab: tg_app::window::section::InfoTab::Downloads,
                ..
            })
        ),
        "global shortcuts must be suppressed while recording a shortcut"
    );
    harness1
        .state_mut()
        .active_workspace_mut()
        .expect("ws")
        .controller_mut()
        .main_widget_mut()
        .settings_mut()
        .expect("settings")
        .shortcuts
        .recording = None;

    // Now pressing Ctrl+Shift+D dispatches ShowDownloads -> InfoSection::Downloads!
    harness1.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::D);
    harness1.step();
    harness1.step();
    assert!(
        matches!(
            harness1
                .state()
                .active_workspace()
                .expect("ws")
                .controller()
                .session()
                .expect("session")
                .main_section(),
            Some(SectionMemento::PeerInfo {
                tab: tg_app::window::section::InfoTab::Downloads,
                ..
            })
        ),
        "rebound Ctrl+Shift+D must open Downloads"
    );
    drop(harness1);

    // Host 2 (fresh restart against same tdata): automatically reloads shortcuts-custom.json
    // even BEFORE opening SettingsController!
    let mut host2 = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        tdata_dir.path().join("shell.json"),
        NotificationSystem::headless(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
            tdata_dir.path().join("notify_cache"),
        ),
        Box::new(Arc::new(Mutex::new(MockTrayPlatform::default()))),
    );
    host2.add_account(
        0,
        DataUserId(503),
        false,
        true,
        Arc::clone(&runtime),
        None,
        true,
        false,
    );
    host2.set_tdata_dir(tdata_dir.path().to_path_buf());

    let mut harness2 = HarnessBuilder::default()
        .with_size(Vec2::new(1080.0, 720.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(register_tdesktop_fonts)
        .build_ui_state(
            |ui, host: &mut MultiAccountWorkspaceHost| {
                let ctx = ui.ctx().clone();
                let _ = host.show(&ctx, ui);
            },
            host2,
        );
    harness2.step();

    // Press Ctrl+Shift+D in fresh workspace without opening Settings -> reloads from tdata!
    harness2.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::D);
    harness2.step();
    harness2.step();
    let sec = harness2
        .state()
        .active_workspace()
        .expect("ws")
        .controller()
        .session()
        .expect("session")
        .main_section()
        .cloned();
    assert!(
        matches!(
            sec,
            Some(SectionMemento::PeerInfo {
                tab: tg_app::window::section::InfoTab::Downloads,
                ..
            })
        ),
        "reloaded workspace must dispatch Ctrl+Shift+D from tdata/shortcuts-custom.json, got {sec:?}"
    );
    let _ = InfoSection::Downloads {
        peer_id: PeerId::ZERO,
    };

    drop(harness2);
    drop(runtime);
    rt.shutdown_timeout(Duration::from_secs(2));
}

// =============================================================================
// 4. LiveLangPackHandle::attach_tdata_dir in MultiAccountWorkspaceHost::set_tdata_dir
// =============================================================================

#[test]
fn wp31_multi_account_set_tdata_dir_attaches_langpack_tdata() {
    let tdata_dir = TempDir::new().expect("tdata tempdir");

    // Pre-persist a German cloud language pack into tdata.
    let mut seed_inst = LangInstance::new();
    seed_inst.switch_to_id(&Language {
        id: "de".to_owned(),
        plural_id: "de".to_owned(),
        base_id: String::new(),
        name: "German".to_owned(),
        native_name: "Deutsch".to_owned(),
    });
    seed_inst
        .apply_difference(
            tg_lang::Pack::Current,
            &LangPackDifference {
                lang_code: "de".to_owned(),
                from_version: 0,
                version: 77,
                strings: Vec::new(),
            },
        )
        .expect("apply diff");
    seed_inst
        .save_to_tdata_dir(tdata_dir.path())
        .expect("save langpack to tdata");

    let rpc = Wp31FakeRpc::new(2);
    let (rt, runtime) = build_live_runtime(rpc, 504);

    // Case A: set_tdata_dir called BEFORE add_account.
    let mut host = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        tdata_dir.path().join("shell.json"),
        NotificationSystem::headless(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
            tdata_dir.path().join("notify_cache"),
        ),
        Box::new(Arc::new(Mutex::new(MockTrayPlatform::default()))),
    );
    host.set_tdata_dir(tdata_dir.path().to_path_buf());
    host.add_account(
        0,
        DataUserId(504),
        false,
        false,
        Arc::clone(&runtime),
        None,
        true,
        false,
    );

    let live0 = host.account(0).expect("slot 0").workspace.live_session();
    assert_eq!(
        live0.langpack().active_language_code(),
        "de",
        "add_account must attach tdata_dir to LiveLangPackHandle"
    );
    assert_eq!(live0.langpack().versions().0, 77);

    // Case B: set_tdata_dir called AFTER add_account.
    let mut host2 = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        tdata_dir.path().join("shell2.json"),
        NotificationSystem::headless(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
            tdata_dir.path().join("notify_cache"),
        ),
        Box::new(Arc::new(Mutex::new(MockTrayPlatform::default()))),
    );
    host2.add_account(
        0,
        DataUserId(504),
        false,
        false,
        Arc::clone(&runtime),
        None,
        true,
        false,
    );
    host2.set_tdata_dir(tdata_dir.path().to_path_buf());

    let live1 = host2.account(0).expect("slot 0").workspace.live_session();
    assert_eq!(
        live1.langpack().active_language_code(),
        "de",
        "set_tdata_dir must attach tdata_dir to existing accounts' LiveLangPackHandle"
    );

    drop(host);
    drop(host2);
    drop(runtime);
    rt.shutdown_timeout(Duration::from_secs(2));
}

// =============================================================================
// 5. IntroAction::OpenSettings Opening Pre-Login Proxy Settings / AddProxyBox
// =============================================================================

#[derive(Default)]
struct TestIntroShim {
    proxy_enabled: bool,
}

impl Backend for TestIntroShim {
    fn current_user(&self) -> Option<&AuthUser> {
        None
    }
    fn send_login_code(&mut self, _phone: &str) -> Result<CodeSentInfo, String> {
        Err("unused".to_owned())
    }
    fn verify_login_code(&mut self, _code: &str) -> Result<CodeVerifyOutcome, String> {
        Err("unused".to_owned())
    }
    fn verify_cloud_password(&mut self, _password: &str) -> Result<AuthUser, String> {
        Err("unused".to_owned())
    }
    fn logout(&mut self) {}
    fn folders(&self) -> Vec<FolderFilter> {
        Vec::new()
    }
    fn dialogs(&self, _folder: FilterId, _query: &str) -> Vec<DialogEntry> {
        Vec::new()
    }
    fn peer_summary(&self, _peer_id: PeerId) -> Option<PeerSummary> {
        None
    }
    fn history_slice(&self, _peer_id: PeerId) -> Vec<MessageItem> {
        Vec::new()
    }
    fn send_message(&mut self, _peer_id: PeerId, _text: &str) -> Result<MessageItem, String> {
        Err("unused".to_owned())
    }
    fn mark_read(&mut self, _peer_id: PeerId) {}
    fn toggle_mute(&mut self, _peer_id: PeerId) -> bool {
        false
    }
    fn connection_status(&self) -> ConnectionStatus {
        ConnectionStatus::Connected
    }
    fn proxy_enabled(&self) -> bool {
        self.proxy_enabled
    }
    fn set_connection_status(&mut self, _status: ConnectionStatus) {}
    fn set_proxy_enabled(&mut self, enabled: bool) {
        self.proxy_enabled = enabled;
    }
    fn retry_connection(&mut self) {}
}

struct IntroProxyHarnessState {
    intro: IntroWidget,
    shim: TestIntroShim,
    proxy_modal: IntroProxyModal,
    theme_manager: ThemeManager,
    strings: TgLangStrings,
    manager: AccountManager,
    applied: usize,
}

fn fake_credentials() -> ApiCredentials {
    ApiCredentials {
        api_id: 12345,
        api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
    }
}

#[test]
fn wp31_intro_open_settings_opens_add_proxy_box_and_persists_to_tdata() {
    let rpc = Wp31FakeRpc::new(2);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let bridged = RuntimeBridgedRpc::new(rpc, rt.handle().clone());
    let mut intro = IntroWidget::with_rpc_client(fake_credentials(), bridged);
    // Move to Phone step so top-right SETTINGS button is visible (same as live Intro).
    intro.switch_to_phone();
    let work_dir = TempDir::new().expect("working dir");

    let state = IntroProxyHarnessState {
        intro,
        shim: TestIntroShim::default(),
        proxy_modal: IntroProxyModal::new(),
        theme_manager: ThemeManager::default(),
        strings: TgLangStrings::new(),
        manager: AccountManager::open(WorkingDirOpenOptions::new(
            work_dir.path(),
            fake_credentials(),
        ))
        .expect("open AccountManager"),
        applied: 0,
    };

    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(800.0, 620.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(register_tdesktop_fonts)
        .build_ui_state(
            |ui, st: &mut IntroProxyHarnessState| {
                let ctx = ui.ctx().clone();
                let palette = Palette::from_theme_manager(&st.theme_manager);
                let open_with = matches!(
                    st.intro.show(ui, &mut st.shim, &palette, &st.strings),
                    Some(IntroAction::OpenSettings)
                )
                .then(|| st.manager.proxy_settings().clone());
                let manager = &mut st.manager;
                if let Some(enabled) = drive_intro_proxy_modal(
                    &mut st.proxy_modal,
                    open_with,
                    &ctx,
                    &palette,
                    &st.theme_manager,
                    |next| manager.set_proxy_settings(next).map_err(|e| e.to_string()),
                ) {
                    st.shim.set_proxy_enabled(enabled);
                    st.applied += 1;
                }
            },
            state,
        );
    harness.step();
    assert!(!harness.state().proxy_modal.is_open());

    // Click the top-right SETTINGS button on Intro -> opens IntroProxyModal with AddProxyBoxState!
    let settings_label = tg_lang::tr::lng_menu_settings().to_uppercase();
    harness.get_by_label(&settings_label).click();
    harness.step();
    harness.step();

    assert!(
        harness.state().proxy_modal.is_open(),
        "IntroAction::OpenSettings must open pre-login proxy modal"
    );
    assert!(
        harness.state().proxy_modal.add_proxy_box().is_some(),
        "IntroAction::OpenSettings must initialize AddProxyBoxState"
    );

    // Stage a valid SOCKS5 proxy link via open_from_url -> AddProxyBoxState validates and renders
    // "Connect Proxy" button.
    let opened = harness.state_mut().proxy_modal.open_from_url(
        "tg://socks?server=127.0.0.1&port=1080&user=demo&pass=secret",
        SettingsProxy::new(),
    );
    assert!(opened);
    assert!(
        harness
            .state()
            .proxy_modal
            .add_proxy_box()
            .is_some_and(|b| b.is_valid() && b.proxy.host == "127.0.0.1" && b.proxy.port == 1080)
    );
    harness.step();

    // Click "Connect Proxy" button in the modal UI!
    harness.get_by_label("Connect Proxy").click();
    harness.step();
    harness.step();

    assert!(!harness.state().proxy_modal.is_open());
    assert!(harness.state().shim.proxy_enabled());
    assert_eq!(harness.state().applied, 1);

    let applied = harness.state().manager.proxy_settings().clone();
    assert_eq!(applied.settings(), ProxySettingsMode::Enabled);
    assert_eq!(
        applied.selected(),
        &CoreProxyData {
            proxy_type: CoreProxyType::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 1080,
            user: "demo".to_owned(),
            password: "secret".to_owned(),
        }
    );

    // Verify persistence to tdata/settingss round-trips identically (as AccountManager::set_proxy_settings does).
    let reopened = AccountManager::open(WorkingDirOpenOptions::new(
        work_dir.path(),
        fake_credentials(),
    ))
    .expect("reopen AccountManager");
    let reloaded = reopened.proxy_settings();
    assert_eq!(reloaded.settings(), ProxySettingsMode::Enabled);
    assert_eq!(reloaded.selected(), applied.selected());

    drop(harness);
    rt.shutdown_timeout(Duration::from_secs(2));
}
