//! Byte-level tdesktop conformance tests for Telegram Stories protocol (`tg-api`,
//! `tg-data`, `tg-session`, `tg-tl`) against an in-process scripted fake MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses obviously fake credentials (`api_id = 1`, `api_hash = "0000...0000"`),
//!   synthetic peer/user IDs, and an ephemeral test-only RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::convert::{apply_tl_chat, apply_tl_user};
use tg_api::{
    ApiCredentials, FileOrigin, InvocationError, RpcClient, UpdateEvent, UpdatesHandler,
    refresh_file_reference,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{ChannelFlags, ChannelId, PeerId, Session, UserFlags, UserId};
use tg_mtproto::dc_options::DcId;
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, PacketConnection, TransportFactory,
};
use tg_session::{
    MtprotoRpc, SessionEvent, SessionRuntime, SessionRuntimeConfig, build_mtproto_client_config,
};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, RawObject, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::mpsc;

// ============================================================================
// Test-only constants & scripted fake MTProto 2.0 server
// ============================================================================

const FAKE_API_ID: i32 = 1;
const FAKE_API_HASH: &str = "00000000000000000000000000000000";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

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

#[derive(Debug, Clone)]
struct RpcRecord {
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct IncomingRpc {
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
enum ServerAction {
    Reply(Vec<u8>),
    RpcError(i32, String),
}

impl ServerAction {
    fn ok<T: Serializable>(val: &T) -> Self {
        Self::Reply(val.to_bytes())
    }

    fn err(code: i32, msg: impl Into<String>) -> Self {
        Self::RpcError(code, msg.into())
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

    fn rpcs_of<F: Identifiable>(&self) -> Vec<RpcRecord> {
        self.inner
            .rpcs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.ctor == F::CONSTRUCTOR_ID)
            .cloned()
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
                        let Some(raw) = maybe_raw else { break; };
                        if raw.len() < 8 { break; }
                        let auth_key_id = u64::from_le_bytes(raw[0..8].try_into().unwrap());
                        if auth_key_id == 0 {
                            let msg_len = u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
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
                                ).to_bytes();
                                let digest = sha1(&[&inner]);
                                let mut answer_with_hash = Vec::with_capacity(20 + inner.len() + 16);
                                answer_with_hash.extend_from_slice(&digest);
                                answer_with_hash.extend_from_slice(&inner);
                                while answer_with_hash.len() % 16 != 0 {
                                    answer_with_hash.push(0);
                                }
                                let (aes_key, aes_iv) = dh_server_aes_key_iv(&new_nonce, &req.server_nonce);
                                let encrypted_answer = aes_ige_encrypt(&answer_with_hash, &aes_key, &aes_iv);
                                let resp = mtp_enums::ServerDHParams::Ok(mtp_types::ServerDHParamsOk {
                                    nonce: req.nonce,
                                    server_nonce: req.server_nonce,
                                    encrypted_answer,
                                });
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
                                let dec = aes_ige_decrypt(&req.encrypted_data, &pending.aes_key, &pending.aes_iv);
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
                                let nh1 = dh_new_nonce_hash(&pending.new_nonce, 1, &auth_key.aux_hash());
                                server.inner.keys.lock().unwrap().insert(
                                    auth_key.key_id_u64(),
                                    ServerKeyEntry { key: auth_key, server_salt },
                                );
                                let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(mtp_types::DhGenOk {
                                    nonce: pending.nonce,
                                    server_nonce: pending.server_nonce,
                                    new_nonce_hash1: nh1,
                                });
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
                            if body.len() < 4 { continue; }
                            let outer_ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());
                            if outer_ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                                let ping = mtp_functions::Ping::from_bytes(&body).unwrap();
                                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                                    msg_id: msg_id as i64,
                                    ping_id: ping.ping_id,
                                });
                                let p_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(&peer, &entry.key, entry.server_salt, session_id, p_id, false, &mut server_seq_counter, &pong.to_bytes());
                                continue;
                            }
                            if outer_ctor == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID {
                                let ping = mtp_functions::PingDelayDisconnect::from_bytes(&body).unwrap();
                                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                                    msg_id: msg_id as i64,
                                    ping_id: ping.ping_id,
                                });
                                let p_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(&peer, &entry.key, entry.server_salt, session_id, p_id, false, &mut server_seq_counter, &pong.to_bytes());
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
                                }).to_bytes();
                                let r_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(&peer, &entry.key, entry.server_salt, session_id, r_id, true, &mut server_seq_counter, &rpc_res);
                                continue;
                            }

                            let inner_body = unwrap_init_layer(&body);
                            let inner_ctor = u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                            server.inner.rpcs.lock().unwrap().push(RpcRecord {
                                ctor: inner_ctor,
                                body: inner_body.clone(),
                            });

                            let handler = server.inner.handler.lock().unwrap().clone();
                            let action = handler(&server, IncomingRpc { ctor: inner_ctor, body: inner_body });

                            match action {
                                ServerAction::Reply(payload) => {
                                    let rpc_res = Boxed(RpcResult {
                                        req_msg_id: msg_id as i64,
                                        result: RawObject(payload),
                                    }).to_bytes();
                                    let r_id = next_server_msg_id(&mut server_msg_counter, true);
                                    send_encrypted(&peer, &entry.key, entry.server_salt, session_id, r_id, true, &mut server_seq_counter, &rpc_res);
                                }
                                ServerAction::RpcError(code, message) => {
                                    let err_bytes = mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                        error_code: code,
                                        error_message: message,
                                    }).to_bytes();
                                    let rpc_res = Boxed(RpcResult {
                                        req_msg_id: msg_id as i64,
                                        result: RawObject(err_bytes),
                                    }).to_bytes();
                                    let r_id = next_server_msg_id(&mut server_msg_counter, true);
                                    send_encrypted(&peer, &entry.key, entry.server_salt, session_id, r_id, true, &mut server_seq_counter, &rpc_res);
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

fn tl_user(
    id: i64,
    access_hash: i64,
    first: &str,
    min: bool,
    stories_hidden: bool,
    stories_unavailable: bool,
    stories_max_id: Option<(Option<i32>, bool)>,
) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
        contact: true,
        mutual_contact: true,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min,
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
        stories_hidden,
        stories_unavailable,
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
        access_hash: Some(access_hash),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: None,
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: stories_max_id.map(|(max_id, live)| {
            tl_enums::RecentStory::RecentStory(tl_types::RecentStory { live, max_id })
        }),
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn tl_channel(
    id: i64,
    access_hash: i64,
    title: &str,
    min: bool,
    stories_hidden: bool,
    stories_hidden_min: bool,
    stories_unavailable: bool,
    stories_max_id: Option<(Option<i32>, bool)>,
) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: false,
        left: false,
        broadcast: true,
        verified: false,
        megagroup: false,
        restricted: false,
        signatures: false,
        min,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden,
        stories_hidden_min,
        stories_unavailable,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(100),
        usernames: None,
        stories_max_id: stories_max_id.map(|(max_id, live)| {
            tl_enums::RecentStory::RecentStory(tl_types::RecentStory { live, max_id })
        }),
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    })
}

fn tl_story_photo_item(id: i32, photo_id: i64, file_ref: Vec<u8>) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: true,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out: false,
        id,
        date: 1_750_000_000,
        from_id: None,
        fwd_from: None,
        expire_date: 1_750_086_400,
        caption: Some(format!("story #{id}")),
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler: false,
            live_photo: false,
            photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                has_stickers: false,
                id: photo_id,
                access_hash: photo_id * 3,
                file_reference: file_ref,
                date: 1_750_000_000,
                sizes: vec![],
                video_sizes: None,
                dc_id: 2,
            })),
            ttl_seconds: None,
            video: None,
        })),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    })
}

// ============================================================================
// 1. Stories RPCs over encrypted MTProto 2.0 wire
// ============================================================================

#[tokio::test]
async fn stories_all_rpcs_paging_actions_and_albums_over_mtproto_wire() {
    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        x if x == tl_fns::stories::GetAllStories::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::GetAllStories::from_bytes(&rpc.body).unwrap();
            let stealth =
                tl_enums::StoriesStealthMode::StoriesStealthMode(tl_types::StoriesStealthMode {
                    active_until_date: Some(1_750_001_000),
                    cooldown_until_date: Some(1_750_004_000),
                });
            if !req.next {
                ServerAction::ok(&tl_enums::stories::AllStories::AllStories(
                    tl_types::stories::AllStories {
                        has_more: true,
                        count: 2,
                        state: if req.hidden { "h_p1" } else { "m_p1" }.to_owned(),
                        peer_stories: vec![tl_enums::PeerStories::PeerStories(
                            tl_types::PeerStories {
                                peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 2001 }),
                                max_read_id: Some(4),
                                stories: vec![tl_story_photo_item(5, 8001, vec![1, 2])],
                            },
                        )],
                        chats: vec![],
                        users: vec![],
                        stealth_mode: stealth,
                    },
                ))
            } else if req.state.as_deref() == Some("m_p1") {
                ServerAction::ok(&tl_enums::stories::AllStories::AllStories(
                    tl_types::stories::AllStories {
                        has_more: false,
                        count: 2,
                        state: "m_p2".to_owned(),
                        peer_stories: vec![],
                        chats: vec![],
                        users: vec![],
                        stealth_mode: stealth,
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::stories::AllStories::NotModified(
                    tl_types::stories::AllStoriesNotModified {
                        state: "m_p2".to_owned(),
                        stealth_mode: stealth,
                    },
                ))
            }
        }
        x if x == tl_fns::stories::GetPeerStories::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::GetPeerStories::from_bytes(&rpc.body).unwrap();
            let uid = match req.peer {
                tl_enums::InputPeer::User(u) => u.user_id,
                _ => 2001,
            };
            ServerAction::ok(&tl_enums::stories::PeerStories::PeerStories(
                tl_types::stories::PeerStories {
                    stories: tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
                        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: uid }),
                        max_read_id: Some(10),
                        stories: vec![tl_story_photo_item(11, 8011, vec![3, 4])],
                    }),
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        x if x == tl_fns::stories::GetPinnedStories::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::GetPinnedStories::from_bytes(&rpc.body).unwrap();
            let stories = if req.offset_id == 0 {
                vec![
                    tl_story_photo_item(20, 8020, vec![1]),
                    tl_story_photo_item(15, 8015, vec![1]),
                ]
            } else {
                vec![tl_story_photo_item(10, 8010, vec![1])]
            };
            ServerAction::ok(&tl_enums::stories::Stories::Stories(
                tl_types::stories::Stories {
                    count: 3,
                    stories,
                    pinned_to_top: Some(vec![20]),
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        x if x == tl_fns::stories::GetStoriesArchive::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::GetStoriesArchive::from_bytes(&rpc.body).unwrap();
            let stories = if req.offset_id == 0 {
                vec![tl_story_photo_item(30, 8030, vec![1])]
            } else {
                vec![]
            };
            ServerAction::ok(&tl_enums::stories::Stories::Stories(
                tl_types::stories::Stories {
                    count: 1,
                    stories,
                    pinned_to_top: None,
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        x if x == tl_fns::stories::ReadStories::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::ReadStories::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(&vec![req.max_id])
        }
        x if x == tl_fns::stories::IncrementStoryViews::CONSTRUCTOR_ID => ServerAction::ok(&true),
        x if x == tl_fns::stories::GetStoryViewsList::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::GetStoryViewsList::from_bytes(&rpc.body).unwrap();
            let (views, next_offset) = if req.offset.is_empty() {
                (
                    vec![tl_enums::StoryView::StoryView(tl_types::StoryView {
                        blocked: false,
                        blocked_my_stories_from: false,
                        user_id: 2001,
                        date: 1_750_000_050,
                        reaction: None,
                    })],
                    Some("views_cursor_2".to_owned()),
                )
            } else {
                (
                    vec![tl_enums::StoryView::StoryView(tl_types::StoryView {
                        blocked: false,
                        blocked_my_stories_from: false,
                        user_id: 2002,
                        date: 1_750_000_040,
                        reaction: None,
                    })],
                    None,
                )
            };
            ServerAction::ok(&tl_enums::stories::StoryViewsList::StoryViewsList(
                tl_types::stories::StoryViewsList {
                    count: 2,
                    views_count: 2,
                    forwards_count: 0,
                    reactions_count: 0,
                    views,
                    chats: vec![],
                    users: vec![],
                    next_offset,
                },
            ))
        }
        x if x == tl_fns::stories::SendReaction::CONSTRUCTOR_ID
            || x == tl_fns::stories::SendStory::CONSTRUCTOR_ID
            || x == tl_fns::stories::EditStory::CONSTRUCTOR_ID
            || x == tl_fns::stories::ActivateStealthMode::CONSTRUCTOR_ID =>
        {
            let update = if x == tl_fns::stories::ActivateStealthMode::CONSTRUCTOR_ID {
                tl_enums::Update::StoriesStealthMode(tl_types::UpdateStoriesStealthMode {
                    stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                        tl_types::StoriesStealthMode {
                            active_until_date: Some(1_750_002_000),
                            cooldown_until_date: Some(1_750_005_000),
                        },
                    ),
                })
            } else {
                tl_enums::Update::StoryID(tl_types::UpdateStoryID {
                    id: 99,
                    random_id: 4242,
                })
            };
            ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![update],
                users: vec![],
                chats: vec![],
                date: 1_750_000_100,
                seq: 0,
            }))
        }
        x if x == tl_fns::stories::TogglePinned::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::TogglePinned::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(&req.id)
        }
        x if x == tl_fns::stories::TogglePinnedToTop::CONSTRUCTOR_ID => ServerAction::ok(&true),
        x if x == tl_fns::stories::DeleteStories::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::DeleteStories::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(&req.id)
        }
        x if x == tl_fns::stories::GetAlbums::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::GetAlbums::from_bytes(&rpc.body).unwrap();
            if req.hash == 777 {
                ServerAction::ok(&tl_enums::stories::Albums::NotModified)
            } else {
                ServerAction::ok(&tl_enums::stories::Albums::Albums(
                    tl_types::stories::Albums {
                        hash: 777,
                        albums: vec![tl_enums::StoryAlbum::StoryAlbum(tl_types::StoryAlbum {
                            album_id: 1,
                            title: "Highlights".to_owned(),
                            icon_photo: None,
                            icon_video: None,
                        })],
                    },
                ))
            }
        }
        x if x == tl_fns::stories::GetAlbumStories::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                count: 1,
                stories: vec![tl_story_photo_item(20, 8020, vec![5])],
                pinned_to_top: None,
                chats: vec![],
                users: vec![],
            }),
        ),
        x if x == tl_fns::stories::CreateAlbum::CONSTRUCTOR_ID
            || x == tl_fns::stories::UpdateAlbum::CONSTRUCTOR_ID =>
        {
            ServerAction::ok(&tl_enums::StoryAlbum::StoryAlbum(tl_types::StoryAlbum {
                album_id: 2,
                title: "Album 2".to_owned(),
                icon_photo: None,
                icon_video: None,
            }))
        }
        x if x == tl_fns::stories::ReorderAlbums::CONSTRUCTOR_ID
            || x == tl_fns::stories::DeleteAlbum::CONSTRUCTOR_ID =>
        {
            ServerAction::ok(&true)
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let peer_a = tl_enums::InputPeer::User(tl_types::InputPeerUser {
        user_id: 2001,
        access_hash: 20010,
    });

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. getAllStories paging: initial -> next(m_p1) -> notModified(m_p2) + hidden=true.
        let p1 = rpc
            .invoke(&tl_fns::stories::GetAllStories {
                next: false,
                hidden: false,
                state: None,
            })
            .await
            .unwrap();
        let state1 = match p1 {
            tl_enums::stories::AllStories::AllStories(a) => {
                assert!(a.has_more);
                a.state
            }
            _ => panic!("expected AllStories"),
        };
        assert_eq!(state1, "m_p1");

        let p2 = rpc
            .invoke(&tl_fns::stories::GetAllStories {
                next: true,
                hidden: false,
                state: Some(state1),
            })
            .await
            .unwrap();
        let state2 = match p2 {
            tl_enums::stories::AllStories::AllStories(a) => {
                assert!(!a.has_more);
                a.state
            }
            _ => panic!("expected AllStories page 2"),
        };
        let p3 = rpc
            .invoke(&tl_fns::stories::GetAllStories {
                next: true,
                hidden: false,
                state: Some(state2),
            })
            .await
            .unwrap();
        assert!(matches!(p3, tl_enums::stories::AllStories::NotModified(_)));

        let hidden_p1 = rpc
            .invoke(&tl_fns::stories::GetAllStories {
                next: false,
                hidden: true,
                state: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            hidden_p1,
            tl_enums::stories::AllStories::AllStories(a) if a.state == "h_p1"
        ));

        // 2. getPeerStories, getPinnedStories (2 pages), getStoriesArchive.
        let ps = rpc
            .invoke(&tl_fns::stories::GetPeerStories {
                peer: peer_a.clone(),
            })
            .await
            .unwrap();
        let tl_enums::stories::PeerStories::PeerStories(ps) = ps;
        let tl_enums::PeerStories::PeerStories(inner) = ps.stories;
        assert_eq!(inner.max_read_id, Some(10));

        let pin1 = rpc
            .invoke(&tl_fns::stories::GetPinnedStories {
                peer: tl_enums::InputPeer::Self_,
                offset_id: 0,
                limit: 10,
            })
            .await
            .unwrap();
        let tl_enums::stories::Stories::Stories(pin1) = pin1;
        assert_eq!(pin1.stories.len(), 2);
        assert_eq!(pin1.pinned_to_top, Some(vec![20]));

        let pin2 = rpc
            .invoke(&tl_fns::stories::GetPinnedStories {
                peer: tl_enums::InputPeer::Self_,
                offset_id: 15,
                limit: 30,
            })
            .await
            .unwrap();
        let tl_enums::stories::Stories::Stories(pin2) = pin2;
        assert_eq!(pin2.stories.len(), 1);

        let arch = rpc
            .invoke(&tl_fns::stories::GetStoriesArchive {
                peer: tl_enums::InputPeer::Self_,
                offset_id: 0,
                limit: 10,
            })
            .await
            .unwrap();
        let tl_enums::stories::Stories::Stories(arch) = arch;
        assert_eq!(arch.count, 1);

        // 3. readStories, incrementStoryViews, getStoryViewsList (2 pages with filters).
        let read_ids = rpc
            .invoke(&tl_fns::stories::ReadStories {
                peer: peer_a.clone(),
                max_id: 11,
            })
            .await
            .unwrap();
        assert_eq!(read_ids, vec![11]);

        assert!(
            rpc.invoke(&tl_fns::stories::IncrementStoryViews {
                peer: peer_a.clone(),
                id: vec![5, 11],
            })
            .await
            .unwrap()
        );

        let v1 = rpc
            .invoke(&tl_fns::stories::GetStoryViewsList {
                just_contacts: true,
                reactions_first: true,
                forwards_first: false,
                peer: tl_enums::InputPeer::Self_,
                q: None,
                id: 20,
                offset: String::new(),
                limit: 50,
            })
            .await
            .unwrap();
        let tl_enums::stories::StoryViewsList::StoryViewsList(v1) = v1;
        assert_eq!(v1.next_offset.as_deref(), Some("views_cursor_2"));
        let v2 = rpc
            .invoke(&tl_fns::stories::GetStoryViewsList {
                just_contacts: true,
                reactions_first: true,
                forwards_first: false,
                peer: tl_enums::InputPeer::Self_,
                q: None,
                id: 20,
                offset: "views_cursor_2".to_owned(),
                limit: 50,
            })
            .await
            .unwrap();
        let tl_enums::stories::StoryViewsList::StoryViewsList(v2) = v2;
        assert!(v2.next_offset.is_none());

        // 4. sendReaction, togglePinned, togglePinnedToTop, deleteStories, sendStory, editStory, activateStealthMode.
        let _ = rpc
            .invoke(&tl_fns::stories::SendReaction {
                add_to_recent: false,
                peer: peer_a.clone(),
                story_id: 11,
                reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "\u{2764}".to_owned(),
                }),
            })
            .await
            .unwrap();
        assert_eq!(
            rpc.invoke(&tl_fns::stories::TogglePinned {
                peer: tl_enums::InputPeer::Self_,
                id: vec![20],
                pinned: true,
            })
            .await
            .unwrap(),
            vec![20]
        );
        assert!(
            rpc.invoke(&tl_fns::stories::TogglePinnedToTop {
                peer: tl_enums::InputPeer::Self_,
                id: vec![20, 15],
            })
            .await
            .unwrap()
        );
        assert_eq!(
            rpc.invoke(&tl_fns::stories::DeleteStories {
                peer: tl_enums::InputPeer::Self_,
                id: vec![15],
            })
            .await
            .unwrap(),
            vec![15]
        );

        let _ = rpc
            .invoke(&tl_fns::stories::SendStory {
                pinned: true,
                noforwards: true,
                fwd_modified: false,
                peer: tl_enums::InputPeer::Self_,
                media: tl_enums::InputMedia::Empty,
                media_areas: Some(vec![tl_enums::MediaArea::Url(tl_types::MediaAreaUrl {
                    coordinates: tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(
                        tl_types::MediaAreaCoordinates {
                            x: 50.0,
                            y: 50.0,
                            w: 40.0,
                            h: 10.0,
                            rotation: 0.0,
                            radius: Some(8.0),
                        },
                    ),
                    url: "https://example.test".to_owned(),
                })]),
                caption: Some("Caption".to_owned()),
                entities: None,
                privacy_rules: vec![tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll],
                random_id: 4242,
                period: Some(86_400),
                fwd_from_id: None,
                fwd_from_story: None,
                albums: Some(vec![1]),
                music: None,
            })
            .await
            .unwrap();

        let _ = rpc
            .invoke(&tl_fns::stories::EditStory {
                peer: tl_enums::InputPeer::Self_,
                id: 99,
                media: None,
                media_areas: None,
                caption: Some("Edited".to_owned()),
                entities: Some(vec![]),
                privacy_rules: Some(vec![
                    tl_enums::InputPrivacyRule::InputPrivacyValueAllowCloseFriends,
                ]),
                music: None,
            })
            .await
            .unwrap();

        let stealth_upd = rpc
            .invoke(&tl_fns::stories::ActivateStealthMode {
                past: true,
                future: true,
            })
            .await
            .unwrap();
        let mut session = Session::new(UserId::from_tl(1000));
        let mut handler = UpdatesHandler::new();
        handler
            .state_machine_mut()
            .init_state(10, 1_750_000_000, 0, 1, 1_000);
        handler
            .handle_updates(&rpc, &mut session, stealth_upd, None, 10_000)
            .await
            .unwrap();
        let events = handler.drain_events();
        assert!(events.iter().any(|e| matches!(
            e,
            UpdateEvent::StoryUpdate(u) if matches!(**u, tl_enums::Update::StoriesStealthMode(_))
        )));

        // 5. Layer 233 story albums RPCs.
        let alb1 = rpc
            .invoke(&tl_fns::stories::GetAlbums {
                peer: tl_enums::InputPeer::Self_,
                hash: 0,
            })
            .await
            .unwrap();
        assert!(matches!(alb1, tl_enums::stories::Albums::Albums(a) if a.hash == 777));
        let alb2 = rpc
            .invoke(&tl_fns::stories::GetAlbums {
                peer: tl_enums::InputPeer::Self_,
                hash: 777,
            })
            .await
            .unwrap();
        assert!(matches!(alb2, tl_enums::stories::Albums::NotModified));
        let _ = rpc
            .invoke(&tl_fns::stories::GetAlbumStories {
                peer: tl_enums::InputPeer::Self_,
                album_id: 1,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        let _ = rpc
            .invoke(&tl_fns::stories::CreateAlbum {
                peer: tl_enums::InputPeer::Self_,
                title: "Album 2".to_owned(),
                stories: vec![20],
            })
            .await
            .unwrap();
        let _ = rpc
            .invoke(&tl_fns::stories::UpdateAlbum {
                peer: tl_enums::InputPeer::Self_,
                album_id: 2,
                title: Some("Renamed".to_owned()),
                delete_stories: None,
                add_stories: Some(vec![15]),
                order: None,
            })
            .await
            .unwrap();
        assert!(
            rpc.invoke(&tl_fns::stories::ReorderAlbums {
                peer: tl_enums::InputPeer::Self_,
                order: vec![2, 1],
            })
            .await
            .unwrap()
        );
        assert!(
            rpc.invoke(&tl_fns::stories::DeleteAlbum {
                peer: tl_enums::InputPeer::Self_,
                album_id: 2,
            })
            .await
            .unwrap()
        );
    })
    .await
    .expect("stories_all_rpcs test timed out");

    // Verify exact decoded request shapes received by server.
    assert_eq!(server.rpcs_of::<tl_fns::stories::GetAllStories>().len(), 4);
    let send_rec = &server.rpcs_of::<tl_fns::stories::SendStory>()[0];
    let send_req = tl_fns::stories::SendStory::from_bytes(&send_rec.body).unwrap();
    assert!(send_req.pinned && send_req.noforwards);
    assert_eq!(send_req.period, Some(86_400));
    assert_eq!(send_req.media_areas.as_ref().map(Vec::len), Some(1));
    assert_eq!(send_req.albums, Some(vec![1]));

    let edit_rec = &server.rpcs_of::<tl_fns::stories::EditStory>()[0];
    let edit_req = tl_fns::stories::EditStory::from_bytes(&edit_rec.body).unwrap();
    assert_eq!(edit_req.caption.as_deref(), Some("Edited"));
    assert_eq!(edit_req.entities, Some(vec![]));
}

// ============================================================================
// 2. Story file_reference refresh via `stories.getStoriesByID`
// ============================================================================

#[tokio::test]
async fn story_file_reference_refresh_invokes_get_stories_by_id() {
    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        x if x == tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID => {
            let req = tl_fns::stories::GetStoriesByID::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.id, vec![77]);
            ServerAction::ok(&tl_enums::stories::Stories::Stories(
                tl_types::stories::Stories {
                    count: 1,
                    stories: vec![tl_story_photo_item(77, 99901, vec![0xDE, 0xAD, 0xBE, 0xEF])],
                    pinned_to_top: None,
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        other => ServerAction::err(400, format!("UNEXPECTED_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId::from_tl(1000));
    apply_tl_user(
        &mut session,
        &tl_user(2001, 555_666, "Alice", false, false, false, None),
    );

    tokio::time::timeout(TEST_TIMEOUT, async {
        let updated = refresh_file_reference(
            &rpc,
            &session,
            &FileOrigin::Story {
                peer_id: PeerId::from_user(UserId::from_tl(2001)),
                story_id: 77,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            updated.photos.get(&99901u64),
            Some(&vec![0xDE, 0xAD, 0xBE, 0xEF])
        );
    })
    .await
    .expect("story file ref refresh timed out");

    assert_eq!(server.rpcs_of::<tl_fns::stories::GetStoriesByID>().len(), 1);
}

// ============================================================================
// 3. Live + getDifference story updates delivered through SessionRuntime
// ============================================================================

#[tokio::test]
async fn story_updates_delivered_both_live_and_via_get_difference_in_session_runtime() {
    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        x if x == tl_fns::users::GetUsers::CONSTRUCTOR_ID => {
            ServerAction::ok(&vec![tl_user(1000, 1, "Self", false, false, false, None)])
        }
        x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 0,
            }))
        }
        x if x == tl_fns::updates::GetDifference::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
                new_messages: vec![],
                new_encrypted_messages: vec![],
                other_updates: vec![
                    tl_enums::Update::Story(tl_types::UpdateStory {
                        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 2001 }),
                        story: tl_story_photo_item(501, 5010, vec![1]),
                    }),
                    tl_enums::Update::ReadStories(tl_types::UpdateReadStories {
                        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 2001 }),
                        max_id: 501,
                    }),
                ],
                chats: vec![],
                users: vec![],
                state: tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 12,
                    qts: 0,
                    date: 1_750_000_010,
                    seq: 2,
                    unread_count: 0,
                }),
            }),
        ),
        x if x == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: vec![],
                messages: vec![],
                chats: vec![],
                users: vec![],
            }),
        ),
        x if x == tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::DialogFilters::DialogFilters(tl_types::messages::DialogFilters {
                tags_enabled: false,
                filters: vec![],
            }),
        ),
        _ => ServerAction::ok(&true),
    });

    let (_client, rpc) = build_client(&server, 2);
    let cfg = SessionRuntimeConfig::default().with_idle_ping_enabled(false);
    let runtime = SessionRuntime::start_mtproto_on(
        UserId::from_tl(1000),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .unwrap();
    let mut events_rx = runtime.subscribe_events();

    tokio::time::timeout(TEST_TIMEOUT, async {
        // Wait for Ready.
        loop {
            match events_rx.recv().await.unwrap() {
                SessionEvent::Ready => break,
                _ => continue,
            }
        }

        // Push live updateStoriesStealthMode over MTProto -> SessionRuntime must emit SessionEvent::StoryUpdate.
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::StoriesStealthMode(
                    tl_types::UpdateStoriesStealthMode {
                        stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                            tl_types::StoriesStealthMode {
                                active_until_date: Some(1_750_003_000),
                                cooldown_until_date: Some(1_750_006_000),
                            },
                        ),
                    },
                )],
                users: vec![],
                chats: vec![],
                date: 1_750_000_005,
                seq: 0,
            }),
        ));

        let mut saw_live_stealth = false;
        while !saw_live_stealth {
            if let SessionEvent::StoryUpdate(u) = events_rx.recv().await.unwrap()
                && matches!(*u, tl_enums::Update::StoriesStealthMode(_))
            {
                saw_live_stealth = true;
            }
        }

        // Now trigger `updates.getDifference` via a common-PTS gap (pts=15, pts_count=1 > local 10)
        // and verify story updates inside `getDifference.other_updates` are forwarded too!
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::DeleteMessages(
                    tl_types::UpdateDeleteMessages {
                        messages: vec![1],
                        pts: 15,
                        pts_count: 1,
                    },
                )],
                users: vec![],
                chats: vec![],
                date: 1_750_000_008,
                seq: 0,
            }),
        ));

        let mut saw_diff_story = false;
        let mut saw_diff_read = false;
        while !(saw_diff_story && saw_diff_read) {
            if let SessionEvent::StoryUpdate(u) = events_rx.recv().await.unwrap() {
                match *u {
                    tl_enums::Update::Story(_) => saw_diff_story = true,
                    tl_enums::Update::ReadStories(r) if r.max_id == 501 => saw_diff_read = true,
                    _ => {}
                }
            }
        }
    })
    .await
    .expect("story updates test timed out");

    runtime.shutdown();
}

// ============================================================================
// 4. Peer story flags (`stories_hidden_min`, `stories_unavailable`, `min`) & errors
// ============================================================================

#[tokio::test]
async fn peer_story_flags_and_story_rpc_errors() {
    let mut session = Session::new(UserId::from_tl(1000));

    // Full user with active live story & hidden stories.
    apply_tl_user(
        &mut session,
        &tl_user(
            2001,
            10,
            "Alice",
            false,
            true,
            false,
            Some((Some(55), true)),
        ),
    );
    let u = session.user(UserId::from_tl(2001)).unwrap();
    assert!(u.flags.contains(UserFlags::STORIES_HIDDEN));
    assert!(u.flags.contains(UserFlags::HAS_ACTIVE_STORIES));
    assert!(u.flags.contains(UserFlags::HAS_ACTIVE_VIDEO_STREAM));

    // `min: true` user must NOT wipe `HAS_ACTIVE_STORIES` or `STORIES_HIDDEN`.
    apply_tl_user(
        &mut session,
        &tl_user(2001, 10, "Alice", true, false, true, None),
    );
    let u = session.user(UserId::from_tl(2001)).unwrap();
    assert!(u.flags.contains(UserFlags::STORIES_HIDDEN));
    assert!(u.flags.contains(UserFlags::HAS_ACTIVE_STORIES));
    assert!(u.flags.contains(UserFlags::HAS_ACTIVE_VIDEO_STREAM));

    // Non-min user with `stories_unavailable: true` MUST clear active story flags.
    apply_tl_user(
        &mut session,
        &tl_user(2001, 10, "Alice", false, true, true, None),
    );
    let u = session.user(UserId::from_tl(2001)).unwrap();
    assert!(!u.flags.contains(UserFlags::HAS_ACTIVE_STORIES));
    assert!(!u.flags.contains(UserFlags::HAS_ACTIVE_VIDEO_STREAM));

    // Channel: `stories_hidden_min: true` must preserve `STORIES_HIDDEN` even when `min: false`!
    apply_tl_chat(
        &mut session,
        &tl_channel(
            3001,
            20,
            "News",
            false,
            true,
            false,
            false,
            Some((Some(9), true)),
        ),
    );
    let ch = session.channel(ChannelId::from_tl(3001)).unwrap();
    assert!(ch.flags.contains(ChannelFlags::STORIES_HIDDEN));
    assert!(ch.flags.contains(ChannelFlags::HAS_ACTIVE_STORIES));
    assert!(ch.flags.contains(ChannelFlags::HAS_ACTIVE_VIDEO_STREAM));

    apply_tl_chat(
        &mut session,
        &tl_channel(3001, 20, "News", false, false, true, true, None),
    );
    let ch = session.channel(ChannelId::from_tl(3001)).unwrap();
    assert!(
        ch.flags.contains(ChannelFlags::STORIES_HIDDEN),
        "stories_hidden_min=true must preserve STORIES_HIDDEN"
    );
    assert!(
        !ch.flags.contains(ChannelFlags::HAS_ACTIVE_STORIES),
        "non-min stories_unavailable=true must clear HAS_ACTIVE_STORIES"
    );
    assert!(!ch.flags.contains(ChannelFlags::HAS_ACTIVE_VIDEO_STREAM));

    // Error paths over MTProto wire: FLOOD_WAIT_12, PREMIUM_ACCOUNT_REQUIRED, STORY_ID_INVALID.
    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        x if x == tl_fns::stories::ActivateStealthMode::CONSTRUCTOR_ID => {
            ServerAction::err(403, "PREMIUM_ACCOUNT_REQUIRED")
        }
        x if x == tl_fns::stories::SendStory::CONSTRUCTOR_ID => {
            ServerAction::err(420, "FLOOD_WAIT_12")
        }
        x if x == tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID => {
            ServerAction::err(400, "STORY_ID_INVALID")
        }
        _ => ServerAction::ok(&true),
    });
    let (_client, rpc) = build_client(&server, 2);
    tokio::time::timeout(TEST_TIMEOUT, async {
        let e1 = rpc
            .invoke(&tl_fns::stories::ActivateStealthMode {
                past: true,
                future: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(e1, InvocationError::Rpc(ref r) if r.code == 403 && r.name == "PREMIUM_ACCOUNT_REQUIRED"));

        let e2 = rpc
            .invoke_handle_flood(&tl_fns::stories::SendStory {
                pinned: false,
                noforwards: false,
                fwd_modified: false,
                peer: tl_enums::InputPeer::Self_,
                media: tl_enums::InputMedia::Empty,
                media_areas: None,
                caption: None,
                entities: None,
                privacy_rules: vec![tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll],
                random_id: 1,
                period: Some(86_400),
                fwd_from_id: None,
                fwd_from_story: None,
                albums: None,
                music: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(e2, InvocationError::Rpc(ref r) if r.code == 420 && r.flood_wait_seconds() == Some(12)));

        let e3 = rpc
            .invoke(&tl_fns::stories::GetStoriesByID {
                peer: tl_enums::InputPeer::Self_,
                id: vec![99999],
            })
            .await
            .unwrap_err();
        assert!(matches!(e3, InvocationError::Rpc(ref r) if r.code == 400 && r.name == "STORY_ID_INVALID"));
    })
    .await
    .expect("story error paths timed out");
}
