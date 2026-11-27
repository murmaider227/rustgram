//! Wave 9-U (TGB-3153): switching the app language re-sends `initConnection` with the new
//! language on the main DC, like `Lang::CloudManager::changeIdAndReInitConnection`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_mtproto::crypto::{AuthKey, Side};
use tg_mtproto::message::{build_encrypted_packet, parse_encrypted_packet, unpack_container};
use tg_mtproto::tl::mtproto::{CoreMessage, MsgContainer, RpcResult, enums as mtp_enums};
use tg_mtproto::tl::mtproto::{functions as mtp_functions, types as mtp_types};
use tg_mtproto::tl::{
    Boxed, Cursor, Deserializable, DeserializeError, Identifiable, RawObject, RemoteCall,
    Serializable, functions as tl_functions,
};
use tg_mtproto::transport_adapter::FakeServerPeer;
use tg_mtproto::{Client, ClientConfig, FakeTransportFactory, InitLang};

const DUMMY_CALL: u32 = 0x1234_5678;
const DUMMY_REPLY: u32 = 0x8765_4321;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DummyCall {
    tag: u32,
}

impl Serializable for DummyCall {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&DUMMY_CALL.to_le_bytes());
        buf.extend_from_slice(&self.tag.to_le_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DummyReply {
    value: u32,
}

impl Deserializable for DummyReply {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let ctor = buf.read_u32()?;
        if ctor != DUMMY_REPLY {
            return Err(DeserializeError::UnexpectedConstructor { id: ctor });
        }
        Ok(Self {
            value: buf.read_u32()?,
        })
    }
}

impl RemoteCall for DummyCall {
    type Return = DummyReply;
}

fn key_data() -> AuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37).wrapping_add(11);
    }
    AuthKey::new(data)
}

fn ctor(body: &[u8]) -> u32 {
    body.get(0..4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

/// `(connection, tag, lang_code of the initConnection wrapper if any)` per request seen.
type Seen = Arc<Mutex<Vec<(usize, u32, Option<String>)>>>;

async fn serve(index: usize, mut peer: FakeServerPeer, key: AuthKey, seen: Seen) {
    let mut counter = (index as u64 + 1) * 1000;
    while let Some(raw) = peer.rx.recv().await {
        let Ok(env) = parse_encrypted_packet(&raw, &key, Side::Client) else {
            continue;
        };
        let session_id = env.session_id;
        let msgs = if ctor(&env.body) == MsgContainer::CONSTRUCTOR_ID {
            unpack_container(&env.body).unwrap_or_default()
        } else {
            vec![CoreMessage {
                msg_id: env.msg_id as i64,
                seqno: env.seq_no as i32,
                body: RawObject(env.body),
            }]
        };
        for m in msgs {
            let (inner, lang) = match tl_functions::InvokeWithLayer::<
                tl_functions::InitConnection<RawObject>,
            >::from_bytes(&m.body.0)
            {
                Ok(wrapped) => (wrapped.query.query.0, Some(wrapped.query.lang_code)),
                Err(_) => (m.body.0.clone(), None),
            };
            counter += 1;
            let secs = tg_mtproto::time::local_unix_secs() as u64;
            let msg_id = (secs << 32) | (counter << 2) | 1;
            let reply = if ctor(&inner) == mtp_functions::Ping::CONSTRUCTOR_ID
                || ctor(&inner) == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID
            {
                let ping_id = mtp_functions::Ping::from_bytes(&inner)
                    .map(|p| p.ping_id)
                    .or_else(|_| {
                        mtp_functions::PingDelayDisconnect::from_bytes(&inner).map(|p| p.ping_id)
                    })
                    .unwrap_or(0);
                mtp_enums::Pong::Pong(mtp_types::Pong {
                    msg_id: m.msg_id,
                    ping_id,
                })
                .to_bytes()
            } else if ctor(&inner) == DUMMY_CALL {
                let tag = u32::from_le_bytes(inner[4..8].try_into().unwrap_or([0; 4]));
                seen.lock().expect("seen").push((index, tag, lang));
                let mut result = DUMMY_REPLY.to_le_bytes().to_vec();
                result.extend_from_slice(&tag.to_le_bytes());
                Boxed(RpcResult {
                    req_msg_id: m.msg_id,
                    result: RawObject(result),
                })
                .to_bytes()
            } else if m.seqno & 1 == 1 {
                mtp_enums::MsgsAck::MsgsAck(mtp_types::MsgsAck {
                    msg_ids: vec![m.msg_id],
                })
                .to_bytes()
            } else {
                continue;
            };
            let packet = build_encrypted_packet(
                0x5555,
                session_id,
                msg_id,
                (counter as u32) * 2 + 1,
                &reply,
                &key,
                Side::Server,
            );
            let _ = peer.tx.send(Ok(packet));
        }
    }
}

fn lang(code: &str) -> InitLang {
    InitLang {
        system_lang_code: "en".to_owned(),
        lang_pack: "tdesktop".to_owned(),
        lang_code: code.to_owned(),
    }
}

#[tokio::test(start_paused = true)]
async fn language_switch_resends_init_connection_with_the_new_code() {
    let factory = Arc::new(FakeTransportFactory::new());
    let seen: Seen = Arc::default();
    let key = key_data();
    for index in 0..2 {
        let peer = factory.push_connection().await;
        tokio::spawn(serve(index, peer, key.clone(), Arc::clone(&seen)));
    }
    let config = ClientConfig {
        api_id: 12345,
        use_pfs: false,
        ping_interval: Duration::from_secs(300),
        main_dc_id: 2,
        lang_code: "en".to_owned(),
        ..ClientConfig::default()
    };
    let (client, _updates) = Client::with_transport(config, factory.clone());
    client.import_persistent_keys(vec![(2, key)]);

    let call = |tag| {
        let client = client.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(30), client.invoke(&DummyCall { tag }))
                .await
                .expect("answered")
                .expect("rpc ok")
        }
    };
    assert_eq!(call(1).await.value, 1);
    assert_eq!(call(2).await.value, 2);

    client.set_init_lang(lang("uk"));
    assert_eq!(client.init_lang().lang_code, "uk");
    assert_eq!(call(3).await.value, 3);
    // Setting the same language again does not re-initialize.
    client.set_init_lang(lang("uk"));
    assert_eq!(call(4).await.value, 4);

    let seen = seen.lock().expect("seen").clone();
    assert_eq!(
        seen,
        [
            (0, 1, Some("en".to_owned())),
            (0, 2, None),
            (1, 3, Some("uk".to_owned())),
            (1, 4, None),
        ],
        "requests seen by the server"
    );
    assert_eq!(factory.connect_calls(), 2);
    client.shutdown();
}
