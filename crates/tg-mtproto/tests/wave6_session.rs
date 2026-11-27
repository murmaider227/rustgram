//! Wave 6 (TGB-3059) regression tests for the session send path.

use std::sync::Arc;
use std::time::Duration;

use tg_mtproto::auth_key::{Dcenter, ManagedAuthKey};
use tg_mtproto::dc_options::{DcOptions, DcType, Environment};
use tg_mtproto::message::{is_file_part_upload, next_request_id};
use tg_mtproto::session::{QueuedRequest, SessionHandle, SessionState};
use tg_mtproto::tl::mtproto::GzipPacked;
use tg_mtproto::tl::{Identifiable, Serializable, functions as tl_functions};
use tg_mtproto::{ClientConfig, FakeTransportFactory, MtprotoClock};

fn connected_state() -> SessionState {
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        MtprotoClock::new(),
        dcenter,
        0x1111,
    )
}

fn sent_body(request_body: Vec<u8>) -> Vec<u8> {
    let mut state = connected_state();
    let mut req = QueuedRequest::new(next_request_id(), request_body);
    req.needs_layer = false;
    state.enqueue_request(req);
    let out = state.build_next_outbound().expect("one outbound message");
    out.body
}

fn ctor(body: &[u8]) -> u32 {
    u32::from_le_bytes([body[0], body[1], body[2], body[3]])
}

#[test]
fn file_part_uploads_are_sent_without_gzip() {
    // Highly compressible bytes: the old send path wrapped these parts in gzip_packed.
    let small = tl_functions::upload::SaveFilePart {
        file_id: 7,
        file_part: 3,
        bytes: vec![0u8; 64 * 1024],
    }
    .to_bytes();
    assert!(is_file_part_upload(&small));
    assert_eq!(sent_body(small.clone()), small);

    let big = tl_functions::upload::SaveBigFilePart {
        file_id: 7,
        file_part: 3,
        file_total_parts: 40,
        bytes: vec![0u8; 64 * 1024],
    }
    .to_bytes();
    assert!(is_file_part_upload(&big));
    assert_eq!(sent_body(big.clone()), big);
}

#[test]
fn other_large_requests_are_still_gzip_packed() {
    let mut body = 0x1234_5678u32.to_le_bytes().to_vec();
    body.extend(std::iter::repeat_n(0u8, 4096));
    assert!(!is_file_part_upload(&body));
    let sent = sent_body(body);
    assert_eq!(ctor(&sent), GzipPacked::CONSTRUCTOR_ID);
}

#[test]
fn file_part_detection_needs_a_full_constructor() {
    assert!(!is_file_part_upload(&[]));
    let ctor = tl_functions::upload::SaveFilePart::CONSTRUCTOR_ID.to_le_bytes();
    assert!(!is_file_part_upload(&ctor[..3]));
    assert!(is_file_part_upload(&ctor));
}

fn managed_key(seed: u8, key_type: tg_mtproto::auth_key::AuthKeyType) -> ManagedAuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(17).wrapping_add(seed);
    }
    ManagedAuthKey::new(tg_mtproto::AuthKey::new(data), key_type, 2)
}

/// A session whose DC key is being created by another session waits for that key on its open
/// connection. The old code slept 50 ms, dropped the connection and reconnected with growing
/// backoff (1, 2, 3 ms, then 1 s, 2 s, ...), so it came up seconds late on a new connection.
#[tokio::test]
async fn session_waits_for_concurrent_key_creation_on_the_same_connection() {
    use tg_mtproto::auth_key::{AuthKeyType, CreatingKeyType};

    let perm = managed_key(1, AuthKeyType::ReadFromFile);
    let temp = managed_key(2, AuthKeyType::Temporary);
    let dcenter = Dcenter::new(2, Some(perm.clone()));
    // Another session on DC 2 holds the key-creation slot.
    let creating = dcenter.acquire_key_creation(DcType::Regular);
    assert_eq!(creating, CreatingKeyType::TemporaryRegular);

    let factory = Arc::new(FakeTransportFactory::new());
    let mut peer = factory.push_connection().await;
    let (updates_tx, _updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let config = ClientConfig {
        use_pfs: true,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let session = SessionHandle::spawn(
        2,
        config,
        DcOptions::new(Environment::Production),
        MtprotoClock::new(),
        dcenter.clone(),
        factory.clone(),
        updates_tx,
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(dcenter.release_key_creation_on_done(creating, temp.clone(), Some(perm)));

    let first_packet = tokio::time::timeout(Duration::from_secs(5), peer.rx.recv())
        .await
        .expect("session sent nothing within 5 s of the key becoming ready")
        .expect("session closed its first connection instead of waiting");
    assert_eq!(first_packet[..8], temp.key.key_id());
    assert_eq!(factory.connect_calls(), 1);
    session.shutdown();
}

/// A shutdown while waiting for another session's key creation is handled at once.
#[tokio::test]
async fn shutdown_while_waiting_for_key_creation_is_prompt() {
    use tg_mtproto::auth_key::AuthKeyType;

    let dcenter = Dcenter::new(2, Some(managed_key(1, AuthKeyType::ReadFromFile)));
    let _creating = dcenter.acquire_key_creation(DcType::Regular);
    let factory = Arc::new(FakeTransportFactory::new());
    let mut peer = factory.push_connection().await;
    let (updates_tx, _updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let session = SessionHandle::spawn(
        2,
        ClientConfig::default(),
        DcOptions::new(Environment::Production),
        MtprotoClock::new(),
        dcenter,
        factory.clone(),
        updates_tx,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let pending = {
        let session = session.clone();
        tokio::spawn(async move { session.ping().await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    session.shutdown();
    let ping = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .expect("the waiting session did not handle the shutdown")
        .expect("ping task");
    assert!(ping.is_err());
    // The connection was closed without any traffic.
    assert!(peer.rx.recv().await.is_none());
    assert_eq!(factory.connect_calls(), 1);
}

fn ping_ids_in(out: &tg_mtproto::session::OutboundMessage) -> Vec<i64> {
    use tg_mtproto::message::unpack_container;
    use tg_mtproto::tl::mtproto::{MsgContainer, functions as mtp_functions};
    use tg_mtproto::tl::{Deserializable, RawObject};
    let bodies: Vec<RawObject> = if ctor(&out.body) == MsgContainer::CONSTRUCTOR_ID {
        unpack_container(&out.body)
            .expect("container")
            .into_iter()
            .map(|m| m.body)
            .collect()
    } else {
        vec![RawObject(out.body.clone())]
    };
    bodies
        .iter()
        .filter_map(|body| match ctor(&body.0) {
            id if id == mtp_functions::Ping::CONSTRUCTOR_ID => {
                mtp_functions::Ping::from_bytes(&body.0)
                    .ok()
                    .map(|p| p.ping_id)
            }
            id if id == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID => {
                mtp_functions::PingDelayDisconnect::from_bytes(&body.0)
                    .ok()
                    .map(|p| p.ping_id)
            }
            _ => None,
        })
        .collect()
}

/// A ping in flight when the MTProto session resets (bad_msg_notification 32/33, -404, key
/// change) is sent again in the new session. Before, its id was dropped: the pong never came,
/// `SessionHandle::ping` never returned and tg-session's idle keepalive stayed "in flight"
/// for the rest of the session.
#[test]
fn ping_in_flight_is_resent_after_session_reset() {
    let mut state = connected_state();
    state.schedule_ping(42);
    let first = state.build_next_outbound().expect("ping packet");
    assert_eq!(ping_ids_in(&first), vec![42]);

    state.reset_session();
    state.on_transport_connected();
    let after_reset = state.build_next_outbound().expect("packet after reset");
    assert_eq!(ping_ids_in(&after_reset), vec![42]);
}

/// Every completed request used to `mem::forget` its cancel guard, leaking a clone of the
/// session's command sender: the command channel never closed, so a session whose handles
/// were all dropped kept running (task, connection and buffers) forever.
#[tokio::test]
async fn dropping_the_last_handle_stops_the_session_after_requests_complete() {
    use tg_mtproto::auth_key::AuthKeyType;
    use tg_mtproto::crypto::Side;
    use tg_mtproto::message::{build_encrypted_packet, parse_encrypted_packet, unpack_container};
    use tg_mtproto::tl::mtproto::{
        MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions, types as mtp_types,
    };
    use tg_mtproto::tl::{BOOL_TRUE_ID, Boxed, Deserializable, RawObject};

    let perm = managed_key(3, AuthKeyType::ReadFromFile);
    let key = perm.key.clone();
    let dcenter = Dcenter::new(2, Some(perm));
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

    let server = tokio::spawn(async move {
        let now = tg_mtproto::time::local_unix_secs() as u64;
        let mut counter = 1u64;
        while let Some(raw) = peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key, Side::Client).expect("server decrypt");
            let messages = if ctor(&env.body) == MsgContainer::CONSTRUCTOR_ID {
                unpack_container(&env.body).expect("container")
            } else {
                vec![tg_mtproto::tl::mtproto::CoreMessage {
                    msg_id: env.msg_id as i64,
                    seqno: env.seq_no as i32,
                    body: RawObject(env.body.clone()),
                }]
            };
            for m in messages {
                let reply = match ctor(&m.body.0) {
                    0x1234_5678 => Boxed(RpcResult {
                        req_msg_id: m.msg_id,
                        result: RawObject(BOOL_TRUE_ID.to_le_bytes().to_vec()),
                    })
                    .to_bytes(),
                    id if id == mtp_functions::Ping::CONSTRUCTOR_ID => {
                        let ping = mtp_functions::Ping::from_bytes(&m.body.0).expect("ping");
                        mtp_enums::Pong::Pong(mtp_types::Pong {
                            msg_id: m.msg_id,
                            ping_id: ping.ping_id,
                        })
                        .to_bytes()
                    }
                    _ => continue,
                };
                let msg_id = (now << 32) | (counter << 2) | 1;
                counter += 1;
                let pkt = build_encrypted_packet(
                    0x55,
                    env.session_id,
                    msg_id,
                    1,
                    &reply,
                    &key,
                    Side::Server,
                );
                let _ = peer.tx.send(Ok(pkt));
            }
        }
    });

    let mut body = 0x1234_5678u32.to_le_bytes().to_vec();
    body.extend_from_slice(&[0u8; 4]);
    let mut req = QueuedRequest::new(next_request_id(), body);
    req.needs_layer = false;
    let reply = tokio::time::timeout(Duration::from_secs(5), session.send_raw(req))
        .await
        .expect("reply within 5 s")
        .expect("rpc reply");
    assert_eq!(reply, BOOL_TRUE_ID.to_le_bytes());

    drop(session);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("session kept its connection open after the last handle was dropped")
        .expect("server task");
}
