//! Byte-level tdesktop conformance tests for text entities, message sending,
//! update state synchronization, and read state (`tg-text`, `tg-data`, `tg-api`,
//! `tg-session`) against an in-process scripted fake Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses obviously fake credentials (`api_id = 1`, `api_hash = "0000...0000"`),
//!   synthetic peer/user IDs, and an ephemeral test-only RSA keypair.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tg_api::convert::{
    entities_from_tl, entities_from_tl_with_self, entities_to_tl, entities_to_tl_for_sending,
    input_peer_for, input_reply_to_from_reply_info,
};
use tg_api::{
    AlbumMediaItem, ApiCredentials, ForwardItem, ForwardOptions, MessageSender,
    PendingMessageState, ReadHistoryManager, SendMessageErrorKind, SendOptions, UpdatesHandler,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    ChannelId, ChatId, DocumentId, EntityKind, FullMsgId, Message, MessageContent, MessageFlags,
    MsgId, MsgRange, PeerId, ReplyInfo, Session, TextEntity, TextWithEntities, UserId,
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
use tg_session::{
    MtprotoRpc, SessionError, SessionEvent, SessionRuntime, SessionRuntimeConfig,
    build_mtproto_client_config,
};
use tg_text::{
    EntityInText, EntityType, TextWithEntities as RichText, cut_part, parse_markdown, utf16_len,
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
// Test-only constants & helpers (no real credentials or personal data)
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
// Byte-Level Scripted Fake MTProto 2.0 Server
// ============================================================================

#[derive(Debug, Clone)]
struct RpcRecord {
    #[allow(dead_code)]
    dc_id: DcId,
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct IncomingRpc {
    #[allow(dead_code)]
    dc_id: DcId,
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
                                dc_id,
                                ctor: inner_ctor,
                                body: inner_body.clone(),
                            });

                            let handler = server.inner.handler.lock().unwrap().clone();
                            let action = handler(
                                &server,
                                IncomingRpc {
                                    dc_id,
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
                                ServerAction::RpcError(code, message) => {
                                    let err_bytes =
                                        mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                            error_code: code,
                                            error_message: message,
                                        })
                                        .to_bytes();
                                    let rpc_res = Boxed(RpcResult {
                                        req_msg_id: msg_id as i64,
                                        result: RawObject(err_bytes),
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

// ============================================================================
// TL Fixture Builders (exact Layer 233 shapes)
// ============================================================================

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

fn tl_user(id: i64, access_hash: i64, first: &str, min: bool) -> tl_enums::User {
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
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

fn tl_chat(id: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Chat(tl_types::Chat {
        creator: false,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 3,
        date: 1_700_000_000,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    })
}

fn tl_channel(
    id: i64,
    access_hash: i64,
    title: &str,
    broadcast: bool,
    min: bool,
) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: false,
        left: false,
        broadcast,
        verified: false,
        megagroup: !broadcast,
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
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        forum_sidebar: false,
        in_news: false,
        id,
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
        linked_monoforum_id: None,
        pinned_msg_id: None,
    })
}

fn tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from: Option<tl_enums::Peer>,
    out: bool,
    date: i32,
    text: &str,
    entities: Option<Vec<tl_enums::MessageEntity>>,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out,
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
        from_id: from,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities,
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
    })
}

fn tl_dialog(
    peer: tl_enums::Peer,
    top_message: i32,
    read_inbox_max_id: i32,
    read_outbox_max_id: i32,
    unread_count: i32,
    pts: Option<i32>,
) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer,
        top_message,
        read_inbox_max_id,
        read_outbox_max_id,
        unread_count,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
            tl_types::PeerNotifySettings {
                show_previews: None,
                silent: None,
                mute_until: None,
                ios_sound: None,
                android_sound: None,
                other_sound: None,
                stories_muted: None,
                stories_hide_sender: None,
                stories_ios_sound: None,
                stories_android_sound: None,
                stories_other_sound: None,
            },
        ),
        pts,
        draft: None,
        folder_id: None,
        ttl_period: None,
    })
}

fn tl_state(pts: i32, qts: i32, date: i32, seq: i32) -> tl_enums::updates::State {
    tl_enums::updates::State::State(tl_types::updates::State {
        pts,
        qts,
        date,
        seq,
        unread_count: 0,
    })
}

async fn wait_until<F>(desc: &str, timeout: Duration, mut pred: F)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for: {desc}");
}

// ============================================================================
// 1. Text entities: UTF-16, emoji/ZWJ/combining/RTL, Markdown, mention_name,
//    custom_emoji, blockquote/expandable, nested/overlapping, SkipLocal, internal:
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_entities_utf16_emoji_zwj_combining_rtl_and_special_entities_over_wire() {
    let self_uid = UserId(1001);
    let peer_uid = UserId(2002);
    let mention_uid = UserId(3003);
    let peer_id = PeerId::from_user(peer_uid);

    // 1A. Markdown & `cut_part` edge cases (`tg-text`):
    // - `\r\n` after Pre language tag must not leave a stray `\n` inside code block
    // - Nested inline formatting & emoji UTF-16 offsets
    let md_pre = parse_markdown("```rust\r\nfn main() {}\n```");
    assert_eq!(md_pre.text, "fn main() {}");
    assert_eq!(
        md_pre.entities,
        vec![EntityInText::with_data(EntityType::Pre, 0, 12, "rust")]
    );

    let md_emoji = parse_markdown("👨‍👩‍👧‍👦 **e\u{0301} __שלום__**");
    // "👨‍👩‍👧‍👦 " = 11 + 1 = 12 UTF-16 units; "e\u{0301} " = 3 UTF-16 units; "שלום" = 4 UTF-16 units
    assert_eq!(md_emoji.text, "👨‍👩‍👧‍👦 e\u{0301} שלום");
    assert!(
        md_emoji
            .entities
            .contains(&EntityInText::new(EntityType::Bold, 12, 7)),
        "expected Bold(12, 7), got {:?}",
        md_emoji.entities
    );
    assert!(
        md_emoji
            .entities
            .contains(&EntityInText::new(EntityType::Italic, 15, 4)),
        "expected Italic(15, 4), got {:?}",
        md_emoji.entities
    );

    // `cut_part` with nested Blockquote + Bold must preserve Bold inside chunk 0 AND split Blockquote
    let mut left = RichText {
        text: "Alpha bold text.\nSecond line here.".to_owned(),
        entities: vec![
            EntityInText::new(EntityType::Blockquote, 0, 34),
            EntityInText::new(EntityType::Bold, 6, 4),
            EntityInText::new(EntityType::Italic, 17, 6),
        ],
    };
    let mut first_part = RichText::new();
    assert!(cut_part(&mut first_part, &mut left, 20));
    assert_eq!(first_part.text, "Alpha bold text.");
    assert!(
        first_part
            .entities
            .contains(&EntityInText::new(EntityType::Blockquote, 0, 16))
    );
    assert!(
        first_part
            .entities
            .contains(&EntityInText::new(EntityType::Bold, 6, 4)),
        "nested Bold inside Blockquote must not be dropped by cut_part: {:?}",
        first_part.entities
    );
    assert_eq!(left.text, "\nSecond line here.");
    assert!(
        left.entities
            .contains(&EntityInText::new(EntityType::Blockquote, 0, 18))
    );
    assert!(
        left.entities
            .contains(&EntityInText::new(EntityType::Italic, 1, 6))
    );

    // 1B. Full UTF-16 entity conversion & wire round-trip over encrypted MTProto 2.0:
    let text = "😀 e\u{0301} 👨‍👩‍👧‍👦 👩‍💻 مرحبا שלום 🚀 @peer https://example.org";
    let off_combining = utf16_len("😀 ") as i32; // 3
    let off_zwj = utf16_len("😀 e\u{0301} ") as i32; // 6
    let off_tech = utf16_len("😀 e\u{0301} 👨‍👩‍👧‍👦 ") as i32; // 18
    let off_rtl = utf16_len("😀 e\u{0301} 👨‍👩‍👧‍👦 👩‍💻 ") as i32; // 24
    let off_rocket = utf16_len("😀 e\u{0301} 👨‍👩‍👧‍👦 👩‍💻 مرحبا שלום ") as i32; // 35
    let off_mention = utf16_len("😀 e\u{0301} 👨‍👩‍👧‍👦 👩‍💻 مرحبا שלום 🚀 ") as i32; // 38
    let off_url = utf16_len("😀 e\u{0301} 👨‍👩‍👧‍👦 👩‍💻 مرحبا שלום 🚀 @peer ") as i32; // 44

    let mut session = Session::new(self_uid);
    tg_api::convert::apply_tl_users_and_chats(
        &mut session,
        &[
            tl_user(peer_uid.0 as i64, 0x1111, "Peer Alpha", false),
            tl_user(mention_uid.0 as i64, 0x3333, "Peer Gamma", false),
        ],
        &[],
    );

    let domain_entities = vec![
        // Inner Bold inside outer Blockquote (given BEFORE outer to test canonical sorting)
        TextEntity::new(off_combining, 2, EntityKind::Bold),
        // Expandable blockquote spanning from combining mark through RTL
        TextEntity::new(
            off_combining,
            off_rocket - off_combining - 1,
            EntityKind::Blockquote { collapsed: true },
        ),
        // Regular non-collapsed blockquote on rocket
        TextEntity::new(off_rocket, 2, EntityKind::Blockquote { collapsed: false }),
        // Overlapping Italic + TextUrl on RTL text
        TextEntity::new(off_rtl, 10, EntityKind::Italic),
        TextEntity::new(
            off_rtl,
            10,
            EntityKind::TextUrl {
                url: "https://telegram.org".to_owned(),
            },
        ),
        // MentionName targeting Peer Gamma (3003, access_hash = 0x3333)
        TextEntity::new(
            off_tech,
            5,
            EntityKind::MentionName {
                user_id: mention_uid,
            },
        ),
        // CustomEmoji with real document_id on "🚀" (2 UTF-16 code units!)
        TextEntity::new(
            off_rocket,
            2,
            EntityKind::CustomEmoji {
                document_id: DocumentId(5_312_345_678_901_234_567),
                force_static: false,
            },
        ),
        // Must be DROPPED on send:
        TextEntity::new(off_zwj, 0, EntityKind::Bold),
        TextEntity::new(
            off_zwj,
            11,
            EntityKind::TextUrl {
                url: "INTERNAL:settings".to_owned(),
            },
        ),
        TextEntity::new(
            off_zwj,
            11,
            EntityKind::MentionName {
                user_id: UserId::ZERO,
            },
        ),
        TextEntity::new(
            off_zwj,
            11,
            EntityKind::CustomEmoji {
                document_id: DocumentId::ZERO,
                force_static: false,
            },
        ),
        // Local server-parsed entities omitted by `entities_to_tl_for_sending` (`SkipLocal`):
        TextEntity::new(off_mention, 5, EntityKind::Mention),
        TextEntity::new(off_url, 19, EntityKind::Url),
    ];

    let outgoing_tl = entities_to_tl_for_sending(&domain_entities, &session);
    assert_eq!(outgoing_tl.len(), 7, "outgoing_tl = {outgoing_tl:?}");
    assert!(
        matches!(
            &outgoing_tl[0],
            tl_enums::MessageEntity::Blockquote(b) if b.offset == off_combining && b.collapsed
        ),
        "outer Blockquote must precede inner Bold: {:?}",
        outgoing_tl[0]
    );
    assert!(
        matches!(
            &outgoing_tl[1],
            tl_enums::MessageEntity::Bold(b) if b.offset == off_combining && b.length == 2
        ),
        "inner Bold expected second: {:?}",
        outgoing_tl[1]
    );
    assert!(outgoing_tl.iter().any(|e| matches!(
        e,
        tl_enums::MessageEntity::InputMessageEntityMentionName(m)
            if m.offset == off_tech
                && m.length == 5
                && matches!(
                    &m.user_id,
                    tl_enums::InputUser::InputUser(u) if u.user_id == 3003 && u.access_hash == 0x3333
                )
    )));
    assert!(outgoing_tl.iter().any(|e| matches!(
        e,
        tl_enums::MessageEntity::CustomEmoji(c)
            if c.offset == off_rocket
                && c.length == 2
                && c.document_id == 5_312_345_678_901_234_567
    )));
    assert_eq!(entities_to_tl(&domain_entities, &session).len(), 9);

    let server = ScriptedServer::new(move |_srv, req| match req.ctor {
        x if x == tl_fns::messages::SendMessage::CONSTRUCTOR_ID => {
            let sm = tl_fns::messages::SendMessage::from_bytes(&req.body).unwrap();
            let mut echoed = sm.entities.unwrap_or_default();
            echoed.push(tl_enums::MessageEntity::TextUrl(
                tl_types::MessageEntityTextUrl {
                    offset: 0,
                    length: 2,
                    url: "internal:evil".to_owned(),
                },
            ));
            echoed.push(tl_enums::MessageEntity::InputMessageEntityMentionName(
                tl_types::InputMessageEntityMentionName {
                    offset: 0,
                    length: 2,
                    user_id: tl_enums::InputUser::Self_,
                },
            ));
            let msg = tl_message(
                501,
                tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: peer_uid.0 as i64,
                }),
                Some(tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: self_uid.0 as i64,
                })),
                true,
                1_700_000_100,
                &sm.message,
                Some(echoed),
            );
            ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                        id: 501,
                        random_id: sm.random_id,
                    }),
                    tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                        message: msg,
                        pts: 11,
                        pts_count: 1,
                    }),
                ],
                users: vec![],
                chats: vec![],
                date: 1_700_000_100,
                seq: 1,
            }))
        }
        other => panic!("unexpected rpc ctor 0x{other:08x}"),
    });

    let (client, rpc) = build_client(&server, 2);
    let mut sender = MessageSender::default();
    let mut updates = UpdatesHandler::new();
    updates
        .state_machine_mut()
        .init_state(10, 1_700_000_000, 0, 0, 0);

    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        sender.send_message(
            &rpc,
            input_peer_for(&session, peer_id),
            text,
            outgoing_tl.clone(),
            None,
            SendOptions::default(),
        ),
    )
    .await
    .expect("timeout")
    .expect("send_message ok");

    let sent_req = tl_fns::messages::SendMessage::from_bytes(
        &server.rpcs_of::<tl_fns::messages::SendMessage>()[0].body,
    )
    .unwrap();
    assert_eq!(sent_req.message, text);
    assert_eq!(sent_req.entities.as_deref(), Some(outgoing_tl.as_slice()));

    updates
        .handle_updates(
            &rpc,
            &mut session,
            outcome.updates[0].clone(),
            Some(outcome.messages[0].random_id),
            100,
        )
        .await
        .expect("handle_updates ok");

    let stored = session
        .message(FullMsgId::new(peer_id, MsgId(501)))
        .expect("message 501 stored");
    let MessageContent::Regular {
        text: stored_txt, ..
    } = &stored.content
    else {
        panic!("expected regular text message");
    };
    assert_eq!(stored_txt.text, text);
    assert!(
        !stored_txt.entities.iter().any(
            |e| matches!(&e.kind, EntityKind::TextUrl { url } if url.starts_with("internal:"))
        ),
        "internal: URL leaked into received entities: {:?}",
        stored_txt.entities
    );
    assert!(
        stored_txt.entities.iter().any(|e| matches!(
            e.kind,
            EntityKind::MentionName { user_id } if user_id == self_uid
        )),
        "Self_ mention name did not resolve to self_uid: {:?}",
        stored_txt.entities
    );
    assert!(
        entities_from_tl(&[tl_enums::MessageEntity::TextUrl(
            tl_types::MessageEntityTextUrl {
                offset: 0,
                length: 4,
                url: "Internal:foo".to_owned(),
            },
        )])
        .is_empty()
    );
    assert_eq!(
        entities_from_tl_with_self(
            &[tl_enums::MessageEntity::InputMessageEntityMentionName(
                tl_types::InputMessageEntityMentionName {
                    offset: 0,
                    length: 2,
                    user_id: tl_enums::InputUser::Self_,
                },
            )],
            self_uid,
        ),
        vec![TextEntity::new(
            0,
            2,
            EntityKind::MentionName { user_id: self_uid }
        )]
    );

    client.shutdown();
}

// ============================================================================
// 2. Sending: options, reply_to (quote+offset+top_msg_id), multi-chunk UTF-16
//    entities & reply_to preservation, updateMessageID + updateNewMessage &
//    updateShortSentMessage merging, reversed order protection
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_message_options_reply_to_chunks_and_pending_merge_over_wire() {
    let self_uid = UserId(1001);
    let peer_uid = UserId(2002);
    let send_as_ch = ChannelId(9001);
    let peer_id = PeerId::from_user(peer_uid);
    let send_as_peer_id = PeerId::from_channel(send_as_ch);

    let mut session = Session::new(self_uid);
    tg_api::convert::apply_tl_users_and_chats(
        &mut session,
        &[tl_user(peer_uid.0 as i64, 0x2222, "Peer Alpha", false)],
        &[tl_channel(
            send_as_ch.0 as i64,
            0x9999,
            "Alias Channel",
            true,
            false,
        )],
    );

    let empty_quote_reply = ReplyInfo {
        reply_to_msg_id: Some(MsgId(42)),
        top_msg_id: Some(MsgId(7)),
        quote_text: Some(TextWithEntities::plain("")),
        quote_offset: Some(15),
        ..ReplyInfo::default()
    };
    let tl_empty_quote = input_reply_to_from_reply_info(&empty_quote_reply, &session).unwrap();
    match tl_empty_quote {
        tl_enums::InputReplyTo::Message(m) => {
            assert_eq!(m.reply_to_msg_id, 42);
            assert_eq!(m.top_msg_id, Some(7));
            assert_eq!(m.quote_text, None, "empty quote_text must become None");
            assert_eq!(
                m.quote_offset, None,
                "quote_offset must be None when quote_text is empty"
            );
        }
        other => panic!("expected InputReplyTo::Message, got {other:?}"),
    }

    let rich_reply = ReplyInfo {
        reply_to_msg_id: Some(MsgId(42)),
        top_msg_id: Some(MsgId(7)),
        quote_text: Some(TextWithEntities::new(
            "😀 quoted https://x.test",
            vec![
                TextEntity::new(3, 6, EntityKind::Bold),
                TextEntity::new(10, 14, EntityKind::Url),
            ],
        )),
        quote_offset: Some(9),
        ..ReplyInfo::default()
    };
    let tl_rich_reply = input_reply_to_from_reply_info(&rich_reply, &session).unwrap();

    let call_num = Arc::new(AtomicU64::new(0));
    let call_num_srv = Arc::clone(&call_num);

    let server = ScriptedServer::new(move |_srv, req| match req.ctor {
        x if x == tl_fns::messages::SendMessage::CONSTRUCTOR_ID => {
            let sm = tl_fns::messages::SendMessage::from_bytes(&req.body).unwrap();
            let n = call_num_srv.fetch_add(1, Ordering::SeqCst);
            if n == 0 || n == 1 {
                let server_id = 600 + n as i32;
                let pts = 21 + n as i32;
                let msg = tl_message(
                    server_id,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: self_uid.0 as i64,
                    })),
                    true,
                    1_700_000_200 + n as i32,
                    &sm.message,
                    sm.entities.clone(),
                );
                let updates_vec = if n == 0 {
                    vec![
                        tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                            id: server_id,
                            random_id: sm.random_id,
                        }),
                        tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                            message: msg,
                            pts,
                            pts_count: 1,
                        }),
                    ]
                } else {
                    // Reversed: NewMessage before MessageID
                    vec![
                        tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                            message: msg,
                            pts,
                            pts_count: 1,
                        }),
                        tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                            id: server_id,
                            random_id: sm.random_id,
                        }),
                    ]
                };
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates: updates_vec,
                    users: vec![],
                    chats: vec![],
                    date: 1_700_000_200 + n as i32,
                    seq: 1 + n as i32,
                }))
            } else {
                ServerAction::ok(&tl_enums::Updates::UpdateShortSentMessage(
                    tl_types::UpdateShortSentMessage {
                        out: true,
                        id: 610,
                        pts: 23,
                        pts_count: 1,
                        date: 1_700_000_300,
                        media: None,
                        entities: None,
                        ttl_period: Some(86400),
                    },
                ))
            }
        }
        other => panic!("unexpected rpc ctor 0x{other:08x}"),
    });

    let (client, rpc) = build_client(&server, 2);
    let mut sender = MessageSender::default();
    sender.set_message_length_limit(10);
    let mut updates = UpdatesHandler::new();
    updates
        .state_machine_mut()
        .init_state(20, 1_700_000_000, 0, 0, 0);

    let send_opts = SendOptions {
        silent: true,
        no_webpage: true,
        clear_draft: true,
        noforwards: true,
        invert_media: true,
        schedule_date: Some(1_700_050_000),
        schedule_repeat_period: None,
        send_as: Some(input_peer_for(&session, send_as_peer_id)),
        effect_id: Some(5_104_841_245_755_180_586),
        allow_paid_stars: None,
    };

    let full_text = "  😀123456789abcdefghij";
    let raw_entities = vec![tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
        offset: 4,
        length: 15,
    })];

    let prepared = sender.prepare_text_chunks(peer_id, full_text, &raw_entities);
    assert_eq!(prepared.len(), 2, "expected 2 chunks");
    assert_ne!(prepared[0].random_id, prepared[1].random_id);
    assert_ne!(prepared[0].random_id, 0);
    assert_ne!(prepared[1].random_id, 0);

    for chunk in &prepared {
        let mut local = Message::new_text(
            peer_id,
            chunk.local_id.msg,
            Some(PeerId::from_user(self_uid)),
            1_700_000_190,
            TextWithEntities::new(
                chunk.text.clone(),
                entities_from_tl_with_self(&chunk.entities, self_uid),
            ),
        );
        local
            .flags
            .insert(MessageFlags::OUTGOING | MessageFlags::BEING_SENT);
        session.register_random_id(chunk.random_id, chunk.local_id);
        session.insert_message(local);

        let (pending, batch) = sender
            .send_prepared_chunk(
                &rpc,
                input_peer_for(&session, peer_id),
                chunk,
                Some(tl_rich_reply.clone()),
                &send_opts,
            )
            .await
            .expect("send chunk ok");
        assert!(matches!(pending.state, PendingMessageState::Sent { .. }));
        updates
            .handle_updates(&rpc, &mut session, batch, Some(chunk.random_id), 200)
            .await
            .expect("apply chunk updates");
    }

    let sent_rpcs = server.rpcs_of::<tl_fns::messages::SendMessage>();
    assert_eq!(sent_rpcs.len(), 2);
    let r0 = tl_fns::messages::SendMessage::from_bytes(&sent_rpcs[0].body).unwrap();
    let r1 = tl_fns::messages::SendMessage::from_bytes(&sent_rpcs[1].body).unwrap();

    for r in [&r0, &r1] {
        assert!(r.silent);
        assert!(r.no_webpage);
        assert!(r.clear_draft);
        assert!(r.noforwards);
        assert!(r.invert_media);
        assert_eq!(r.schedule_date, Some(1_700_050_000));
        assert_eq!(r.effect, Some(5_104_841_245_755_180_586));
        assert!(matches!(
            &r.send_as,
            Some(tl_enums::InputPeer::Channel(c)) if c.channel_id == 9001 && c.access_hash == 0x9999
        ));
        match &r.reply_to {
            Some(tl_enums::InputReplyTo::Message(rm)) => {
                assert_eq!(rm.reply_to_msg_id, 42);
                assert_eq!(rm.top_msg_id, Some(7));
                assert_eq!(rm.quote_text.as_deref(), Some("😀 quoted https://x.test"));
                assert_eq!(rm.quote_offset, Some(9));
                assert_eq!(
                    rm.quote_entities.as_deref(),
                    Some(
                        &[tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                            offset: 3,
                            length: 6,
                        })][..]
                    )
                );
            }
            other => panic!("expected InputReplyTo::Message on every chunk, got {other:?}"),
        }
    }
    assert_eq!(r0.message, "😀123456789");
    assert_eq!(
        r0.entities,
        Some(vec![tl_enums::MessageEntity::Bold(
            tl_types::MessageEntityBold {
                offset: 2,
                length: 9,
            }
        )])
    );
    assert_eq!(r1.message, "abcdefghij");
    assert_eq!(
        r1.entities,
        Some(vec![tl_enums::MessageEntity::Bold(
            tl_types::MessageEntityBold {
                offset: 0,
                length: 6,
            }
        )])
    );

    for (idx, sid) in [MsgId(600), MsgId(601)].into_iter().enumerate() {
        assert!(session.message(prepared[idx].local_id).is_none());
        let m = session.message(FullMsgId::new(peer_id, sid)).unwrap();
        assert!(!m.flags.contains(MessageFlags::BEING_SENT));
        assert_eq!(m.date, 1_700_000_200 + idx as i32);
    }

    // UpdateShortSentMessage merging: server returns `entities: None` -> clears local entities.
    let short_chunk = sender.prepare_text_chunks(
        peer_id,
        "short",
        &[tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
            offset: 0,
            length: 5,
        })],
    );
    let sc = &short_chunk[0];
    let mut local_short = Message::new_text(
        peer_id,
        sc.local_id.msg,
        Some(PeerId::from_user(self_uid)),
        1_700_000_250,
        TextWithEntities::new("short", vec![TextEntity::new(0, 5, EntityKind::Bold)]),
    );
    local_short
        .flags
        .insert(MessageFlags::OUTGOING | MessageFlags::BEING_SENT);
    session.register_random_id(sc.random_id, sc.local_id);
    session.insert_message(local_short);

    let (short_pending, short_batch) = sender
        .send_prepared_chunk(
            &rpc,
            input_peer_for(&session, peer_id),
            sc,
            None,
            &SendOptions::default(),
        )
        .await
        .expect("short send ok");
    assert_eq!(
        short_pending.state,
        PendingMessageState::Sent {
            server_id: MsgId(610)
        }
    );
    updates
        .handle_updates(&rpc, &mut session, short_batch, Some(sc.random_id), 300)
        .await
        .expect("apply short sent");

    assert!(session.message(sc.local_id).is_none());
    let merged_short = session
        .message(FullMsgId::new(peer_id, MsgId(610)))
        .expect("promoted message 610");
    assert_eq!(merged_short.date, 1_700_000_300);
    assert_eq!(merged_short.ttl_period, Some(86400));
    assert!(!merged_short.flags.contains(MessageFlags::BEING_SENT));
    let MessageContent::Regular { text: st, .. } = &merged_short.content else {
        panic!("expected regular text");
    };
    assert_eq!(st.text, "short");
    assert!(
        st.entities.is_empty(),
        "UpdateShortSentMessage with entities: None must clear local entities, got {:?}",
        st.entities
    );

    client.shutdown();
}

// ============================================================================
// 3. Send failure: PEER_FLOOD marks SENDING_FAILED in SessionRuntime;
//    MESSAGE_EMPTY destroys the local pending message
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_failure_marks_failed_or_destroys_on_message_empty_over_wire() {
    let self_uid = UserId(1001);
    let peer_uid = UserId(2002);
    let peer_id = PeerId::from_user(peer_uid);

    let send_count = Arc::new(AtomicU64::new(0));
    let send_count_srv = Arc::clone(&send_count);

    let server = ScriptedServer::new(move |_srv, req| match req.ctor {
        x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_state(10, 0, 1_700_000_000, 1))
        }
        x if x == tl_fns::messages::SendMessage::CONSTRUCTOR_ID => {
            let n = send_count_srv.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ServerAction::err(400, "PEER_FLOOD")
            } else {
                ServerAction::err(400, "MESSAGE_EMPTY")
            }
        }
        other => panic!("unexpected rpc ctor 0x{other:08x}"),
    });

    let (client, rpc) = build_client(&server, 2);
    let runtime = SessionRuntime::start_mtproto_on(
        self_uid,
        rpc,
        tokio::runtime::Handle::current(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");

    wait_until("updates.getState bootstrapped", TEST_TIMEOUT, || {
        !server.rpcs_of::<tl_fns::updates::GetState>().is_empty()
    })
    .await;

    {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_user(peer_uid.0 as i64, 0x2222, "Peer Alpha", false)],
            &[],
        );
    }

    // First send fails with PEER_FLOOD -> local pending message stays in Session with SENDING_FAILED.
    let err1 = runtime
        .send_message(peer_id, "will fail", SendOptions::default())
        .await
        .expect_err("expected PEER_FLOOD");
    assert!(matches!(
        err1,
        SessionError::SendMessage(SendMessageErrorKind::PeerFlood)
    ));
    {
        let s = runtime.session().read().await;
        let top = s.history(peer_id).and_then(|h| h.top_message_id).unwrap();
        let failed = s.message(FullMsgId::new(peer_id, top)).unwrap();
        assert!(failed.id.is_client());
        assert!(failed.flags.contains(MessageFlags::SENDING_FAILED));
        assert!(!failed.flags.contains(MessageFlags::BEING_SENT));
    }

    // Second send fails with MESSAGE_EMPTY -> local pending message is destroyed from Session!
    let err2 = runtime
        .send_message(peer_id, "   ", SendOptions::default())
        .await
        .expect_err("expected MESSAGE_EMPTY");
    assert!(matches!(
        err2,
        SessionError::SendMessage(SendMessageErrorKind::MessageEmpty)
    ));
    {
        let s = runtime.session().read().await;
        let top = s.history(peer_id).unwrap().top_message_id.unwrap();
        let top_msg = s.message(FullMsgId::new(peer_id, top)).unwrap();
        let MessageContent::Regular { text, .. } = &top_msg.content else {
            panic!("expected regular text");
        };
        assert_eq!(text.text, "will fail");
    }

    runtime.shutdown();
    client.shutdown();
}

// ============================================================================
// 4. sendMedia, sendMultiMedia (album grouped_id + unique random_ids),
//    and forwardMessages (unique random_ids, noforwards, kGeneralId omission)
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_media_send_multi_media_album_and_forward_messages_over_wire() {
    let self_uid = UserId(1001);
    let peer_uid = UserId(2002);
    let ch_id = ChannelId(7777);
    let peer_id = PeerId::from_user(peer_uid);
    let ch_peer_id = PeerId::from_channel(ch_id);

    let mut session = Session::new(self_uid);
    tg_api::convert::apply_tl_users_and_chats(
        &mut session,
        &[tl_user(peer_uid.0 as i64, 0x2222, "Peer Alpha", false)],
        &[tl_channel(
            ch_id.0 as i64,
            0x7777,
            "Topic Group",
            false,
            false,
        )],
    );

    let server_grouped_id: i64 = 0x5544_3322_1100_9988;

    let server = ScriptedServer::new(move |_srv, req| match req.ctor {
        x if x == tl_fns::messages::SendMedia::CONSTRUCTOR_ID => {
            let sm = tl_fns::messages::SendMedia::from_bytes(&req.body).unwrap();
            let msg = tl_message(
                701,
                tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: peer_uid.0 as i64,
                }),
                Some(tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: self_uid.0 as i64,
                })),
                true,
                1_700_000_400,
                &sm.message,
                sm.entities.clone(),
            );
            ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                        id: 701,
                        random_id: sm.random_id,
                    }),
                    tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                        message: msg,
                        pts: 31,
                        pts_count: 1,
                    }),
                ],
                users: vec![],
                chats: vec![],
                date: 1_700_000_400,
                seq: 1,
            }))
        }
        x if x == tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID => {
            let smm = tl_fns::messages::SendMultiMedia::from_bytes(&req.body).unwrap();
            let mut updates_vec = Vec::new();
            for (idx, item) in smm.multi_media.iter().enumerate() {
                let tl_enums::InputSingleMedia::InputSingleMedia(single) = item;
                let server_id = 710 + idx as i32;
                updates_vec.push(tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                    id: server_id,
                    random_id: single.random_id,
                }));
                let mut msg = tl_message(
                    server_id,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: self_uid.0 as i64,
                    })),
                    true,
                    1_700_000_410,
                    &single.message,
                    single.entities.clone(),
                );
                if let tl_enums::Message::Message(ref mut m) = msg {
                    m.grouped_id = Some(server_grouped_id);
                }
                updates_vec.push(tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                    message: msg,
                    pts: 32 + idx as i32,
                    pts_count: 1,
                }));
            }
            ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                updates: updates_vec,
                users: vec![],
                chats: vec![],
                date: 1_700_000_410,
                seq: 2,
            }))
        }
        x if x == tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID => {
            let fm = tl_fns::messages::ForwardMessages::from_bytes(&req.body).unwrap();
            let mut updates_vec = Vec::new();
            for (idx, &rand_id) in fm.random_id.iter().enumerate() {
                updates_vec.push(tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                    id: 750 + idx as i32,
                    random_id: rand_id,
                }));
            }
            ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                updates: updates_vec,
                users: vec![],
                chats: vec![],
                date: 1_700_000_420,
                seq: 3,
            }))
        }
        other => panic!("unexpected rpc ctor 0x{other:08x}"),
    });

    let (client, rpc) = build_client(&server, 2);
    let mut sender = MessageSender::default();
    let mut updates = UpdatesHandler::new();
    updates
        .state_machine_mut()
        .init_state(30, 1_700_000_000, 0, 0, 0);

    let opts = SendOptions {
        silent: true,
        noforwards: true,
        invert_media: true,
        schedule_date: Some(1_700_060_000),
        effect_id: Some(9_999_111),
        ..SendOptions::default()
    };

    let dummy_photo = tl_enums::InputMedia::Photo(tl_types::InputMediaPhoto {
        spoiler: false,
        live_photo: false,
        id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
            id: 100,
            access_hash: 200,
            file_reference: vec![1, 2, 3],
        }),
        ttl_seconds: None,
        video: None,
    });
    let media_outcome = sender
        .send_media(
            &rpc,
            input_peer_for(&session, peer_id),
            dummy_photo.clone(),
            "😀 caption",
            vec![tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                offset: 3,
                length: 7,
            })],
            None,
            opts.clone(),
        )
        .await
        .expect("send_media ok");
    assert_eq!(
        media_outcome.message.state,
        PendingMessageState::Sent {
            server_id: MsgId(701)
        }
    );
    let wire_media = tl_fns::messages::SendMedia::from_bytes(
        &server.rpcs_of::<tl_fns::messages::SendMedia>()[0].body,
    )
    .unwrap();
    assert!(wire_media.silent);
    assert!(wire_media.noforwards);
    assert!(wire_media.invert_media);
    assert_eq!(wire_media.schedule_date, Some(1_700_060_000));
    assert_eq!(wire_media.effect, Some(9_999_111));
    assert_ne!(wire_media.random_id, 0);

    updates
        .handle_updates(
            &rpc,
            &mut session,
            media_outcome.updates,
            Some(media_outcome.message.random_id),
            100,
        )
        .await
        .expect("apply send_media updates");

    let album_items = vec![
        AlbumMediaItem {
            media: dummy_photo.clone(),
            caption: "Album cap".to_owned(),
            entities: vec![tl_enums::MessageEntity::Italic(
                tl_types::MessageEntityItalic {
                    offset: 0,
                    length: 5,
                },
            )],
        },
        AlbumMediaItem {
            media: dummy_photo.clone(),
            caption: String::new(),
            entities: vec![],
        },
        AlbumMediaItem {
            media: dummy_photo,
            caption: String::new(),
            entities: vec![],
        },
    ];
    let album_outcome = sender
        .send_multi_media(
            &rpc,
            input_peer_for(&session, peer_id),
            album_items,
            None,
            opts.clone(),
        )
        .await
        .expect("send_multi_media ok");
    assert_ne!(album_outcome.grouped_id, 0);
    assert_eq!(album_outcome.messages.len(), 3);
    let rand_set: HashSet<i64> = album_outcome.messages.iter().map(|m| m.random_id).collect();
    assert_eq!(
        rand_set.len(),
        3,
        "every album item needs a unique random_id"
    );
    assert_eq!(
        album_outcome.messages[0].state,
        PendingMessageState::Sent {
            server_id: MsgId(710)
        }
    );
    assert_eq!(
        album_outcome.messages[2].state,
        PendingMessageState::Sent {
            server_id: MsgId(712)
        }
    );

    let wire_album = tl_fns::messages::SendMultiMedia::from_bytes(
        &server.rpcs_of::<tl_fns::messages::SendMultiMedia>()[0].body,
    )
    .unwrap();
    assert_eq!(wire_album.multi_media.len(), 3);
    assert!(wire_album.noforwards);
    assert!(wire_album.invert_media);
    assert!(wire_album.silent);
    assert_eq!(wire_album.effect, Some(9_999_111));

    updates
        .handle_updates(&rpc, &mut session, album_outcome.updates, None, 200)
        .await
        .expect("apply album updates");
    for sid in [MsgId(710), MsgId(711), MsgId(712)] {
        let m = session
            .message(FullMsgId::new(peer_id, sid))
            .expect("album item stored");
        assert_eq!(m.grouped_id, Some(server_grouped_id as u64));
    }

    let fwd_items = vec![
        ForwardItem {
            from_peer: input_peer_for(&session, peer_id),
            msg_id: MsgId(710),
        },
        ForwardItem {
            from_peer: input_peer_for(&session, peer_id),
            msg_id: MsgId(711),
        },
    ];
    sender
        .forward_messages(
            &rpc,
            &fwd_items,
            input_peer_for(&session, ch_peer_id),
            Some(MsgId(1)),
            ForwardOptions::PreserveInfo,
            opts.clone(),
        )
        .await
        .expect("forward to general ok");
    sender
        .forward_messages(
            &rpc,
            &fwd_items,
            input_peer_for(&session, ch_peer_id),
            Some(MsgId(42)),
            ForwardOptions::NoSenderNames,
            opts,
        )
        .await
        .expect("forward to topic 42 ok");

    let fwd_rpcs = server.rpcs_of::<tl_fns::messages::ForwardMessages>();
    assert_eq!(fwd_rpcs.len(), 2);
    let f0 = tl_fns::messages::ForwardMessages::from_bytes(&fwd_rpcs[0].body).unwrap();
    let f1 = tl_fns::messages::ForwardMessages::from_bytes(&fwd_rpcs[1].body).unwrap();
    assert_eq!(
        f0.top_msg_id, None,
        "General topic (1) must omit top_msg_id"
    );
    assert!(f0.noforwards);
    assert_eq!(f0.random_id.len(), 2);
    assert_ne!(f0.random_id[0], f0.random_id[1]);
    assert_eq!(f1.top_msg_id, Some(42));
    assert!(f1.drop_author);
    assert!(f1.noforwards);
    assert_ne!(f1.random_id[0], f1.random_id[1]);
    assert_ne!(f0.random_id[0], f1.random_id[0]);

    client.shutdown();
}

// ============================================================================
// 5. Update state: pts/pts_count/qts/seq/date, gap fill & timeout getDifference,
//    updatesTooLong, differenceSlice loop, differenceTooLong dialog refetch,
//    updateShortMessage/ChatMessage loaded vs missing/min, updatesCombined seq
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updates_pts_qts_seq_gap_fill_timeout_slice_too_long_and_combined_over_wire() {
    let self_uid = UserId(1001);
    let peer_uid = UserId(2002);
    let missing_uid = UserId(4004);
    let min_uid = UserId(5005);
    let chat_id = ChatId(6006);
    let peer_id = PeerId::from_user(peer_uid);
    let chat_peer_id = PeerId::from_chat(chat_id);

    let diff_responses: Arc<Mutex<VecDeque<tl_enums::updates::Difference>>> =
        Arc::new(Mutex::new(VecDeque::new()));
    let diff_responses_srv = Arc::clone(&diff_responses);

    {
        let mut q = diff_responses.lock().unwrap();
        q.push_back(tl_enums::updates::Difference::Slice(
            tl_types::updates::DifferenceSlice {
                new_messages: vec![tl_message(
                    104,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    })),
                    false,
                    1_700_001_004,
                    "from slice 1",
                    None,
                )],
                new_encrypted_messages: vec![],
                other_updates: vec![],
                chats: vec![],
                users: vec![],
                intermediate_state: tl_state(14, 1, 1_700_001_004, 2),
            },
        ));
        q.push_back(tl_enums::updates::Difference::Difference(
            tl_types::updates::Difference {
                new_messages: vec![tl_message(
                    105,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    })),
                    false,
                    1_700_001_005,
                    "from final diff",
                    None,
                )],
                new_encrypted_messages: vec![],
                other_updates: vec![],
                chats: vec![],
                users: vec![],
                state: tl_state(15, 2, 1_700_001_005, 3),
            },
        ));
        q.push_back(tl_enums::updates::Difference::Difference(
            tl_types::updates::Difference {
                new_messages: vec![tl_message(
                    106,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: missing_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: missing_uid.0 as i64,
                    })),
                    false,
                    1_700_001_006,
                    "from missing user diff",
                    None,
                )],
                new_encrypted_messages: vec![],
                other_updates: vec![],
                chats: vec![],
                users: vec![tl_user(missing_uid.0 as i64, 0x4444, "Peer Delta", false)],
                state: tl_state(16, 2, 1_700_001_006, 4),
            },
        ));
        q.push_back(tl_enums::updates::Difference::Difference(
            tl_types::updates::Difference {
                new_messages: vec![tl_message(
                    107,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: min_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: min_uid.0 as i64,
                    })),
                    false,
                    1_700_001_007,
                    "from min user diff",
                    None,
                )],
                new_encrypted_messages: vec![],
                other_updates: vec![],
                chats: vec![],
                users: vec![tl_user(min_uid.0 as i64, 0x5555, "Peer Epsilon", false)],
                state: tl_state(17, 2, 1_700_001_007, 5),
            },
        ));
        q.push_back(tl_enums::updates::Difference::TooLong(
            tl_types::updates::DifferenceTooLong { pts: 200 },
        ));
    }

    let server = ScriptedServer::new(move |_srv, req| match req.ctor {
        x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_state(10, 0, 1_700_001_000, 1))
        }
        x if x == tl_fns::updates::GetDifference::CONSTRUCTOR_ID => {
            let next = diff_responses_srv
                .lock()
                .unwrap()
                .pop_front()
                .expect("queued difference response");
            ServerAction::ok(&next)
        }
        x if x == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: vec![tl_dialog(
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    999,
                    990,
                    990,
                    3,
                    None,
                )],
                messages: vec![tl_message(
                    999,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    })),
                    false,
                    1_700_002_000,
                    "refetched dialog top",
                    None,
                )],
                chats: vec![],
                users: vec![tl_user(peer_uid.0 as i64, 0x2222, "Peer Alpha", false)],
            }),
        ),
        x if x == tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                dialogs: vec![],
                messages: vec![],
                chats: vec![],
                users: vec![],
                state: tl_state(200, 2, 1_700_002_000, 5),
            }),
        ),
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
        other => panic!("unexpected rpc ctor 0x{other:08x}"),
    });

    let (client, rpc) = build_client(&server, 2);
    let runtime = SessionRuntime::start_mtproto_on(
        self_uid,
        rpc,
        tokio::runtime::Handle::current(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let mut events_rx = runtime.subscribe_events();

    wait_until("updates.getState bootstrapped", TEST_TIMEOUT, || {
        !server.rpcs_of::<tl_fns::updates::GetState>().is_empty()
    })
    .await;

    {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[
                tl_user(peer_uid.0 as i64, 0x2222, "Peer Alpha", false),
                tl_user(min_uid.0 as i64, 0x5555, "Min Peer", true),
            ],
            &[tl_chat(chat_id.0 as i64, "Group Chat")],
        );
    }

    // 5A. Exact pts == local + count (10 + 1 == 11) via UpdateShortMessage (user 2002 loaded!)
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 101,
            user_id: peer_uid.0 as i64,
            message: "short pm".to_owned(),
            pts: 11,
            pts_count: 1,
            date: 1_700_001_001,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        })
    ));
    wait_until("msg 101 applied", TEST_TIMEOUT, || {
        runtime
            .session()
            .try_read()
            .is_ok_and(|s| s.message(FullMsgId::new(peer_id, MsgId(101))).is_some())
    })
    .await;
    assert_eq!(server.rpcs_of::<tl_fns::updates::GetDifference>().len(), 0);

    // 5B. Duplicate pts <= local (pts=11) skipped without calling getDifference.
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 9999,
            user_id: peer_uid.0 as i64,
            message: "duplicate".to_owned(),
            pts: 11,
            pts_count: 1,
            date: 1_700_001_001,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        })
    ));

    // 5C. Gap fill within wait window: pts=13 arrives BEFORE pts=12 (UpdateShortChatMessage)!
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShortChatMessage(tl_types::UpdateShortChatMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 103,
            from_id: peer_uid.0 as i64,
            chat_id: chat_id.0 as i64,
            message: "chat 103".to_owned(),
            pts: 13,
            pts_count: 1,
            date: 1_700_001_003,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        })
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        runtime
            .session()
            .read()
            .await
            .message(FullMsgId::new(chat_peer_id, MsgId(103)))
            .is_none()
    );
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShortChatMessage(tl_types::UpdateShortChatMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 102,
            from_id: peer_uid.0 as i64,
            chat_id: chat_id.0 as i64,
            message: "chat 102".to_owned(),
            pts: 12,
            pts_count: 1,
            date: 1_700_001_002,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        })
    ));
    wait_until("gap filled 102 & 103", TEST_TIMEOUT, || {
        runtime.session().try_read().is_ok_and(|s| {
            s.message(FullMsgId::new(chat_peer_id, MsgId(102)))
                .is_some()
                && s.message(FullMsgId::new(chat_peer_id, MsgId(103)))
                    .is_some()
        })
    })
    .await;
    assert!(
        runtime
            .session()
            .read()
            .await
            .message(FullMsgId::new(peer_id, MsgId(9999)))
            .is_none()
    );
    assert_eq!(server.rpcs_of::<tl_fns::updates::GetDifference>().len(), 0);

    // 5D. Unfilled PTS gap (pts=15 while local=13) -> waits ~1s -> calls updates.getDifference
    // -> loops differenceSlice (pts=14, qts=1) -> final difference (pts=15, qts=2, seq=3)!
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 105,
            user_id: peer_uid.0 as i64,
            message: "gapped".to_owned(),
            pts: 15,
            pts_count: 1,
            date: 1_700_001_005,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        })
    ));
    wait_until("slice + final diff applied", TEST_TIMEOUT, || {
        runtime.session().try_read().is_ok_and(|s| {
            s.message(FullMsgId::new(peer_id, MsgId(104))).is_some()
                && s.message(FullMsgId::new(peer_id, MsgId(105))).is_some()
        })
    })
    .await;
    let diff_rpcs = server.rpcs_of::<tl_fns::updates::GetDifference>();
    assert_eq!(diff_rpcs.len(), 2, "expected slice + final getDifference");
    let gd0 = tl_fns::updates::GetDifference::from_bytes(&diff_rpcs[0].body).unwrap();
    let gd1 = tl_fns::updates::GetDifference::from_bytes(&diff_rpcs[1].body).unwrap();
    assert_eq!(gd0.pts, 13);
    assert_eq!(gd1.pts, 14);
    assert_eq!(gd1.qts, 1);

    // 5E. UpdateShortMessage for UNLOADED user 4004 -> immediate getDifference!
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 106,
            user_id: missing_uid.0 as i64,
            message: "from unknown".to_owned(),
            pts: 16,
            pts_count: 1,
            date: 1_700_001_006,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        })
    ));
    wait_until("missing user diff applied", TEST_TIMEOUT, || {
        runtime.session().try_read().is_ok_and(|s| {
            s.user_loaded(missing_uid)
                && s.message(FullMsgId::new(PeerId::from_user(missing_uid), MsgId(106)))
                    .is_some()
        })
    })
    .await;

    // 5F. UpdateShortMessage for `min`-only user 5005 -> immediate getDifference!
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 107,
            user_id: min_uid.0 as i64,
            message: "from min user".to_owned(),
            pts: 17,
            pts_count: 1,
            date: 1_700_001_007,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        })
    ));
    wait_until("min user diff applied", TEST_TIMEOUT, || {
        runtime.session().try_read().is_ok_and(|s| {
            s.user_loaded(min_uid)
                && s.message(FullMsgId::new(PeerId::from_user(min_uid), MsgId(107)))
                    .is_some()
        })
    })
    .await;

    // 5G. `updatesCombined` with `seq_start` / `seq`: current seq is 5.
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::Combined(tl_types::UpdatesCombined {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: tl_message(
                    109,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    })),
                    false,
                    1_700_001_009,
                    "combined 7..8",
                    None,
                ),
                pts: 19,
                pts_count: 1,
            })],
            users: vec![],
            chats: vec![],
            date: 1_700_001_009,
            seq_start: 7,
            seq: 8,
        })
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        runtime
            .session()
            .read()
            .await
            .message(FullMsgId::new(peer_id, MsgId(109)))
            .is_none(),
        "out-of-order updatesCombined(7..8) must buffer until seq=6 arrives"
    );
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::Combined(tl_types::UpdatesCombined {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: tl_message(
                    108,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    }),
                    Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_uid.0 as i64,
                    })),
                    false,
                    1_700_001_008,
                    "combined 6",
                    None,
                ),
                pts: 18,
                pts_count: 1,
            })],
            users: vec![],
            chats: vec![],
            date: 1_700_001_008,
            seq_start: 6,
            seq: 6,
        })
    ));
    wait_until("updatesCombined 6 and 7..8 cascaded", TEST_TIMEOUT, || {
        runtime.session().try_read().is_ok_and(|s| {
            s.message(FullMsgId::new(peer_id, MsgId(108))).is_some()
                && s.message(FullMsgId::new(peer_id, MsgId(109))).is_some()
        })
    })
    .await;

    // 5H. `updatesTooLong` -> `getDifference` -> `updates.differenceTooLong(pts=200)`
    // -> emits `SessionEvent::DifferenceTooLong { pts: 200 }` AND refetches dialogs!
    assert!(server.push_update_to_dc(2, tl_enums::Updates::TooLong));
    wait_until("differenceTooLong refetched dialogs", TEST_TIMEOUT, || {
        runtime.session().try_read().is_ok_and(|s| {
            s.message(FullMsgId::new(peer_id, MsgId(999))).is_some()
                && s.history(peer_id).and_then(|h| h.unread_count) == Some(3)
        })
    })
    .await;
    assert!(
        !server.rpcs_of::<tl_fns::messages::GetDialogs>().is_empty(),
        "differenceTooLong must trigger messages.getDialogs refetch"
    );

    let mut saw_diff_too_long = false;
    while let Ok(ev) = events_rx.try_recv() {
        if matches!(ev, SessionEvent::DifferenceTooLong { pts: 200 }) {
            saw_diff_too_long = true;
        }
    }
    assert!(
        saw_diff_too_long,
        "expected SessionEvent::DifferenceTooLong"
    );

    runtime.shutdown();
    client.shutdown();
}

// ============================================================================
// 6. Per-channel PTS + updates.getChannelDifference: gap, non-final pagination
//    (force=false), final + timeout short-poll (force=true), channelDifferenceTooLong
//    preserving gap between old slice and snapshot
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_channel_pts_get_channel_difference_too_long_timeout_final_and_force_over_wire() {
    let self_uid = UserId(1001);
    let ch_id = ChannelId(8800);
    let ch_peer_id = PeerId::from_channel(ch_id);
    let ch_tl_peer = tl_enums::Peer::Channel(tl_types::PeerChannel {
        channel_id: ch_id.0 as i64,
    });

    let mut session = Session::new(self_uid);
    tg_api::convert::apply_tl_users_and_chats(
        &mut session,
        &[],
        &[tl_channel(
            ch_id.0 as i64,
            0x8800,
            "Broadcast Feed",
            true,
            false,
        )],
    );

    let ch_diffs: Arc<Mutex<VecDeque<tl_enums::updates::ChannelDifference>>> =
        Arc::new(Mutex::new(VecDeque::new()));
    {
        let mut q = ch_diffs.lock().unwrap();
        q.push_back(tl_enums::updates::ChannelDifference::ChannelDifference(
            tl_types::updates::ChannelDifference {
                r#final: false,
                pts: 53,
                timeout: None,
                new_messages: vec![tl_message(
                    203,
                    ch_tl_peer.clone(),
                    None,
                    false,
                    1_700_002_003,
                    "ch page 1",
                    None,
                )],
                other_updates: vec![],
                chats: vec![],
                users: vec![],
            },
        ));
        q.push_back(tl_enums::updates::ChannelDifference::Empty(
            tl_types::updates::ChannelDifferenceEmpty {
                r#final: true,
                pts: 54,
                timeout: Some(2),
            },
        ));
        q.push_back(tl_enums::updates::ChannelDifference::TooLong(
            tl_types::updates::ChannelDifferenceTooLong {
                r#final: true,
                timeout: Some(30),
                dialog: tl_dialog(ch_tl_peer.clone(), 300, 290, 290, 5, Some(120)),
                messages: vec![tl_message(
                    300,
                    ch_tl_peer.clone(),
                    None,
                    false,
                    1_700_002_300,
                    "snapshot msg 300",
                    None,
                )],
                chats: vec![],
                users: vec![],
            },
        ));
        // Follow-up range difference (`requestChannelRangeDifference` in
        // `api/api_updates.cpp:797-890`) validating loaded messages after `TooLong`.
        q.push_back(tl_enums::updates::ChannelDifference::Empty(
            tl_types::updates::ChannelDifferenceEmpty {
                r#final: true,
                pts: 120,
                timeout: None,
            },
        ));
    }
    let ch_diffs_srv = Arc::clone(&ch_diffs);

    let server = ScriptedServer::new(move |_srv, req| match req.ctor {
        x if x == tl_fns::updates::GetChannelDifference::CONSTRUCTOR_ID => {
            let next = ch_diffs_srv
                .lock()
                .unwrap()
                .pop_front()
                .expect("queued channel difference");
            ServerAction::ok(&next)
        }
        other => panic!("unexpected rpc ctor 0x{other:08x}"),
    });

    let (client, rpc) = build_client(&server, 2);
    let mut updates = UpdatesHandler::new();
    updates
        .state_machine_mut()
        .init_state(10, 1_700_002_000, 0, 1, 1_000);
    updates.init_channel_pts(ch_id, 50);
    updates.set_active_channel(ch_id, true, 1_000);

    // Contiguous channel update pts=51 -> applies immediately.
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                    message: tl_message(
                        201,
                        ch_tl_peer.clone(),
                        None,
                        false,
                        1_700_002_001,
                        "ch 201",
                        None,
                    ),
                    pts: 51,
                    pts_count: 1,
                }),
                date: 1_700_002_001,
            }),
            None,
            1_100,
        )
        .await
        .expect("ch 201 ok");
    assert!(
        session
            .message(FullMsgId::new(ch_peer_id, MsgId(201)))
            .is_some()
    );
    assert_eq!(updates.channel_pts(ch_id), Some(51));

    // Forward channel PTS gap (pts=53, missing 52) -> buffers, then at +1000ms calls
    // `updates.getChannelDifference(pts=51, force=true)` -> non-final -> chains
    // `updates.getChannelDifference(pts=53, force=false)` -> final with timeout=2s!
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                    message: tl_message(
                        203,
                        ch_tl_peer.clone(),
                        None,
                        false,
                        1_700_002_003,
                        "gapped ch 203",
                        None,
                    ),
                    pts: 53,
                    pts_count: 1,
                }),
                date: 1_700_002_003,
            }),
            None,
            2_000,
        )
        .await
        .expect("buffer ch gap ok");
    assert!(
        session
            .message(FullMsgId::new(ch_peer_id, MsgId(203)))
            .is_none()
    );

    updates
        .on_tick(&rpc, &mut session, 3_000)
        .await
        .expect("ch diff tick ok");
    assert_eq!(updates.channel_pts(ch_id), Some(54));
    assert!(
        session
            .message(FullMsgId::new(ch_peer_id, MsgId(203)))
            .is_some()
    );

    // Short poll timer should fire at 3_000 + 2_000 = 5_000ms with `force = true` -> TooLong!
    assert_eq!(updates.next_deadline(), Some(5_000));
    updates
        .on_tick(&rpc, &mut session, 5_000)
        .await
        .expect("short poll too long ok");

    let ch_rpcs = server.rpcs_of::<tl_fns::updates::GetChannelDifference>();
    assert_eq!(ch_rpcs.len(), 4);
    let c0 = tl_fns::updates::GetChannelDifference::from_bytes(&ch_rpcs[0].body).unwrap();
    let c1 = tl_fns::updates::GetChannelDifference::from_bytes(&ch_rpcs[1].body).unwrap();
    let c2 = tl_fns::updates::GetChannelDifference::from_bytes(&ch_rpcs[2].body).unwrap();
    let c3 = tl_fns::updates::GetChannelDifference::from_bytes(&ch_rpcs[3].body).unwrap();
    assert_eq!((c0.pts, c0.force), (51, true));
    assert_eq!(
        (c1.pts, c1.force),
        (53, false),
        "non-final pagination must send force=false per api_updates.cpp:443,743-748"
    );
    assert_eq!(
        (c2.pts, c2.force),
        (54, true),
        "short-poll timer must send force=true per api_updates.cpp:652,743-748"
    );
    assert_eq!(
        (c3.pts, c3.force),
        (54, true),
        "channelDifferenceTooLong must follow up with requestChannelRangeDifference (`api_updates.cpp:797-890`)"
    );
    assert!(matches!(
        c3.filter,
        tl_enums::ChannelMessagesFilter::ChannelMessagesFilter(_)
    ));

    assert_eq!(updates.channel_pts(ch_id), Some(120));
    assert!(
        session
            .message(FullMsgId::new(ch_peer_id, MsgId(300)))
            .is_some()
    );
    let hist = session.history(ch_peer_id).unwrap();
    assert_eq!(hist.unread_count, Some(5));
    assert_eq!(hist.top_message_id, Some(MsgId(300)));
    assert!(
        !hist.loaded_ranges.loaded_at_bottom(),
        "channelDifferenceTooLong must leave history bottom not loaded (`api_updates.cpp:414`)"
    );
    assert_eq!(
        hist.loaded_ranges.slices().len(),
        2,
        "gap between old messages (201..=203) and TooLong snapshot (300) must be preserved"
    );
    assert_eq!(
        hist.loaded_ranges.slices()[0].range,
        MsgRange::new(MsgId(201), MsgId(203))
    );

    client.shutdown();
}

// ============================================================================
// 7. Read state: updateReadHistoryInbox/Outbox, updateReadChannelInbox with
//    still_unread_count & channel PTS mismatch fallback, unread mentions/reactions
//    counters, messages.readHistory / channels.readHistory max_id & retry,
//    updateDeleteMessages / updateDeleteChannelMessages, updateEditMessage
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_state_counters_deletes_and_edits_over_wire() {
    let self_uid = UserId(1001);
    let peer_uid = UserId(2002);
    let ch_id = ChannelId(9900);
    let peer_id = PeerId::from_user(peer_uid);
    let ch_peer_id = PeerId::from_channel(ch_id);
    let peer_tl = tl_enums::Peer::User(tl_types::PeerUser {
        user_id: peer_uid.0 as i64,
    });

    let mut session = Session::new(self_uid);
    tg_api::convert::apply_tl_users_and_chats(
        &mut session,
        &[tl_user(peer_uid.0 as i64, 0x2222, "Peer Alpha", false)],
        &[tl_channel(
            ch_id.0 as i64,
            0x9900,
            "Topic Group",
            false,
            false,
        )],
    );

    let read_attempts = Arc::new(AtomicU64::new(0));
    let read_attempts_srv = Arc::clone(&read_attempts);

    let server = ScriptedServer::new(move |_srv, req| match req.ctor {
        x if x == tl_fns::messages::ReadHistory::CONSTRUCTOR_ID => {
            let n = read_attempts_srv.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Use 400 (non-auto-retried by MTProto) to verify ReadHistoryManager
                // rolls back `sent_read_till` on RPC failure (`data_histories.cpp:763-770`).
                ServerAction::err(400, "PEER_ID_INVALID")
            } else {
                ServerAction::ok(&tl_enums::messages::AffectedMessages::AffectedMessages(
                    tl_types::messages::AffectedMessages {
                        pts: 11,
                        pts_count: 1,
                    },
                ))
            }
        }
        x if x == tl_fns::channels::ReadHistory::CONSTRUCTOR_ID => ServerAction::ok(&true),
        other => panic!("unexpected rpc ctor 0x{other:08x}"),
    });

    let (client, rpc) = build_client(&server, 2);
    let mut updates = UpdatesHandler::new();
    updates
        .state_machine_mut()
        .init_state(10, 1_700_003_000, 0, 0, 0);
    updates.init_channel_pts(ch_id, 40);

    session.set_history_unread_count(peer_id, 0);
    let mut m10 = Message::new_text(
        peer_id,
        MsgId(10),
        Some(peer_id),
        1_700_003_010,
        TextWithEntities::plain("@me hi"),
    );
    m10.flags.insert(MessageFlags::MENTIONS_ME);
    let m11 = Message::new_text(
        peer_id,
        MsgId(11),
        Some(peer_id),
        1_700_003_011,
        TextWithEntities::plain("second"),
    );
    let mut m12 = Message::new_text(
        peer_id,
        MsgId(12),
        Some(PeerId::from_user(self_uid)),
        1_700_003_012,
        TextWithEntities::plain("my outgoing"),
    );
    m12.flags.insert(MessageFlags::OUTGOING);
    let m13 = Message::new_text(
        peer_id,
        MsgId(13),
        Some(peer_id),
        1_700_003_013,
        TextWithEntities::plain("latest incoming"),
    );
    // New messages (`NewMessageType::Unread`): only they raise the unread count (TGB-3160).
    session.insert_new_unread_message(m10);
    session.insert_new_unread_message(m11);
    session.insert_new_unread_message(m12);
    session.insert_new_unread_message(m13);

    assert_eq!(session.history(peer_id).unwrap().unread_count, Some(3));
    assert_eq!(session.history(peer_id).unwrap().unread_mentions_count, 1);

    // 7A. `messages.readHistory` RPC failure rolls back `sent_read_till`, retry succeeds with `max_id = 11`.
    let mut reader = ReadHistoryManager::new();
    let t0 = Instant::now();
    assert!(reader.read_inbox_till(input_peer_for(&session, peer_id), MsgId(11), None, true, t0,));
    assert!(reader.flush_due(&rpc, t0).await.is_err());
    assert!(reader.read_inbox_till(
        input_peer_for(&session, peer_id),
        MsgId(11),
        None,
        true,
        t0 + Duration::from_secs(1),
    ));
    let flushed = reader
        .flush_due(&rpc, t0 + Duration::from_secs(1))
        .await
        .expect("retry readHistory ok");
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].max_id, MsgId(11));
    let wire_pm_read = tl_fns::messages::ReadHistory::from_bytes(
        &server.rpcs_of::<tl_fns::messages::ReadHistory>()[1].body,
    )
    .unwrap();
    assert_eq!(wire_pm_read.max_id, 11);

    // 7B. `channels.readHistory` sends exact `max_id = 201`.
    assert!(reader.read_inbox_till(
        input_peer_for(&session, ch_peer_id),
        MsgId(201),
        None,
        true,
        t0,
    ));
    let ch_flushed = reader
        .flush_due(&rpc, t0)
        .await
        .expect("channels.readHistory ok");
    assert_eq!(ch_flushed[0].max_id, MsgId(201));
    assert!(ch_flushed[0].affected.is_none());
    let wire_ch_read = tl_fns::channels::ReadHistory::from_bytes(
        &server.rpcs_of::<tl_fns::channels::ReadHistory>()[0].body,
    )
    .unwrap();
    assert_eq!(wire_ch_read.max_id, 201);

    // 7C. `updateReadHistoryInbox` advances watermark to 11 (still_unread=1), then stale
    // `max_id = 10, still_unread = 99` MUST be ignored (`history.cpp:2277`).
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ReadHistoryInbox(tl_types::UpdateReadHistoryInbox {
                    folder_id: None,
                    peer: peer_tl.clone(),
                    max_id: 11,
                    still_unread_count: 1,
                    pts: 11,
                    pts_count: 1,
                    top_msg_id: None,
                }),
                date: 1_700_003_020,
            }),
            None,
            100,
        )
        .await
        .unwrap();
    assert_eq!(
        session.history(peer_id).unwrap().inbox_read_till_id(),
        MsgId(11)
    );
    assert_eq!(session.history(peer_id).unwrap().unread_count, Some(1));

    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ReadHistoryInbox(tl_types::UpdateReadHistoryInbox {
                    folder_id: None,
                    peer: peer_tl.clone(),
                    max_id: 10,
                    still_unread_count: 99,
                    pts: 12,
                    pts_count: 1,
                    top_msg_id: None,
                }),
                date: 1_700_003_021,
            }),
            None,
            110,
        )
        .await
        .unwrap();
    assert_eq!(
        session.history(peer_id).unwrap().inbox_read_till_id(),
        MsgId(11)
    );
    assert_eq!(
        session.history(peer_id).unwrap().unread_count,
        Some(1),
        "stale updateReadHistoryInbox must NOT overwrite unread_count"
    );

    // 7D. `updateReadHistoryOutbox` marks outgoing msg 12 read.
    assert!(
        session
            .history(peer_id)
            .unwrap()
            .is_server_side_unread(session.message(FullMsgId::new(peer_id, MsgId(12))).unwrap())
    );
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ReadHistoryOutbox(tl_types::UpdateReadHistoryOutbox {
                    peer: peer_tl.clone(),
                    max_id: 12,
                    pts: 13,
                    pts_count: 1,
                }),
                date: 1_700_003_022,
            }),
            None,
            120,
        )
        .await
        .unwrap();
    assert_eq!(
        session.history(peer_id).unwrap().outbox_read_till_id(),
        MsgId(12)
    );
    assert!(
        !session
            .history(peer_id)
            .unwrap()
            .is_server_side_unread(session.message(FullMsgId::new(peer_id, MsgId(12))).unwrap())
    );

    // 7E. `updateReadChannelInbox`:
    let c201 = Message::new_text(
        ch_peer_id,
        MsgId(201),
        Some(peer_id),
        1_700_003_030,
        TextWithEntities::plain("ch in 1"),
    );
    let mut c202 = Message::new_text(
        ch_peer_id,
        MsgId(202),
        Some(PeerId::from_user(self_uid)),
        1_700_003_031,
        TextWithEntities::plain("ch out"),
    );
    c202.flags.insert(MessageFlags::OUTGOING);
    let c203 = Message::new_text(
        ch_peer_id,
        MsgId(203),
        Some(peer_id),
        1_700_003_032,
        TextWithEntities::plain("ch in 2"),
    );
    session.insert_message(c201);
    session.insert_message(c202);
    session.insert_message(c203);

    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ReadChannelInbox(tl_types::UpdateReadChannelInbox {
                    folder_id: None,
                    channel_id: ch_id.0 as i64,
                    max_id: 200,
                    still_unread_count: 7,
                    pts: 40,
                }),
                date: 1_700_003_035,
            }),
            None,
            130,
        )
        .await
        .unwrap();
    assert_eq!(session.history(ch_peer_id).unwrap().unread_count, Some(7));

    // Mismatched channel pts (999 != 40) -> falls back to a local count: like
    // `History::countStillUnreadLocal` (`history.cpp:2200-2225`), the known 7 minus the incoming
    // messages read since the old read mark (201) -> 6 (TGB-3160).
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ReadChannelInbox(tl_types::UpdateReadChannelInbox {
                    folder_id: None,
                    channel_id: ch_id.0 as i64,
                    max_id: 201,
                    still_unread_count: 50,
                    pts: 999,
                }),
                date: 1_700_003_036,
            }),
            None,
            140,
        )
        .await
        .unwrap();
    assert_eq!(
        session.history(ch_peer_id).unwrap().unread_count,
        Some(6),
        "fallback local unread count subtracts the incoming messages read since the old mark"
    );

    // 7F. Unread mentions & reactions counters:
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ReadMessagesContents(
                    tl_types::UpdateReadMessagesContents {
                        messages: vec![10],
                        pts: 14,
                        pts_count: 1,
                        date: None,
                    },
                ),
                date: 1_700_003_040,
            }),
            None,
            150,
        )
        .await
        .unwrap();
    assert_eq!(session.history(peer_id).unwrap().unread_mentions_count, 0);
    assert!(
        !session
            .message(FullMsgId::new(peer_id, MsgId(10)))
            .unwrap()
            .mentions_me()
    );

    let unread_reactions =
        tl_enums::MessageReactions::MessageReactions(tl_types::MessageReactions {
            min: false,
            can_see_list: true,
            reactions_as_tags: false,
            results: vec![tl_enums::ReactionCount::ReactionCount(
                tl_types::ReactionCount {
                    chosen_order: None,
                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "👍".to_owned(),
                    }),
                    count: 1,
                },
            )],
            recent_reactions: Some(vec![tl_enums::MessagePeerReaction::MessagePeerReaction(
                tl_types::MessagePeerReaction {
                    big: false,
                    unread: true,
                    my: false,
                    peer_id: peer_tl.clone(),
                    date: 1_700_003_045,
                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "👍".to_owned(),
                    }),
                },
            )]),
            top_reactors: None,
        });
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::MessageReactions(tl_types::UpdateMessageReactions {
                    peer: peer_tl.clone(),
                    msg_id: 12,
                    top_msg_id: None,
                    saved_peer_id: None,
                    reactions: unread_reactions,
                }),
                date: 1_700_003_045,
            }),
            None,
            160,
        )
        .await
        .unwrap();
    assert_eq!(session.history(peer_id).unwrap().unread_reactions_count, 1);
    assert!(
        session
            .message(FullMsgId::new(peer_id, MsgId(12)))
            .unwrap()
            .has_unread_reaction()
    );

    let read_reactions = tl_enums::MessageReactions::MessageReactions(tl_types::MessageReactions {
        min: false,
        can_see_list: true,
        reactions_as_tags: false,
        results: vec![tl_enums::ReactionCount::ReactionCount(
            tl_types::ReactionCount {
                chosen_order: None,
                reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "👍".to_owned(),
                }),
                count: 1,
            },
        )],
        recent_reactions: Some(vec![tl_enums::MessagePeerReaction::MessagePeerReaction(
            tl_types::MessagePeerReaction {
                big: false,
                unread: false,
                my: false,
                peer_id: peer_tl.clone(),
                date: 1_700_003_045,
                reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "👍".to_owned(),
                }),
            },
        )]),
        top_reactors: None,
    });
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::MessageReactions(tl_types::UpdateMessageReactions {
                    peer: peer_tl.clone(),
                    msg_id: 12,
                    top_msg_id: None,
                    saved_peer_id: None,
                    reactions: read_reactions,
                }),
                date: 1_700_003_046,
            }),
            None,
            170,
        )
        .await
        .unwrap();
    assert_eq!(session.history(peer_id).unwrap().unread_reactions_count, 0);
    assert!(
        !session
            .message(FullMsgId::new(peer_id, MsgId(12)))
            .unwrap()
            .has_unread_reaction()
    );

    // 7G. `updateEditMessage` on existing vs unknown message:
    let mut edited_13 = tl_message(
        13,
        peer_tl.clone(),
        Some(peer_tl.clone()),
        false,
        1_700_003_013,
        "edited @me",
        None,
    );
    if let tl_enums::Message::Message(ref mut m) = edited_13 {
        m.mentioned = true;
        m.edit_date = Some(1_700_003_050);
    }
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                    message: edited_13,
                    pts: 15,
                    pts_count: 1,
                }),
                date: 1_700_003_050,
            }),
            None,
            180,
        )
        .await
        .unwrap();
    let msg13 = session.message(FullMsgId::new(peer_id, MsgId(13))).unwrap();
    assert_eq!(msg13.edit_date, Some(1_700_003_050));
    assert_eq!(session.history(peer_id).unwrap().unread_mentions_count, 1);

    let unknown_edit_5 = tl_message(
        5,
        peer_tl.clone(),
        Some(peer_tl.clone()),
        false,
        1_700_000_005,
        "unknown old msg",
        None,
    );
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                    message: unknown_edit_5,
                    pts: 16,
                    pts_count: 1,
                }),
                date: 1_700_003_051,
            }),
            None,
            190,
        )
        .await
        .unwrap();
    assert!(
        session.message(FullMsgId::new(peer_id, MsgId(5))).is_none(),
        "updateEditMessage on unknown message must NOT insert it into Session"
    );

    // 7H. `updateDeleteMessages` & `updateDeleteChannelMessages`:
    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::DeleteMessages(tl_types::UpdateDeleteMessages {
                    messages: vec![13],
                    pts: 17,
                    pts_count: 1,
                }),
                date: 1_700_003_060,
            }),
            None,
            200,
        )
        .await
        .unwrap();
    assert!(
        session
            .message(FullMsgId::new(peer_id, MsgId(13)))
            .is_none()
    );
    let pm_hist = session.history(peer_id).unwrap();
    assert_eq!(pm_hist.unread_count, Some(0));
    assert_eq!(pm_hist.unread_mentions_count, 0);
    assert_eq!(pm_hist.top_message_id, Some(MsgId(12)));
    assert_eq!(pm_hist.chat_list_time_id, 1_700_003_012);

    updates
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::DeleteChannelMessages(
                    tl_types::UpdateDeleteChannelMessages {
                        channel_id: ch_id.0 as i64,
                        messages: vec![203],
                        pts: 41,
                        pts_count: 1,
                    },
                ),
                date: 1_700_003_061,
            }),
            None,
            210,
        )
        .await
        .unwrap();
    assert!(
        session
            .message(FullMsgId::new(ch_peer_id, MsgId(203)))
            .is_none()
    );
    let ch_hist = session.history(ch_peer_id).unwrap();
    // The deleted unread 203 leaves the 6 counted after the read at 201 (7E).
    assert_eq!(ch_hist.unread_count, Some(5));
    assert_eq!(ch_hist.top_message_id, Some(MsgId(202)));
    assert_eq!(ch_hist.chat_list_time_id, 1_700_003_031);

    client.shutdown();
}
