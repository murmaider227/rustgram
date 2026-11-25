//! Integration & unit tests for WP-35:
//! - Windows Geolocation (`tg-platform::geolocation::FakeGeoLocationProvider`) wired into
//!   `LocationPickerBoxState`, bot `RequestLocation` (`bots/boxes.rs`), and Mini App
//!   `web_app_check_location` / `web_app_request_location` (`webview/mini_app.rs`);
//! - Real Telegram static map tiles via `upload.getWebFile(InputWebFileGeoPointLocation)`;
//! - Frameless titlebar (`tg-platform::frameless` + `FramelessTitleBarState`) controlled by
//!   `CoreSettings::native_window_frame`;
//! - Windows Hello (`tg-platform::windows_hello::FakeWindowsHelloVerifier`) for `LocalPasscode`
//!   and `PasscodeLockWidget`;
//! - Windows taskbar thumbnail toolbar buttons (`tg-platform::taskbar_buttons` / `ITaskbarList3`).
//!
//! Every Win32 / WinRT call runs behind a trait with a deterministic fake. No test ever pops a
//! real OS consent dialog, location prompt, or native window.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_core::{CoreSettings, GlobalConfig};
use tg_data::{PeerUpdateFlags, User, UserId as DataUserId};
use tg_platform::frameless::FakeWindowFrameBackend;
use tg_platform::geolocation::{
    FakeGeoLocationProvider, GeoLocation, GeoLocationAccuracy, GeoPoint, GeolocationStatus,
};
use tg_platform::taskbar_buttons::{
    FakeTaskbarButtonsBackend, THBN_CLICKED_CODE, TaskbarButtonEvent, TaskbarButtonIcon,
    TaskbarButtonId, TaskbarMediaState,
};
use tg_platform::windows_hello::{
    FakeWindowsHelloVerifier, WindowsHelloAvailability, WindowsHelloResult,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_theme::Palette as TgPalette;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_webview::WebMessage;
use tokio::sync::broadcast;

use tg_app::bots::BotsDemoScreen;
use tg_app::bots::boxes::{BotModalBox, BotModalOutcome};
use tg_app::bots::mock::MockBotsBackend;
use tg_app::history::HistoryWidget;
use tg_app::history::backend::HistoryBackend;
use tg_app::history::backend::HistoryMedia;
use tg_app::history::compose_boxes::{LocationPickerAction, LocationPickerBoxState};
use tg_app::history::mock::MockHistoryStore;
use tg_app::history::theme::HistoryColors;
use tg_app::live::LiveSession;
use tg_app::live::geo_map::StaticMapTileRequest;
use tg_app::media::mock::MockMediaBackend;
use tg_app::media::player_bar::AudioPlayerBar;
use tg_app::settings::local_passcode::LocalPasscodeState;
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::PeerId;
use tg_app::webview::backend::{
    AppWebViewEvent, BotInfo, ClipboardAccess, ExternalOpener, WebViewLaunchInfo,
};
use tg_app::webview::mini_app::{ActiveMiniAppModal, MiniAppWindow};
use tg_app::webview::mock::MockWebViewBackend;
use tg_app::window::controller::{RootScreen, WindowController};
use tg_app::window::lock_widgets::{PasscodeAttempt, PasscodeLockAction, PasscodeLockWidget};
use tg_app::window::platform_shell::{
    PlatformWindowShell, dispatch_taskbar_button_click, extract_taskbar_media_state,
    should_use_frameless,
};

/// Upper bound for a fake platform call to come back from its worker thread.
const PLATFORM_WAIT: Duration = Duration::from_secs(10);

/// Runs `harness` frames until `done` holds (worker threads answer between frames).
fn run_until<S>(harness: &mut Harness<'_, S>, what: &str, done: impl Fn(&S) -> bool) {
    for _ in 0..1000 {
        harness.run();
        if done(harness.state()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

// =============================================================================
// FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(16);
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

    fn recorded(&self) -> Vec<RequestCapture> {
        self.recorded.lock().expect("lock recorded").clone()
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

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, Ok(raw))) => {
                assert_eq!(ctor, expected, "unexpected RPC constructor");
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            Some((expected, Err(err))) => {
                assert_eq!(ctor, expected);
                Err(err)
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("UNMOCKED_RPC_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }
}

fn make_live_session() -> (LiveSession, FakeRpcClient, PeerId) {
    let client = FakeRpcClient::new(2);
    let self_uid = DataUserId(7101);
    let peer_uid = DataUserId(7102);
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = SessionRuntime::start(
        self_uid,
        tg_app::live::ErasedRpcClient::new(client.clone()),
        cfg,
    )
    .expect("start runtime");
    let session_lock = runtime.session();

    runtime.tokio_handle().block_on(async {
        let mut s = session_lock.write().await;
        let mut self_user = User::new(self_uid, true);
        self_user.first_name = "Jordan".to_owned();
        self_user.access_hash = 10_001;
        s.upsert_user(self_user, PeerUpdateFlags::empty());

        let mut peer_user = User::new(peer_uid, false);
        peer_user.first_name = "Casey".to_owned();
        peer_user.access_hash = 20_002;
        s.upsert_user(peer_user, PeerUpdateFlags::empty());
    });

    let live = LiveSession::new(Arc::new(runtime), 1);
    (live, client, PeerId::from_user(peer_uid.0))
}

fn encode_test_png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for px in pixels.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&[0x4A, 0x90, 0xD9, 0xFF]);
        }
        writer.write_image_data(&pixels).expect("png pixels");
    }
    out
}

// =============================================================================
// 1. Geolocation + upload.getWebFile(InputWebFileGeoPointLocation) Map Tiles
// =============================================================================

#[test]
fn wp35_location_picker_fake_geo_and_upload_get_web_file_static_map_tile() {
    let (live, client, _) = make_live_session();
    let tile_png = encode_test_png(16, 12);

    client.enqueue_ok(
        tl_fns::upload::GetWebFile::CONSTRUCTOR_ID,
        &tl_enums::upload::WebFile::WebFile(tl_types::upload::WebFile {
            size: tile_png.len() as i32,
            mime_type: "image/png".to_owned(),
            file_type: tl_enums::storage::FileType::FilePng,
            mtime: 1_700_000_000,
            bytes: tile_png.clone(),
        }),
    );

    let fake_geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation {
        point: GeoPoint::new(48.8566, 2.3522),
        accuracy: GeoLocationAccuracy::Exact,
        horizontal_accuracy_meters: Some(12.0),
    }));

    let mut picker = LocationPickerBoxState::new(Vec::new()).with_geo_provider(fake_geo.clone());
    assert!(picker.needs_tile_fetch());
    assert!(!picker.has_loaded_map_tile());

    // Clicking "My Location" resolves coordinates through FakeGeoLocationProvider on a worker
    // thread, so the window keeps painting while Windows asks for consent or waits for a fix.
    assert!(picker.start_current_location_request(None));
    assert!(picker.is_locating());
    assert!(
        !picker.start_current_location_request(None),
        "one lookup at a time"
    );
    let resolved = picker
        .wait_current_location(PLATFORM_WAIT)
        .expect("geo lookup should finish")
        .expect("geo lookup should succeed")
        .expect("location should be present");
    assert!(!picker.is_locating());
    assert_eq!(resolved.point, GeoPoint::new(48.8566, 2.3522));
    assert!((picker.lat - 48.8566).abs() < 1e-6);
    assert!((picker.lon - 2.3522).abs() < 1e-6);
    assert!(picker.take_venues_dirty());
    assert_eq!(fake_geo.resolve_call_count(), 1);

    // Fetch static map tile through LiveSession -> upload.getWebFile(InputWebFileGeoPointLocation).
    let loaded = picker.sync_static_map_tile_with(|lat, lon, w, h, z, s| {
        live.load_static_map_tile(lat, lon, w, h, z, s)
    });
    assert!(loaded, "static map tile should decode into RGBA texture");
    assert!(picker.has_loaded_map_tile());
    assert_eq!(picker.loaded_tile_dimensions(), Some((16, 12)));
    assert!(!picker.needs_tile_fetch());

    // Verify exact TL request payload sent over FakeRpcClient.
    let recorded = client.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, tl_fns::upload::GetWebFile::CONSTRUCTOR_ID);
    let decoded_req =
        tl_fns::upload::GetWebFile::from_bytes(&recorded[0].1).expect("decode GetWebFile");
    let tl_enums::InputWebFileLocation::InputWebFileGeoPointLocation(geo_loc) =
        decoded_req.location
    else {
        panic!("expected InputWebFileGeoPointLocation");
    };
    let tl_enums::InputGeoPoint::InputGeoPoint(pt) = geo_loc.geo_point else {
        panic!("expected InputGeoPoint");
    };
    assert!((pt.lat - 48.8566).abs() < 1e-6);
    assert!((pt.long - 2.3522).abs() < 1e-6);
    assert_eq!(geo_loc.w, 368);
    assert_eq!(geo_loc.h, 170);
    assert_eq!(
        geo_loc.zoom,
        StaticMapTileRequest::from_picker(48.8566, 2.3522, 368, 170, 3, 1).zoom
    );

    // Render LocationPickerBoxState in egui_kittest and verify "My Location" + Send Pin flow.
    let colors = HistoryColors::from_palette(&Palette::day());
    let emitted = Arc::new(Mutex::new(None::<LocationPickerAction>));
    let emitted_cb = Arc::clone(&emitted);
    let mut harness = Harness::new_ui_state(
        move |ui, state: &mut LocationPickerBoxState| {
            if let Some(act) = state.show(ui, &colors) {
                *emitted_cb.lock().expect("lock") = Some(act);
            }
        },
        picker,
    );
    harness.run();
    harness.get_by_label("My Location").click();
    run_until(&mut harness, "the My Location fix", |p| !p.is_locating());
    assert_eq!(fake_geo.resolve_call_count(), 2);
    assert!(
        harness
            .state()
            .location_status
            .as_deref()
            .is_some_and(|s| s.starts_with("Current location updated"))
    );

    harness.get_by_label("Send Selected Pin").click();
    harness.run();
    let act = emitted.lock().expect("lock").clone();
    match act {
        Some(LocationPickerAction::Send(att)) => {
            assert!((att.lat - 48.8566).abs() < 1e-6);
            assert!((att.lon - 2.3522).abs() < 1e-6);
        }
        other => panic!("expected Send action, got {other:?}"),
    }
}

// =============================================================================
// 2. Bot RequestLocation Modal + Mini App web_app_check_location / request_location
// =============================================================================

#[test]
fn wp35_bot_request_location_modal_uses_fake_geo_provider() {
    let fake_geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation::exact(
        35.6762, 139.6503,
    )));

    let mut modal = BotModalBox::request_location("HelperBot");
    let resolved = modal
        .resolve_location_with(fake_geo.as_ref())
        .expect("resolve")
        .expect("location");
    assert_eq!(resolved.point, GeoPoint::new(35.6762, 139.6503));
    assert_eq!(fake_geo.resolve_call_count(), 1);

    // Clicking "Share Location" in the modal emits ShareLocationConfirmed with resolved coords.
    let palette = Palette::day();
    let outcome = Arc::new(Mutex::new(None::<BotModalOutcome>));
    let outcome_cb = Arc::clone(&outcome);
    let mut harness = Harness::new_ui_state(
        move |ui, modal_state: &mut BotModalBox| {
            let bounds = ui.max_rect();
            if let Some(ev) =
                tg_app::bots::boxes::show_bot_modal_box(ui, bounds, modal_state, &[], &palette)
            {
                *outcome_cb.lock().expect("lock") = Some(ev);
            }
        },
        modal,
    );
    harness.run();
    harness.get_by_label("Share Location").click();
    harness.run();
    assert_eq!(
        *outcome.lock().expect("lock"),
        Some(BotModalOutcome::ShareLocationConfirmed {
            lat_e7: 356_762_000,
            lon_e7: 1_396_503_000,
        })
    );

    // Disabled geolocation returns None without crashing and updates status_message.
    let disabled_geo = FakeGeoLocationProvider::unavailable(GeolocationStatus::Disabled);
    let mut disabled_modal = BotModalBox::request_location("HelperBot");
    assert_eq!(
        disabled_modal
            .resolve_location_with(&disabled_geo)
            .expect("disabled returns Ok(None)"),
        None
    );

    // BotsDemoScreen accepts an injected FakeGeoLocationProvider.
    let mut screen = BotsDemoScreen::new(Box::new(MockBotsBackend::new()));
    screen.set_geo_provider(fake_geo);
}

#[derive(Default)]
struct TestExternalOpener {
    opened: Mutex<Vec<String>>,
}

impl ExternalOpener for TestExternalOpener {
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.opened.lock().expect("lock").push(url.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct TestClipboard;

impl ClipboardAccess for TestClipboard {
    fn read_text(&self) -> Option<String> {
        None
    }

    fn write_text(&self, _text: &str) -> bool {
        true
    }
}

#[test]
fn wp35_mini_app_check_and_request_location_events() {
    let launch = WebViewLaunchInfo {
        bot: BotInfo {
            id: 42,
            username: "geo_miniapp_bot".to_owned(),
            title: "Geo Mini App".to_owned(),
            verified: false,
            in_attach_menu: false,
            in_main_menu: false,
            allow_clipboard_read: false,
            is_game: false,
        },
        url: "https://miniapp.example.org/app".to_owned(),
        query_id: 0,
        same_origin: true,
        fullscreen: false,
        local_routes: None,
    };
    let mut window = MiniAppWindow::new(launch);
    let fake_geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation {
        point: GeoPoint::new(52.52, 13.405),
        accuracy: GeoLocationAccuracy::Exact,
        horizontal_accuracy_meters: Some(8.5),
    }));
    window.set_geo_provider(fake_geo.clone());

    let palette = TgPalette::default();
    let mut backend = MockWebViewBackend::new();
    let external = TestExternalOpener::default();
    let clipboard = TestClipboard;
    let now = Instant::now();

    let check_msg = WebMessage {
        text: r#"["web_app_check_location", {}]"#.to_owned(),
        source_url: "https://miniapp.example.org/app".to_owned(),
    };
    let _ = window.handle_web_message(
        &check_msg,
        &palette,
        &mut backend,
        &external,
        &clipboard,
        now,
    );

    let req_msg = WebMessage {
        text: r#"["web_app_request_location", {}]"#.to_owned(),
        source_url: "https://miniapp.example.org/app".to_owned(),
    };
    // The first request asks for per-bot consent and does not touch the location provider.
    let _ = window.handle_web_message(&req_msg, &palette, &mut backend, &external, &clipboard, now);
    let _ = window.handle_web_message(&req_msg, &palette, &mut backend, &external, &clipboard, now);
    assert_eq!(fake_geo.resolve_call_count(), 0);
    assert_eq!(window.pending_modals().len(), 1, "one consent box, not two");
    assert!(window.promote_next_modal_headless());
    assert!(matches!(
        window.active_modal(),
        Some(ActiveMiniAppModal::ConfirmLocation)
    ));
    let _ = window.resolve_active_modal("allow", &mut backend, &external, now);
    assert_eq!(window.location_access(), Some(true));
    assert!(window.is_location_pending());
    assert!(window.wait_pending_location(PLATFORM_WAIT));
    assert_eq!(fake_geo.resolve_call_count(), 1);

    let settings_msg = WebMessage {
        text: r#"["web_app_open_location_settings", {}]"#.to_owned(),
        source_url: "https://miniapp.example.org/app".to_owned(),
    };
    let evs = window.handle_web_message(
        &settings_msg,
        &palette,
        &mut backend,
        &external,
        &clipboard,
        now,
    );
    assert_eq!(
        evs,
        vec![AppWebViewEvent::OpenExternalUrl {
            url: "ms-settings:privacy-location".to_owned()
        }]
    );

    let posted = window.posted_raw_events();
    assert_eq!(posted.len(), 2);
    assert_eq!(posted[0].0, "location_checked");
    assert_eq!(posted[0].1["available"], serde_json::json!(true));
    assert_eq!(posted[0].1["access_requested"], serde_json::json!(false));
    assert_eq!(posted[0].1["access_granted"], serde_json::json!(false));
    assert_eq!(posted[1].0, "location_requested");
    assert_eq!(posted[1].1["available"], serde_json::json!(true));
    assert_eq!(posted[1].1["latitude"], serde_json::json!(52.52));
    assert_eq!(posted[1].1["longitude"], serde_json::json!(13.405));
    assert_eq!(posted[1].1["horizontal_accuracy"], serde_json::json!(8.5));

    // After consent, check reports access granted and a new request answers without a box.
    let _ = window.handle_web_message(
        &check_msg,
        &palette,
        &mut backend,
        &external,
        &clipboard,
        now,
    );
    let _ = window.handle_web_message(&req_msg, &palette, &mut backend, &external, &clipboard, now);
    assert!(window.pending_modals().is_empty());
    assert!(window.wait_pending_location(PLATFORM_WAIT));
    assert_eq!(fake_geo.resolve_call_count(), 2);
    let posted = window.posted_raw_events();
    assert_eq!(posted.len(), 4);
    assert_eq!(posted[2].1["access_requested"], serde_json::json!(true));
    assert_eq!(posted[2].1["access_granted"], serde_json::json!(true));
    assert_eq!(posted[3].1["latitude"], serde_json::json!(52.52));

    // Disabled location provider reports access_granted=false and available=false on request.
    let disabled = Arc::new(FakeGeoLocationProvider::unavailable(
        GeolocationStatus::Disabled,
    ));
    window.set_geo_provider(disabled);
    let _ = window.handle_web_message(
        &check_msg,
        &palette,
        &mut backend,
        &external,
        &clipboard,
        now,
    );
    let _ = window.handle_web_message(&req_msg, &palette, &mut backend, &external, &clipboard, now);
    let posted = window.posted_raw_events();
    assert_eq!(posted[4].0, "location_checked");
    assert_eq!(posted[4].1["access_granted"], serde_json::json!(false));
    assert_eq!(posted[5].0, "location_requested");
    assert_eq!(posted[5].1["available"], serde_json::json!(false));
}

#[test]
fn wp35_mini_app_location_denied_never_reads_device_location() {
    let launch = WebViewLaunchInfo {
        bot: BotInfo {
            id: 43,
            username: "geo_denied_bot".to_owned(),
            title: "Geo Denied".to_owned(),
            verified: false,
            in_attach_menu: false,
            in_main_menu: false,
            allow_clipboard_read: false,
            is_game: false,
        },
        url: "https://miniapp.example.org/denied".to_owned(),
        query_id: 0,
        same_origin: true,
        fullscreen: false,
        local_routes: None,
    };
    let mut window = MiniAppWindow::new(launch);
    let fake_geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation::exact(
        40.0, -3.7,
    )));
    window.set_geo_provider(fake_geo.clone());
    let palette = TgPalette::default();
    let mut backend = MockWebViewBackend::new();
    let external = TestExternalOpener::default();
    let clipboard = TestClipboard;
    let now = Instant::now();
    let req_msg = WebMessage {
        text: r#"["web_app_request_location", {}]"#.to_owned(),
        source_url: "https://miniapp.example.org/denied".to_owned(),
    };

    let _ = window.handle_web_message(&req_msg, &palette, &mut backend, &external, &clipboard, now);
    assert!(window.promote_next_modal_headless());
    let _ = window.resolve_active_modal("cancel", &mut backend, &external, now);
    assert_eq!(window.location_access(), Some(false));

    // A later request is answered "unavailable" straight away, without a new box.
    let _ = window.handle_web_message(&req_msg, &palette, &mut backend, &external, &clipboard, now);
    assert!(window.pending_modals().is_empty());
    let posted = window.posted_raw_events();
    assert_eq!(posted.len(), 2);
    assert!(
        posted
            .iter()
            .all(|(name, payload)| name == "location_requested"
                && payload["available"] == serde_json::json!(false))
    );
    assert_eq!(fake_geo.resolve_call_count(), 0);
}

// =============================================================================
// 3. Windows Hello LocalPasscode & PasscodeLockWidget (FakeWindowsHelloVerifier)
// =============================================================================

#[test]
fn wp35_windows_hello_local_passcode_and_lock_screen() {
    let fake_hello = Arc::new(FakeWindowsHelloVerifier::available_verified());
    let mut core = CoreSettings::default();
    let mut global = GlobalConfig::default();
    let mut state = LocalPasscodeState::default();
    state.set_windows_hello_verifier(fake_hello.clone());
    state.set_window_hwnd(0x4242);
    state
        .apply_new_passcode("9876", "9876", &mut global)
        .expect("set passcode");
    state.unlocked_for_manage = false;

    // Attempting Hello unlock while disabled in CoreSettings fails without calling OS verifier.
    assert!(state.start_windows_hello_unlock(&core, None).is_err());
    assert!(!state.is_windows_hello_pending());
    assert_eq!(fake_hello.verification_call_count(), 0);

    // Enable Windows Hello & verify availability + unlock.
    assert_eq!(
        state.refresh_windows_hello_availability(),
        WindowsHelloAvailability::Available
    );
    core.system_unlock_enabled = true;
    assert_eq!(state.start_windows_hello_unlock(&core, None), Ok(true));
    assert!(state.is_windows_hello_pending());
    state
        .wait_windows_hello_unlock(PLATFORM_WAIT, &mut global)
        .expect("Hello prompt should finish")
        .expect("Hello unlock should succeed");
    assert!(!state.is_windows_hello_pending());
    assert!(state.unlocked_for_manage);
    assert_eq!(fake_hello.verification_call_count(), 1);
    assert_eq!(fake_hello.recorded_requests()[0].0, 0x4242);

    // PasscodeLockWidget: RetriesExhausted -> Flood, Canceled -> Wrong, Verified -> Unlocked via UI click.
    let strings = EnglishStrings;
    let mut lock = PasscodeLockWidget::new();
    lock.set_system_unlock_enabled(true);
    lock.set_window_hwnd(0xBEEF);
    lock.set_windows_hello_verifier(Some(fake_hello.clone()));

    fake_hello.set_result(WindowsHelloResult::RetriesExhausted);
    assert_eq!(lock.start_windows_hello_unlock(None), PasscodeAttempt::Busy);
    assert!(lock.is_windows_hello_pending());
    assert_eq!(
        lock.wait_windows_hello_unlock(PLATFORM_WAIT, &strings),
        Some(PasscodeAttempt::Flood)
    );

    fake_hello.set_result(WindowsHelloResult::Canceled);
    assert_eq!(lock.start_windows_hello_unlock(None), PasscodeAttempt::Busy);
    assert_eq!(
        lock.wait_windows_hello_unlock(PLATFORM_WAIT, &strings),
        Some(PasscodeAttempt::Wrong)
    );
    assert!(!lock.is_windows_hello_pending());

    fake_hello.set_result(WindowsHelloResult::Verified);
    let palette = Palette::day();
    let lock_action = Arc::new(Mutex::new(None::<PasscodeLockAction>));
    let lock_action_cb = Arc::clone(&lock_action);
    let mut harness = Harness::new_ui_state(
        move |ui, widget: &mut PasscodeLockWidget| {
            if let Some(act) = widget.show(ui, "9876", &palette, &EnglishStrings) {
                *lock_action_cb.lock().expect("lock") = Some(act);
            }
        },
        lock,
    );
    harness.run();
    harness.get_by_label("Unlock with Windows Hello").click();
    let lock_action_wait = Arc::clone(&lock_action);
    run_until(&mut harness, "the lock screen Hello prompt", move |_| {
        lock_action_wait.lock().expect("lock").is_some()
    });
    assert_eq!(
        *lock_action.lock().expect("lock"),
        Some(PasscodeLockAction::Unlock)
    );

    // Windows Hello is offered only when enabled in Local Passcode settings.
    let mut hidden_lock = PasscodeLockWidget::new();
    hidden_lock.set_windows_hello_verifier(Some(fake_hello.clone()));
    let before = fake_hello.verification_call_count();
    assert_eq!(
        hidden_lock.start_windows_hello_unlock(None),
        PasscodeAttempt::Empty
    );
    assert!(!hidden_lock.is_windows_hello_pending());
    assert_eq!(fake_hello.verification_call_count(), before);

    // WindowController wires verifier and HWND into PasscodeLockWidget cleanly.
    let settings = ShellSettings {
        local_passcode: Some("1111".to_owned()),
        ..ShellSettings::default()
    };
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        settings,
        std::env::temp_dir().join("rustgram_wp35_ctrl.json"),
    );
    ctrl.set_windows_hello_verifier(Some(fake_hello.clone()));
    ctrl.set_window_hwnd(0x7777);
    ctrl.setup_passcode_lock();
    assert_eq!(ctrl.root_screen(), RootScreen::PasscodeLock);
    ctrl.passcode_lock_mut().set_system_unlock_enabled(true);
    assert_eq!(
        ctrl.passcode_lock_mut().start_windows_hello_unlock(None),
        PasscodeAttempt::Busy
    );
    assert_eq!(
        ctrl.passcode_lock_mut()
            .wait_windows_hello_unlock(PLATFORM_WAIT, &strings),
        Some(PasscodeAttempt::Unlocked)
    );
    let last = fake_hello.recorded_requests();
    assert_eq!(last.last().map(|r| r.0), Some(0x7777));
}

#[test]
fn wp35_history_bot_request_location_sends_location_message_after_async_fix() {
    let mut store = MockHistoryStore::new();
    let bot_chat = PeerId::from_user(1002);
    let fake_geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation::exact(
        59.3293, 18.0686,
    )));
    let mut widget = HistoryWidget::new();
    widget.set_geo_provider(fake_geo.clone());

    // "Share Location" in the bot RequestLocation box starts the fix without blocking.
    assert!(widget.start_bot_location_share(bot_chat, None, 1.0));
    assert!(
        !widget.start_bot_location_share(bot_chat, None, 1.0),
        "one lookup at a time"
    );
    assert_eq!(
        widget.wait_bot_location_share(PLATFORM_WAIT, &mut store, 2.0),
        Some(true)
    );
    assert_eq!(fake_geo.resolve_call_count(), 1);

    let slice =
        store.load_slice_around(bot_chat, tg_app::stand_in::types::MsgId::SHOW_AT_END, 5, 0);
    let last = slice.messages.last().expect("sent message");
    match &last.media {
        Some(HistoryMedia::Location(loc)) => {
            assert!((loc.lat - 59.3293).abs() < 1e-6);
            assert!((loc.lon - 18.0686).abs() < 1e-6);
            assert!(loc.live_period_secs.is_none());
        }
        other => panic!("expected a location message, got {other:?}"),
    }

    // With Windows Location Services off nothing is resolved or sent.
    let disabled = Arc::new(FakeGeoLocationProvider::unavailable(
        GeolocationStatus::Disabled,
    ));
    widget.set_geo_provider(disabled.clone());
    assert!(!widget.start_bot_location_share(bot_chat, None, 3.0));
    assert_eq!(widget.poll_bot_location_share(&mut store, 3.0), None);
    assert_eq!(disabled.resolve_call_count(), 0);
}

// =============================================================================
// 4. Frameless Titlebar + Win32 Taskbar Media Thumbnail Buttons
// =============================================================================

#[test]
fn wp35_frameless_shell_and_taskbar_media_buttons() {
    assert!(should_use_frameless(false, false));
    assert!(!should_use_frameless(true, false));
    assert!(!should_use_frameless(false, true));

    let fake_taskbar = FakeTaskbarButtonsBackend::new();
    let mut shell = PlatformWindowShell::with_backends(
        FakeWindowFrameBackend::new(),
        Some(fake_taskbar.clone()),
    );

    // Initial frameless dark unlocked sync -> Aero snap ON, dark mode ON, iconic preview OFF.
    shell.sync_window_frame(None, 0x5000, true, false, false, false);
    assert_eq!(shell.frame_backend().aero_snap_calls, vec![(0x5000, true)]);
    assert_eq!(shell.frame_backend().dark_mode_calls, vec![(0x5000, true)]);
    assert_eq!(
        shell.frame_backend().passcode_iconic_calls,
        vec![(0x5000, false)]
    );

    // Repeated identical sync -> deduplicated (no redundant Win32 calls).
    shell.sync_window_frame(None, 0x5000, true, false, false, false);
    assert_eq!(shell.frame_backend().aero_snap_calls.len(), 1);
    assert_eq!(shell.frame_backend().dark_mode_calls.len(), 1);

    // Toggling CoreSettings::native_window_frame = true and passcode lock = true updates Win32 state.
    shell.sync_window_frame(None, 0x5000, false, true, false, true);
    assert_eq!(
        shell.frame_backend().aero_snap_calls,
        vec![(0x5000, true), (0x5000, false)]
    );
    assert_eq!(
        shell.frame_backend().dark_mode_calls,
        vec![(0x5000, true), (0x5000, false)]
    );
    assert_eq!(
        shell.frame_backend().passcode_iconic_calls,
        vec![(0x5000, false), (0x5000, true)]
    );

    // Custom frameless title bar renders when native_window_frame=false, skipped when true.
    let palette = Palette::night();
    let rendered = Arc::new(Mutex::new((false, false)));
    let rendered_cb = Arc::clone(&rendered);
    let mut harness =
        Harness::new_ui_state(
            move |ui,
                  sh: &mut PlatformWindowShell<
                FakeWindowFrameBackend,
                FakeTaskbarButtonsBackend,
            >| {
                let shown_frameless =
                    sh.show_frameless_title_bar(ui, "Rustgram", &palette, false, false);
                let shown_native =
                    sh.show_frameless_title_bar(ui, "Rustgram", &palette, true, false);
                *rendered_cb.lock().expect("lock") = (shown_frameless, shown_native);
            },
            shell,
        );
    harness.run();
    assert_eq!(*rendered.lock().expect("lock"), (true, false));

    // Taskbar thumbnail buttons synchronized from headless AudioPlayerBar (no hardware audio).
    let backend = MockMediaBackend::new();
    let mut bar = AudioPlayerBar::new(&backend, false);
    let shell = harness.state_mut();

    // Idle bar -> inactive state -> 0 taskbar COM calls.
    assert_eq!(
        extract_taskbar_media_state(&bar),
        TaskbarMediaState::inactive()
    );
    assert!(!shell.sync_taskbar_from_audio_bar(0x5000, &bar, true));
    assert!(fake_taskbar.calls().is_empty());

    // Start playing song -> ThumbBarAddButtons with Pause icon.
    let first_song = *backend.sample_song_ids().first().expect("mock sample song");
    let _ = bar.play_item(first_song, None);
    let playing_state = extract_taskbar_media_state(&bar);
    assert!(playing_state.active);
    assert!(playing_state.playing);
    assert!(shell.sync_taskbar_from_audio_bar(0x5000, &bar, true));
    let add_call = fake_taskbar.last_call().expect("add call");
    assert!(add_call.is_add);
    assert_eq!(add_call.specs[1].icon, TaskbarButtonIcon::Pause);

    // Clicking taskbar PlayPause (decoded from WM_COMMAND THBN_CLICKED) pauses the player.
    let wparam = ((THBN_CLICKED_CODE as usize) << 16) | (TaskbarButtonId::PlayPause as usize);
    let clicked = TaskbarButtonId::from_wm_command(wparam).expect("decode button");
    assert!(dispatch_taskbar_button_click(clicked, &mut bar));
    let paused_state = extract_taskbar_media_state(&bar);
    assert!(paused_state.active);
    assert!(!paused_state.playing);
    assert!(shell.sync_taskbar_from_audio_bar(0x5000, &bar, true));
    let upd_call = fake_taskbar.last_call().expect("update call");
    assert!(!upd_call.is_add);
    assert_eq!(upd_call.specs[1].icon, TaskbarButtonIcon::Play);

    // Backend-queued click + TaskbarButtonCreated events flow through PlatformWindowShell.
    fake_taskbar.push_event(TaskbarButtonEvent::Clicked(TaskbarButtonId::PlayPause));
    fake_taskbar.push_event(TaskbarButtonEvent::TaskbarButtonCreated);
    assert_eq!(shell.poll_and_dispatch_taskbar_clicks(&mut bar), 1);
    assert!(extract_taskbar_media_state(&bar).playing);
    // Explorer restart cleared `added`, so next sync re-issues ThumbBarAddButtons.
    assert!(shell.sync_taskbar_from_audio_bar(0x5000, &bar, false));
    let readd_call = fake_taskbar.last_call().expect("re-add call");
    assert!(readd_call.is_add);
    assert_eq!(readd_call.specs[1].icon, TaskbarButtonIcon::Pause);

    // Stopping player hides all 3 taskbar buttons.
    bar.stop_and_close();
    assert!(shell.sync_taskbar_from_audio_bar(0x5000, &bar, true));
    let hide_call = fake_taskbar.last_call().expect("hide call");
    assert!(hide_call.specs.iter().all(|b| !b.visible));
}

#[test]
fn wp35_native_window_frame_setting_seeds_settings_panel_and_persists_toggle() {
    let settings = ShellSettings {
        native_window_frame: true,
        ..ShellSettings::default()
    };
    let path = std::env::temp_dir().join(format!(
        "rustgram_wp35_native_frame_{}.json",
        std::process::id()
    ));
    let mut ctrl = WindowController::with_session(MockSession::new(true), settings, path);
    // No Settings panel yet: the persisted shell value is used.
    assert!(ctrl.sync_native_window_frame_setting());

    ctrl.open_settings();
    if ctrl.main_widget_mut().settings_mut().is_none() {
        // Mock sessions create the Settings panel lazily on first render; force it here.
        let backend = Box::new(tg_app::settings::mock::MockSettingsBackend::new());
        ctrl.main_widget_mut().ensure_settings(backend);
    }
    // First sync seeds the freshly created panel (CoreSettings::new() defaults to `false`).
    assert!(ctrl.sync_native_window_frame_setting());
    assert!(ctrl.native_window_frame());

    // Turning the toggle off in Settings > Advanced reaches the shell settings.
    if let Some(panel) = ctrl.main_widget_mut().settings_mut() {
        panel.core_settings.native_window_frame = false;
    }
    assert!(!ctrl.sync_native_window_frame_setting());
    assert!(!ctrl.settings().native_window_frame);
}
