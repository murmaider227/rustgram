//! WP-34 integration tests:
//! - `tg-mtproto::web_proxy` wired into `tg-session`, `live/proxy.rs`, and `tg-webview` carrier routing
//! - `tg://webproxy` / `https://t.me/webproxy` deep-link import into `AddProxyBoxState` & live session
//! - `MiniAppWindow` `ConfirmPreparedMessage` wired to live `messages.getPreparedInlineMessage` + `messages.sendInlineBotResult`
//! - `BotLocalStorage` per-bot / per-account persistence inside `tdata` across Mini App reloads
//!
//! Uses local loopback fake servers and an in-process `FakeRpcClient` only; zero real network access.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_core::CoreSettings;
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType, SettingsProxy,
};
use tg_data::{PeerUpdateFlags, User, UserId};
use tg_mtproto::web_proxy::{
    BridgeEvent, FrameType, HttpRequestAction, WebProxyState, WebSocketDecoder, WebSocketMessage,
    encode_websocket_frame, parse_frames, parse_local_http_request, serialize_frame,
    web_proxy_bridge_url,
};
use tg_mtproto::{PacketConnection, WebProxyTransportFactory};
use tg_session::{SessionRuntime, SessionRuntimeConfig, WebProxyCheckTarget};
use tg_theme::Palette as TgPalette;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_transport::{
    ProxyData as TransportProxyData, ProxyType as TransportProxyType, encode_web_proxy_link_secret,
};
use tg_webview::{
    HostEvent, InlineChatType, WEB_PROXY_STORAGE_TOKEN, WebMessage, WindowMode,
    parse_web_proxy_bridge_message,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use tg_app::boxes::BoxKind;
use tg_app::boxes::add_proxy::{AddProxyBoxState, apply_confirmed_proxy, route_proxy_link};
use tg_app::live::host::LiveActiveWorkspace;
use tg_app::live::session::LiveSession;
use tg_app::live::web_proxy_carrier::{
    WebProxyCarrierFactory, WebProxyCarrierHost, WebProxyCarrierView,
};
use tg_app::session_api::AppSession;
use tg_app::settings::backend::{ProxyCheckStatus, SettingsBackend};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{ConnectionStatus, PeerId};
use tg_app::webview::AppWebViewHost;
use tg_app::webview::backend::{
    AppWebViewEvent, ClipboardAccess, ExternalOpener, WebViewBackend, WebViewRequest,
};
use tg_app::webview::mini_app::ActiveMiniAppModal;
use tg_app::window::deep_links::{
    DeepLinkOutcome, dispatch_unauthenticated_deep_link, parse_deep_link,
};

// ============================================================================
// In-process FakeRpcClient & Base64 helper (zero external/network dependencies)
// ============================================================================

type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    recorded: Arc<Mutex<Vec<u32>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_err(&self, ctor: u32, err: RpcError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Err(InvocationError::Rpc(err))));
    }

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded.lock().expect("lock recorded").clone()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(2, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded.lock().expect("lock recorded").push(ctor);
        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, Ok(raw))) => {
                assert_eq!(ctor, expected, "unexpected RPC constructor");
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            Some((expected, Err(err))) => {
                assert_eq!(ctor, expected, "unexpected RPC constructor on error");
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
        2
    }
}

fn b64_standard_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn no_sync_runtime_config() -> SessionRuntimeConfig {
    SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        changes_capacity: 64,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    }
}

fn make_live(self_uid: u64, rpc: FakeRpcClient) -> LiveSession {
    let runtime = SessionRuntime::start(
        UserId(self_uid),
        tg_app::live::ErasedRpcClient::new(rpc),
        no_sync_runtime_config(),
    )
    .expect("start runtime");
    LiveSession::new(Arc::new(runtime), self_uid)
}

#[derive(Default)]
struct NoopExternalOpener;

impl ExternalOpener for NoopExternalOpener {
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct NoopClipboard {
    text: RefCell<Option<String>>,
}

impl ClipboardAccess for NoopClipboard {
    fn read_text(&self) -> Option<String> {
        self.text.borrow().clone()
    }

    fn write_text(&self, text: &str) -> bool {
        *self.text.borrow_mut() = Some(text.to_owned());
        true
    }
}

/// Spawns a local loopback fake WebProxy WebSocket relay server that performs RFC 6455
/// handshake validation via `tg_mtproto::web_proxy` and replies `Welcome` to `Hello`.
async fn spawn_fake_web_proxy_ws_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind fake ws relay");
    let addr = listener.local_addr().expect("local_addr");
    let port = addr.port();

    let handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = Vec::with_capacity(1024);
                let mut tmp = [0u8; 2048];
                let consumed = loop {
                    match parse_local_http_request(&buf, port) {
                        Ok(HttpRequestAction::Incomplete) => {
                            let Ok(n) = stream.read(&mut tmp).await else {
                                return;
                            };
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        Ok(HttpRequestAction::UpgradeWebSocket { response, consumed }) => {
                            if stream.write_all(&response).await.is_err() {
                                return;
                            }
                            let _ = stream.flush().await;
                            break consumed;
                        }
                        _ => return,
                    }
                };
                buf.drain(..consumed);

                let mut decoder = WebSocketDecoder::default();
                let mut relay_buf = Vec::new();
                loop {
                    if let Ok(msgs) = decoder.decode(&mut buf) {
                        for msg in msgs {
                            match msg {
                                WebSocketMessage::Binary(payload) => {
                                    relay_buf.extend_from_slice(&payload);
                                    if let Ok(frames) = parse_frames(&mut relay_buf) {
                                        for frame in frames {
                                            if frame.frame_type == FrameType::Hello
                                                && frame.stream_id == 0
                                            {
                                                let welcome =
                                                    serialize_frame(FrameType::Welcome, 0, &[])
                                                        .expect("welcome frame");
                                                let ws = encode_websocket_frame(0x02, &welcome);
                                                let _ = stream.write_all(&ws).await;
                                                let _ = stream.flush().await;
                                            }
                                        }
                                    }
                                }
                                WebSocketMessage::Close(_) => return,
                                _ => {}
                            }
                        }
                    }
                    let Ok(n) = stream.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
            });
        }
    });

    (addr, handle)
}

/// Spawns a local loopback fake HTTP WebProxy bridge server returning 200 OK.
async fn spawn_fake_web_proxy_http_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind fake http bridge");
    let port = listener.local_addr().expect("local_addr").port();
    let handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf).await;
                let resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                let _ = stream.write_all(resp).await;
                let _ = stream.flush().await;
            });
        }
    });
    (port, handle)
}

fn sample_valid_web_proxy(with_path: bool) -> CoreProxyData {
    CoreProxyData {
        proxy_type: CoreProxyType::Web,
        host: "proxy.example.com".to_owned(),
        port: 443,
        user: if with_path {
            "dobry-cola-super-app".to_owned()
        } else {
            String::new()
        },
        password: "000102030405060708090a0b0c0d0e0f".to_owned(),
    }
}

fn seed_user_and_bot(session: &LiveSession, self_uid: u64, bot_uid: u64) {
    let s_arc = session.runtime().session();
    session.runtime().tokio_handle().block_on(async {
        let mut s = s_arc.write().await;

        let mut self_user = User::new(UserId(self_uid), true);
        self_user.access_hash = 111;
        self_user.first_name = "Test".to_owned();
        self_user.last_name = "Account".to_owned();
        s.upsert_user(self_user, PeerUpdateFlags::empty());

        let mut bot_user = User::new(UserId(bot_uid), false);
        bot_user.access_hash = 222;
        bot_user.first_name = "Prepared".to_owned();
        bot_user.last_name = "Bot".to_owned();
        bot_user.bot_info = Some(Default::default());
        s.upsert_user(bot_user, PeerUpdateFlags::empty());

        let mut target_user = User::new(UserId(555_001), false);
        target_user.access_hash = 333;
        target_user.first_name = "Target".to_owned();
        target_user.last_name = "Chat".to_owned();
        s.upsert_user(target_user, PeerUpdateFlags::empty());
    });
}

// ============================================================================
// 1. Fake WebProxy server check & session/webview carrier bridge test
// ============================================================================

#[test]
fn web_proxy_fake_server_checks_and_webview_carrier_session_bridge() {
    let rpc = FakeRpcClient::new();
    let mut live = make_live(100_001, rpc);

    let (ws_addr, ws_handle) = live
        .runtime()
        .tokio_handle()
        .block_on(spawn_fake_web_proxy_ws_server());
    let (http_port, http_handle) = live
        .runtime()
        .tokio_handle()
        .block_on(spawn_fake_web_proxy_http_server());

    let web_proxy = sample_valid_web_proxy(true);

    // 1a. Check WebProxy via local fake WebSocket relay (RFC 6455 101 + Hello -> Welcome).
    live.proxy_controller()
        .set_web_proxy_check_target(Some(WebProxyCheckTarget::WebSocketRelay {
            addr: ws_addr,
            path: "/transport".to_owned(),
            host_header: format!("127.0.0.1:{}", ws_addr.port()),
            origin: format!("http://127.0.0.1:{}", ws_addr.port()),
        }));
    live.check_proxy(&web_proxy);

    let deadline = Instant::now() + Duration::from_secs(5);
    let ws_status = loop {
        let st = live.proxy_check_status(&web_proxy);
        if !matches!(st, ProxyCheckStatus::Checking | ProxyCheckStatus::NotTested)
            || Instant::now() >= deadline
        {
            break st;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    assert!(
        matches!(ws_status, ProxyCheckStatus::Available { .. }),
        "expected WebSocket WebProxy check to succeed, got {ws_status:?}"
    );

    // 1b. Check WebProxy via local fake HTTP bridge server.
    live.proxy_controller()
        .set_web_proxy_check_target(Some(WebProxyCheckTarget::BridgeUrl(format!(
            "http://127.0.0.1:{http_port}/"
        ))));
    live.check_proxy(&web_proxy);

    let deadline = Instant::now() + Duration::from_secs(5);
    let http_status = loop {
        let st = live.proxy_check_status(&web_proxy);
        if !matches!(st, ProxyCheckStatus::Checking | ProxyCheckStatus::NotTested)
            || Instant::now() >= deadline
        {
            break st;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    assert!(
        matches!(http_status, ProxyCheckStatus::Available { .. }),
        "expected HTTP WebProxy check to succeed, got {http_status:?}"
    );

    // 1c. Apply WebProxy settings -> starts live WebProxyBridgeServer and wires tg-webview carrier.
    let mut settings = SettingsProxy::new();
    settings.add_to_list(web_proxy.clone());
    settings.set_selected(web_proxy.clone());
    settings.set_settings(ProxySettingsMode::Enabled);
    live.apply_proxy_settings(&settings)
        .expect("apply web proxy settings");

    let ctrl = live.proxy_controller().clone();
    assert_eq!(ctrl.web_proxy_state(), WebProxyState::WaitingForBrowser);
    let local_bridge = ctrl
        .web_proxy_local_bridge_url()
        .expect("local bridge server bound");
    assert!(local_bridge.starts_with("http://127.0.0.1:"));
    let remote_bridge = ctrl
        .web_proxy_remote_bridge_url()
        .expect("canonical remote bridge url");
    assert_eq!(
        remote_bridge,
        "https://proxy.example.com/dobry-cola-super-app/?bridge=hHz99Xs93EN1j91G9gpNepXwGNNt5YdAFkEVk_LlqdQ"
    );

    // Prepare locked-down tg-webview hidden carrier config.
    let (carrier_url, wv_cfg) = ctrl
        .prepare_web_proxy_webview_carrier(Some("wp34-nonce".to_owned()))
        .expect("prepare webview carrier");
    assert_eq!(carrier_url, format!("{remote_bridge}#android=wp34-nonce"));
    assert_eq!(wv_cfg.mode, WindowMode::Hidden);
    assert!(wv_cfg.safe);
    assert!(!wv_cfg.debug);
    assert_eq!(
        wv_cfg.restricted_origin.as_deref(),
        Some("https://proxy.example.com")
    );
    assert_eq!(wv_cfg.user_data_token.as_slice(), &WEB_PROXY_STORAGE_TOKEN);
    let nav_policy = wv_cfg
        .callbacks
        .on_navigation_policy
        .as_ref()
        .expect("navigation policy");
    assert!(nav_policy(&carrier_url, false));
    assert!(!nav_policy(&carrier_url, true));
    assert!(!nav_policy(
        "https://evil.example/dobry-cola-super-app/",
        false
    ));

    // Drive the hidden WebView carrier protocol through tg-webview -> LiveProxyController -> WebProxySession.
    let hb = WebMessage {
        text: "h".to_owned(),
        source_url: carrier_url.clone(),
    };
    assert!(parse_web_proxy_bridge_message(&hb).is_ok());
    let evs = ctrl
        .route_web_proxy_carrier_message(&hb)
        .expect("heartbeat");
    assert_eq!(evs, vec![BridgeEvent::Heartbeat]);

    let init = WebMessage {
        text: r#"c{"t":"tproxy-android-init","v":1,"nonce":"wp34-nonce"}"#.to_owned(),
        source_url: carrier_url.clone(),
    };
    let init_evs = ctrl.route_web_proxy_carrier_message(&init).expect("init");
    assert!(matches!(init_evs.as_slice(), [BridgeEvent::Eval(out)] if out.sequence == 1));

    let ack = WebMessage {
        text: "a1".to_owned(),
        source_url: carrier_url.clone(),
    };
    let _ = ctrl.route_web_proxy_carrier_message(&ack).expect("ack");

    let welcome = serialize_frame(FrameType::Welcome, 0, &[]).expect("welcome");
    let welcome_msg = WebMessage {
        text: format!("b{}", b64_standard_encode(&welcome)),
        source_url: carrier_url.clone(),
    };
    let ready_evs = ctrl
        .route_web_proxy_carrier_message(&welcome_msg)
        .expect("welcome");
    assert!(ready_evs.contains(&BridgeEvent::Ready));
    assert_eq!(ctrl.web_proxy_state(), WebProxyState::Connected);
    assert_eq!(
        live.bridge_state().connection_status(),
        ConnectionStatus::Connected
    );

    // Open a multiplexed MTProto PacketConnection stream over the active bridge & exchange data.
    let server = ctrl.web_proxy_bridge().expect("active server");
    let factory = WebProxyTransportFactory::new(server);
    let mut conn = factory.server().open_stream().expect("open stream");
    let sid = conn.stream_id();
    let evals = ctrl.drain_web_proxy_carrier_outbound();
    assert!(!evals.is_empty(), "Open frame should be queued for carrier");

    // Simulate relay sending Data on stream `sid` via WebView carrier.
    let data_frame = serialize_frame(FrameType::Data, sid, b"mtproto-response").expect("data");
    let data_msg = WebMessage {
        text: format!("b{}", b64_standard_encode(&data_frame)),
        source_url: carrier_url,
    };
    let _ = ctrl
        .route_web_proxy_carrier_message(&data_msg)
        .expect("relay data");

    let received = live
        .runtime()
        .tokio_handle()
        .block_on(async { conn.recv().await })
        .expect("recv stream data");
    assert_eq!(received, b"mtproto-response");

    // Disabling proxy shuts down the WebProxyBridgeServer cleanly.
    settings.set_settings(ProxySettingsMode::Disabled);
    live.apply_proxy_settings(&settings).expect("disable proxy");
    assert!(ctrl.web_proxy_bridge().is_none());
    assert_eq!(ctrl.web_proxy_state(), WebProxyState::Idle);

    ws_handle.abort();
    http_handle.abort();
}

// ============================================================================
// 2. `tg://webproxy` & `https://t.me/webproxy` deep-link import test
// ============================================================================

#[test]
fn webproxy_deep_links_import_normalize_and_apply_in_add_proxy_box() {
    let plain_secret = "000102030405060708090a0b0c0d0e0f";
    let mut tp = TransportProxyData::none();
    tp.proxy_type = TransportProxyType::Web;
    tp.port = 443;
    tp.set_web_address("https://Proxy.Example.COM/dobry-cola-super-app/");
    tp.password = plain_secret.to_owned();
    let marked_secret = encode_web_proxy_link_secret(&tp);

    // 2a. Root `tg://webproxy` link via deep_links parser + dispatch -> AddProxyBoxState.
    let root_url = format!("tg://webproxy?server=Proxy.Example.COM&secret={plain_secret}");
    assert!(parse_deep_link(&root_url).is_some());
    let outcome = dispatch_unauthenticated_deep_link(&root_url);
    let DeepLinkOutcome::ShowBox(boxed) = outcome else {
        panic!("expected ShowBox(AddProxy) for tg://webproxy, got {outcome:?}");
    };
    let BoxKind::AddProxy(root_box) = *boxed else {
        panic!("expected BoxKind::AddProxy");
    };
    assert!(root_box.is_valid(), "{:?}", root_box.validation_error);
    assert_eq!(root_box.proxy.proxy_type, CoreProxyType::Web);
    assert_eq!(root_box.proxy.host, "proxy.example.com");
    assert_eq!(root_box.proxy.port, 443);
    assert_eq!(root_box.proxy.user, "");
    assert_eq!(root_box.proxy.password, plain_secret);
    assert_eq!(root_box.to_modal("wp").title, "Web Proxy");

    // 2b. Path-scoped `https://t.me/webproxy` link with 0x70-marked base64url secret.
    let path_url = format!(
        "https://t.me/webproxy?server=Proxy.Example.COM/dobry-cola-super-app/&secret={marked_secret}"
    );
    let BoxKind::AddProxy(path_box) =
        route_proxy_link(&path_url).expect("t.me/webproxy should route to AddProxy")
    else {
        panic!("expected BoxKind::AddProxy");
    };
    assert!(path_box.is_valid(), "{:?}", path_box.validation_error);
    assert_eq!(path_box.proxy.proxy_type, CoreProxyType::Web);
    assert_eq!(path_box.proxy.host, "proxy.example.com");
    assert_eq!(path_box.proxy.port, 443);
    assert_eq!(path_box.proxy.user, "dobry-cola-super-app");
    assert_eq!(path_box.proxy.password, plain_secret);
    assert_eq!(
        web_proxy_bridge_url(&tg_session::core_proxy_to_transport(&path_box.proxy)).unwrap(),
        "https://proxy.example.com/dobry-cola-super-app/?bridge=hHz99Xs93EN1j91G9gpNepXwGNNt5YdAFkEVk_LlqdQ"
    );

    // 2c. Confirming the box adds & activates the Web proxy on LiveSession.
    let rpc = FakeRpcClient::new();
    let mut live = make_live(100_002, rpc);
    let mut core = CoreSettings::default();
    apply_confirmed_proxy(path_box.proxy.clone(), &mut core, &mut live)
        .expect("apply confirmed web proxy");
    assert!(core.proxy.is_enabled());
    assert_eq!(core.proxy.selected().proxy_type, CoreProxyType::Web);
    assert!(live.proxy_controller().web_proxy_bridge().is_some());

    // 2d. Invalid webproxy links (unmarked secret with base path, or fake-TLS ee secret) show errors.
    let unmarked_with_path = AddProxyBoxState::from_url(&format!(
        "tg://webproxy?server=proxy.example.com/dobry-cola-super-app&secret={plain_secret}"
    ))
    .expect("routes to box with error");
    assert!(!unmarked_with_path.is_valid());
    assert!(
        unmarked_with_path
            .to_modal("bad1")
            .buttons
            .iter()
            .all(|b| b.id != "connect")
    );

    let ee_secret = format!("ee{}6578616d706c652e636f6d", "11".repeat(16));
    let unsupported_ee = AddProxyBoxState::from_url(&format!(
        "tg://webproxy?server=proxy.example.com&secret={ee_secret}"
    ))
    .expect("routes to box with unsupported error");
    assert!(!unsupported_ee.is_valid());
}

// ============================================================================
// 3. `ConfirmPreparedMessage` wired to live `messages.getPreparedInlineMessage`
// ============================================================================

#[test]
fn mini_app_confirm_prepared_message_invokes_live_rpc_and_sends_inline_result() {
    let rpc = FakeRpcClient::new();
    let bot_id = 777_100u64;
    let self_uid = 100_001u64;
    let recipient_uid = 555_001u64;

    // 1) Mini App launch response.
    let wv_res = tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
        fullsize: false,
        fullscreen: false,
        same_origin: false,
        query_id: None,
        url: "https://bot.example/app".to_owned(),
    });
    rpc.enqueue_ok(tl_fns::messages::RequestWebView::CONSTRUCTOR_ID, &wv_res);

    // 2) messages.getPreparedInlineMessage response.
    let prepared_resp = tl_enums::messages::PreparedInlineMessage::PreparedInlineMessage(
        tl_types::messages::PreparedInlineMessage {
            query_id: 998_877,
            result: tl_enums::BotInlineResult::BotInlineResult(tl_types::BotInlineResult {
                id: "prep_res_42".to_owned(),
                r#type: "article".to_owned(),
                title: Some("Order Receipt".to_owned()),
                description: Some("Summary".to_owned()),
                url: None,
                thumb: None,
                content: None,
                send_message: tl_enums::BotInlineMessage::Text(tl_types::BotInlineMessageText {
                    no_webpage: false,
                    invert_media: false,
                    message: "Receipt #42 confirmed".to_owned(),
                    entities: None,
                    reply_markup: None,
                }),
            }),
            peer_types: vec![
                tl_enums::InlineQueryPeerType::PM,
                tl_enums::InlineQueryPeerType::Chat,
                tl_enums::InlineQueryPeerType::SameBotPM,
            ],
            cache_time: 300,
            users: Vec::new(),
        },
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetPreparedInlineMessage::CONSTRUCTOR_ID,
        &prepared_resp,
    );

    // 3) messages.sendInlineBotResult response.
    let send_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_305_600,
        seq: 1,
    });
    rpc.enqueue_ok(
        tl_fns::messages::SendInlineBotResult::CONSTRUCTOR_ID,
        &send_updates,
    );

    let mut live = make_live(self_uid, rpc.clone());
    seed_user_and_bot(&live, self_uid, bot_id);
    // Put the recipient and the bot's own chat into the loaded main chat list.
    {
        let s_arc = live.runtime().session();
        live.runtime().tokio_handle().block_on(async {
            let mut s = s_arc.write().await;
            for uid in [recipient_uid, bot_id] {
                let pid = tg_data::PeerId::from_user(UserId(uid));
                s.ensure_history(pid).in_chat_list = true;
                s.sync_dialog_lists(pid);
            }
        });
    }

    let launch = live
        .request_web_view(WebViewRequest {
            bot_id,
            start_param: None,
            url_override: Some("https://bot.example/app".to_owned()),
            fullscreen: false,
            theme_params_json: "{}".to_owned(),
        })
        .expect("request web view");
    let mut win = tg_app::webview::mini_app::MiniAppWindow::new(launch);
    let palette = TgPalette::default();
    let external = NoopExternalOpener;
    let clipboard = NoopClipboard::default();
    let now = Instant::now();

    // 3a. Mini App posts `web_app_send_prepared_message` -> fetches via `messages.getPreparedInlineMessage`.
    let msg = WebMessage {
        text: json!(["web_app_send_prepared_message", { "id": "prep-42" }]).to_string(),
        source_url: "https://bot.example/app".to_owned(),
    };
    let _ = win.handle_web_message(&msg, &palette, &mut live, &external, &clipboard, now);

    let modal = win
        .promote_pending_modal_headless()
        .expect("ConfirmPreparedMessage modal queued");
    let ActiveMiniAppModal::ConfirmPreparedMessage(prompt) = modal else {
        panic!("expected ConfirmPreparedMessage modal, got {modal:?}");
    };
    assert_eq!(prompt.prepared_id, "prep-42");
    assert_eq!(prompt.query_id, 998_877);
    assert_eq!(prompt.result_id, "prep_res_42");
    assert_eq!(prompt.preview_title, "Order Receipt");
    assert_eq!(prompt.preview_text, "Receipt #42 confirmed");
    // `SameBotPM` maps to no chat type (`PeerTypesFromMTP`).
    assert_eq!(
        prompt.allowed_peer_types,
        vec![InlineChatType::Users, InlineChatType::Groups]
    );
    // The chooser offers the private chat but never the bot's own chat.
    let candidate_ids: Vec<PeerId> = prompt.candidates.iter().map(|(p, _)| *p).collect();
    assert_eq!(candidate_ids, vec![PeerId::from_user(recipient_uid)]);
    let (chosen_peer, chosen_title) = prompt.candidates[0].clone();
    assert!(prompt.recipient.is_none());

    // "Send" before choosing a chat keeps the box open and sends nothing.
    let no_recipient = win.resolve_active_modal("send", &mut live, &external, now);
    assert!(no_recipient.is_empty());
    assert!(
        !rpc.recorded_ctors()
            .contains(&tl_fns::messages::SendInlineBotResult::CONSTRUCTOR_ID),
        "nothing may be sent before a recipient is chosen"
    );
    assert!(
        matches!(
            win.promote_pending_modal_headless(),
            Some(ActiveMiniAppModal::ConfirmPreparedMessage(_))
        ),
        "the confirmation box stays open until a chat is chosen"
    );
    assert!(
        !win.drain_emitted_host_events()
            .iter()
            .any(|ev| matches!(ev, HostEvent::PreparedMessageFailed { .. })),
        "the page gets no failure while the user is still choosing"
    );

    // Choose recipient and confirm "send" -> invokes `messages.sendInlineBotResult`.
    assert!(win.set_prepared_message_recipient(chosen_peer, chosen_title));
    let app_events = win.resolve_active_modal("send", &mut live, &external, now);
    assert_eq!(
        app_events,
        vec![AppWebViewEvent::PreparedMessageSent {
            bot_id,
            prepared_id: "prep-42".to_owned(),
            query_id: 998_877,
        }]
    );
    let host_events = win.drain_emitted_host_events();
    assert!(host_events.contains(&HostEvent::PreparedMessageSent));

    let calls = rpc.recorded_ctors();
    assert!(
        calls.contains(&tl_fns::messages::GetPreparedInlineMessage::CONSTRUCTOR_ID),
        "expected messages.getPreparedInlineMessage RPC call"
    );
    assert!(
        calls.contains(&tl_fns::messages::SendInlineBotResult::CONSTRUCTOR_ID),
        "expected messages.sendInlineBotResult RPC call"
    );

    // 3b. Expired / invalid prepared message RPC error emits `PreparedMessageFailed` without modal.
    rpc.enqueue_err(
        tl_fns::messages::GetPreparedInlineMessage::CONSTRUCTOR_ID,
        RpcError {
            code: 400,
            name: "MESSAGE_ID_INVALID".to_owned(),
            value: None,
        },
    );
    let expired_msg = WebMessage {
        text: json!(["web_app_send_prepared_message", { "id": "expired-99" }]).to_string(),
        source_url: "https://bot.example/app".to_owned(),
    };
    let _ = win.handle_web_message(
        &expired_msg,
        &palette,
        &mut live,
        &external,
        &clipboard,
        now,
    );
    assert!(win.promote_pending_modal_headless().is_none());
    let fail_events = win.drain_emitted_host_events();
    assert!(fail_events.contains(&HostEvent::PreparedMessageFailed {
        error: "MESSAGE_EXPIRED".to_owned(),
    }));
}

// ============================================================================
// 4. `BotLocalStorage` per-bot / per-account `tdata` reload persistence test
// ============================================================================

#[test]
fn bot_local_storage_persists_per_bot_and_per_account_in_tdata_across_reloads() {
    let tdata = TempDir::new().expect("temp tdata");
    let bot_id = 777_200u64;

    // Account 1 (`user_101`)
    let rpc1 = FakeRpcClient::new();
    let wv_res = tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
        fullsize: false,
        fullscreen: false,
        same_origin: false,
        query_id: None,
        url: "https://miniapp.example/index".to_owned(),
    });
    rpc1.enqueue_ok(tl_fns::messages::RequestWebView::CONSTRUCTOR_ID, &wv_res);
    rpc1.enqueue_ok(tl_fns::messages::RequestWebView::CONSTRUCTOR_ID, &wv_res);
    let live1 = make_live(101, rpc1);
    seed_user_and_bot(&live1, 101, bot_id);
    live1.attach_webview_tdata_dir(tdata.path().to_path_buf(), None);
    // Bot storage is encrypted with the account storage key (SEC3-B2), which the session gets
    // together with its account media cache.
    let account_key = |seed: u8| {
        tg_crypto::StorageEncryptionKey::new(vec![seed; tg_crypto::StorageEncryptionKey::SIZE_V2])
            .expect("account storage key")
    };
    live1
        .set_media_cache_dir_with_key(tdata.path().join("media_cache").join("a1"), account_key(1))
        .expect("attach account 1 cache");

    // Account 2 (`user_202`) sharing the same `tdata` directory.
    let rpc2 = FakeRpcClient::new();
    rpc2.enqueue_ok(tl_fns::messages::RequestWebView::CONSTRUCTOR_ID, &wv_res);
    let live2 = make_live(202, rpc2);
    seed_user_and_bot(&live2, 202, bot_id);
    live2.attach_webview_tdata_dir(tdata.path().to_path_buf(), None);
    live2
        .set_media_cache_dir_with_key(tdata.path().join("media_cache").join("a2"), account_key(2))
        .expect("attach account 2 cache");

    let palette = TgPalette::default();
    let external = NoopExternalOpener;
    let clipboard = NoopClipboard::default();
    let now = Instant::now();

    // 4a. Open Mini App on Account 1 and save two keys via DeviceStorage bridge commands.
    let mut host1 = AppWebViewHost::with_dependencies(
        live1.create_webview_backend(),
        Box::new(NoopExternalOpener),
        Box::new(NoopClipboard::default()),
    );
    host1.open_mini_app(WebViewRequest {
        bot_id,
        start_param: None,
        url_override: Some("https://miniapp.example/index".to_owned()),
        fullscreen: false,
        theme_params_json: "{}".to_owned(),
    });
    let mut backend1_mut = live1.clone();
    {
        let win = host1.mini_app_mut().expect("mini app open");
        let save_theme = WebMessage {
            text: json!([
                "web_app_device_storage_save_key",
                { "req_id": "s1", "key": "theme", "value": "night" }
            ])
            .to_string(),
            source_url: "https://miniapp.example/index".to_owned(),
        };
        let save_draft = WebMessage {
            text: json!([
                "web_app_device_storage_save_key",
                { "req_id": "s2", "key": "draft", "value": "hello-tdata" }
            ])
            .to_string(),
            source_url: "https://miniapp.example/index".to_owned(),
        };
        let _ = win.handle_web_message(
            &save_theme,
            &palette,
            &mut backend1_mut,
            &external,
            &clipboard,
            now,
        );
        let _ = win.handle_web_message(
            &save_draft,
            &palette,
            &mut backend1_mut,
            &external,
            &clipboard,
            now,
        );
        let emitted = win.drain_emitted_host_events();
        assert!(emitted.contains(&HostEvent::DeviceStorageKeySaved {
            req_id: json!("s1"),
        }));
        assert!(emitted.contains(&HostEvent::DeviceStorageKeySaved {
            req_id: json!("s2"),
        }));
    }
    host1.close_mini_app();

    let account_dir = tdata.path().join("bot_storage").join("user_101");
    let expected_file = account_dir.join(format!("bot_storage_{bot_id}.tdef"));
    assert!(
        expected_file.exists(),
        "expected persisted bot storage file at {}",
        expected_file.display()
    );
    // The file is encrypted: no plaintext JSON file, and the saved value is not on disk in clear.
    assert!(
        !account_dir
            .join(format!("bot_storage_{bot_id}.json"))
            .exists(),
        "bot storage must not be written as plaintext JSON"
    );
    let raw = std::fs::read(&expected_file).expect("read bot storage file");
    assert!(
        !raw.windows(b"hello-tdata".len())
            .any(|w| w == b"hello-tdata"),
        "bot storage values must be encrypted on disk"
    );

    // 4b. Reopen Mini App in a fresh AppWebViewHost on Account 1 -> keys reload automatically from tdata.
    let mut host1_reloaded = AppWebViewHost::with_dependencies(
        live1.create_webview_backend(),
        Box::new(NoopExternalOpener),
        Box::new(NoopClipboard::default()),
    );
    host1_reloaded.open_mini_app(WebViewRequest {
        bot_id,
        start_param: None,
        url_override: Some("https://miniapp.example/index".to_owned()),
        fullscreen: false,
        theme_params_json: "{}".to_owned(),
    });
    {
        let win = host1_reloaded.mini_app_mut().expect("reloaded mini app");
        assert_eq!(win.bot_storage().read(bot_id, "theme"), Some("night"));
        assert_eq!(win.bot_storage().read(bot_id, "draft"), Some("hello-tdata"));

        let get_msg = WebMessage {
            text: json!([
                "web_app_device_storage_get_key",
                { "req_id": "g1", "key": "draft" }
            ])
            .to_string(),
            source_url: "https://miniapp.example/index".to_owned(),
        };
        let _ = win.handle_web_message(
            &get_msg,
            &palette,
            &mut backend1_mut,
            &external,
            &clipboard,
            now,
        );
        let emitted = win.drain_emitted_host_events();
        assert!(emitted.contains(&HostEvent::DeviceStorageKeyReceived {
            req_id: json!("g1"),
            value: json!("hello-tdata"),
        }));
    }

    // 4c. Account 2 opening the SAME bot_id has isolated storage inside tdata/bot_storage/user_202.
    let mut host2 = AppWebViewHost::with_dependencies(
        live2.create_webview_backend(),
        Box::new(NoopExternalOpener),
        Box::new(NoopClipboard::default()),
    );
    host2.open_mini_app(WebViewRequest {
        bot_id,
        start_param: None,
        url_override: Some("https://miniapp.example/index".to_owned()),
        fullscreen: false,
        theme_params_json: "{}".to_owned(),
    });
    {
        let win2 = host2.mini_app_mut().expect("account 2 mini app");
        assert_eq!(win2.bot_storage().read(bot_id, "theme"), None);
        assert_eq!(win2.bot_storage().read(bot_id, "draft"), None);
    }

    // 4d. Clearing storage removes persisted file on disk.
    {
        let win = host1_reloaded.mini_app_mut().expect("reloaded mini app");
        let clear_msg = WebMessage {
            text: json!([
                "web_app_device_storage_clear",
                { "req_id": "c1" }
            ])
            .to_string(),
            source_url: "https://miniapp.example/index".to_owned(),
        };
        let _ = win.handle_web_message(
            &clear_msg,
            &palette,
            &mut backend1_mut,
            &external,
            &clipboard,
            now,
        );
        assert!(
            win.drain_emitted_host_events()
                .contains(&HostEvent::DeviceStorageCleared {
                    req_id: json!("c1"),
                })
        );
    }
    assert!(
        !expected_file.exists(),
        "clearing bot storage must remove persisted tdata file"
    );
}

// ============================================================================
// 5. Live window owns the hidden Web proxy carrier and MTProto streams reach it
// ============================================================================

#[derive(Default)]
struct FakeCarrierState {
    created_urls: Vec<String>,
    on_message: Option<tg_webview::MessageHandler>,
    evals: Vec<String>,
    closed: usize,
}

struct FakeCarrierView(std::rc::Rc<RefCell<FakeCarrierState>>);

impl WebProxyCarrierView for FakeCarrierView {
    fn eval(&self, script: &str) -> Result<(), String> {
        self.0.borrow_mut().evals.push(script.to_owned());
        Ok(())
    }

    fn close(&self) {
        self.0.borrow_mut().closed += 1;
    }
}

struct FakeCarrierFactory(std::rc::Rc<RefCell<FakeCarrierState>>);

impl WebProxyCarrierFactory for FakeCarrierFactory {
    fn create(
        &self,
        url: &str,
        config: tg_webview::WebViewConfig,
    ) -> Result<Box<dyn WebProxyCarrierView>, String> {
        assert_eq!(config.mode, WindowMode::Hidden);
        let mut st = self.0.borrow_mut();
        st.created_urls.push(url.to_owned());
        st.on_message = config.callbacks.on_message.clone();
        Ok(Box::new(FakeCarrierView(std::rc::Rc::clone(&self.0))))
    }
}

#[test]
fn live_window_opens_hidden_web_proxy_carrier_and_routes_mtproto_streams_through_it() {
    let tmp = TempDir::new().expect("tempdir");
    let live = make_live(100_301, FakeRpcClient::new());
    let tokio = live.runtime().tokio_handle().clone();
    let mut workspace = LiveActiveWorkspace::new_headless(
        live,
        ShellSettings::default(),
        tmp.path().join("shell.json"),
        false,
    );
    let carrier = std::rc::Rc::new(RefCell::new(FakeCarrierState::default()));
    *workspace.web_proxy_carrier_mut() = WebProxyCarrierHost::with_factory(Box::new(
        FakeCarrierFactory(std::rc::Rc::clone(&carrier)),
    ));

    let ctx = egui::Context::default();
    let run_frame = |ws: &mut LiveActiveWorkspace| {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(960.0, 720.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = ws.show(&ctx, ui);
            },
        );
        out.textures_delta.clear();
    };

    // No Web proxy selected: no carrier.
    run_frame(&mut workspace);
    assert!(carrier.borrow().created_urls.is_empty());
    assert!(!workspace.web_proxy_carrier_mut().is_active());

    let web_proxy = CoreProxyData {
        proxy_type: CoreProxyType::Web,
        host: "carrier.example.com".to_owned(),
        port: 443,
        user: "wp34-carrier".to_owned(),
        password: "000102030405060708090a0b0c0d0e0f".to_owned(),
    };
    let transport_proxy = tg_session::core_proxy_to_transport(&web_proxy);
    let mut settings = SettingsProxy::new();
    settings.add_to_list(web_proxy.clone());
    settings.set_selected(web_proxy.clone());
    settings.set_settings(ProxySettingsMode::Enabled);
    let ctrl = workspace.live_session().proxy_controller().clone();
    ctrl.apply_proxy_settings(&settings)
        .expect("apply web proxy settings");

    // The bridge is registered for the default MTProto transport factory.
    let server = ctrl.web_proxy_bridge().expect("bridge running");
    let registered =
        tg_mtproto::active_web_proxy_bridge(&transport_proxy).expect("bridge registered");
    assert!(Arc::ptr_eq(&server, &registered));

    // The next frame opens exactly one hidden carrier on the proxy's bridge page.
    run_frame(&mut workspace);
    run_frame(&mut workspace);
    let url = {
        let st = carrier.borrow();
        assert_eq!(st.created_urls.len(), 1);
        st.created_urls[0].clone()
    };
    assert!(
        url.starts_with("https://carrier.example.com/wp34-carrier/?bridge="),
        "{url}"
    );
    let nonce = url
        .split("#android=")
        .nth(1)
        .expect("carrier nonce")
        .to_owned();
    assert!(workspace.web_proxy_carrier_mut().is_active());

    // The carrier page reports in; its replies come back as evals on the next frame.
    let on_message = carrier
        .borrow()
        .on_message
        .clone()
        .expect("message handler");
    on_message(WebMessage {
        text: "h".to_owned(),
        source_url: url.clone(),
    });
    on_message(WebMessage {
        text: format!(r#"c{{"t":"tproxy-android-init","v":1,"nonce":"{nonce}"}}"#),
        source_url: url.clone(),
    });
    run_frame(&mut workspace);
    let after_init = carrier.borrow().evals.len();
    assert!(
        after_init >= 1,
        "init reply must be evaluated in the carrier"
    );
    on_message(WebMessage {
        text: "a1".to_owned(),
        source_url: url.clone(),
    });
    let welcome = serialize_frame(FrameType::Welcome, 0, &[]).expect("welcome");
    on_message(WebMessage {
        text: format!("b{}", b64_standard_encode(&welcome)),
        source_url: url.clone(),
    });
    run_frame(&mut workspace);
    assert_eq!(ctrl.web_proxy_state(), WebProxyState::Connected);
    assert_eq!(
        workspace.live_session().bridge_state().connection_status(),
        ConnectionStatus::Connected
    );

    // An MTProto connection dialed through the Web proxy is carried by the hidden WebView.
    let before_send = carrier.borrow().evals.len();
    let mut conn = tokio
        .block_on(tg_mtproto::connect_via_active_web_proxy(
            &transport_proxy,
            2,
            tg_transport::TransportKind::Intermediate,
        ))
        .expect("connect through carrier");
    tokio
        .block_on(conn.send(b"0123456789abcdef"))
        .expect("send through carrier");
    run_frame(&mut workspace);
    assert!(
        carrier.borrow().evals.len() > before_send,
        "Open + Data frames must be evaluated in the carrier"
    );

    // Turning the proxy off closes the carrier and unregisters the bridge.
    settings.set_settings(ProxySettingsMode::Disabled);
    ctrl.apply_proxy_settings(&settings).expect("disable proxy");
    run_frame(&mut workspace);
    assert_eq!(carrier.borrow().closed, 1);
    assert!(!workspace.web_proxy_carrier_mut().is_active());
    assert!(tg_mtproto::active_web_proxy_bridge(&transport_proxy).is_none());
    drop(conn);
}
