//! Wave 7 (TGB-3075): waiters failed when the session stops.

use std::sync::Arc;
use std::time::Duration;

use tg_mtproto::auth_key::{AuthKeyType, Dcenter, ManagedAuthKey};
use tg_mtproto::dc_options::{DcOptions, Environment};
use tg_mtproto::message::next_request_id;
use tg_mtproto::session::{QueuedRequest, SessionHandle};
use tg_mtproto::{ClientConfig, FakeTransportFactory, InvocationError, MtprotoClock};

fn managed_key(seed: u8) -> ManagedAuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(13).wrapping_add(seed);
    }
    ManagedAuthKey::new(tg_mtproto::AuthKey::new(data), AuthKeyType::ReadFromFile, 2)
}

/// A ping waiting for its pong when the session shuts down used to get
/// `Protocol("request dropped before completion")` while RPC waiters got `Dropped`; both now
/// get `Dropped`.
#[tokio::test]
async fn shutdown_fails_pending_rpc_and_ping_with_dropped() {
    let dcenter = Dcenter::new(2, Some(managed_key(5)));
    let factory = Arc::new(FakeTransportFactory::new());
    let mut peer = factory.push_connection().await;
    let (updates_tx, _updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let config = ClientConfig {
        use_pfs: false,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let session = SessionHandle::spawn(
        2,
        config,
        DcOptions::new(Environment::Production),
        MtprotoClock::new(),
        dcenter,
        factory.clone(),
        updates_tx,
    );

    let ping = {
        let session = session.clone();
        tokio::spawn(async move { session.ping().await })
    };
    let rpc = {
        let session = session.clone();
        tokio::spawn(async move {
            let mut body = 0x1234_5678u32.to_le_bytes().to_vec();
            body.extend_from_slice(&[0u8; 4]);
            let mut req = QueuedRequest::new(next_request_id(), body);
            req.needs_layer = false;
            session.send_raw(req).await
        })
    };
    // The server reads the first packet and never answers; give the session time to queue
    // the other request too (ping and request may share one container).
    let packet = tokio::time::timeout(Duration::from_secs(5), peer.rx.recv())
        .await
        .expect("session sent its requests")
        .expect("connection open");
    assert!(!packet.is_empty());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!ping.is_finished() && !rpc.is_finished());

    session.shutdown();
    let ping = tokio::time::timeout(Duration::from_secs(2), ping)
        .await
        .expect("ping finished after shutdown")
        .expect("ping task");
    let rpc = tokio::time::timeout(Duration::from_secs(2), rpc)
        .await
        .expect("rpc finished after shutdown")
        .expect("rpc task");
    assert!(
        matches!(ping, Err(InvocationError::Dropped)),
        "ping: {ping:?}"
    );
    assert!(matches!(rpc, Err(InvocationError::Dropped)), "rpc: {rpc:?}");
}
