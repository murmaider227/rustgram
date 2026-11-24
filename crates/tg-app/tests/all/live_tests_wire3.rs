//! Wave 9 (`wire3`) live wiring integration tests and headless screenshot harness:
//! - Item 6: Async static map tile fetch via `HistoryBackend::load_static_map_tile_async`
//!   (non-blocking `upload.getWebFile(InputWebFileGeoPointLocation)` + stale-key discard)
//! - Item 7: Off-UI-thread bot `RequestLocation` in `BotsDemoScreen` routed through
//!   `BotModalBox::resolve_location_with`
//! - Item 8: Location picker initial country centre (phone country or `help.getNearestDc`),
//!   with zero OS geolocation prompt on open
//! - Item 9: Live `HistoryBackend::query_inline_bot` wired to `contacts.resolveUsername` +
//!   `messages.getInlineBotResults`
//! - Before/after headless `wgpu` screenshots for UI surfaces under `RUSTGRAM_SNAPSHOT_DIR` (or temp dir).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use egui::Vec2;
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{PeerUpdateFlags, User, UserId as DataUserId};
use tg_platform::geolocation::{
    FakeGeoLocationProvider, GeoLocation, GeoLocationAccuracy, GeoPoint, GeolocationStatus,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::{Notify, broadcast};

use tg_app::bots::BotsDemoScreen;
use tg_app::bots::boxes::BotModalBox;
use tg_app::bots::mock::MockBotsBackend;
use tg_app::history::HistoryWidget;
use tg_app::history::backend::HistoryBackend;
use tg_app::history::compose_boxes::{
    AttachFilesBoxState, LOCATION_MAP_TILE_HEIGHT, LOCATION_MAP_TILE_WIDTH, LocationPickerBoxState,
};
use tg_app::history::compose_extras::InlineBotPanelState;
use tg_app::history::sections_model::{InlineBotResultKind, VenueItem};
use tg_app::history::theme::HistoryColors;
use tg_app::live::{ErasedRpcClient, LiveSession};
use tg_app::media::{
    VideoStreamComment, VideoStreamDescriptor, VideoStreamDonor, VideoStreamViewer,
};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::PeerId;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::common::{
    TEST_STEP_DT, apply_deterministic_style, build_widget_harness, init_headless_env,
};

static SNAP_LOCK: Mutex<()> = Mutex::new(());

// =============================================================================
// FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);
type CtorGate = Arc<Mutex<Option<(u32, Arc<Notify>)>>>;

#[derive(Clone)]
struct Wire3FakeRpc {
    main_dc: DcId,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    gate_ctor: CtorGate,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wire3FakeRpc {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(16);
        Self {
            main_dc,
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            gate_ctor: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn gate_request(&self, ctor: u32) -> Arc<Notify> {
        let notify = Arc::new(Notify::new());
        *self.gate_ctor.lock().expect("lock gate") = Some((ctor, Arc::clone(&notify)));
        notify
    }

    fn recorded(&self) -> Vec<RequestCapture> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }
}

impl RpcClient for Wire3FakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        let gate = {
            let mut guard = self.gate_ctor.lock().expect("lock gate");
            if guard.as_ref().is_some_and(|(c, _)| *c == ctor) {
                guard.take().map(|(_, n)| n)
            } else {
                None
            }
        };
        if let Some(notify) = gate {
            notify.notified().await;
        }

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
        self.main_dc
    }
}

fn make_live_session(self_phone: Option<&str>) -> (LiveSession, Wire3FakeRpc, PeerId) {
    let client = Wire3FakeRpc::new(2);
    let self_uid = DataUserId(9301);
    let peer_uid = DataUserId(9302);
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime =
        SessionRuntime::start(self_uid, ErasedRpcClient::new(client.clone()), cfg).expect("rt");
    let session_lock = runtime.session();

    runtime.tokio_handle().block_on(async {
        let mut s = session_lock.write().await;
        let mut self_user = User::new(self_uid, true);
        self_user.first_name = "Morgan".to_owned();
        self_user.access_hash = 30_001;
        self_user.phone = self_phone.unwrap_or_default().to_owned();
        s.upsert_user(self_user, PeerUpdateFlags::empty());

        let mut peer_user = User::new(peer_uid, false);
        peer_user.first_name = "Taylor".to_owned();
        peer_user.access_hash = 40_002;
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
            px.copy_from_slice(&[0x33, 0x90, 0xEC, 0xFF]);
        }
        writer.write_image_data(&pixels).expect("png pixels");
    }
    out
}

/// Barrier-gated [`GeoLocationProvider`] proving a lookup runs off the UI thread (calling it on
/// the UI thread would deadlock).
struct BlockingGeoProvider {
    barrier: Arc<Barrier>,
    location: GeoLocation,
    calls: std::sync::atomic::AtomicUsize,
}

impl BlockingGeoProvider {
    fn new(location: GeoLocation) -> (Self, Arc<Barrier>) {
        let barrier = Arc::new(Barrier::new(2));
        (
            Self {
                barrier: Arc::clone(&barrier),
                location,
                calls: std::sync::atomic::AtomicUsize::new(0),
            },
            barrier,
        )
    }
}

impl tg_platform::geolocation::GeoLocationProvider for BlockingGeoProvider {
    fn status(&self) -> GeolocationStatus {
        GeolocationStatus::Ready
    }

    fn resolve_current_exact_location(&self) -> tg_platform::Result<Option<GeoLocation>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.barrier.wait();
        Ok(Some(self.location))
    }

    fn resolve_location_address(
        &self,
        _location: &GeoLocation,
        _language: &str,
    ) -> tg_platform::Result<Option<tg_platform::geolocation::GeoAddress>> {
        Ok(None)
    }
}

// =============================================================================
// Item 6: Async Static Map Tile Fetch (`HistoryBackend::load_static_map_tile_async`)
// =============================================================================

#[test]
fn wire3_static_map_tile_fetch_is_async_and_never_blocks_ui() {
    let (live, client, _) = make_live_session(Some("+49 30 5550199"));
    let tile_png = encode_test_png(16, 12);
    let gate = client.gate_request(tl_fns::upload::GetWebFile::CONSTRUCTOR_ID);
    client.enqueue_ok(
        tl_fns::upload::GetWebFile::CONSTRUCTOR_ID,
        &tl_enums::upload::WebFile::WebFile(tl_types::upload::WebFile {
            size: tile_png.len() as i32,
            mime_type: "image/png".to_owned(),
            file_type: tl_enums::storage::FileType::FilePng,
            mtime: 1_700_000_000,
            bytes: tile_png,
        }),
    );

    let mut picker = LocationPickerBoxState::new(Vec::new())
        .with_initial_country(live.default_location_country_iso2().as_deref());
    assert!(picker.needs_tile_fetch());

    // Starting the async fetch returns immediately while the RPC is paused on `gate`.
    let call = live.load_static_map_tile_async(
        picker.lat,
        picker.lon,
        LOCATION_MAP_TILE_WIDTH,
        LOCATION_MAP_TILE_HEIGHT,
        picker.zoom,
        1,
        None,
    );
    picker.start_static_map_tile_fetch(call);
    assert!(picker.is_tile_loading());
    assert!(!picker.needs_tile_fetch());
    assert!(
        picker.poll_static_map_tile().is_none(),
        "tile must stay pending while upload.getWebFile is paused"
    );

    // Release the RPC gate and wait for completion.
    gate.notify_one();
    let res = picker
        .wait_static_map_tile(Duration::from_secs(5))
        .expect("async tile fetch must finish");
    assert!(res.is_ok(), "tile decode must succeed: {res:?}");
    assert!(!picker.is_tile_loading());
    assert!(picker.has_loaded_map_tile());
    assert_eq!(picker.loaded_tile_dimensions(), Some((16, 12)));

    // Verify exact TL `upload.getWebFile(InputWebFileGeoPointLocation)` payload.
    let get_web_calls = client.recorded_of(tl_fns::upload::GetWebFile::CONSTRUCTOR_ID);
    assert_eq!(get_web_calls.len(), 1);
    let decoded =
        tl_fns::upload::GetWebFile::from_bytes(&get_web_calls[0]).expect("decode GetWebFile");
    let tl_enums::InputWebFileLocation::InputWebFileGeoPointLocation(geo) = decoded.location else {
        panic!("expected InputWebFileGeoPointLocation");
    };
    let tl_enums::InputGeoPoint::InputGeoPoint(pt) = geo.geo_point else {
        panic!("expected InputGeoPoint");
    };
    assert!((pt.lat - 51.14).abs() < 1e-4);
    assert!((pt.long - 10.505).abs() < 1e-4);
    assert_eq!(geo.w, LOCATION_MAP_TILE_WIDTH);
    assert_eq!(geo.h, LOCATION_MAP_TILE_HEIGHT);

    // Stale tile response is ignored if zoom changes before it finishes.
    let gate2 = client.gate_request(tl_fns::upload::GetWebFile::CONSTRUCTOR_ID);
    client.enqueue_ok(
        tl_fns::upload::GetWebFile::CONSTRUCTOR_ID,
        &tl_enums::upload::WebFile::WebFile(tl_types::upload::WebFile {
            size: 4,
            mime_type: "image/png".to_owned(),
            file_type: tl_enums::storage::FileType::FilePng,
            mtime: 1_700_000_000,
            bytes: vec![0, 1, 2, 3],
        }),
    );
    picker.zoom = 4;
    picker.invalidate_map_tile();
    let stale_call = live.load_static_map_tile_async(
        picker.lat,
        picker.lon,
        LOCATION_MAP_TILE_WIDTH,
        LOCATION_MAP_TILE_HEIGHT,
        picker.zoom,
        1,
        None,
    );
    picker.start_static_map_tile_fetch(stale_call);
    picker.zoom = 5; // user zoomed again before RPC returned
    gate2.notify_one();
    let _ = picker.wait_static_map_tile(Duration::from_secs(5));
    assert!(
        picker.tile_error.is_none(),
        "stale error bytes for superseded zoom must be ignored"
    );
}

// =============================================================================
// Item 7: BotsDemoScreen Off-UI-Thread Location + `BotModalBox::resolve_location_with`
// =============================================================================

#[test]
fn wire3_bots_demo_screen_resolves_location_off_ui_thread_via_resolve_location_with() {
    let (blocking_geo, barrier) = BlockingGeoProvider::new(GeoLocation::exact(35.6762, 139.6503));
    let mut screen = BotsDemoScreen::new(Box::new(MockBotsBackend::new()));
    screen.set_geo_provider(Arc::new(blocking_geo));
    screen.active_modal = Some(BotModalBox::request_location("DeliveryHelperBot"));

    let initial_msg_count = screen.chat().messages.len();
    let palette = Palette::day();
    let mut harness = build_widget_harness(screen, Vec2::new(960.0, 640.0), move |ui, st| {
        let _ = st.show_at(ui, &palette, true, 1_000);
    });
    harness.step();
    harness
        .get_all_by_label("Share Location")
        .last()
        .expect("modal Share Location button")
        .click();
    // Must return immediately without deadlocking on `barrier`.
    harness.step();
    assert!(
        harness.state().is_location_pending(),
        "Share Location must run off the UI thread"
    );

    // Release worker thread and verify the location message arrives.
    barrier.wait();
    let outcome = harness
        .state_mut()
        .wait_location_share(Duration::from_secs(5), 2_000);
    assert_eq!(outcome, Some(true));
    assert!(!harness.state().is_location_pending());
    assert_eq!(harness.state().chat().messages.len(), initial_msg_count + 1);
    let sent = harness
        .state()
        .chat()
        .messages
        .last()
        .expect("sent location message");
    assert!(
        sent.text.contains("35.67620, 139.65030"),
        "expected resolved coords in bot message, got {:?}",
        sent.text
    );

    // Disabled provider flows through `BotModalBox::resolve_location_with` and surfaces its exact status message.
    let disabled = Arc::new(FakeGeoLocationProvider::unavailable(
        GeolocationStatus::Disabled,
    ));
    let mut disabled_screen = BotsDemoScreen::new(Box::new(MockBotsBackend::new()));
    disabled_screen.set_geo_provider(disabled.clone());
    assert!(disabled_screen.start_location_share(None, 3_000));
    assert_eq!(
        disabled_screen.wait_location_share(Duration::from_secs(5), 3_100),
        Some(false)
    );
    assert_eq!(disabled.resolve_call_count(), 0);
    assert_eq!(
        disabled_screen.toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Windows Location Services are disabled in Settings.")
    );
}

// =============================================================================
// Item 8: Location Picker Starts at Country Centre (Phone / `help.getNearestDc`), No Prompt
// =============================================================================

#[test]
fn wire3_location_picker_starts_at_phone_country_center_with_no_geo_prompt() {
    let (live, client, _) = make_live_session(Some("+49 30 5550199"));
    let fake_geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation {
        point: GeoPoint::new(48.8566, 2.3522),
        accuracy: GeoLocationAccuracy::Exact,
        horizontal_accuracy_meters: Some(10.0),
    }));

    let iso2 = live.default_location_country_iso2();
    assert_eq!(iso2.as_deref(), Some("DE"));
    assert!(
        client
            .recorded_of(tl_fns::help::GetNearestDc::CONSTRUCTOR_ID)
            .is_empty(),
        "phone country must resolve without calling help.getNearestDc"
    );

    let picker = LocationPickerBoxState::new(Vec::new())
        .with_geo_provider(fake_geo.clone())
        .with_initial_country(iso2.as_deref());
    assert!((picker.lat - 51.14).abs() < 1e-6);
    assert!((picker.lon - 10.505).abs() < 1e-6);
    assert_eq!(picker.default_country_iso2(), Some("DE"));
    assert!(!picker.is_locating());
    assert_eq!(
        fake_geo.resolve_call_count(),
        0,
        "opening location picker must never trigger an OS location prompt"
    );
}

#[test]
fn wire3_location_picker_falls_back_to_help_get_nearest_dc_country_with_no_geo_prompt() {
    let (live, client, peer_id) = make_live_session(None);
    client.enqueue_ok(
        tl_fns::help::GetNearestDc::CONSTRUCTOR_ID,
        &tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
            country: "JP".to_owned(),
            this_dc: 5,
            nearest_dc: 5,
        }),
    );

    // First call invokes `help.getNearestDc` and caches `"JP"`.
    assert_eq!(live.default_location_country_iso2().as_deref(), Some("JP"));
    assert_eq!(
        client
            .recorded_of(tl_fns::help::GetNearestDc::CONSTRUCTOR_ID)
            .len(),
        1
    );
    // Second call uses the per-session cache (no duplicate RPC).
    assert_eq!(live.default_location_country_iso2().as_deref(), Some("JP"));
    assert_eq!(
        client
            .recorded_of(tl_fns::help::GetNearestDc::CONSTRUCTOR_ID)
            .len(),
        1
    );

    let (lat, lon) = live
        .default_location_center()
        .expect("JP country centre must resolve");
    assert!((lat - 38.29).abs() < 1e-6);
    assert!((lon - 137.475).abs() < 1e-6);

    // Opening location picker through HistoryWidget must NOT prompt OS geolocation.
    let fake_geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation::exact(
        40.7128, -74.0060,
    )));
    let mut widget = HistoryWidget::new();
    widget.set_geo_provider(fake_geo.clone());
    let palette = Palette::day();
    let strings = EnglishStrings;
    let mut harness = build_widget_harness(
        (widget, live),
        Vec2::new(800.0, 600.0),
        move |ui, (w, backend)| {
            let _ = w.show_with_backend(
                ui, peer_id, backend, false, false, false, &palette, &strings,
            );
        },
    );
    harness.step();
    assert_eq!(fake_geo.resolve_call_count(), 0);
}

// =============================================================================
// Item 9: Live `HistoryBackend::query_inline_bot` (`messages.getInlineBotResults`)
// =============================================================================

#[test]
fn wire3_history_backend_query_inline_bot_wires_get_inline_bot_results() {
    let (live, client, _) = make_live_session(Some("+1 555 0100"));
    let bot_id = 88_421_i64;
    let bot_access_hash = 555_001_i64;

    let tl_bot_user = tl_enums::User::User(tl_types::User {
        self_: false,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: true,
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
        id: bot_id,
        access_hash: Some(bot_access_hash),
        first_name: Some("Wiki Inline".to_owned()),
        last_name: None,
        username: Some("wiki".to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: Some(1),
        restriction_reason: None,
        bot_inline_placeholder: Some("Search...".to_owned()),
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
    });

    client.enqueue_ok(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bot_id }),
            chats: Vec::new(),
            users: vec![tl_bot_user],
        }),
    );

    client.enqueue_ok(
        tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID,
        &tl_enums::messages::BotResults::BotResults(tl_types::messages::BotResults {
            gallery: false,
            query_id: 991_001,
            next_offset: None,
            switch_pm: None,
            switch_webview: None,
            results: vec![
                tl_enums::BotInlineResult::BotInlineResult(tl_types::BotInlineResult {
                    id: "art_1".to_owned(),
                    r#type: "article".to_owned(),
                    title: Some("Rust Programming Language".to_owned()),
                    description: Some("Systems programming language".to_owned()),
                    url: Some("https://example.org/rust".to_owned()),
                    thumb: None,
                    content: None,
                    send_message: tl_enums::BotInlineMessage::Text(
                        tl_types::BotInlineMessageText {
                            no_webpage: false,
                            invert_media: false,
                            message: "Rust is a memory-safe systems language.".to_owned(),
                            entities: None,
                            reply_markup: None,
                        },
                    ),
                }),
                tl_enums::BotInlineResult::BotInlineMediaResult(tl_types::BotInlineMediaResult {
                    id: "gif_2".to_owned(),
                    r#type: "gif".to_owned(),
                    photo: None,
                    document: None,
                    title: Some("Ferris Wave".to_owned()),
                    description: Some("Animated GIF".to_owned()),
                    send_message: tl_enums::BotInlineMessage::MediaAuto(
                        tl_types::BotInlineMessageMediaAuto {
                            invert_media: false,
                            message: "Hello from Ferris!".to_owned(),
                            entities: None,
                            reply_markup: None,
                        },
                    ),
                }),
            ],
            cache_time: 60,
            users: Vec::new(),
        }),
    );

    // Query through `InlineBotPanelState::sync_from_text`, exercising `HistoryBackend::query_inline_bot`.
    let mut popup = InlineBotPanelState::new();
    popup.sync_from_text("@wiki rust", &live);
    let results = popup.results();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].id, "art_1");
    assert_eq!(results[0].bot_username, "wiki");
    assert_eq!(results[0].title, "Rust Programming Language");
    assert_eq!(results[0].description, "Systems programming language");
    assert_eq!(
        results[0].send_text,
        "Rust is a memory-safe systems language."
    );
    assert_eq!(results[0].kind, InlineBotResultKind::Article);

    assert_eq!(results[1].id, "gif_2");
    assert_eq!(results[1].kind, InlineBotResultKind::Gif);
    assert_eq!(results[1].send_text, "Hello from Ferris!");

    // Verify exact TL requests recorded on `FakeRpcClient`.
    let resolve_calls = client.recorded_of(tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID);
    assert_eq!(resolve_calls.len(), 1);
    let resolved_req = tl_fns::contacts::ResolveUsername::from_bytes(&resolve_calls[0])
        .expect("decode ResolveUsername");
    assert_eq!(resolved_req.username, "wiki");

    let inline_calls = client.recorded_of(tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID);
    assert_eq!(inline_calls.len(), 1);
    let inline_req = tl_fns::messages::GetInlineBotResults::from_bytes(&inline_calls[0])
        .expect("decode GetInlineBotResults");
    assert_eq!(inline_req.query, "rust");
    let tl_enums::InputUser::InputUser(tl_types::InputUser {
        user_id,
        access_hash,
    }) = inline_req.bot
    else {
        panic!("expected InputUser::InputUser for resolved bot");
    };
    assert_eq!(user_id, bot_id);
    assert_eq!(access_hash, bot_access_hash);

    // Second query reuses cached bot user (zero additional `contacts.resolveUsername` RPCs).
    client.enqueue_ok(
        tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID,
        &tl_enums::messages::BotResults::BotResults(tl_types::messages::BotResults {
            gallery: false,
            query_id: 991_002,
            next_offset: None,
            switch_pm: None,
            switch_webview: None,
            results: Vec::new(),
            cache_time: 60,
            users: Vec::new(),
        }),
    );
    let _ = live.query_inline_bot("wiki", "cargo");
    assert_eq!(
        client
            .recorded_of(tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID)
            .len(),
        1,
        "cached bot must not re-invoke contacts.resolveUsername"
    );
    let inline_calls = client.recorded_of(tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID);
    assert_eq!(inline_calls.len(), 2);
    let second_req = tl_fns::messages::GetInlineBotResults::from_bytes(&inline_calls[1])
        .expect("decode second GetInlineBotResults");
    assert_eq!(second_req.query, "cargo");
    assert!(client.recorded().iter().all(|(c, _)| *c
        == tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID
        || *c == tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID));
}

// =============================================================================
// Headless `wgpu` Before/After Screenshot Generator (`RUSTGRAM_SNAPSHOT_DIR` / temp dir)
// =============================================================================

fn render_wgpu_png<S: 'static>(
    size: Vec2,
    light_theme: bool,
    state: S,
    mut render: impl FnMut(&mut egui::Ui, &mut S) + 'static,
    out_path: &std::path::Path,
) {
    init_headless_env();
    tg_app::clock::init_test_clock();
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(move |ctx| {
            register_tdesktop_fonts(ctx);
            ctx.set_visuals(if light_theme {
                egui::Visuals::light()
            } else {
                egui::Visuals::dark()
            });
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            move |ui, st: &mut S| {
                render(ui, st);
            },
            state,
        );
    for _ in 0..4 {
        harness.step();
    }
    let img = harness.render().expect("wgpu render");
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    img.save(out_path).expect("save screenshot png");
}

#[test]
#[ignore = "Manual screenshot capture for wire3 before/after design review"]
fn wire3_capture_screenshots() {
    let _guard = SNAP_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let phase = std::env::var("WIRE3_SNAP_PHASE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wire3"));
    let sizes = [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ];

    for (size_tag, size) in sizes {
        // 1. SendFilesBox with "Stash files" button
        let mut attach = AttachFilesBoxState::new(false, false);
        attach.add_sample_photo("summer_coast.jpg", 1920, 1280, 1_420_000);
        attach.add_sample_photo("mountain_trail.jpg", 1600, 1200, 980_000);
        attach.add_sample_document("itinerary.pdf", "application/pdf", 340_000);
        attach.caption = "Weekend trip photos and schedule".to_owned();
        let colors = HistoryColors::from_palette(&Palette::day());
        render_wgpu_png(
            size,
            true,
            attach,
            move |ui, st| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(colors.canvas_bg))
                    .show(ui, |ui| {
                        let _ = st.show(ui, &colors);
                    });
            },
            &dir.join(format!("send_files_box_{size_tag}_{phase}.png")),
        );

        // 2. LocationPickerBoxState initial country-centre start point (Germany, no OS prompt)
        let venues = vec![
            VenueItem {
                id: "v1".to_owned(),
                title: "Central Coffee Roasters".to_owned(),
                address: "120 Market Street".to_owned(),
                category: "Cafe".to_owned(),
                lat: 51.14,
                lon: 10.505,
            },
            VenueItem {
                id: "v2".to_owned(),
                title: "City Transit Hub".to_owned(),
                address: "Station Plaza 4".to_owned(),
                category: "Station".to_owned(),
                lat: 51.142,
                lon: 10.509,
            },
        ];
        let picker = LocationPickerBoxState::new(venues).with_initial_country(Some("DE"));
        let colors = HistoryColors::from_palette(&Palette::day());
        render_wgpu_png(
            size,
            true,
            picker,
            move |ui, st| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(colors.canvas_bg))
                    .show(ui, |ui| {
                        let _ = st.show(ui, &colors);
                    });
            },
            &dir.join(format!("location_picker_{size_tag}_{phase}.png")),
        );

        // 3. VideoStreamViewer
        let desc = VideoStreamDescriptor {
            call_id: 4201,
            access_hash: 99,
            peer_id: 1001,
            title: "Community Town Hall Live Stream".to_owned(),
            call_link_slug: String::new(),
            join_msg_id: 0,
            rtmp_stream: true,
            stream_url: None,
            is_live_story: false,
            stream_dc_id: 2,
        };
        let mut viewer = VideoStreamViewer::new(desc, 0.8);
        viewer.update_metadata(None, 1284, false);
        viewer.set_donors(vec![
            VideoStreamDonor {
                peer_id: 1,
                name: "Alex".to_owned(),
                stars: 500,
                is_my: false,
            },
            VideoStreamDonor {
                peer_id: 2,
                name: "Sam".to_owned(),
                stars: 250,
                is_my: true,
            },
        ]);
        viewer.push_comment(VideoStreamComment {
            id: 1,
            sender_peer_id: 1,
            sender_name: "Alex".to_owned(),
            text: "Audio and video are super crisp today!".to_owned(),
            paid_stars: 500,
        });
        viewer.push_comment(VideoStreamComment {
            id: 2,
            sender_peer_id: 3,
            sender_name: "River".to_owned(),
            text: "Looking forward to the Q&A segment.".to_owned(),
            paid_stars: 0,
        });
        let bg = Palette::night().window_bg;
        render_wgpu_png(
            size,
            false,
            viewer,
            move |ui, st| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(bg))
                    .show(ui, |ui| {
                        let rect = ui.max_rect();
                        let _ = st.show(ui, rect, 600);
                    });
            },
            &dir.join(format!("video_stream_{size_tag}_{phase}.png")),
        );

        // 4. BotsDemoScreen with RequestLocation modal
        let mut bots = BotsDemoScreen::with_mock();
        bots.active_modal = Some(BotModalBox::request_location("DeliveryHelperBot"));
        let palette = Palette::day();
        render_wgpu_png(
            size,
            true,
            bots,
            move |ui, st| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(palette.window_bg))
                    .show(ui, |ui| {
                        let _ = st.show_at(ui, &palette, true, 1_000);
                    });
            },
            &dir.join(format!("bots_request_location_{size_tag}_{phase}.png")),
        );
    }
}
