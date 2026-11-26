//! Conformance tests for unified update-state tracking in `SessionRuntime` / `SessionActor`.
//!
//! Verifies that forum-topic replies (`messages.getForumTopics` / `ChannelData::ptsReceived`),
//! `messages.affectedHistory` pages (`messages.deleteTopicHistory`, `messages.deleteSavedHistory`
//! / `ApiWrap::applyAffectedHistory`), `ChannelMessages.pts` from search, and server-pushed
//! MTProto 2.0 updates all feed the single `UpdatesHandler` owned by `SessionActor`, triggering
//! `updates.getChannelDifference` / `updates.getDifference` once when — and ONLY when — a real
//! PTS gap exists.
//!
//! Reference: `api/api_updates.cpp`, `data/data_channel.h` (`ptsInit`, `ptsReceived`,
//! `ptsUpdateAndApply`), `data/data_pts_waiter.cpp`, `apiwrap.cpp` (`applyAffectedHistory`),
//! `data/data_forum.cpp` (`applyReceivedTopics`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{ApiCredentials, RpcClient};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    Channel, ChannelFlags, ChannelId, FullMsgId, LoadedStatus, MsgId, PeerId, PeerUpdateFlags,
    User, UserId,
};
use tg_mtproto::dc_options::DcId;
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, PacketConnection, TransportFactory,
};
use tg_session::{MtprotoRpc, SessionRuntime, SessionRuntimeConfig, build_mtproto_client_config};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, RawObject, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::mpsc;

const FAKE_API_ID: i32 = 1;
const FAKE_API_HASH: &str = "00000000000000000000000000000000";
const TEST_TIMEOUT: Duration = Duration::from_secs(20);

const TEST_RSA_N_HEX: &str = "\
9000000000000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000000\
000c000000000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000535\
8000000000000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000000\
40d0000000000000000000000000000000000000000000000000000000000000\
00000000000000000000000000000000000000000000000000000000000bb9a1";

const TEST_RSA_D_HEX: &str = "\
6a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec\
0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec\
0a1ccceb3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb\
3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb3314d0c0\
6bb794486bb794486bb794486bb794486bb794486bb794486bb794486bb79448\
6bb794486bb794486bb794486bb794486bb794486bb794486bb794486bb79448\
9b6a5515aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515\
aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515aaf2f321";

fn hex_decode(hex: &str) -> Vec<u8> {
    let bytes = hex.as_bytes();
    assert_eq!(bytes.len() % 2, 0);
    (0..bytes.len())
        .step_by(2)
        .map(|i| {
            let hi = (bytes[i] as char).to_digit(16).expect("hex hi");
            let lo = (bytes[i + 1] as char).to_digit(16).expect("hex lo");
            ((hi << 4) | lo) as u8
        })
        .collect()
}

fn test_rsa_public_key() -> PublicKey {
    PublicKey::from_components(&hex_decode(TEST_RSA_N_HEX), &[0x01, 0x00, 0x01])
        .expect("valid test RSA public key")
}

fn pad_be_256(raw: &[u8]) -> [u8; 256] {
    let mut out = [0u8; 256];
    let len = raw.len().min(256);
    out[256 - len..].copy_from_slice(&raw[raw.len() - len..]);
    out
}

fn now_unix_secs() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i32
}

// ============================================================================
// Scripted Fake MTProto 2.0 Server
// ============================================================================

#[derive(Debug, Clone)]
struct RpcRecord {
    ctor: u32,
}

#[derive(Debug, Clone)]
struct IncomingRpc {
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
enum ServerAction {
    Reply(Vec<u8>),
}

impl ServerAction {
    fn ok<T: Serializable>(val: &T) -> Self {
        Self::Reply(val.to_bytes())
    }
}

type RpcHandler = Arc<dyn Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync>;

#[derive(Debug, Clone)]
struct ServerKeyEntry {
    key: AuthKey,
    server_salt: u64,
}

struct LiveConnPush {
    dc_id: DcId,
    tx: mpsc::UnboundedSender<tl_enums::Updates>,
}

struct ScriptedServerInner {
    rsa_pub: PublicKey,
    rsa_n: Vec<u8>,
    rsa_d: Vec<u8>,
    next_conn_id: AtomicU64,
    dh_counter: AtomicU64,
    keys: Mutex<HashMap<u64, ServerKeyEntry>>,
    rpcs: Mutex<Vec<RpcRecord>>,
    live_pushers: Mutex<HashMap<u64, LiveConnPush>>,
    handler: Mutex<RpcHandler>,
}

#[derive(Clone)]
struct ScriptedServer {
    inner: Arc<ScriptedServerInner>,
}

impl ScriptedServer {
    fn new<F>(handler: F) -> Self
    where
        F: Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(ScriptedServerInner {
                rsa_pub: test_rsa_public_key(),
                rsa_n: hex_decode(TEST_RSA_N_HEX),
                rsa_d: hex_decode(TEST_RSA_D_HEX),
                next_conn_id: AtomicU64::new(1),
                dh_counter: AtomicU64::new(1),
                keys: Mutex::new(HashMap::new()),
                rpcs: Mutex::new(Vec::new()),
                live_pushers: Mutex::new(HashMap::new()),
                handler: Mutex::new(Arc::new(handler)),
            }),
        }
    }

    fn register_rsa_key_on(&self, client: &Client) {
        client
            .dc_options()
            .add_public_key(self.inner.rsa_pub.clone());
    }

    fn rpcs(&self) -> Vec<RpcRecord> {
        self.inner.rpcs.lock().unwrap().clone()
    }

    fn rpcs_of<F: Identifiable>(&self) -> Vec<RpcRecord> {
        self.rpcs()
            .into_iter()
            .filter(|r| r.ctor == F::CONSTRUCTOR_ID)
            .collect()
    }

    fn push_update_to_dc(&self, dc_id: DcId, updates: tl_enums::Updates) -> bool {
        let guard = self.inner.live_pushers.lock().unwrap();
        if let Some((_, pusher)) = guard
            .iter()
            .filter(|(_, p)| p.dc_id == dc_id)
            .max_by_key(|&(&cid, _)| cid)
        {
            pusher.tx.send(updates).is_ok()
        } else {
            false
        }
    }

    fn decrypt_rsa_pad(&self, encrypted_data: &[u8]) -> Vec<u8> {
        let dec_raw = mod_exp(encrypted_data, &self.inner.rsa_d, &self.inner.rsa_n)
            .expect("RSA private modexp");
        let block = pad_be_256(&dec_raw);
        let temp_key_xor = &block[0..32];
        let aes_encrypted = &block[32..256];
        let aes_hash = sha256(&[aes_encrypted]);
        let mut temp_key = [0u8; 32];
        for i in 0..32 {
            temp_key[i] = temp_key_xor[i] ^ aes_hash[i];
        }
        let data_with_hash = aes_ige_decrypt(aes_encrypted, &temp_key, &[0u8; 32]);
        let mut data_with_padding = data_with_hash[0..192].to_vec();
        data_with_padding.reverse();
        data_with_padding
    }

    fn spawn_connection(&self, conn_id: u64, protocol_dc_id: i16, mut peer: FakeServerPeer) {
        let server = self.clone();
        let bare_dc = protocol_dc_id.unsigned_abs() as i32;
        let dc_id = if bare_dc >= 10_000 {
            bare_dc - 10_000
        } else {
            bare_dc
        };
        let (push_tx, mut push_rx) = mpsc::unbounded_channel::<tl_enums::Updates>();
        server
            .inner
            .live_pushers
            .lock()
            .unwrap()
            .insert(conn_id, LiveConnPush { dc_id, tx: push_tx });

        tokio::spawn(async move {
            struct DhPending {
                nonce: [u8; 16],
                server_nonce: [u8; 16],
                new_nonce: [u8; 32],
                a_srv: [u8; 256],
                aes_key: [u8; 32],
                aes_iv: [u8; 32],
            }

            let mut dh_pending: Option<DhPending> = None;
            let mut active_key: Option<(AuthKey, u64, u64)> = None;
            let mut server_msg_counter: u64 = 1;
            let mut server_seq_counter: u32 = 0;

            let next_server_msg_id = |counter: &mut u64, is_reply: bool| -> u64 {
                let secs = now_unix_secs().max(1_700_000_000) as u64;
                let low_bits = if is_reply { 1 } else { 3 };
                let id = (secs << 32) | ((*counter << 2) | low_bits);
                *counter += 1;
                id
            };

            let send_encrypted = |peer: &FakeServerPeer,
                                  key: &AuthKey,
                                  salt: u64,
                                  session_id: u64,
                                  msg_id: u64,
                                  content_related: bool,
                                  seq_counter: &mut u32,
                                  body: &[u8]| {
                let seq_no = if content_related {
                    let s = *seq_counter * 2 + 1;
                    *seq_counter += 1;
                    s
                } else {
                    *seq_counter * 2
                };
                let pkt = build_encrypted_packet(
                    salt,
                    session_id,
                    msg_id,
                    seq_no,
                    body,
                    key,
                    Side::Server,
                );
                let _ = peer.tx.send(Ok(pkt));
            };

            loop {
                tokio::select! {
                    Some(pushed) = push_rx.recv() => {
                        if let Some((ref key, salt, session_id)) = active_key {
                            let u_id = next_server_msg_id(&mut server_msg_counter, false);
                            send_encrypted(
                                &peer,
                                key,
                                salt,
                                session_id,
                                u_id,
                                true,
                                &mut server_seq_counter,
                                &pushed.to_bytes(),
                            );
                        }
                    }
                    maybe_raw = peer.rx.recv() => {
                        let Some(raw) = maybe_raw else {
                            break;
                        };
                        if raw.len() < 8 {
                            break;
                        }
                        let auth_key_id = u64::from_le_bytes(raw[0..8].try_into().unwrap());
                        if auth_key_id == 0 {
                            let msg_len =
                                u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
                            let body = &raw[20..20 + msg_len];
                            let ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());
                            if ctor == mtp_functions::ReqPqMulti::CONSTRUCTOR_ID {
                                let req = mtp_functions::ReqPqMulti::from_bytes(body).unwrap();
                                let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                                let mut server_nonce = [0u8; 16];
                                server_nonce[0..8].copy_from_slice(&seq.to_le_bytes());
                                server_nonce[8..16].copy_from_slice(&0xA55A_C33C_1234_5678u64.to_le_bytes());
                                let res_pq = mtp_enums::ResPQ::ResPQ(mtp_types::ResPQ {
                                    nonce: req.nonce,
                                    server_nonce,
                                    pq: vec![0x01, 0x87],
                                    server_public_key_fingerprints: vec![
                                        server.inner.rsa_pub.fingerprint() as i64,
                                    ],
                                });
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let _ = peer.tx.send(Ok(encode_unencrypted_packet(out_id, &res_pq.to_bytes())));
                            } else if ctor == mtp_functions::ReqDHParams::CONSTRUCTOR_ID {
                                let req = mtp_functions::ReqDHParams::from_bytes(body).unwrap();
                                let plain = server.decrypt_rsa_pad(&req.encrypted_data);
                                let mut plain_cur = Cursor::new(&plain);
                                let pq_inner = mtp_enums::PQInnerData::deserialize(&mut plain_cur).unwrap();
                                let new_nonce = match pq_inner {
                                    mtp_enums::PQInnerData::Dc(d) => d.new_nonce,
                                    mtp_enums::PQInnerData::TempDc(d) => d.new_nonce,
                                    other => panic!("unexpected PQInnerData: {other:?}"),
                                };
                                let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                                let mut a_srv = [0x42u8; 256];
                                a_srv[0..8].copy_from_slice(&seq.to_be_bytes());
                                a_srv[248..256].copy_from_slice(&(seq ^ 0x55AA_55AA).to_be_bytes());
                                let g_a = pad_be_256(&mod_exp(&[3], &a_srv, &GOOD_PRIME).unwrap());
                                let inner = mtp_enums::ServerDHInnerData::ServerDHInnerData(
                                    mtp_types::ServerDHInnerData {
                                        nonce: req.nonce,
                                        server_nonce: req.server_nonce,
                                        g: 3,
                                        dh_prime: GOOD_PRIME.to_vec(),
                                        g_a: g_a.to_vec(),
                                        server_time: now_unix_secs(),
                                    },
                                )
                                .to_bytes();
                                let digest = sha1(&[&inner]);
                                let mut answer_with_hash = Vec::with_capacity(20 + inner.len() + 16);
                                answer_with_hash.extend_from_slice(&digest);
                                answer_with_hash.extend_from_slice(&inner);
                                while answer_with_hash.len() % 16 != 0 {
                                    answer_with_hash.push(0);
                                }
                                let (aes_key, aes_iv) =
                                    dh_server_aes_key_iv(&new_nonce, &req.server_nonce);
                                let encrypted_answer =
                                    aes_ige_encrypt(&answer_with_hash, &aes_key, &aes_iv);
                                let resp = mtp_enums::ServerDHParams::Ok(
                                    mtp_types::ServerDHParamsOk {
                                        nonce: req.nonce,
                                        server_nonce: req.server_nonce,
                                        encrypted_answer,
                                    },
                                );
                                dh_pending = Some(DhPending {
                                    nonce: req.nonce,
                                    server_nonce: req.server_nonce,
                                    new_nonce,
                                    a_srv,
                                    aes_key,
                                    aes_iv,
                                });
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let _ = peer.tx.send(Ok(encode_unencrypted_packet(out_id, &resp.to_bytes())));
                            } else if ctor == mtp_functions::SetClientDHParams::CONSTRUCTOR_ID {
                                let req = mtp_functions::SetClientDHParams::from_bytes(body).unwrap();
                                let pending = dh_pending.take().unwrap();
                                let dec = aes_ige_decrypt(
                                    &req.encrypted_data,
                                    &pending.aes_key,
                                    &pending.aes_iv,
                                );
                                let mut dec_cur = Cursor::new(&dec[20..]);
                                let mtp_enums::ClientDHInnerData::ClientDHInnerData(cdh) =
                                    mtp_enums::ClientDHInnerData::deserialize(&mut dec_cur).unwrap();
                                let shared = pad_be_256(
                                    &mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME).unwrap(),
                                );
                                let auth_key = AuthKey::new(shared);
                                let nn_lo = u64::from_le_bytes(pending.new_nonce[0..8].try_into().unwrap());
                                let sn_lo = u64::from_le_bytes(pending.server_nonce[0..8].try_into().unwrap());
                                let server_salt = nn_lo ^ sn_lo;
                                let nh1 = dh_new_nonce_hash(
                                    &pending.new_nonce,
                                    1,
                                    &auth_key.aux_hash(),
                                );
                                server.inner.keys.lock().unwrap().insert(
                                    auth_key.key_id_u64(),
                                    ServerKeyEntry {
                                        key: auth_key,
                                        server_salt,
                                    },
                                );
                                let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(
                                    mtp_types::DhGenOk {
                                        nonce: pending.nonce,
                                        server_nonce: pending.server_nonce,
                                        new_nonce_hash1: nh1,
                                    },
                                );
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let _ = peer.tx.send(Ok(encode_unencrypted_packet(out_id, &ok.to_bytes())));
                            }
                            continue;
                        }

                        let key_entry = server.inner.keys.lock().unwrap().get(&auth_key_id).cloned();
                        let Some(entry) = key_entry else {
                            let _ = peer.tx.send(Err(TransportError::ServerError(
                                TransportErrorCode::AuthKeyNotFound,
                            )));
                            break;
                        };
                        let dec = parse_encrypted_packet(&raw, &entry.key, Side::Client)
                            .expect("decrypt client MTProto 2.0 packet");
                        let session_id = dec.session_id;
                        active_key = Some((entry.key.clone(), entry.server_salt, session_id));

                        let inner_msgs: Vec<(u64, Vec<u8>)> = if dec.body.len() >= 4
                            && u32::from_le_bytes(dec.body[0..4].try_into().unwrap())
                                == MsgContainer::CONSTRUCTOR_ID
                        {
                            unpack_container(&dec.body)
                                .unwrap()
                                .into_iter()
                                .map(|m| (m.msg_id as u64, m.body.0))
                                .collect()
                        } else {
                            vec![(dec.msg_id, dec.body.clone())]
                        };

                        for (msg_id, raw_msg) in inner_msgs {
                            let mut budget = 1024 * 1024;
                            let body = if raw_msg.len() >= 4
                                && u32::from_le_bytes(raw_msg[0..4].try_into().unwrap())
                                    == GzipPacked::CONSTRUCTOR_ID
                            {
                                unwrap_gzip_layers(&raw_msg, &mut budget, 0).unwrap()
                            } else {
                                raw_msg
                            };
                            if body.len() < 4 {
                                continue;
                            }
                            let outer_ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());
                            if outer_ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                                let ping = mtp_functions::Ping::from_bytes(&body).unwrap();
                                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                                    msg_id: msg_id as i64,
                                    ping_id: ping.ping_id,
                                });
                                let p_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(
                                    &peer,
                                    &entry.key,
                                    entry.server_salt,
                                    session_id,
                                    p_id,
                                    false,
                                    &mut server_seq_counter,
                                    &pong.to_bytes(),
                                );
                                continue;
                            }
                            if outer_ctor == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID {
                                let ping =
                                    mtp_functions::PingDelayDisconnect::from_bytes(&body).unwrap();
                                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                                    msg_id: msg_id as i64,
                                    ping_id: ping.ping_id,
                                });
                                let p_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(
                                    &peer,
                                    &entry.key,
                                    entry.server_salt,
                                    session_id,
                                    p_id,
                                    false,
                                    &mut server_seq_counter,
                                    &pong.to_bytes(),
                                );
                                continue;
                            }
                            if outer_ctor == mtp_types::MsgsAck::CONSTRUCTOR_ID
                                || outer_ctor == mtp_functions::GetFutureSalts::CONSTRUCTOR_ID
                            {
                                continue;
                            }
                            if outer_ctor == tl_fns::auth::BindTempAuthKey::CONSTRUCTOR_ID {
                                let rpc_res = Boxed(RpcResult {
                                    req_msg_id: msg_id as i64,
                                    result: RawObject(BOOL_TRUE_ID.to_le_bytes().to_vec()),
                                })
                                .to_bytes();
                                let r_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(
                                    &peer,
                                    &entry.key,
                                    entry.server_salt,
                                    session_id,
                                    r_id,
                                    true,
                                    &mut server_seq_counter,
                                    &rpc_res,
                                );
                                continue;
                            }

                            let inner_body = unwrap_init_layer(&body);
                            let inner_ctor =
                                u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                            server.inner.rpcs.lock().unwrap().push(RpcRecord {
                                ctor: inner_ctor,
                            });

                            let handler = server.inner.handler.lock().unwrap().clone();
                            let action = handler(
                                &server,
                                IncomingRpc {
                                    ctor: inner_ctor,
                                    body: inner_body,
                                },
                            );

                            match action {
                                ServerAction::Reply(payload) => {
                                    let rpc_res = Boxed(RpcResult {
                                        req_msg_id: msg_id as i64,
                                        result: RawObject(payload),
                                    })
                                    .to_bytes();
                                    let r_id = next_server_msg_id(&mut server_msg_counter, true);
                                    send_encrypted(
                                        &peer,
                                        &entry.key,
                                        entry.server_salt,
                                        session_id,
                                        r_id,
                                        true,
                                        &mut server_seq_counter,
                                        &rpc_res,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            server.inner.live_pushers.lock().unwrap().remove(&conn_id);
        });
    }
}

fn unwrap_init_layer(raw: &[u8]) -> Vec<u8> {
    if raw.len() < 4 {
        return raw.to_vec();
    }
    let ctor = u32::from_le_bytes(raw[0..4].try_into().unwrap());
    if ctor != tl_fns::InvokeWithLayer::<RawObject>::CONSTRUCTOR_ID {
        return raw.to_vec();
    }
    let mut cur = Cursor::new(&raw[4..]);
    let _layer = i32::deserialize(&mut cur).unwrap();
    let rest = &raw[raw.len() - cur.remaining()..];
    let mut icur = Cursor::new(&rest[4..]);
    let flags = i32::deserialize(&mut icur).unwrap() as u32;
    let _api_id = i32::deserialize(&mut icur).unwrap();
    let _device_model = String::deserialize(&mut icur).unwrap();
    let _system_version = String::deserialize(&mut icur).unwrap();
    let _app_version = String::deserialize(&mut icur).unwrap();
    let _system_lang_code = String::deserialize(&mut icur).unwrap();
    let _lang_pack = String::deserialize(&mut icur).unwrap();
    let _lang_code = String::deserialize(&mut icur).unwrap();
    if (flags & (1 << 0)) != 0 {
        let _ = tl_enums::InputClientProxy::deserialize(&mut icur).unwrap();
    }
    if (flags & (1 << 1)) != 0 {
        let _ = tl_enums::JSONValue::deserialize(&mut icur).unwrap();
    }
    rest[rest.len() - icur.remaining()..].to_vec()
}

impl TransportFactory for ScriptedServer {
    fn connect<'a>(
        &'a self,
        _ip: &'a str,
        _port: u16,
        protocol_dc_id: i16,
        _secret: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn PacketConnection>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            let conn_id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
            let (client_conn, server_peer) = FakeConnection::pair();
            self.spawn_connection(conn_id, protocol_dc_id, server_peer);
            Ok(Box::new(client_conn) as Box<dyn PacketConnection>)
        })
    }
}

fn build_client(server: &ScriptedServer, main_dc: DcId) -> (Client, MtprotoRpc) {
    let mut cfg: ClientConfig = build_mtproto_client_config(
        &ApiCredentials {
            api_id: FAKE_API_ID,
            api_hash: FAKE_API_HASH.to_owned(),
        },
        main_dc,
        tg_core::Environment::Production,
        None,
    );
    cfg.proxy = ProxyData::none();
    cfg.use_pfs = false;
    cfg.ping_interval = Duration::from_secs(600);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());
    (client, rpc)
}

fn tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from_user_id: Option<i64>,
    date: i32,
    text: &str,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
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
        id,
        from_id: from_user_id.map(|uid| tl_enums::Peer::User(tl_types::PeerUser { user_id: uid })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date,
        message: text.to_owned(),
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
        guestchat_via_from: None,
    })
}

async fn seed_peers(runtime: &SessionRuntime<MtprotoRpc>) {
    let mut s = runtime.session().write().await;
    let mut self_user = User::new(UserId(100), true);
    self_user.common.name = "Self Account".to_owned();
    self_user.common.loaded_status = LoadedStatus::Full;
    self_user.access_hash = 1001;
    s.upsert_user(self_user, PeerUpdateFlags::empty());

    let mut peer_user = User::new(UserId(200), false);
    peer_user.common.name = "Peer Two".to_owned();
    peer_user.common.loaded_status = LoadedStatus::Full;
    peer_user.access_hash = 2002;
    s.upsert_user(peer_user, PeerUpdateFlags::empty());

    let mut ch = Channel::new(ChannelId(500));
    ch.common.name = "Forum Supergroup".to_owned();
    ch.common.loaded_status = LoadedStatus::Full;
    ch.access_hash = 5005;
    ch.flags = ChannelFlags::MEGAGROUP | ChannelFlags::FORUM;
    s.upsert_channel(ch, PeerUpdateFlags::empty());
}

async fn wait_until<F, Fut>(timeout: Duration, mut pred: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if pred().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for condition"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Interleaved `messages.getForumTopics`, `messages.deleteTopicHistory` (`affectedHistory`),
/// and pushed channel updates over a single `SessionRuntime` + scripted MTProto 2.0 server:
/// - No `getChannelDifference` when pushed updates close a gap advertised by `getForumTopics`.
/// - No `getChannelDifference` when an `affectedHistory` page closes a gap opened by an
///   out-of-order pushed channel update (and the buffered pushed message drains in order).
/// - Exactly ONE `getChannelDifference` when a real gap remains past the 1s wait timer.
#[tokio::test]
async fn forum_topics_and_affected_history_interleaved_with_pushed_channel_updates() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let ch_id = ChannelId(500);
        let ch_peer = PeerId::from_channel(ch_id);
        let tl_ch = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

        let forum_pts_reply = Arc::new(Mutex::new(52i32));
        let forum_pts_srv = Arc::clone(&forum_pts_reply);
        let tl_ch_srv = tl_ch.clone();

        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 100,
                    qts: 0,
                    date: 1_795_340_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID => {
                let pts = *forum_pts_srv.lock().unwrap();
                ServerAction::ok(&tl_enums::messages::ForumTopics::ForumTopics(
                    tl_types::messages::ForumTopics {
                        order_by_create_date: false,
                        count: 0,
                        topics: Vec::new(),
                        messages: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                        pts,
                    },
                ))
            }
            tl_fns::messages::DeleteTopicHistory::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                    tl_types::messages::AffectedHistory {
                        pts: 54,
                        pts_count: 2,
                        offset: 0,
                    },
                ))
            }
            tl_fns::updates::GetChannelDifference::CONSTRUCTOR_ID => {
                let req = tl_fns::updates::GetChannelDifference::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.pts, 55, "getChannelDifference must start from good pts=55");
                assert!(req.force, "gap-triggered getChannelDifference must set force=true");
                ServerAction::ok(&tl_enums::updates::ChannelDifference::ChannelDifference(
                    tl_types::updates::ChannelDifference {
                        r#final: true,
                        pts: 58,
                        timeout: None,
                        new_messages: vec![tl_message(
                            903,
                            tl_ch_srv.clone(),
                            Some(200),
                            1_795_340_058,
                            "Recovered from real channel gap",
                        )],
                        other_updates: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                    },
                ))
            }
            x if x == tl_fns::messages::GetPeerDialogs::CONSTRUCTOR_ID => ServerAction::ok(
                &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                    dialogs: vec![],
                    messages: vec![],
                    chats: vec![],
                    users: vec![],
                    state: tl_enums::updates::State::State(tl_types::updates::State {
                        pts: 0,
                        qts: 0,
                        date: 0,
                        seq: 0,
                        unread_count: 0,
                    }),
                }),
            ),
            other => panic!("unexpected RPC ctor: 0x{other:08x}"),
        });

        let (_client, rpc) = build_client(&server, 2);
        let cfg = SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        };
        let runtime =
            SessionRuntime::start_mtproto_on(UserId(100), rpc, tokio::runtime::Handle::current(), cfg)
                .expect("start SessionRuntime");
        runtime.wait_ready().await.expect("wait_ready");
        seed_peers(&runtime).await;

        // Initialize channel 500 at pts = 50 (matching initial dialog / history watermark).
        runtime.init_channel_pts(ch_id, 50).await.unwrap();
        assert_eq!(
            runtime
                .persisted_updates_state()
                .await
                .unwrap()
                .channel_pts
                .get(&ch_id)
                .copied(),
            Some(50)
        );

        // --------------------------------------------------------------------
        // Phase A: `messages.getForumTopics` returns pts=52 BEFORE pushed
        // updates (51, 1) and (52, 1) arrive. Pushed updates close the gap
        // inside the 1s window -> ZERO `getChannelDifference` RPCs!
        // --------------------------------------------------------------------
        let tl_enums::messages::ForumTopics::ForumTopics(ft_resp) = runtime
            .rpc()
            .invoke(&tl_fns::messages::GetForumTopics {
                peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                    channel_id: 500,
                    access_hash: 5005,
                }),
                q: None,
                offset_date: 0,
                offset_id: 0,
                offset_topic: 0,
                limit: 20,
            })
            .await
            .unwrap();
        assert_eq!(ft_resp.pts, 52);
        runtime.apply_channel_pts(ch_id, ft_resp.pts, 0).await.unwrap();

        // Pushed channel messages 51 and 52 arrive right after the forum-topics reply.
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                    message: tl_message(900, tl_ch.clone(), Some(200), 1_795_340_051, "msg 51"),
                    pts: 51,
                    pts_count: 1,
                }),
                date: 1_795_340_051,
            }),
        ));
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                    message: tl_message(901, tl_ch.clone(), Some(200), 1_795_340_052, "msg 52"),
                    pts: 52,
                    pts_count: 1,
                }),
                date: 1_795_340_052,
            }),
        ));

        wait_until(Duration::from_secs(3), || async {
            runtime
                .persisted_updates_state()
                .await
                .ok()
                .and_then(|s| s.channel_pts.get(&ch_id).copied())
                == Some(52)
        })
        .await;

        // Sleep past the 1s gap window to prove the gap timer was canceled.
        tokio::time::sleep(Duration::from_millis(1_150)).await;
        assert_eq!(
            server
                .rpcs_of::<tl_fns::updates::GetChannelDifference>()
                .len(),
            0,
            "pushed updates closing the forum-topics pts=52 window must NOT trigger getChannelDifference"
        );
        {
            let s = runtime.session().read().await;
            assert!(s.message(FullMsgId::new(ch_peer, MsgId(900))).is_some());
            assert!(s.message(FullMsgId::new(ch_peer, MsgId(901))).is_some());
        }

        // --------------------------------------------------------------------
        // Phase B: Out-of-order pushed channel message (pts=55, pts_count=1)
        // arrives BEFORE `deleteTopicHistory`'s `affectedHistory(pts=54, pts_count=2)`.
        // `apply_affected_history` closes the gap (52 + 2 + 1 == 55), drains the
        // buffered message 902, cancels the timer -> STILL ZERO `getChannelDifference`!
        // --------------------------------------------------------------------
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                    message: tl_message(902, tl_ch.clone(), Some(200), 1_795_340_055, "msg 55"),
                    pts: 55,
                    pts_count: 1,
                }),
                date: 1_795_340_055,
            }),
        ));
        // Let SessionActor ingest and buffer (55, 1) before affectedHistory arrives.
        tokio::time::sleep(Duration::from_millis(80)).await;
        {
            let s = runtime.session().read().await;
            assert!(
                s.message(FullMsgId::new(ch_peer, MsgId(902))).is_none(),
                "out-of-order channel msg (pts=55) must stay buffered until affectedHistory fills 53..=54"
            );
        }

        let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = runtime
            .rpc()
            .invoke(&tl_fns::messages::DeleteTopicHistory {
                peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                    channel_id: 500,
                    access_hash: 5005,
                }),
                top_msg_id: 10,
            })
            .await
            .unwrap();
        runtime
            .apply_affected_history(Some(ch_peer), aff.pts, aff.pts_count)
            .await
            .unwrap();

        assert_eq!(
            runtime
                .persisted_updates_state()
                .await
                .unwrap()
                .channel_pts
                .get(&ch_id)
                .copied(),
            Some(55),
            "affectedHistory(54, 2) must close gap and drain buffered (55, 1) to pts=55"
        );
        {
            let s = runtime.session().read().await;
            assert!(
                s.message(FullMsgId::new(ch_peer, MsgId(902))).is_some(),
                "buffered pushed message 902 must be applied once affectedHistory closes the gap"
            );
        }

        tokio::time::sleep(Duration::from_millis(1_150)).await;
        assert_eq!(
            server
                .rpcs_of::<tl_fns::updates::GetChannelDifference>()
                .len(),
            0,
            "affectedHistory closing the buffered gap must NOT trigger getChannelDifference"
        );

        // --------------------------------------------------------------------
        // Phase C: Real channel gap — `messages.getForumTopics` reports pts=58
        // when local good pts is 55 and no pushed updates ever arrive for 56..=58.
        // After the 1s gap timer, SessionActor must send EXACTLY ONE
        // `updates.getChannelDifference(pts=55)`.
        // --------------------------------------------------------------------
        *forum_pts_reply.lock().unwrap() = 58;
        let tl_enums::messages::ForumTopics::ForumTopics(ft_gap) = runtime
            .rpc()
            .invoke(&tl_fns::messages::GetForumTopics {
                peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                    channel_id: 500,
                    access_hash: 5005,
                }),
                q: None,
                offset_date: 0,
                offset_id: 0,
                offset_topic: 0,
                limit: 20,
            })
            .await
            .unwrap();
        assert_eq!(ft_gap.pts, 58);
        runtime.apply_channel_pts(ch_id, ft_gap.pts, 0).await.unwrap();

        // Must NOT fire immediately before the 1s gap wait timer.
        assert_eq!(
            server
                .rpcs_of::<tl_fns::updates::GetChannelDifference>()
                .len(),
            0,
            "getChannelDifference must wait for the 1s PtsWaiter timer before firing"
        );

        wait_until(Duration::from_secs(3), || async {
            runtime
                .persisted_updates_state()
                .await
                .ok()
                .and_then(|s| s.channel_pts.get(&ch_id).copied())
                == Some(58)
        })
        .await;

        tokio::time::sleep(Duration::from_millis(1_150)).await;
        assert_eq!(
            server
                .rpcs_of::<tl_fns::updates::GetChannelDifference>()
                .len(),
            1,
            "real channel gap must trigger EXACTLY ONE updates.getChannelDifference"
        );
        {
            let s = runtime.session().read().await;
            assert!(
                s.message(FullMsgId::new(ch_peer, MsgId(903))).is_some(),
                "message recovered from getChannelDifference must be stored in Session"
            );
        }

        runtime.shutdown_async().await;
    })
    .await
    .expect("test timeout");
}

/// Interleaved `messages.deleteSavedHistory` (`affectedHistory` on common PTS) and pushed
/// `updateNewMessage` updates over a single `SessionRuntime` + scripted MTProto 2.0 server:
/// - No `updates.getDifference` when `affectedHistory` closes a gap from an out-of-order
///   pushed message.
/// - Exactly ONE `updates.getDifference` when `affectedHistory` reveals a real common PTS gap.
#[tokio::test]
async fn common_affected_history_interleaved_with_pushed_updates_and_real_gap() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let user_peer = PeerId::from_user(UserId(200));
        let tl_user = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });
        let tl_user_srv = tl_user.clone();

        let delete_step = Arc::new(Mutex::new(0u32));
        let del_srv = Arc::clone(&delete_step);

        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 100,
                    qts: 0,
                    date: 1_795_340_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::DeleteSavedHistory::CONSTRUCTOR_ID => {
                let mut step = del_srv.lock().unwrap();
                *step += 1;
                if *step == 1 {
                    ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                        tl_types::messages::AffectedHistory {
                            pts: 102,
                            pts_count: 2,
                            offset: 0,
                        },
                    ))
                } else {
                    // Real gap: local good is 103, affectedHistory reports (107, 2) -> missing 104, 105!
                    ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                        tl_types::messages::AffectedHistory {
                            pts: 107,
                            pts_count: 2,
                            offset: 0,
                        },
                    ))
                }
            }
            tl_fns::updates::GetDifference::CONSTRUCTOR_ID => {
                let req = tl_fns::updates::GetDifference::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.pts, 103, "getDifference must request from good pts=103");
                ServerAction::ok(&tl_enums::updates::Difference::Difference(
                    tl_types::updates::Difference {
                        new_messages: vec![tl_message(
                            802,
                            tl_user_srv.clone(),
                            Some(200),
                            1_795_340_107,
                            "Recovered from real common gap",
                        )],
                        new_encrypted_messages: Vec::new(),
                        other_updates: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                        state: tl_enums::updates::State::State(tl_types::updates::State {
                            pts: 107,
                            qts: 0,
                            date: 1_795_340_107,
                            seq: 1,
                            unread_count: 0,
                        }),
                    },
                ))
            }
            x if x == tl_fns::messages::GetPeerDialogs::CONSTRUCTOR_ID => ServerAction::ok(
                &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                    dialogs: vec![],
                    messages: vec![],
                    chats: vec![],
                    users: vec![],
                    state: tl_enums::updates::State::State(tl_types::updates::State {
                        pts: 0,
                        qts: 0,
                        date: 0,
                        seq: 0,
                        unread_count: 0,
                    }),
                }),
            ),
            other => panic!("unexpected RPC ctor: 0x{other:08x}"),
        });

        let (_client, rpc) = build_client(&server, 2);
        let cfg = SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        };
        let runtime =
            SessionRuntime::start_mtproto_on(UserId(100), rpc, tokio::runtime::Handle::current(), cfg)
                .expect("start SessionRuntime");
        runtime.wait_ready().await.expect("wait_ready");
        seed_peers(&runtime).await;

        // --------------------------------------------------------------------
        // Phase A: Pushed `updateNewMessage(pts=103, pts_count=1)` arrives
        // BEFORE `deleteSavedHistory`'s `affectedHistory(pts=102, pts_count=2)`.
        // `apply_affected_history(None, 102, 2)` closes the gap -> ZERO `getDifference`!
        // --------------------------------------------------------------------
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                    message: tl_message(801, tl_user.clone(), Some(200), 1_795_340_103, "msg 103"),
                    pts: 103,
                    pts_count: 1,
                }),
                date: 1_795_340_103,
            }),
        ));
        tokio::time::sleep(Duration::from_millis(80)).await;
        {
            let s = runtime.session().read().await;
            assert!(
                s.message(FullMsgId::new(user_peer, MsgId(801))).is_none(),
                "out-of-order common msg (pts=103) must stay buffered until affectedHistory fills 101..=102"
            );
        }

        let tl_enums::messages::AffectedHistory::AffectedHistory(aff1) = runtime
            .rpc()
            .invoke(&tl_fns::messages::DeleteSavedHistory {
                parent_peer: None,
                peer: tl_enums::InputPeer::User(tl_types::InputPeerUser {
                    user_id: 200,
                    access_hash: 2002,
                }),
                max_id: 0,
                min_date: None,
                max_date: None,
            })
            .await
            .unwrap();
        runtime
            .apply_affected_history(None, aff1.pts, aff1.pts_count)
            .await
            .unwrap();

        assert_eq!(runtime.persisted_updates_state().await.unwrap().pts, 103);
        {
            let s = runtime.session().read().await;
            assert!(s.message(FullMsgId::new(user_peer, MsgId(801))).is_some());
        }

        tokio::time::sleep(Duration::from_millis(1_150)).await;
        assert_eq!(
            server.rpcs_of::<tl_fns::updates::GetDifference>().len(),
            0,
            "affectedHistory closing the common gap must NOT trigger updates.getDifference"
        );

        // --------------------------------------------------------------------
        // Phase B: Real common PTS gap from `affectedHistory(pts=107, pts_count=2)`
        // when local good pts is 103 -> triggers EXACTLY ONE `updates.getDifference`.
        // --------------------------------------------------------------------
        let tl_enums::messages::AffectedHistory::AffectedHistory(aff2) = runtime
            .rpc()
            .invoke(&tl_fns::messages::DeleteSavedHistory {
                parent_peer: None,
                peer: tl_enums::InputPeer::User(tl_types::InputPeerUser {
                    user_id: 200,
                    access_hash: 2002,
                }),
                max_id: 0,
                min_date: None,
                max_date: None,
            })
            .await
            .unwrap();
        runtime
            .apply_affected_history(None, aff2.pts, aff2.pts_count)
            .await
            .unwrap();

        wait_until(Duration::from_secs(3), || async {
            runtime
                .persisted_updates_state()
                .await
                .map(|s| s.pts == 107)
                .unwrap_or(false)
        })
        .await;

        tokio::time::sleep(Duration::from_millis(1_150)).await;
        assert_eq!(
            server.rpcs_of::<tl_fns::updates::GetDifference>().len(),
            1,
            "real common gap must trigger EXACTLY ONE updates.getDifference"
        );
        {
            let s = runtime.session().read().await;
            assert!(s.message(FullMsgId::new(user_peer, MsgId(802))).is_some());
        }

        runtime.shutdown_async().await;
    })
    .await
    .expect("test timeout");
}

/// Verifies `init_channel_pts` initializes a channel watermark and a subsequent stale
/// `apply_channel_pts(ch, lower_pts, 0)` neither regresses `pts` nor fires `getChannelDifference`.
#[tokio::test]
async fn init_channel_pts_and_stale_watermark_never_regress_or_trigger_diff() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let ch_id = ChannelId(500);
        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 200,
                    qts: 0,
                    date: 1_795_340_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            other => panic!("unexpected RPC ctor: 0x{other:08x}"),
        });

        let (_client, rpc) = build_client(&server, 2);
        let cfg = SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        };
        let runtime = SessionRuntime::start_mtproto_on(
            UserId(100),
            rpc,
            tokio::runtime::Handle::current(),
            cfg,
        )
        .expect("start SessionRuntime");
        runtime.wait_ready().await.expect("wait_ready");
        seed_peers(&runtime).await;

        runtime.init_channel_pts(ch_id, 200).await.unwrap();
        // Stale forum/search reply arrives with older pts=198 -> must keep good=200 and fire 0 diffs.
        runtime.apply_channel_pts(ch_id, 198, 0).await.unwrap();

        tokio::time::sleep(Duration::from_millis(1_150)).await;
        assert_eq!(
            runtime
                .persisted_updates_state()
                .await
                .unwrap()
                .channel_pts
                .get(&ch_id)
                .copied(),
            Some(200)
        );
        assert_eq!(
            server
                .rpcs_of::<tl_fns::updates::GetChannelDifference>()
                .len(),
            0
        );

        runtime.shutdown_async().await;
    })
    .await
    .expect("test timeout");
}
