//! Byte-level tdesktop conformance suite for WP-53 (Chat Engagement & Read-Receipt Protocol)
//! against an in-process scripted fake Telegram MTProto 2.0 server.
//!
//! Covers every WP-53 RPC end-to-end through `HistoryBackend` -> `LiveSession` ->
//! `tg_app_live::engagement` -> `ErasedRpcClient` -> `MtprotoRpc` -> `tg_mtproto::Client`
//! (encrypted MTProto 2.0 framing) -> `FakeConnection` -> `ScriptedServer`:
//! - `messages.getMessagesViews#5784d3e1` & `messages.getExtendedMedia#84f80814`
//! - `messages.getUnreadMentions#f107e790`, `messages.getUnreadReactions#bd7f90ac`,
//!   `messages.getUnreadPollVotes#43286cf2`
//! - `messages.readMentions#36e5bf4d`, `messages.readReactions#9ec44f93`,
//!   `messages.readPollVotes#1720b4d8` (`AffectedHistory` pagination drain)
//! - `messages.getMessageReadParticipants#31c1c44f` & `messages.getOutboxReadDate#8c4bfe5d`
//! - `messages.readDiscussion#f731a9f4`
//! - `messages.reportMessagesDelivery#5a6d7395`
//! - `ephemeral.deleteMessage#92f6e797` & `ephemeral.reportMessage#8704f2bf`
//! - `channels.exportMessageLink#e63fadeb` & `channels.getMessageAuthor#ece2a0e6`
//! - `messages.migrateChat#a2875319` & `messages.getFutureChatCreatorAfterLeave#3b7d0ea6`
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//! Zero network access; synthetic test-only IDs and ephemeral test RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::ApiCredentials;
use tg_api::convert::apply_tl_users_and_chats;
use tg_app_core::history::backend::{
    EphemeralReportOutcome, HistoryBackend, OutboxReadDateStatus, REPORT_DELIVERIES_PER_REQUEST,
    UNREAD_THINGS_FIRST_LIMIT, UNREAD_THINGS_NEXT_LIMIT, UnreadThingsKind, UnreadThingsScope,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_live::convert::{from_data_peer, to_data_msg, to_data_peer};
use tg_app_live::engagement::tracked_ephemeral_entry;
use tg_app_live::{LiveSession, start_mtproto_erased_on};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    ChannelId, ChatId, ExtendedMedia, ForumTopicData, FullMsgId as DataFullMsgId, Message,
    MessageContent, MessageMedia, MsgId as DataMsgId, PaidMediaData, PeerId as DataPeerId,
    TextWithEntities as DataText, UserId as DataUserId,
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
use tg_session::{MtprotoRpc, SessionRuntimeConfig, build_mtproto_client_config};
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
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Test-only 2048-bit RSA modulus `n` (ephemeral fixture for in-process DH tests only).
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

    fn rpcs(&self) -> Vec<RpcRecord> {
        self.inner.rpcs.lock().expect("lock rpcs").clone()
    }

    fn rpcs_of<F: Identifiable>(&self) -> Vec<RpcRecord> {
        self.rpcs()
            .into_iter()
            .filter(|r| r.ctor == F::CONSTRUCTOR_ID)
            .collect()
    }

    fn push_update_to_dc(&self, dc_id: DcId, updates: tl_enums::Updates) -> bool {
        let guard = self.inner.live_pushers.lock().expect("lock pushers");
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
            .expect("lock pushers")
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
                                body: inner_body.clone(),
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
// Synthetic TL Fixtures & LiveSession Harness over Encrypted MTProto 2.0
// ============================================================================

fn tl_user(id: i64, first: &str, username: &str, is_self: bool, is_bot: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !is_bot,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
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
        id,
        access_hash: Some(id * 29 + 11),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
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
    }
}

fn tl_channel(
    id: i64,
    title: &str,
    broadcast: bool,
    megagroup: bool,
    forum: bool,
) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified: false,
        megagroup,
        restricted: false,
        signatures: false,
        min: false,
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
        forum,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 17 + 5),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(18),
        usernames: None,
        stories_max_id: None,
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
    }
}

fn tl_chat(
    id: i64,
    title: &str,
    creator: bool,
    deactivated: bool,
    migrated_to: Option<tl_enums::InputChannel>,
) -> tl_types::Chat {
    tl_types::Chat {
        creator,
        left: false,
        deactivated,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 8,
        date: 1_700_000_000,
        version: 1,
        migrated_to,
        admin_rights: None,
        default_banned_rights: None,
    }
}

fn tl_msg(
    peer: tl_enums::Peer,
    from_user: i64,
    id: i32,
    text: &str,
    mentioned: bool,
    media_unread: bool,
    unread_reaction: bool,
) -> tl_enums::Message {
    let reactions = unread_reaction.then(|| {
        tl_enums::MessageReactions::MessageReactions(tl_types::MessageReactions {
            min: false,
            can_see_list: true,
            reactions_as_tags: false,
            results: vec![tl_enums::ReactionCount::ReactionCount(
                tl_types::ReactionCount {
                    chosen_order: None,
                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "❤️".to_owned(),
                    }),
                    count: 1,
                },
            )],
            recent_reactions: Some(vec![tl_enums::MessagePeerReaction::MessagePeerReaction(
                tl_types::MessagePeerReaction {
                    big: false,
                    unread: true,
                    my: false,
                    peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: from_user }),
                    date: 1_700_000_500,
                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "❤️".to_owned(),
                    }),
                },
            )]),
            top_reactors: None,
        })
    });
    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned,
        media_unread,
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
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: from_user,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_700_000_100 + id,
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
        reactions,
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

struct MtprotoConformanceHarness {
    _rt: tokio::runtime::Runtime,
    _client: Client,
    server: ScriptedServer,
    live: LiveSession,
    channel_peer: PeerId,
    megagroup_peer: PeerId,
    basic_chat_peer: PeerId,
    dm_user_peer: PeerId,
    bot_user_peer: PeerId,
}

impl MtprotoConformanceHarness {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync + 'static,
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let server = ScriptedServer::new(handler);
        let (client, live) = rt.block_on(async {
            let mut cfg: ClientConfig = build_mtproto_client_config(
                &ApiCredentials {
                    api_id: FAKE_API_ID,
                    api_hash: FAKE_API_HASH.to_owned(),
                },
                2,
                tg_core::Environment::Production,
                None,
            );
            cfg.proxy = ProxyData::none();
            cfg.use_pfs = false;
            cfg.ping_interval = Duration::from_secs(600);
            let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
            server.register_rsa_key_on(&client);
            let rpc = MtprotoRpc::new(client.clone());
            let runtime = Arc::new(
                start_mtproto_erased_on(
                    DataUserId(1001),
                    rpc,
                    tokio::runtime::Handle::current(),
                    SessionRuntimeConfig::default(),
                )
                .expect("start_mtproto_erased_on"),
            );
            tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
                .await
                .expect("wait_ready timeout")
                .expect("wait_ready ok");
            let live = LiveSession::new(runtime, 530);
            (client, live)
        });

        let self_u = tl_user(1001, "Self", "self_user", true, false);
        let dm_u = tl_user(2002, "ContactOne", "contact_one", false, false);
        let bot_u = tl_user(3003, "EphemeralBot", "eph_bot", false, true);
        let chan = tl_channel(5001, "Broadcast", true, false, false);
        let mg = tl_channel(5002, "Supergroup", false, true, true);
        let chat = tl_chat(6001, "BasicChat", true, false, None);

        live.write_session(|session| {
            apply_tl_users_and_chats(
                session,
                &[
                    tl_enums::User::User(self_u),
                    tl_enums::User::User(dm_u),
                    tl_enums::User::User(bot_u),
                ],
                &[
                    tl_enums::Chat::Channel(chan),
                    tl_enums::Chat::Channel(mg),
                    tl_enums::Chat::Chat(chat),
                ],
            );
        });

        Self {
            _rt: rt,
            _client: client,
            server,
            live,
            channel_peer: from_data_peer(DataPeerId::from_channel(ChannelId(5001))),
            megagroup_peer: from_data_peer(DataPeerId::from_channel(ChannelId(5002))),
            basic_chat_peer: from_data_peer(DataPeerId::from_chat(ChatId(6001))),
            dm_user_peer: from_data_peer(DataPeerId::from_user(DataUserId(2002))),
            bot_user_peer: from_data_peer(DataPeerId::from_user(DataUserId(3003))),
        }
    }
}

fn handle_bootstrap_rpc(ctor: u32) -> Option<ServerAction> {
    if ctor == tl_fns::updates::GetState::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(&tl_enums::updates::State::State(
            tl_types::updates::State {
                pts: 100,
                qts: 0,
                date: 1_700_000_000,
                seq: 1,
                unread_count: 0,
            },
        )));
    }
    if ctor == tl_fns::updates::GetDifference::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(&tl_enums::updates::Difference::Empty(
            tl_types::updates::DifferenceEmpty {
                date: 1_700_000_001,
                seq: 1,
            },
        )));
    }
    if ctor == tl_fns::users::GetUsers::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(&vec![tl_enums::User::User(tl_user(
            1001,
            "Self",
            "self_user",
            true,
            false,
        ))]));
    }
    if ctor == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(&tl_enums::messages::Dialogs::Dialogs(
            tl_types::messages::Dialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            },
        )));
    }
    if ctor == tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(
            &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
                state: tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 100,
                    qts: 0,
                    date: 1_700_000_001,
                    seq: 1,
                    unread_count: 0,
                }),
            }),
        ));
    }
    if ctor == tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(
            &tl_enums::messages::DialogFilters::DialogFilters(tl_types::messages::DialogFilters {
                tags_enabled: false,
                filters: Vec::new(),
            }),
        ));
    }
    if ctor == tl_fns::account::UpdateStatus::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(&true));
    }
    None
}

#[test]
fn conformance_views_increment_and_extended_media_over_mtproto2() {
    let mut h = MtprotoConformanceHarness::start(|_srv, rpc| {
        if let Some(action) = handle_bootstrap_rpc(rpc.ctor) {
            return action;
        }
        match rpc.ctor {
            x if x == tl_fns::messages::GetMessagesViews::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetMessagesViews::from_bytes(&rpc.body).unwrap();
                assert!(req.increment);
                let views = req
                    .id
                    .iter()
                    .map(|&id| {
                        tl_enums::MessageViews::MessageViews(tl_types::MessageViews {
                            views: Some(1000 + id),
                            forwards: Some(50 + id),
                            replies: Some(tl_enums::MessageReplies::MessageReplies(
                                tl_types::MessageReplies {
                                    comments: true,
                                    replies: 7,
                                    replies_pts: 200,
                                    recent_repliers: None,
                                    channel_id: Some(5002),
                                    max_id: Some(900),
                                    read_max_id: Some(850),
                                },
                            )),
                        })
                    })
                    .collect();
                ServerAction::ok(&tl_enums::messages::MessageViews::MessageViews(
                    tl_types::messages::MessageViews {
                        views,
                        chats: Vec::new(),
                        users: Vec::new(),
                    },
                ))
            }
            x if x == tl_fns::messages::GetExtendedMedia::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetExtendedMedia::from_bytes(&rpc.body).unwrap();
                let updates = req
                    .id
                    .into_iter()
                    .map(|msg_id| {
                        tl_enums::Update::MessageExtendedMedia(
                            tl_types::UpdateMessageExtendedMedia {
                                peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                                    channel_id: 5001,
                                }),
                                msg_id,
                                extended_media: vec![
                                    tl_enums::MessageExtendedMedia::MessageExtendedMedia(
                                        tl_types::MessageExtendedMedia {
                                            media: Box::new(tl_enums::MessageMedia::Photo(
                                                tl_types::MessageMediaPhoto {
                                                    spoiler: false,
                                                    live_photo: false,
                                                    photo: Some(tl_enums::Photo::Empty(
                                                        tl_types::PhotoEmpty { id: 8881 },
                                                    )),
                                                    ttl_seconds: None,
                                                    video: None,
                                                },
                                            )),
                                        },
                                    ),
                                ],
                            },
                        )
                    })
                    .collect();
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates,
                    users: Vec::new(),
                    chats: Vec::new(),
                    date: 1_700_000_500,
                    seq: 2,
                }))
            }
            other => panic!("unexpected RPC ctor 0x{other:08x}"),
        }
    });

    let chan_data = to_data_peer(h.channel_peer);
    h.live.write_session(|session| {
        for id in 10..=14 {
            session.insert_message(Message::new_text(
                chan_data,
                DataMsgId(id),
                None,
                1_700_000_000 + id as i32,
                DataText::plain(format!("Post {id}")),
            ));
        }
        for paid_id in [201_i64, 202] {
            let mut m = Message::new_text(
                chan_data,
                DataMsgId(paid_id),
                None,
                1_700_000_200,
                DataText::plain("Paid post"),
            );
            m.content = MessageContent::Regular {
                text: DataText::plain("Paid post"),
                media: MessageMedia::PaidMedia(Box::new(PaidMediaData {
                    stars_amount: 25,
                    extended_media: vec![ExtendedMedia::Preview {
                        width: Some(320),
                        height: Some(240),
                        thumb: None,
                        duration: None,
                    }],
                })),
            };
            session.insert_message(m);
        }
    });

    // Queue 5 visible post IDs & flush over real encrypted MTProto 2.0 connection.
    h.live.schedule_message_views_increment(
        h.channel_peer,
        &[MsgId(10), MsgId(11), MsgId(12), MsgId(13), MsgId(14)],
    );
    let infos = h
        .live
        .flush_message_views(Some(h.channel_peer), true)
        .expect("flush_message_views");
    assert_eq!(infos.len(), 5);
    assert_eq!(infos[4].views, Some(1014));
    assert_eq!(infos[4].forwards, Some(64));

    let view_rpcs = h.server.rpcs_of::<tl_fns::messages::GetMessagesViews>();
    assert_eq!(view_rpcs.len(), 1);
    let wire_views = tl_fns::messages::GetMessagesViews::from_bytes(&view_rpcs[0].body)
        .expect("decode wire GetMessagesViews");
    assert!(wire_views.increment);
    assert_eq!(wire_views.id, vec![10, 11, 12, 13, 14]);

    // Poll extended media for post 201 via RPC.
    let refreshed = h
        .live
        .poll_extended_media(h.channel_peer, &[MsgId(201)], true)
        .expect("poll_extended_media");
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].unlocked_items, 1);

    // Push `updateMessageExtendedMedia` for post 202 from server over live MTProto stream.
    assert!(h.server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::MessageExtendedMedia(tl_types::UpdateMessageExtendedMedia {
                peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 5001 }),
                msg_id: 202,
                extended_media: vec![tl_enums::MessageExtendedMedia::MessageExtendedMedia(
                    tl_types::MessageExtendedMedia {
                        media: Box::new(tl_enums::MessageMedia::Photo(
                            tl_types::MessageMediaPhoto {
                                spoiler: false,
                                live_photo: false,
                                photo: Some(tl_enums::Photo::Empty(tl_types::PhotoEmpty {
                                    id: 8882,
                                })),
                                ttl_seconds: None,
                                video: None,
                            },
                        )),
                    },
                )],
            }),
            date: 1_700_000_600,
        }),
    ));

    let mut unlocked_202 = false;
    for _ in 0..50 {
        h.live.write_session(|session| {
            if let Some(m) = session.message(DataFullMsgId::new(chan_data, DataMsgId(202)))
                && let MessageContent::Regular {
                    media: MessageMedia::PaidMedia(paid),
                    ..
                } = &m.content
                && matches!(paid.extended_media.first(), Some(ExtendedMedia::Media(_)))
            {
                unlocked_202 = true;
            }
        });
        if unlocked_202 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        unlocked_202,
        "pushed updateMessageExtendedMedia should unlock post 202"
    );
}

#[test]
fn conformance_unread_things_and_read_discussion_over_mtproto2() {
    let read_mentions_calls = Arc::new(AtomicUsize::new(0));
    let rm_counter = Arc::clone(&read_mentions_calls);
    let mg_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 5002 });

    let mut h = MtprotoConformanceHarness::start(move |_srv, rpc| {
        if let Some(action) = handle_bootstrap_rpc(rpc.ctor) {
            return action;
        }
        match rpc.ctor {
            x if x == tl_fns::messages::GetUnreadMentions::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetUnreadMentions::from_bytes(&rpc.body).unwrap();
                let msgs = if req.offset_id == 1 {
                    assert_eq!(req.limit, UNREAD_THINGS_FIRST_LIMIT);
                    assert_eq!(req.add_offset, -UNREAD_THINGS_FIRST_LIMIT);
                    vec![tl_msg(
                        mg_peer.clone(),
                        2002,
                        50,
                        "@self first",
                        true,
                        true,
                        false,
                    )]
                } else {
                    assert_eq!(req.offset_id, 50);
                    assert_eq!(req.limit, UNREAD_THINGS_NEXT_LIMIT);
                    assert_eq!(req.add_offset, -(UNREAD_THINGS_NEXT_LIMIT + 1));
                    vec![tl_msg(
                        mg_peer.clone(),
                        2002,
                        60,
                        "@self second",
                        true,
                        true,
                        false,
                    )]
                };
                ServerAction::ok(&tl_enums::messages::Messages::ChannelMessages(
                    tl_types::messages::ChannelMessages {
                        inexact: false,
                        pts: 110,
                        count: 2,
                        offset_id_offset: None,
                        messages: msgs,
                        topics: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                    },
                ))
            }
            x if x == tl_fns::messages::GetUnreadReactions::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetUnreadReactions::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.top_msg_id, Some(700));
                ServerAction::ok(&tl_enums::messages::Messages::Slice(
                    tl_types::messages::MessagesSlice {
                        inexact: false,
                        count: 1,
                        next_rate: None,
                        offset_id_offset: None,
                        search_flood: None,
                        messages: vec![tl_msg(
                            mg_peer.clone(),
                            2002,
                            705,
                            "Loved",
                            false,
                            false,
                            true,
                        )],
                        topics: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                    },
                ))
            }
            x if x == tl_fns::messages::GetUnreadPollVotes::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetUnreadPollVotes::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.top_msg_id, Some(700));
                ServerAction::ok(&tl_enums::messages::Messages::Messages(
                    tl_types::messages::Messages {
                        messages: vec![tl_msg(
                            mg_peer.clone(),
                            2002,
                            706,
                            "Poll",
                            false,
                            false,
                            false,
                        )],
                        topics: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                    },
                ))
            }
            x if x == tl_fns::messages::ReadMentions::CONSTRUCTOR_ID => {
                let n = rm_counter.fetch_add(1, Ordering::Relaxed);
                ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                    tl_types::messages::AffectedHistory {
                        pts: 120 + n as i32,
                        pts_count: 1,
                        offset: if n == 0 { 1 } else { 0 },
                    },
                ))
            }
            x if x == tl_fns::messages::ReadReactions::CONSTRUCTOR_ID
                || x == tl_fns::messages::ReadPollVotes::CONSTRUCTOR_ID =>
            {
                ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                    tl_types::messages::AffectedHistory {
                        pts: 130,
                        pts_count: 1,
                        offset: 0,
                    },
                ))
            }
            x if x == tl_fns::messages::ReadDiscussion::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::ReadDiscussion::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.msg_id, 700);
                assert_eq!(req.read_max_id, 720);
                ServerAction::ok(&true)
            }
            other => panic!("unexpected RPC ctor 0x{other:08x}"),
        }
    });

    let hist_scope = UnreadThingsScope::history(h.megagroup_peer);
    let topic_scope = UnreadThingsScope::topic(h.megagroup_peer, MsgId(700));

    let mg_data = to_data_peer(h.megagroup_peer);
    h.live.write_session(|session| {
        let mut topic = ForumTopicData::new(mg_data, DataMsgId(700));
        topic.title = "General".to_owned();
        topic.unread_count = Some(3);
        topic.last_known_server_msg_id = DataMsgId(720);
        session
            .ensure_forum(mg_data)
            .topics
            .insert(DataMsgId(700), topic);
    });

    let p1 = h
        .live
        .load_unread_things_page(hist_scope, UnreadThingsKind::Mentions)
        .expect("page 1");
    assert!(p1.has_more);
    let p2 = h
        .live
        .load_unread_things_page(hist_scope, UnreadThingsKind::Mentions)
        .expect("page 2");
    assert!(!p2.has_more);
    assert_eq!(p2.loaded_ids, vec![MsgId(50), MsgId(60)]);

    let _ = h
        .live
        .load_unread_things_page(topic_scope, UnreadThingsKind::Reactions)
        .expect("topic reactions");
    let _ = h
        .live
        .load_unread_things_page(topic_scope, UnreadThingsKind::PollVotes)
        .expect("topic poll votes");

    let rm = h
        .live
        .read_unread_mentions(h.megagroup_peer, None)
        .expect("readMentions");
    assert_eq!(rm.pages, 2);
    assert_eq!(read_mentions_calls.load(Ordering::Relaxed), 2);
    assert_eq!(h.live.unread_things_counts(hist_scope).mentions, 0);

    let _ = h
        .live
        .read_unread_reactions(h.megagroup_peer, Some(MsgId(700)), None)
        .expect("readReactions");
    let _ = h
        .live
        .read_unread_poll_votes(h.megagroup_peer, Some(MsgId(700)))
        .expect("readPollVotes");
    assert_eq!(h.live.unread_things_counts(topic_scope).reactions, 0);
    assert_eq!(h.live.unread_things_counts(topic_scope).poll_votes, 0);

    assert!(
        h.live
            .read_discussion(h.megagroup_peer, MsgId(700), MsgId(720))
            .expect("readDiscussion")
    );
    h.live.write_session(|session| {
        let t = session
            .forum(mg_data)
            .and_then(|f| f.topic(to_data_msg(MsgId(700))))
            .expect("topic");
        assert_eq!(t.unread_count, Some(0));
        assert_eq!(t.read_inbox_max_id, DataMsgId(720));
    });
}

#[test]
fn conformance_read_receipts_delivery_ephemeral_links_author_migration_over_mtproto2() {
    let mut h = MtprotoConformanceHarness::start(|_srv, rpc| {
        if let Some(action) = handle_bootstrap_rpc(rpc.ctor) {
            return action;
        }
        match rpc.ctor {
            x if x == tl_fns::messages::GetMessageReadParticipants::CONSTRUCTOR_ID => {
                let req =
                    tl_fns::messages::GetMessageReadParticipants::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.msg_id, 99);
                ServerAction::ok(&vec![tl_enums::ReadParticipantDate::ReadParticipantDate(
                    tl_types::ReadParticipantDate {
                        user_id: 2002,
                        date: 1_700_010_000,
                    },
                )])
            }
            x if x == tl_fns::messages::GetOutboxReadDate::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetOutboxReadDate::from_bytes(&rpc.body).unwrap();
                match req.msg_id {
                    10 => ServerAction::ok(&tl_enums::OutboxReadDate::OutboxReadDate(
                        tl_types::OutboxReadDate {
                            date: 1_700_020_000,
                        },
                    )),
                    11 => ServerAction::err(403, "USER_PRIVACY_RESTRICTED"),
                    12 => ServerAction::err(403, "YOUR_PRIVACY_RESTRICTED"),
                    13 => ServerAction::err(400, "MESSAGE_TOO_OLD"),
                    other => panic!("unexpected msg_id {other}"),
                }
            }
            x if x == tl_fns::messages::ReportMessagesDelivery::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::ReportMessagesDelivery::from_bytes(&rpc.body).unwrap();
                assert!(req.id.len() <= REPORT_DELIVERIES_PER_REQUEST);
                ServerAction::ok(&true)
            }
            x if x == tl_fns::ephemeral::DeleteMessage::CONSTRUCTOR_ID => {
                let req = tl_fns::ephemeral::DeleteMessage::from_bytes(&rpc.body).unwrap();
                assert!(req.peer.is_some());
                assert_eq!(req.id, 888);
                ServerAction::ok(&true)
            }
            x if x == tl_fns::ephemeral::ReportMessage::CONSTRUCTOR_ID => {
                let req = tl_fns::ephemeral::ReportMessage::from_bytes(&rpc.body).unwrap();
                if req.option.is_empty() {
                    ServerAction::ok(&tl_enums::ReportResult::AddComment(
                        tl_types::ReportResultAddComment {
                            optional: true,
                            option: b"opt1".to_vec(),
                        },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::ReportResult::Reported)
                }
            }
            x if x == tl_fns::channels::ExportMessageLink::CONSTRUCTOR_ID => {
                let req = tl_fns::channels::ExportMessageLink::from_bytes(&rpc.body).unwrap();
                assert!(req.grouped);
                assert!(req.thread);
                assert_eq!(req.id, 42);
                ServerAction::ok(&tl_enums::ExportedMessageLink::ExportedMessageLink(
                    tl_types::ExportedMessageLink {
                        link: "https://t.me/chan_5001/42?single&thread=42".to_owned(),
                        html: "<script></script>".to_owned(),
                    },
                ))
            }
            x if x == tl_fns::channels::GetMessageAuthor::CONSTRUCTOR_ID => {
                let req = tl_fns::channels::GetMessageAuthor::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.id, 42);
                ServerAction::ok(&tl_enums::User::User(tl_user(
                    4444,
                    "PostAuthor",
                    "post_author",
                    false,
                    false,
                )))
            }
            x if x == tl_fns::messages::MigrateChat::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::MigrateChat::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.chat_id, 6001);
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates: Vec::new(),
                    users: Vec::new(),
                    chats: vec![
                        tl_enums::Chat::Chat(tl_chat(
                            6001,
                            "BasicChat",
                            true,
                            true,
                            Some(tl_enums::InputChannel::InputChannel(
                                tl_types::InputChannel {
                                    channel_id: 7777,
                                    access_hash: 7777 * 17 + 5,
                                },
                            )),
                        )),
                        tl_enums::Chat::Channel(tl_channel(7777, "BasicChat", false, true, false)),
                    ],
                    date: 1_700_030_000,
                    seq: 3,
                }))
            }
            x if x == tl_fns::messages::GetFutureChatCreatorAfterLeave::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::User::User(tl_user(
                    5555,
                    "Successor",
                    "successor_user",
                    false,
                    false,
                )))
            }
            other => panic!("unexpected RPC ctor 0x{other:08x}"),
        }
    });

    // Read participants
    let readers = h
        .live
        .get_message_read_participants(h.megagroup_peer, MsgId(99))
        .expect("read participants");
    assert_eq!(readers.len(), 1);
    assert_eq!(readers[0].name, "ContactOne");
    assert_eq!(readers[0].read_date_unix, 1_700_010_000);

    // Outbox read date + real MTProto RPC error handling
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(10))
            .unwrap(),
        OutboxReadDateStatus::Read {
            date_unix: 1_700_020_000
        }
    );
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(11))
            .unwrap(),
        OutboxReadDateStatus::PeerPrivacyHidden
    );
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(12))
            .unwrap(),
        OutboxReadDateStatus::MyPrivacyHidden
    );
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(13))
            .unwrap(),
        OutboxReadDateStatus::TooOld
    );

    // Report delivery chunked at 50 (55 -> 2 RPCs)
    let ids: Vec<MsgId> = (1..=55).map(MsgId).collect();
    assert_eq!(
        h.live
            .report_messages_delivery(h.dm_user_peer, &ids, true)
            .unwrap(),
        55
    );
    assert_eq!(
        h.server
            .rpcs_of::<tl_fns::messages::ReportMessagesDelivery>()
            .len(),
        2
    );

    // Push live `updateNewEphemeralMessage` over MTProto 2.0, then delete & report it
    assert!(h.server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::NewEphemeralMessage(tl_types::UpdateNewEphemeralMessage {
                message: tl_enums::EphemeralMessage::EphemeralMessage(tl_types::EphemeralMessage {
                    out: false,
                    invert_media: false,
                    welcome_template: false,
                    noforwards: false,
                    id: 888,
                    peer_id: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: 5002,
                    })),
                    from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 3003 }),
                    receiver_id: 1001,
                    date: 1_700_025_000,
                    message: "Ephemeral over MTProto".to_owned(),
                    entities: None,
                    media: None,
                    reply_markup: None,
                    rich_message: None,
                    chat_instance: None,
                    reply_to: None,
                    top_msg_id: None,
                    anchor_msg_id: None,
                }),
            }),
            date: 1_700_025_000,
        }),
    ));
    for _ in 0..50 {
        if tracked_ephemeral_entry(&h.live, h.megagroup_peer, 888).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(tracked_ephemeral_entry(&h.live, h.megagroup_peer, 888).is_some());

    assert!(
        h.live
            .ephemeral_delete_message(h.megagroup_peer, h.bot_user_peer, 888)
            .unwrap()
    );
    assert!(tracked_ephemeral_entry(&h.live, h.megagroup_peer, 888).is_none());

    assert_eq!(
        h.live
            .ephemeral_report_message(h.megagroup_peer, 888, b"", "")
            .unwrap(),
        EphemeralReportOutcome::AddComment {
            optional: true,
            option: b"opt1".to_vec(),
        }
    );
    assert_eq!(
        h.live
            .ephemeral_report_message(h.megagroup_peer, 888, b"opt1", "comment")
            .unwrap(),
        EphemeralReportOutcome::Reported
    );

    // Export link & author
    let link = h
        .live
        .export_message_link(h.channel_peer, MsgId(42), true, true)
        .unwrap();
    assert_eq!(link.link, "https://t.me/chan_5001/42?single&thread=42");

    let author = h
        .live
        .get_message_author(h.channel_peer, MsgId(42))
        .unwrap();
    assert_eq!(author.name, "PostAuthor");
    assert_eq!(author.username.as_deref(), Some("post_author"));

    // Migrate basic group -> supergroup
    let mig = h.live.migrate_chat(h.basic_chat_peer).unwrap();
    assert!(!mig.already_migrated);
    assert_eq!(
        mig.new_channel_id,
        from_data_peer(DataPeerId::from_channel(ChannelId(7777)))
    );

    // Future creator after leave
    let creator = h
        .live
        .future_chat_creator_after_leave(h.megagroup_peer)
        .unwrap()
        .expect("future creator");
    assert_eq!(creator.name, "Successor");
    assert_eq!(creator.username.as_deref(), Some("successor_user"));
}
