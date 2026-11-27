//! Wave 8 (TGB-2907): the auth transfer to a guest DC is shared by concurrent requests and must
//! never leave one of them waiting forever.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tg_mtproto::crypto::{AuthKey, Side};
use tg_mtproto::message::{DecryptedEnvelope, build_encrypted_packet, parse_encrypted_packet};
use tg_mtproto::tl::mtproto::{
    CoreMessage, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_mtproto::tl::{
    Boxed, Deserializable, Identifiable, RawObject, Serializable, enums as tl_enums,
    functions as tl_functions, types as tl_types,
};
use tg_mtproto::{Client, ClientConfig, FakeTransportFactory};
use tokio::sync::mpsc;

fn test_auth_key(seed: u8) -> AuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(31));
    }
    AuthKey::new(data)
}

fn boxed_rpc_result(req_msg_id: i64, result: Vec<u8>) -> Vec<u8> {
    Boxed(RpcResult {
        req_msg_id,
        result: RawObject(result),
    })
    .to_bytes()
}

fn unwrap_inner_query(body: &[u8]) -> Vec<u8> {
    if let Ok(wrapped) =
        tl_functions::InvokeWithLayer::<tl_functions::InitConnection<RawObject>>::from_bytes(body)
    {
        return wrapped.query.query.0;
    }
    body.to_vec()
}

fn unpack_or_single(env: DecryptedEnvelope) -> Vec<CoreMessage> {
    if env.body.get(0..4) == Some(&MsgContainer::CONSTRUCTOR_ID.to_le_bytes()[..]) {
        tg_mtproto::message::unpack_container(&env.body).unwrap()
    } else {
        vec![CoreMessage {
            msg_id: env.msg_id as i64,
            seqno: env.seq_no as i32,
            body: RawObject(env.body),
        }]
    }
}

/// Serves `peer` with `key`, answering pings and whatever `reply` returns for other requests.
fn serve(
    mut peer: tg_mtproto::FakeServerPeer,
    key: AuthKey,
    salt_seed: u64,
    mut reply: impl FnMut(&[u8]) -> Option<Vec<u8>> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let now_secs = tg_mtproto::time::local_unix_secs() as u64;
        let mut counter = 1u64;
        while let Some(raw) = peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let body = if let Ok(ping) = mtp_functions::Ping::from_bytes(&inner) {
                    mtp_enums::Pong::Pong(mtp_types::Pong {
                        msg_id: m.msg_id,
                        ping_id: ping.ping_id,
                    })
                    .to_bytes()
                } else if let Some(result) = reply(&inner) {
                    boxed_rpc_result(m.msg_id, result)
                } else {
                    continue;
                };
                let server_msg_id = (now_secs << 32) | ((counter << 2) | 1);
                counter += 1;
                let packet = build_encrypted_packet(
                    salt_seed,
                    session_id,
                    server_msg_id,
                    0,
                    &body,
                    &key,
                    Side::Server,
                );
                let _ = peer.tx.send(Ok(packet));
            }
        }
    })
}

/// The request that started the transfer is cancelled while `auth.exportAuthorization` is in
/// flight. The old code kept its entry in the in-flight map with nobody to finish it, so the
/// request already waiting on it and every later request for that DC hung forever.
#[tokio::test]
async fn cancelled_leader_hands_the_transfer_to_a_waiting_request() {
    let factory = Arc::new(FakeTransportFactory::new());
    let dc2_peer = factory.push_connection().await;
    let dc4_peer = factory.push_connection().await;
    let config = ClientConfig {
        api_id: 4242,
        use_pfs: false,
        main_dc_id: 2,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::with_transport(config, factory.clone());
    let key2 = test_auth_key(0x51);
    let key4 = test_auth_key(0x54);
    client.import_persistent_keys(vec![(2, key2.clone()), (4, key4.clone())]);

    let exports = Arc::new(AtomicUsize::new(0));
    let (export_seen_tx, mut export_seen_rx) = mpsc::unbounded_channel();
    let dc2 = {
        let exports = Arc::clone(&exports);
        serve(dc2_peer, key2, 0x2002, move |inner| {
            tl_functions::auth::ExportAuthorization::from_bytes(inner).ok()?;
            let count = exports.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = export_seen_tx.send(count);
            // The first export never gets an answer: its request is cancelled meanwhile.
            (count > 1).then(|| {
                tl_enums::auth::ExportedAuthorization::ExportedAuthorization(
                    tl_types::auth::ExportedAuthorization {
                        id: 77,
                        bytes: b"exported".to_vec(),
                    },
                )
                .to_bytes()
            })
        })
    };
    let dc4 = serve(dc4_peer, key4, 0x4004, |inner| {
        tl_functions::auth::ImportAuthorization::from_bytes(inner).ok()?;
        Some(
            tl_enums::auth::Authorization::SignUpRequired(
                tl_types::auth::AuthorizationSignUpRequired {
                    terms_of_service: None,
                },
            )
            .to_bytes(),
        )
    });

    let leader = {
        let client = client.clone();
        tokio::spawn(async move { client.ensure_dc_authorized(4).await })
    };
    let first = tokio::time::timeout(Duration::from_secs(5), export_seen_rx.recv()).await;
    assert_eq!(first.ok().flatten(), Some(1), "first export reached DC 2");

    let waiter = {
        let client = client.clone();
        tokio::spawn(async move { client.ensure_dc_authorized(4).await })
    };
    tokio::task::yield_now().await;
    leader.abort();

    let waited = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the waiting request must not hang after the leader was cancelled");
    assert!(matches!(waited, Ok(Ok(()))), "{waited:?}");
    assert_eq!(exports.load(Ordering::SeqCst), 2);

    let later = tokio::time::timeout(Duration::from_secs(5), client.ensure_dc_authorized(4))
        .await
        .expect("an authorized DC answers at once");
    assert!(later.is_ok());
    assert_eq!(exports.load(Ordering::SeqCst), 2, "no third export");

    client.shutdown();
    dc2.abort();
    dc4.abort();
}
