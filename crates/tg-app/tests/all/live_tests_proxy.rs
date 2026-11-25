//! Tests for live proxy wiring in `tg-app`:
//! - `tg://proxy`, `tg://socks`, `https://t.me/proxy`, `https://t.me/socks` link routing
//!   into the "Add Proxy" confirmation box (`AddProxyBoxState`) and confirmation apply flow;
//! - Connection indicator proxy state formatting (`"Connecting to proxy..."`, `"Proxy"`);
//! - `LiveSession` `SettingsBackend` proxy methods + real `check_proxy` against an
//!   in-process loopback fake MTProxy (`dd`) endpoint + `tdata` persistence round-trip;
//! - `ProxyFormDraft` add/edit/link-import validation and `MockSettingsBackend` proxy CRUD.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_core::CoreSettings;
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType, SettingsProxy,
};
use tg_crypto::{CtrState, sha256_bytes};
use tg_data::UserId;
use tg_mtproto::message::encode_unencrypted_packet;
use tg_mtproto::{Client as MtprotoClient, ClientConfig};
use tg_session::{ConnectionState, MtprotoRpc, SessionRuntimeConfig};
use tg_transport::{
    CONSTRUCTOR_REQ_PQ_MULTI, CONSTRUCTOR_RES_PQ, OBFUSCATED_HEADER_SIZE, PaddedIntermediate,
    TAG_PADDED_INTERMEDIATE,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use tg_app::boxes::BoxKind;
use tg_app::boxes::add_proxy::{AddProxyBoxState, apply_confirmed_proxy, route_proxy_link};
use tg_app::live::proxy::{
    format_proxy_connection_status, map_mtproto_connection_state, proxy_indicator_state,
};
use tg_app::live::session::LiveSession;
use tg_app::settings::advanced::ProxyFormDraft;
use tg_app::settings::backend::{ProxyCheckStatus, SettingsBackend};
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::stand_in::backend::Backend;
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::types::ConnectionStatus;

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

fn derive_mtproxy_key(raw: &[u8; 32], secret: &[u8; 16]) -> [u8; 32] {
    let mut buf = [0u8; 48];
    buf[..32].copy_from_slice(raw);
    buf[32..].copy_from_slice(secret);
    sha256_bytes(&buf)
}

/// Spawns a local loopback fake MTProxy (`dd` padded obfuscated2) server answering `req_pq_multi`
/// probes with valid `resPQ` replies so `LiveSession::check_proxy` performs a real RTT check.
async fn spawn_fake_mtproxy_probe_server(secret: [u8; 16]) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut hdr = [0u8; OBFUSCATED_HEADER_SIZE];
                if stream.read_exact(&mut hdr).await.is_err() {
                    return;
                }
                let mut raw_recv_key = [0u8; 32];
                raw_recv_key.copy_from_slice(&hdr[8..40]);
                let mut recv_iv = [0u8; 16];
                recv_iv.copy_from_slice(&hdr[40..56]);
                let recv_key = derive_mtproxy_key(&raw_recv_key, &secret);

                let mut reversed = [0u8; 48];
                reversed.copy_from_slice(&hdr[8..56]);
                reversed.reverse();
                let mut raw_send_key = [0u8; 32];
                raw_send_key.copy_from_slice(&reversed[0..32]);
                let mut send_iv = [0u8; 16];
                send_iv.copy_from_slice(&reversed[32..48]);
                let send_key = derive_mtproxy_key(&raw_send_key, &secret);

                let mut recv_ctr = CtrState::new(recv_key, recv_iv);
                let mut send_ctr = CtrState::new(send_key, send_iv);

                let mut dec_hdr = hdr;
                recv_ctr.decrypt(&mut dec_hdr);
                if dec_hdr[56..60] != TAG_PADDED_INTERMEDIATE {
                    return;
                }

                let mut len_buf = [0u8; 4];
                if stream.read_exact(&mut len_buf).await.is_err() {
                    return;
                }
                recv_ctr.decrypt(&mut len_buf);
                let raw_len = (u32::from_le_bytes(len_buf) & 0x7FFF_FFFF) as usize;
                let mut payload = vec![0u8; raw_len];
                if stream.read_exact(&mut payload).await.is_err() {
                    return;
                }
                recv_ctr.decrypt(&mut payload);
                let clean = PaddedIntermediate::strip_mtproto_padding(&payload);

                if clean.len() >= 40 && u64::from_le_bytes(clean[0..8].try_into().unwrap()) == 0 {
                    let client_msg_id = u64::from_le_bytes(clean[8..16].try_into().unwrap());
                    let body_len = u32::from_le_bytes(clean[16..20].try_into().unwrap()) as usize;
                    if body_len >= 20
                        && 20 + body_len <= clean.len()
                        && u32::from_le_bytes(clean[20..24].try_into().unwrap())
                            == CONSTRUCTOR_REQ_PQ_MULTI
                    {
                        let mut nonce = [0u8; 16];
                        nonce.copy_from_slice(&clean[24..40]);
                        let mut resp_body = Vec::with_capacity(64);
                        resp_body.extend_from_slice(&CONSTRUCTOR_RES_PQ.to_le_bytes());
                        resp_body.extend_from_slice(&nonce);
                        resp_body.extend_from_slice(&[0xCD; 16]);
                        resp_body.push(8);
                        resp_body
                            .extend_from_slice(&[0x17, 0xED, 0x48, 0x94, 0x1A, 0x08, 0xF9, 0x81]);
                        resp_body.extend_from_slice(&[0, 0, 0]);
                        resp_body.extend_from_slice(&0x1CB5_C415u32.to_le_bytes());
                        resp_body.extend_from_slice(&0u32.to_le_bytes());
                        let packet =
                            encode_unencrypted_packet((client_msg_id & !3) | 1, &resp_body);

                        let mut wire = Vec::with_capacity(4 + packet.len());
                        wire.extend_from_slice(&(packet.len() as u32).to_le_bytes());
                        wire.extend_from_slice(&packet);
                        send_ctr.encrypt(&mut wire);
                        let _ = stream.write_all(&wire).await;
                        let _ = stream.flush().await;
                    }
                }
            });
        }
    });
    (port, handle)
}

#[test]
fn proxy_link_routes_all_tdesktop_formats_to_add_proxy_box() {
    let secret = format!("dd{}", "ab".repeat(16));
    let valid_urls = [
        (
            format!("tg://proxy?server=127.0.0.1&port=443&secret={secret}"),
            CoreProxyType::Mtproto,
            "127.0.0.1",
            443u32,
            "",
            secret.as_str(),
        ),
        (
            format!("https://t.me/proxy?server=proxy.example&port=8443&secret={secret}"),
            CoreProxyType::Mtproto,
            "proxy.example",
            8443u32,
            "",
            secret.as_str(),
        ),
        (
            "tg://socks?server=127.0.0.1&port=1080&user=alice&pass=pw123".to_owned(),
            CoreProxyType::Socks5,
            "127.0.0.1",
            1080u32,
            "alice",
            "pw123",
        ),
        (
            "https://t.me/socks?server=10.0.0.2&port=1081&user=bob&pass=xyz".to_owned(),
            CoreProxyType::Socks5,
            "10.0.0.2",
            1081u32,
            "bob",
            "xyz",
        ),
        (
            "https://telegram.me/socks?server=10.0.0.3&port=1082".to_owned(),
            CoreProxyType::Socks5,
            "10.0.0.3",
            1082u32,
            "",
            "",
        ),
        (
            format!("t.me/proxy?server=bare.example&port=443&secret={secret}"),
            CoreProxyType::Mtproto,
            "bare.example",
            443u32,
            "",
            secret.as_str(),
        ),
        (
            "t.me/socks?server=127.0.0.1&port=1085&user=u&pass=p".to_owned(),
            CoreProxyType::Socks5,
            "127.0.0.1",
            1085u32,
            "u",
            "p",
        ),
    ];

    for (url, expected_type, expected_host, expected_port, expected_user, expected_pass) in
        valid_urls
    {
        let boxed =
            route_proxy_link(&url).unwrap_or_else(|| panic!("expected proxy route for {url}"));
        let BoxKind::AddProxy(state) = boxed else {
            panic!("expected BoxKind::AddProxy");
        };
        assert!(
            state.is_valid(),
            "expected valid proxy for {url}: {:?}",
            state.validation_error
        );
        assert_eq!(state.proxy.proxy_type, expected_type);
        assert_eq!(state.proxy.host, expected_host);
        assert_eq!(state.proxy.port, expected_port);
        assert_eq!(state.proxy.user, expected_user);
        assert_eq!(state.proxy.password, expected_pass);
        let modal = state.to_modal("add_proxy_test");
        assert!(
            modal.buttons.iter().any(|b| b.id == "connect"),
            "valid proxy box must expose Connect Proxy button"
        );
    }

    // Invalid MTProto secret opens confirmation box with validation error & Close button only.
    let bad = AddProxyBoxState::from_url("tg://proxy?server=127.0.0.1&port=443&secret=nothex")
        .expect("malformed secret still routes to AddProxyBoxState to show error");
    assert!(!bad.is_valid());
    let bad_modal = bad.to_modal("bad_proxy");
    assert!(bad_modal.buttons.iter().any(|b| b.id == "close"));
    assert!(!bad_modal.buttons.iter().any(|b| b.id == "connect"));

    // Non-proxy links do not route to AddProxy.
    assert!(route_proxy_link("https://t.me/some_channel").is_none());
    assert!(route_proxy_link("tg://settings").is_none());
}

#[test]
fn connection_indicator_formats_proxy_and_direct_states() {
    let strings = EnglishStrings;

    // Direct mode: hidden when connected, standard text when connecting/waiting.
    assert_eq!(
        format_proxy_connection_status(ConnectionStatus::Connected, false, &strings),
        ""
    );
    assert_eq!(
        format_proxy_connection_status(ConnectionStatus::Connecting, false, &strings),
        "Connecting..."
    );
    assert_eq!(
        format_proxy_connection_status(
            ConnectionStatus::Waiting { remaining_ms: 4200 },
            false,
            &strings
        ),
        "Reconnecting in 5s..."
    );

    // Proxy mode: "Connecting to proxy..." while connecting, "Proxy" when connected.
    assert_eq!(
        format_proxy_connection_status(ConnectionStatus::Connecting, true, &strings),
        "Connecting to proxy..."
    );
    assert_eq!(
        format_proxy_connection_status(ConnectionStatus::Connected, true, &strings),
        "Proxy"
    );
    let waiting_proxy = format_proxy_connection_status(
        ConnectionStatus::Waiting { remaining_ms: 3200 },
        true,
        &strings,
    );
    assert!(waiting_proxy.starts_with("Connecting to proxy..."));
    assert!(waiting_proxy.contains("Reconnecting in 4s..."));

    // ProxyIndicatorState combines SettingsProxy + ConnectionStatus.
    let mut sp = SettingsProxy::new();
    let p = CoreProxyData {
        proxy_type: CoreProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: 1080,
        user: String::new(),
        password: String::new(),
    };
    sp.add_to_list(p.clone());
    sp.set_selected(p);
    sp.set_settings(ProxySettingsMode::Enabled);

    let ind_connecting = proxy_indicator_state(ConnectionStatus::Connecting, &sp, &strings);
    assert!(ind_connecting.visible);
    assert!(ind_connecting.proxy_active);
    assert!(ind_connecting.connecting);
    assert_eq!(ind_connecting.label, "Connecting to proxy...");
    assert_eq!(
        ind_connecting.detail.as_deref(),
        Some("SOCKS5  127.0.0.1:1080")
    );

    let ind_connected = proxy_indicator_state(ConnectionStatus::Connected, &sp, &strings);
    assert!(ind_connected.visible);
    assert!(ind_connected.proxy_active);
    assert!(!ind_connected.connecting);
    assert_eq!(ind_connected.label, "Proxy");

    // ConnectionState mapping.
    assert_eq!(
        map_mtproto_connection_state(ConnectionState::Connected),
        ConnectionStatus::Connected
    );
    assert_eq!(
        map_mtproto_connection_state(ConnectionState::Connecting),
        ConnectionStatus::Connecting
    );
    assert_eq!(
        map_mtproto_connection_state(ConnectionState::Waiting {
            retry_after_ms: 2500
        }),
        ConnectionStatus::Waiting { remaining_ms: 2500 }
    );
}

#[test]
fn proxy_form_edit_and_mock_settings_backend_crud() {
    let mut mock = MockSettingsBackend::new();
    let mut core = CoreSettings::new();
    let box_state =
        AddProxyBoxState::from_url("tg://socks?server=127.0.0.1&port=1080&user=u&pass=p")
            .expect("socks link");
    apply_confirmed_proxy(box_state.proxy.clone(), &mut core, &mut mock).expect("apply proxy");

    let saved = mock.proxy_settings().expect("mock returns proxy settings");
    assert_eq!(saved.settings(), ProxySettingsMode::Enabled);
    assert_eq!(saved.selected(), &box_state.proxy);
    assert_eq!(saved.list().len(), 1);

    // Edit round-trip via ProxyFormDraft::from_proxy.
    let mut draft = ProxyFormDraft::from_proxy(&box_state.proxy);
    assert_eq!(draft.host, "127.0.0.1");
    assert_eq!(draft.port, "1080");
    draft.port = "1081".to_owned();
    let updated = draft.build_validated().expect("edited proxy valid");
    core.proxy
        .replace_in_list(&box_state.proxy, updated.clone());
    core.proxy.set_selected(updated.clone());
    mock.apply_proxy_settings(&core.proxy).expect("save edit");

    assert_eq!(mock.proxy_settings().unwrap().selected().port, 1081);

    // Mock check_proxy marks valid proxy available.
    assert_eq!(
        mock.proxy_check_status(&updated),
        ProxyCheckStatus::NotTested
    );
    mock.check_proxy(&updated);
    assert!(matches!(
        mock.proxy_check_status(&updated),
        ProxyCheckStatus::Available { .. }
    ));
}

#[test]
fn live_session_proxy_backend_checks_real_mtproxy_and_persists_tdata() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let secret = [0x5Au8; 16];
    let (mtproxy_port, server_task) = rt.block_on(spawn_fake_mtproxy_probe_server(secret));
    let closed_port = rt.block_on(async {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        l.local_addr().unwrap().port()
    });

    let tdata_dir = TempDir::new().unwrap();
    let cfg = ClientConfig {
        use_ipv6: false,
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = MtprotoClient::new(cfg);
    let rpc = MtprotoRpc::new(client.clone());

    let runtime = Arc::new(
        tg_app::live::start_mtproto_erased_on(
            UserId(42),
            rpc.clone(),
            rt.handle().clone(),
            no_sync_runtime_config(),
        )
        .expect("start SessionRuntime"),
    );

    let mut live = LiveSession::new(Arc::clone(&runtime), 1);
    assert!(live.is_live_proxy_backend());
    live.attach_proxy_tdata_path(tdata_dir.path().to_path_buf())
        .expect("attach fresh tdata");

    // 1. Add and apply MTProxy via AddProxy confirmation helper.
    let dd_hex = format!("dd{}", "5a".repeat(16));
    let url = format!("https://t.me/proxy?server=127.0.0.1&port={mtproxy_port}&secret={dd_hex}");
    let add_box = AddProxyBoxState::from_url(&url).expect("parse t.me/proxy");
    assert!(add_box.is_valid());

    let mut core = CoreSettings::new();
    core.proxy.set_use_proxy_for_calls(true);
    core.proxy.set_try_ipv6(true);
    apply_confirmed_proxy(add_box.proxy.clone(), &mut core, &mut live)
        .expect("apply confirmed proxy to LiveSession");

    // Live MtprotoRpc + Backend::proxy_enabled updated immediately!
    assert!(live.proxy_enabled());
    assert_eq!(rpc.proxy().host, "127.0.0.1");
    assert_eq!(rpc.proxy().port, mtproxy_port);
    assert!(client.try_ipv6());

    // Connection indicator reflects "Connecting to proxy..." while connecting.
    let ind = live.proxy_controller().indicator_state(&EnglishStrings);
    assert_eq!(ind.label, "Connecting to proxy...");

    // 2. Real check_proxy against our local fake MTProxy succeeds with RTT.
    live.check_proxy(&add_box.proxy);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut final_status = ProxyCheckStatus::NotTested;
    while Instant::now() < deadline {
        final_status = live.proxy_check_status(&add_box.proxy);
        if matches!(
            final_status,
            ProxyCheckStatus::Available { .. } | ProxyCheckStatus::Unavailable { .. }
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        matches!(final_status, ProxyCheckStatus::Available { .. }),
        "expected Available status from local fake MTProxy, got {final_status:?}"
    );

    // 3. Real check_proxy against a closed port reports Unavailable.
    let dead = CoreProxyData {
        proxy_type: CoreProxyType::Mtproto,
        host: "127.0.0.1".to_owned(),
        port: u32::from(closed_port),
        user: String::new(),
        password: dd_hex,
    };
    live.proxy_controller()
        .check_proxy_with_timeout(&dead, Duration::from_secs(2));
    let deadline2 = Instant::now() + Duration::from_secs(5);
    let mut dead_status = ProxyCheckStatus::NotTested;
    while Instant::now() < deadline2 {
        dead_status = live.proxy_check_status(&dead);
        if matches!(dead_status, ProxyCheckStatus::Unavailable { .. }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        matches!(dead_status, ProxyCheckStatus::Unavailable { .. }),
        "expected Unavailable for closed port, got {dead_status:?}"
    );

    // 4. Reload persisted tdata into a fresh LiveSession and verify full round-trip.
    let (client2, _updates_rx2) = MtprotoClient::new(ClientConfig::default());
    let rpc2 = MtprotoRpc::new(client2);
    let runtime2 = Arc::new(
        tg_app::live::start_mtproto_erased_on(
            UserId(42),
            rpc2.clone(),
            rt.handle().clone(),
            no_sync_runtime_config(),
        )
        .expect("start second SessionRuntime"),
    );
    let live2 = LiveSession::new(runtime2, 2);
    let reloaded = live2
        .attach_proxy_tdata_path(tdata_dir.path().to_path_buf())
        .expect("reload persisted proxy settings");
    assert_eq!(reloaded.settings(), ProxySettingsMode::Enabled);
    assert_eq!(reloaded.selected(), &add_box.proxy);
    assert!(reloaded.use_proxy_for_calls());
    assert!(reloaded.try_ipv6());
    assert_eq!(rpc2.proxy().port, mtproxy_port);

    server_task.abort();
}

#[test]
fn system_proxy_default_mode_is_not_counted_as_proxy_enabled() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (client, _updates_rx) = tg_mtproto::Client::with_transport(
        ClientConfig::default(),
        Arc::new(tg_mtproto::FakeTransportFactory::new()),
    );
    let rpc = MtprotoRpc::new(client.clone());
    let runtime = Arc::new(
        tg_app::live::start_mtproto_erased_on(
            UserId(99),
            rpc,
            rt.handle().clone(),
            no_sync_runtime_config(),
        )
        .expect("start SessionRuntime"),
    );

    let mut live = LiveSession::new(runtime, 99);
    // Default SettingsProxy starts in ProxySettingsMode::System — matches tdesktop `useProxy`
    // (`Core::App().settings().proxy().isEnabled()`), which is false unless a custom proxy is set.
    assert_eq!(
        live.proxy_controller().proxy_settings().settings(),
        ProxySettingsMode::System
    );
    assert!(
        !live.proxy_controller().is_proxy_enabled(),
        "default System proxy mode must not count as enabled"
    );
    assert!(
        !live.proxy_enabled(),
        "Backend::proxy_enabled() must be false in System proxy mode"
    );

    let strings = EnglishStrings;
    let default_ind =
        proxy_indicator_state(ConnectionStatus::Connected, &SettingsProxy::new(), &strings);
    assert!(
        !default_ind.proxy_active,
        "System proxy mode must not mark proxy_active"
    );
    assert!(
        !default_ind.visible,
        "connected direct/system-proxy session must hide the proxy pill"
    );

    // Enabling a custom proxy makes is_proxy_enabled() true.
    let custom = CoreProxyData {
        proxy_type: CoreProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: 1080,
        user: String::new(),
        password: String::new(),
    };
    let mut sp = SettingsProxy::new();
    sp.add_to_list(custom.clone());
    sp.set_selected(custom);
    sp.set_settings(ProxySettingsMode::Enabled);
    live.apply_proxy_settings(&sp).expect("enable custom proxy");
    assert!(live.proxy_controller().is_proxy_enabled());
    assert!(live.proxy_enabled());

    // Switching back to System proxy disables the custom proxy indicator again.
    sp.set_settings(ProxySettingsMode::System);
    live.apply_proxy_settings(&sp).expect("switch to system");
    assert!(!live.proxy_controller().is_proxy_enabled());
    assert!(!live.proxy_enabled());

    // Switching to Disabled also keeps proxy_enabled false.
    sp.set_settings(ProxySettingsMode::Disabled);
    live.apply_proxy_settings(&sp).expect("disable proxy");
    assert!(!live.proxy_controller().is_proxy_enabled());
    assert!(!live.proxy_enabled());

    drop(live);
    client.shutdown();
}
