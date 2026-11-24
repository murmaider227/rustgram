//! Byte-level tdesktop conformance tests for Search, Forum Topics, Saved Messages Dialogs &
//! Reaction Tags, Scheduled Messages, and Business Quick Replies (`tg-data`, `tg-api`,
//! `tg-session`) against an in-process scripted fake Telegram MTProto 2.0 server.
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
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{
    ApiCredentials, ApiError, FIRST_SHARED_MEDIA_LIMIT, ForumTopicEdit, GlobalChatTypeFilter,
    GlobalSearchController, GlobalSearchParams, GlobalSearchScope, HashOrCashtag,
    InChatSearchRequest, InvocationError, LoadDirection, MessagesSearch, MessagesSearchMerged,
    PEER_SEARCH_TYPED_LIMIT, PeerSearchController, PeerSearchType, PostsSearchController,
    QuickRepliesOutcome, QuickReplyMessagesOutcome, RpcClient, SEARCH_PEOPLE_LIMIT,
    SEARCH_PER_PAGE, SHARED_MEDIA_LIMIT, ScheduledHistoryOutcome, SharedMediaFilter, UpdateEvent,
    UpdatesHandler, classify_hash_or_cashtag, convert_peer_search_query, create_forum_topic,
    delete_quick_reply_messages, delete_quick_reply_shortcut, delete_saved_history,
    delete_scheduled_messages, delete_topic_history, edit_forum_topic, edit_quick_reply_shortcut,
    get_quick_replies, get_quick_reply_messages, get_saved_reaction_tags, get_scheduled_history,
    get_search_counters, load_forum_topics_by_id, load_forum_topics_page,
    load_pinned_saved_dialogs, load_saved_dialogs_by_id, load_saved_dialogs_page,
    load_saved_history, parse_history_result, parse_search_result, prepare_global_media_request,
    prepare_shared_media_search_request, read_saved_history, read_topic_discussion,
    reorder_pinned_forum_topics, reorder_pinned_saved_dialogs, send_quick_reply_messages,
    send_scheduled_messages, toggle_saved_dialog_pin, update_pinned_forum_topic,
    update_saved_reaction_tag,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    ChangeEvent, Channel, ChannelFlags, Chat, DocumentId, FORUM_GENERAL_TOPIC_ID, FullMsgId,
    Message, MessageFlags, MsgId, MsgRange, Peer, PeerId, PeerUpdateFlags, ReactionId,
    SCHEDULED_MAX_MSG_ID, SCHEDULED_REQUEST_TIME_LIMIT_MS, SCHEDULED_UNTIL_ONLINE_TIMESTAMP,
    SERVER_MAX_MSG_ID, SHORTCUT_MAX_MSG_ID, Session, TextWithEntities, TopicUpdateFlags, User,
    UserId, count_hash, has_scheduled_date, hash_finalize, hash_init, hash_update_i32,
    hash_update_i64, hash_update_u64, scheduled_local_to_remote_id, scheduled_remote_to_local_id,
    shortcut_local_to_remote_id, shortcut_remote_to_local_id,
};
use tg_mtproto::dc_options::DcId;
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, PacketConnection, TransportFactory,
};
use tg_session::{MtprotoRpc, build_mtproto_client_config};
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
// Test-only constants & byte-level scripted MTProto 2.0 fake server
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

fn build_client_and_rpc(server: &ScriptedServer) -> (Client, MtprotoRpc) {
    let creds = ApiCredentials {
        api_id: FAKE_API_ID,
        api_hash: FAKE_API_HASH.to_owned(),
    };
    let mut cfg: ClientConfig =
        build_mtproto_client_config(&creds, 2, tg_core::Environment::Production, None);
    cfg.use_pfs = false;
    let (client, _updates_rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());
    (client, rpc)
}

// ============================================================================
// Synthetic TL Builders (zero real names / phone numbers / credentials)
// ============================================================================

fn tl_peer_user(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser { user_id: id })
}

fn tl_peer_chat(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::Chat(tl_types::PeerChat { chat_id: id })
}

fn tl_peer_channel(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: id })
}

fn tl_user(id: i64, access_hash: i64, first_name: &str, bot: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: id == 100,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot,
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
        bot_forum_view: bot,
        bot_forum_can_manage_topics: bot,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(format!("user_{id}")),
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

fn tl_channel(
    id: i64,
    access_hash: i64,
    title: &str,
    broadcast: bool,
    forum: bool,
) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified: false,
        megagroup: !broadcast,
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
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
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
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    })
}

fn tl_chat(id: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Chat(tl_types::Chat {
        creator: true,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 5,
        date: 1_700_000_000,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    })
}

fn tl_message(
    peer: tl_enums::Peer,
    from: Option<tl_enums::Peer>,
    id: i32,
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
    })
}

#[allow(clippy::too_many_arguments)]
fn tl_forum_topic(
    peer: tl_enums::Peer,
    id: i32,
    title: &str,
    top_message: i32,
    unread_count: i32,
    read_inbox_max_id: i32,
    pinned: bool,
    short: bool,
) -> tl_enums::ForumTopic {
    tl_enums::ForumTopic::ForumTopic(tl_types::ForumTopic {
        my: false,
        closed: false,
        pinned,
        short,
        hidden: false,
        title_missing: false,
        id,
        date: 1_700_000_000 + id,
        peer,
        title: title.to_owned(),
        icon_color: 0x6F_B9_F0,
        icon_emoji_id: None,
        top_message,
        read_inbox_max_id,
        read_outbox_max_id: read_inbox_max_id,
        unread_count,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        from_id: tl_peer_user(100),
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
        draft: None,
    })
}

fn seed_session() -> Session {
    let mut session = Session::new(UserId(100));
    let mut me = User::new(UserId(100), true);
    me.common.loaded_status = tg_data::LoadedStatus::Normal;
    me.access_hash = 1000;
    me.set_name("Self", "", "", "self_acct");
    session.upsert_peer(Peer::User(Box::new(me)), PeerUpdateFlags::NAME);

    let mut u200 = User::new(UserId(200), false);
    u200.common.loaded_status = tg_data::LoadedStatus::Normal;
    u200.access_hash = 2000;
    u200.set_name("Peer", "Two", "", "peer_200");
    session.upsert_peer(Peer::User(Box::new(u200)), PeerUpdateFlags::NAME);

    let mut u300 = User::new(UserId(300), false);
    u300.common.loaded_status = tg_data::LoadedStatus::Normal;
    u300.access_hash = 3000;
    u300.set_name("Peer", "Three", "", "peer_300");
    session.upsert_peer(Peer::User(Box::new(u300)), PeerUpdateFlags::NAME);

    let mut bot = User::new(UserId(400), false);
    bot.common.loaded_status = tg_data::LoadedStatus::Normal;
    bot.access_hash = 4000;
    bot.bot_info = Some(tg_data::BotInfo::default());
    bot.set_name("Helper", "Bot", "", "helper_bot");
    session.upsert_peer(Peer::User(Box::new(bot)), PeerUpdateFlags::NAME);

    let mut ch = Channel::new(tg_data::ChannelId(500));
    ch.common.loaded_status = tg_data::LoadedStatus::Normal;
    ch.access_hash = 5000;
    ch.flags = ChannelFlags::MEGAGROUP | ChannelFlags::FORUM | ChannelFlags::CREATOR;
    ch.set_name("Forum Channel", "forum_500");
    session.upsert_peer(Peer::Channel(Box::new(ch)), PeerUpdateFlags::NAME);

    let mut bc = Channel::new(tg_data::ChannelId(600));
    bc.common.loaded_status = tg_data::LoadedStatus::Normal;
    bc.access_hash = 6000;
    bc.flags = ChannelFlags::BROADCAST;
    bc.set_name("ReadOnly Broadcast", "bc_600");
    session.upsert_peer(Peer::Channel(Box::new(bc)), PeerUpdateFlags::NAME);

    let mut legacy = Chat::new(tg_data::ChatId(700));
    legacy.common.loaded_status = tg_data::LoadedStatus::Normal;
    legacy.set_name("Legacy Group");
    session.upsert_peer(Peer::Chat(Box::new(legacy)), PeerUpdateFlags::NAME);

    session
}

// ============================================================================
// AREA 1: SEARCH CONFORMANCE TESTS
// ============================================================================

#[tokio::test]
async fn search_in_chat_filters_from_saved_topic_tags_paging_cache_and_empty_error() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let ch_peer = PeerId::from_channel(tg_data::ChannelId(500));
        let user_peer = PeerId::from_user(UserId(200));
        let self_peer = PeerId::from_user(UserId(100));
        let from_peer = PeerId::from_user(UserId(300));

        // 1. Verify all 14 SharedMediaFilter -> TL MessagesFilter mappings (`data_search_controller.cpp:29-60`).
        assert!(matches!(
            SharedMediaFilter::Empty.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterEmpty
        ));
        assert!(matches!(
            SharedMediaFilter::Photo.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterPhotos
        ));
        assert!(matches!(
            SharedMediaFilter::Video.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterVideo
        ));
        assert!(matches!(
            SharedMediaFilter::PhotoVideo.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterPhotoVideo
        ));
        assert!(matches!(
            SharedMediaFilter::MusicFile.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterMusic
        ));
        assert!(matches!(
            SharedMediaFilter::File.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterDocument
        ));
        assert!(matches!(
            SharedMediaFilter::VoiceFile.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterVoice
        ));
        assert!(matches!(
            SharedMediaFilter::RoundVoiceFile.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterRoundVoice
        ));
        assert!(matches!(
            SharedMediaFilter::RoundFile.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterRoundVideo
        ));
        assert!(matches!(
            SharedMediaFilter::Gif.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterGif
        ));
        assert!(matches!(
            SharedMediaFilter::Link.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterUrl
        ));
        assert!(matches!(
            SharedMediaFilter::ChatPhoto.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterChatPhotos
        ));
        assert!(matches!(
            SharedMediaFilter::Pinned.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterPinned
        ));
        assert!(matches!(
            SharedMediaFilter::Poll.to_tl(),
            tl_enums::MessagesFilter::InputMessagesFilterPoll
        ));

        // 2. Verify `prepare_shared_media_search_request` exact rules (`data_search_controller.cpp:141-202`).
        assert!(
            prepare_shared_media_search_request(
                &session,
                ch_peer,
                MsgId::ZERO,
                PeerId::ZERO,
                SharedMediaFilter::Empty,
                "",
                MsgId(50),
                LoadDirection::Before
            )
            .is_none()
        );
        assert!(
            prepare_shared_media_search_request(
                &session,
                ch_peer,
                MsgId(10),
                PeerId::ZERO,
                SharedMediaFilter::ChatPhoto,
                "",
                MsgId(50),
                LoadDirection::Before
            )
            .is_none()
        );
        let first_req = prepare_shared_media_search_request(
            &session,
            ch_peer,
            MsgId(10),
            user_peer,
            SharedMediaFilter::PhotoVideo,
            "",
            MsgId::ZERO,
            LoadDirection::Before,
        )
        .unwrap();
        assert_eq!(first_req.limit, FIRST_SHARED_MEDIA_LIMIT);
        assert_eq!(first_req.top_msg_id, Some(10));
        assert!(first_req.saved_peer_id.is_some());

        let around_req = prepare_shared_media_search_request(
            &session,
            ch_peer,
            MsgId::ZERO,
            PeerId::ZERO,
            SharedMediaFilter::File,
            "report",
            MsgId(500),
            LoadDirection::Around,
        )
        .unwrap();
        assert_eq!(around_req.limit, SHARED_MEDIA_LIMIT);
        assert_eq!(around_req.offset_id, 500);
        assert_eq!(around_req.add_offset, -SHARED_MEDIA_LIMIT / 2);

        let after_clamped = prepare_shared_media_search_request(
            &session,
            ch_peer,
            MsgId::ZERO,
            PeerId::ZERO,
            SharedMediaFilter::Link,
            "",
            MsgId(0x5000_0000),
            LoadDirection::After,
        )
        .unwrap();
        assert_eq!(after_clamped.offset_id, 0x3FFF_FFFF);
        assert_eq!(after_clamped.add_offset, -SHARED_MEDIA_LIMIT);

        // 3. Scripted MTProto 2.0 server for `messages.search`.
        let server = ScriptedServer::new(|_, rpc| {
            assert_eq!(rpc.ctor, tl_fns::messages::Search::CONSTRUCTOR_ID);
            let req = tl_fns::messages::Search::from_bytes(&rpc.body).unwrap();
            if req.q == "empty_err" {
                return ServerAction::err(400, "SEARCH_QUERY_EMPTY");
            }
            if req.offset_id == 0 {
                ServerAction::ok(&tl_enums::messages::Messages::ChannelMessages(
                    tl_types::messages::ChannelMessages {
                        inexact: false,
                        pts: 77,
                        count: 3,
                        offset_id_offset: None,
                        messages: vec![
                            tl_message(
                                tl_peer_channel(500),
                                Some(tl_peer_user(300)),
                                50,
                                1_700_000_050,
                                "hit one",
                            ),
                            tl_message(
                                tl_peer_channel(500),
                                Some(tl_peer_user(300)),
                                40,
                                1_700_000_040,
                                "hit two",
                            ),
                        ],
                        topics: vec![tl_forum_topic(
                            tl_peer_channel(500),
                            10,
                            "Topic Ten",
                            50,
                            2,
                            45,
                            false,
                            false,
                        )],
                        chats: vec![tl_channel(500, 5000, "Forum Channel", false, true)],
                        users: vec![tl_user(300, 3000, "Three", false)],
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::messages::Messages::ChannelMessages(
                    tl_types::messages::ChannelMessages {
                        inexact: false,
                        pts: 78,
                        count: 3,
                        offset_id_offset: None,
                        messages: vec![tl_message(
                            tl_peer_channel(500),
                            Some(tl_peer_user(300)),
                            25,
                            1_700_000_025,
                            "hit three",
                        )],
                        topics: vec![],
                        chats: vec![],
                        users: vec![],
                    },
                ))
            }
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();

        // Channel search with `from_id` + `top_msg_id` + paging + start-page cache.
        let mut ch_search = MessagesSearch::new(ch_peer);
        let req_params = InChatSearchRequest {
            query: "hit".to_owned(),
            from: Some(from_peer),
            saved_peer_id: None,
            top_msg_id: Some(MsgId(10)),
            tags: vec![],
            filter: SharedMediaFilter::Empty,
            min_date: 0,
            max_date: 0,
        };
        let p1 = ch_search
            .search_messages(
                &rpc,
                &mut session,
                Some(&mut updates),
                req_params.clone(),
                1_700_000_100,
            )
            .await
            .unwrap();
        assert_eq!(p1.total, 3);
        assert_eq!(p1.messages.len(), 2);
        assert_eq!(ch_search.offset_id(), MsgId(40));
        assert_eq!(updates.channel_pts(tg_data::ChannelId(500)), Some(77));
        assert_eq!(
            session.forum_topic(ch_peer, MsgId(10)).unwrap().title,
            "Topic Ten"
        );

        let p2 = ch_search
            .search_more(&rpc, &mut session, Some(&mut updates), 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(p2.messages.len(), 1);
        assert_eq!(ch_search.offset_id(), MsgId(25));

        let rpcs_before_cache = server.rpcs_of::<tl_fns::messages::Search>().len();
        let cached = ch_search
            .search_messages(
                &rpc,
                &mut session,
                Some(&mut updates),
                req_params,
                1_700_000_100,
            )
            .await
            .unwrap();
        assert_eq!(cached.messages.len(), 2);
        assert_eq!(
            server.rpcs_of::<tl_fns::messages::Search>().len(),
            rpcs_before_cache,
            "start-page cache must serve repeat query without a new RPC"
        );

        // Private user chat MUST omit `from_id` (`api_messages_search.cpp:115`).
        let mut user_search = MessagesSearch::new(user_peer);
        let _ = user_search
            .search_messages(
                &rpc,
                &mut session,
                None,
                InChatSearchRequest {
                    query: "dm".to_owned(),
                    from: Some(from_peer),
                    ..Default::default()
                },
                1_700_000_100,
            )
            .await
            .unwrap();

        // Own Saved Messages moves `from` -> `saved_peer_id` AND encodes `saved_reaction`.
        let mut self_search = MessagesSearch::new(self_peer);
        let _ = self_search
            .search_messages(
                &rpc,
                &mut session,
                None,
                InChatSearchRequest {
                    query: "notes".to_owned(),
                    from: Some(user_peer),
                    tags: vec![
                        ReactionId::Emoji("🔥".to_owned()),
                        ReactionId::Custom(DocumentId(999_001)),
                    ],
                    filter: SharedMediaFilter::Pinned,
                    ..Default::default()
                },
                1_700_000_100,
            )
            .await
            .unwrap();

        // `SEARCH_QUERY_EMPTY` must return Ok(empty).
        let empty_res = self_search
            .search_messages(
                &rpc,
                &mut session,
                None,
                InChatSearchRequest {
                    query: "empty_err".to_owned(),
                    ..Default::default()
                },
                1_700_000_100,
            )
            .await
            .unwrap();
        assert_eq!(empty_res.total, 0);
        assert!(empty_res.messages.is_empty());

        let sent_reqs: Vec<tl_fns::messages::Search> = server
            .rpcs_of::<tl_fns::messages::Search>()
            .into_iter()
            .map(|r| tl_fns::messages::Search::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(sent_reqs.len(), 5);
        // Req 0: channel page 1
        assert!(sent_reqs[0].from_id.is_some());
        assert_eq!(sent_reqs[0].top_msg_id, Some(10));
        assert_eq!(sent_reqs[0].offset_id, 0);
        assert_eq!(sent_reqs[0].limit, SEARCH_PER_PAGE);
        assert_eq!(sent_reqs[0].hash, 0);
        // Req 1: channel page 2
        assert_eq!(sent_reqs[1].offset_id, 40);
        // Req 2: private user omits `from_id` and `saved_peer_id`
        assert!(sent_reqs[2].from_id.is_none());
        assert!(sent_reqs[2].saved_peer_id.is_none());
        // Req 3: self Saved Messages sets `saved_peer_id`, omits `from_id`, sets `saved_reaction` + `Pinned`
        assert!(sent_reqs[3].from_id.is_none());
        assert!(sent_reqs[3].saved_peer_id.is_some());
        assert_eq!(sent_reqs[3].saved_reaction.as_ref().map(Vec::len), Some(2));
        assert!(matches!(
            sent_reqs[3].filter,
            tl_enums::MessagesFilter::InputMessagesFilterPinned
        ));

        // 4. Verify `parse_search_result` empty boundary expansion (`data_search_controller.cpp:278-290`).
        let empty_slice = tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![],
            topics: vec![],
            chats: vec![],
            users: vec![],
        });
        let r_before = parse_search_result(
            &mut session,
            None,
            ch_peer,
            MsgId(50),
            LoadDirection::Before,
            &empty_slice,
            1_700_000_100,
        );
        assert_eq!(
            r_before.no_skip_range,
            MsgRange::new(MsgId::ZERO, MsgId(50))
        );
        let r_after = parse_search_result(
            &mut session,
            None,
            ch_peer,
            MsgId(50),
            LoadDirection::After,
            &empty_slice,
            1_700_000_100,
        );
        assert_eq!(
            r_after.no_skip_range,
            MsgRange::new(MsgId(50), SERVER_MAX_MSG_ID)
        );
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn search_in_chat_merged_migrated_supergroup_and_legacy_chat() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let ch_peer = PeerId::from_channel(tg_data::ChannelId(500));
        let legacy_peer = PeerId::from_chat(tg_data::ChatId(700));

        let server = ScriptedServer::new(|_, rpc| {
            let req = tl_fns::messages::Search::from_bytes(&rpc.body).unwrap();
            match req.peer {
                tl_enums::InputPeer::Channel(_) if req.offset_id == 0 => {
                    ServerAction::ok(&tl_enums::messages::Messages::ChannelMessages(
                        tl_types::messages::ChannelMessages {
                            inexact: false,
                            pts: 10,
                            count: 2,
                            offset_id_offset: None,
                            messages: vec![tl_message(
                                tl_peer_channel(500),
                                None,
                                90,
                                1_700_000_090,
                                "supergroup 1",
                            )],
                            topics: vec![],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
                tl_enums::InputPeer::Channel(_) => {
                    ServerAction::ok(&tl_enums::messages::Messages::ChannelMessages(
                        tl_types::messages::ChannelMessages {
                            inexact: false,
                            pts: 10,
                            count: 2,
                            offset_id_offset: None,
                            messages: vec![tl_message(
                                tl_peer_channel(500),
                                None,
                                80,
                                1_700_000_080,
                                "supergroup 2",
                            )],
                            topics: vec![],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
                tl_enums::InputPeer::Chat(_) if req.offset_id == 0 => ServerAction::ok(
                    &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
                        inexact: false,
                        count: 2,
                        next_rate: None,
                        offset_id_offset: None,
                        search_flood: None,
                        messages: vec![tl_message(
                            tl_peer_chat(700),
                            Some(tl_peer_user(200)),
                            30,
                            1_699_000_030,
                            "legacy 1",
                        )],
                        topics: vec![],
                        chats: vec![tl_chat(700, "Legacy Group")],
                        users: vec![],
                    }),
                ),
                tl_enums::InputPeer::Chat(_) => ServerAction::ok(
                    &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
                        messages: vec![tl_message(
                            tl_peer_chat(700),
                            Some(tl_peer_user(200)),
                            10,
                            1_699_000_010,
                            "legacy 2",
                        )],
                        topics: vec![],
                        chats: vec![],
                        users: vec![],
                    }),
                ),
                other => panic!("unexpected peer: {other:?}"),
            }
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut merged = MessagesSearchMerged::new(ch_peer, Some(legacy_peer));

        let first = merged
            .search(
                &rpc,
                &mut session,
                None,
                InChatSearchRequest {
                    query: "legacy".to_owned(),
                    ..Default::default()
                },
                1_700_000_100,
            )
            .await
            .unwrap();
        assert_eq!(first.total, 4, "total must sum supergroup (2) + legacy (2)");
        assert_eq!(first.messages, vec![FullMsgId::new(ch_peer, MsgId(90))]);

        // Second page finishes supergroup (80) and automatically appends cached legacy page 1 (30).
        let second = merged
            .search_more(&rpc, &mut session, None, 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(
            second.messages,
            vec![
                FullMsgId::new(ch_peer, MsgId(90)),
                FullMsgId::new(ch_peer, MsgId(80)),
                FullMsgId::new(legacy_peer, MsgId(30)),
            ]
        );

        // Third page fetches legacy page 2 with `offset_id = 30`.
        let third = merged
            .search_more(&rpc, &mut session, None, 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(third.messages.len(), 4);
        assert_eq!(third.messages[3], FullMsgId::new(legacy_peer, MsgId(10)));
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn search_global_paging_scopes_chat_types_and_global_media() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let u200 = PeerId::from_user(UserId(200));

        // Verify `prepare_global_media_request` (`data_search_controller.cpp:62-93`).
        let gm_first = prepare_global_media_request(
            &session,
            0,
            None,
            MsgId::ZERO,
            SharedMediaFilter::Photo,
            "",
        )
        .unwrap();
        assert_eq!(gm_first.folder_id, Some(0), "GlobalMedia must exclude archive (folder_id=0)");
        assert_eq!(gm_first.limit, FIRST_SHARED_MEDIA_LIMIT);
        let gm_next = prepare_global_media_request(
            &session,
            1_700_000_050,
            Some(u200),
            MsgId(42),
            SharedMediaFilter::Photo,
            "",
        )
        .unwrap();
        assert_eq!(gm_next.limit, SHARED_MEDIA_LIMIT);
        assert_eq!(gm_next.offset_rate, 1_700_000_050);
        assert_eq!(gm_next.offset_id, 42);

        let page_counter = Arc::new(AtomicU32::new(0));
        let pc = page_counter.clone();
        let server = ScriptedServer::new(move |_, rpc| {
            assert_eq!(rpc.ctor, tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID);
            let req = tl_fns::messages::SearchGlobal::from_bytes(&rpc.body).unwrap();
            let call = pc.fetch_add(1, Ordering::SeqCst);
            match call {
                0 => {
                    assert_eq!(req.offset_rate, 0);
                    assert_eq!(req.offset_id, 0);
                    assert!(matches!(req.offset_peer, tl_enums::InputPeer::Empty));
                    assert_eq!(req.folder_id, Some(1));
                    assert!(req.broadcasts_only && !req.groups_only && !req.users_only);
                    ServerAction::ok(&tl_enums::messages::Messages::Slice(
                        tl_types::messages::MessagesSlice {
                            inexact: false,
                            count: 5,
                            next_rate: Some(1_700_000_080),
                            offset_id_offset: None,
                            search_flood: None,
                            messages: vec![tl_message(
                                tl_peer_user(200),
                                Some(tl_peer_user(200)),
                                101,
                                1_700_000_080,
                                "global page 1",
                            )],
                            topics: vec![],
                            chats: vec![],
                            users: vec![tl_user(200, 2000, "Two", false)],
                        },
                    ))
                }
                1 => {
                    // Page 2 MUST carry previous `next_rate`, `offset_peer = User(200)`, `offset_id = 101`.
                    assert_eq!(req.offset_rate, 1_700_000_080);
                    assert_eq!(req.offset_id, 101);
                    assert!(matches!(
                        req.offset_peer,
                        tl_enums::InputPeer::User(ref u) if u.user_id == 200 && u.access_hash == 2000
                    ));
                    // Returning identical `next_rate` MUST terminate pagination (`dialogs_widget.cpp:3669-3673`).
                    ServerAction::ok(&tl_enums::messages::Messages::Slice(
                        tl_types::messages::MessagesSlice {
                            inexact: false,
                            count: 5,
                            next_rate: Some(1_700_000_080),
                            offset_id_offset: None,
                            search_flood: None,
                            messages: vec![tl_message(
                                tl_peer_user(300),
                                Some(tl_peer_user(300)),
                                99,
                                1_700_000_070,
                                "global page 2 stalled rate",
                            )],
                            topics: vec![],
                            chats: vec![],
                            users: vec![tl_user(300, 3000, "Three", false)],
                        },
                    ))
                }
                _ => {
                    assert!(req.users_only && !req.groups_only && !req.broadcasts_only);
                    assert_eq!(req.folder_id, Some(0));
                    ServerAction::ok(&tl_enums::messages::Messages::Messages(
                        tl_types::messages::Messages {
                            messages: vec![],
                            topics: vec![],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
            }
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut ctrl = GlobalSearchController::new();

        let p1 = ctrl
            .search(
                &rpc,
                &mut session,
                GlobalSearchParams {
                    query: "global".to_owned(),
                    chat_type: GlobalChatTypeFilter::Channels,
                    scope: GlobalSearchScope::ArchiveOnly,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!p1.finished);
        assert_eq!(p1.next_rate, 1_700_000_080);
        assert_eq!(p1.last_peer, Some(u200));
        assert_eq!(p1.last_id, MsgId(101));

        let p2 = ctrl.load_more(&rpc, &mut session).await.unwrap().unwrap();
        assert!(p2.finished, "unchanged next_rate must finish global search");
        assert!(ctrl.load_more(&rpc, &mut session).await.unwrap().is_none());

        let p3 = ctrl
            .search(
                &rpc,
                &mut session,
                GlobalSearchParams {
                    query: "dm".to_owned(),
                    chat_type: GlobalChatTypeFilter::Private,
                    scope: GlobalSearchScope::MainOnly,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(p3.finished);
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn search_counters_contacts_and_public_posts_hashtag_flood() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let ch_peer = PeerId::from_channel(tg_data::ChannelId(500));
        let u200 = PeerId::from_user(UserId(200));

        // Verify hashtag / cashtag classification & peer query conversion.
        assert_eq!(classify_hash_or_cashtag("#rust"), HashOrCashtag::Hashtag);
        assert_eq!(classify_hash_or_cashtag("#rust lang"), HashOrCashtag::None);
        assert_eq!(classify_hash_or_cashtag("$TON"), HashOrCashtag::Cashtag);
        assert_eq!(classify_hash_or_cashtag("$ton"), HashOrCashtag::None);
        assert_eq!(
            convert_peer_search_query("tg://resolve?domain=newsroom&post=5"),
            "newsroom"
        );

        let posts_calls = Arc::new(AtomicU32::new(0));
        let flood_calls = Arc::new(AtomicU32::new(0));
        let pc = posts_calls.clone();
        let fc = flood_calls.clone();

        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::messages::GetSearchCounters::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetSearchCounters::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.top_msg_id, Some(10));
                assert!(req.saved_peer_id.is_some());
                assert_eq!(req.filters.len(), 2);
                ServerAction::ok(&vec![
                    tl_enums::messages::SearchCounter::SearchCounter(
                        tl_types::messages::SearchCounter {
                            inexact: false,
                            filter: tl_enums::MessagesFilter::InputMessagesFilterPhotos,
                            count: 14,
                        },
                    ),
                    tl_enums::messages::SearchCounter::SearchCounter(
                        tl_types::messages::SearchCounter {
                            inexact: true,
                            filter: tl_enums::MessagesFilter::InputMessagesFilterDocument,
                            count: 100,
                        },
                    ),
                ])
            }
            tl_fns::contacts::Search::CONSTRUCTOR_ID => {
                let req = tl_fns::contacts::Search::from_bytes(&rpc.body).unwrap();
                ServerAction::ok(&tl_enums::contacts::Found::Found(
                    tl_types::contacts::Found {
                        my_results: vec![tl_peer_user(200)],
                        results: vec![tl_peer_channel(500)],
                        chats: vec![tl_channel(
                            500,
                            5000,
                            "Forum Channel",
                            req.broadcasts,
                            false,
                        )],
                        users: vec![tl_user(200, 2000, "Two", req.bots)],
                    },
                ))
            }
            tl_fns::channels::CheckSearchPostsFlood::CONSTRUCTOR_ID => {
                let req = tl_fns::channels::CheckSearchPostsFlood::from_bytes(&rpc.body).unwrap();
                let n = fc.fetch_add(1, Ordering::SeqCst);
                let free = req.query.as_deref() == Some("free_topic");
                ServerAction::ok(&tl_enums::SearchPostsFlood::SearchPostsFlood(
                    tl_types::SearchPostsFlood {
                        query_is_free: free,
                        total_daily: 10,
                        remains: if free { 3 } else { 0 },
                        wait_till: Some(1_700_005_000 + n as i32),
                        stars_amount: 10,
                    },
                ))
            }
            tl_fns::channels::SearchPosts::CONSTRUCTOR_ID => {
                let req = tl_fns::channels::SearchPosts::from_bytes(&rpc.body).unwrap();
                if req.query.as_deref() == Some("free_topic") && req.offset_id == 0 {
                    // Simulate FLOOD_WAIT_15 on initial page -> must trigger CheckSearchPostsFlood re-check.
                    let n = pc.fetch_add(1, Ordering::SeqCst);
                    if n == 2 {
                        return ServerAction::err(420, "FLOOD_WAIT_15");
                    }
                }
                if req.hashtag.is_some() && req.offset_id == 0 {
                    pc.fetch_add(1, Ordering::SeqCst);
                    ServerAction::ok(&tl_enums::messages::Messages::Slice(
                        tl_types::messages::MessagesSlice {
                            inexact: false,
                            count: 2,
                            next_rate: None,
                            offset_id_offset: None,
                            search_flood: None,
                            messages: vec![tl_message(
                                tl_peer_channel(600),
                                None,
                                500,
                                1_700_000_500,
                                "#rust post 1",
                            )],
                            topics: vec![],
                            chats: vec![tl_channel(600, 6000, "Public Chan", true, false)],
                            users: vec![],
                        },
                    ))
                } else if req.hashtag.is_some() {
                    pc.fetch_add(1, Ordering::SeqCst);
                    ServerAction::ok(&tl_enums::messages::Messages::Slice(
                        tl_types::messages::MessagesSlice {
                            inexact: false,
                            count: 2,
                            next_rate: None,
                            offset_id_offset: None,
                            search_flood: None,
                            messages: vec![],
                            topics: vec![],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::messages::Messages::Messages(
                        tl_types::messages::Messages {
                            messages: vec![tl_message(
                                tl_peer_channel(600),
                                None,
                                501,
                                1_700_000_501,
                                "paid result",
                            )],
                            topics: vec![],
                            chats: vec![tl_channel(600, 6000, "Public Chan", true, false)],
                            users: vec![],
                        },
                    ))
                }
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);

        // 1. `messages.getSearchCounters`
        let counters = get_search_counters(
            &rpc,
            &session,
            ch_peer,
            Some(u200),
            Some(MsgId(10)),
            &[SharedMediaFilter::Photo, SharedMediaFilter::File],
        )
        .await
        .unwrap();
        assert_eq!(counters.len(), 2);
        assert_eq!(counters[0].count, 14);
        assert!(!counters[0].inexact);
        assert_eq!(counters[1].count, 100);
        assert!(counters[1].inexact);

        // 2. `contacts.search`: Default (limit=5), Channels (broadcasts=true, limit=50), Bots (bots=true, limit=50),
        //    hashtag/cashtag skip, and cache hit.
        let mut default_ps = PeerSearchController::new(PeerSearchType::Default);
        let skipped = default_ps
            .search(&rpc, &mut session, "#rust")
            .await
            .unwrap();
        assert!(skipped.my_results.is_empty());
        assert_eq!(server.rpcs_of::<tl_fns::contacts::Search>().len(), 0);

        let res = default_ps
            .search(&rpc, &mut session, "tg://resolve?domain=newsroom")
            .await
            .unwrap();
        assert_eq!(res.query, "newsroom");
        assert_eq!(res.my_results, vec![u200]);
        // Repeat hits cache (0 new RPCs).
        let _ = default_ps
            .search(&rpc, &mut session, "newsroom")
            .await
            .unwrap();
        assert_eq!(server.rpcs_of::<tl_fns::contacts::Search>().len(), 1);

        let mut ch_ps = PeerSearchController::new(PeerSearchType::Channels);
        let _ = ch_ps.search(&rpc, &mut session, "news").await.unwrap();
        let mut bot_ps = PeerSearchController::new(PeerSearchType::Bots);
        let _ = bot_ps.search(&rpc, &mut session, "helper").await.unwrap();

        let contact_reqs: Vec<tl_fns::contacts::Search> = server
            .rpcs_of::<tl_fns::contacts::Search>()
            .into_iter()
            .map(|r| tl_fns::contacts::Search::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(contact_reqs.len(), 3);
        assert_eq!(contact_reqs[0].limit, SEARCH_PEOPLE_LIMIT);
        assert!(!contact_reqs[0].broadcasts && !contact_reqs[0].bots);
        assert_eq!(contact_reqs[1].limit, PEER_SEARCH_TYPED_LIMIT);
        assert!(contact_reqs[1].broadcasts && !contact_reqs[1].bots);
        assert_eq!(contact_reqs[2].limit, PEER_SEARCH_TYPED_LIMIT);
        assert!(!contact_reqs[2].broadcasts && contact_reqs[2].bots);

        // 3. Hashtag `channels.searchPosts`: strips `#`, continues on `next_rate == None` until empty.
        let mut posts = PostsSearchController::new();
        let hp1 = posts
            .search_hashtag(&rpc, &mut session, "#rust")
            .await
            .unwrap();
        assert!(
            !hp1.finished,
            "hashtag search must finish only when messages list is empty"
        );
        assert_eq!(hp1.messages.len(), 1);
        let hp2 = posts.load_more(&rpc, &mut session).await.unwrap().unwrap();
        assert!(hp2.finished);

        // 4. Paid posts search: `checkSearchPostsFlood` -> `query_is_free == false` -> Stars clamp.
        let maybe_page = posts
            .search_query(&rpc, &mut session, "paid_q")
            .await
            .unwrap();
        assert!(maybe_page.is_none());
        let flood = posts.flood_state().unwrap();
        assert_eq!(flood.stars_amount, 10);
        // Requesting 50 stars clamps down to `stars_amount = 10` (`dialogs_search_posts.cpp:126`).
        let paid_page = posts
            .search_query_with_stars(&rpc, &mut session, 50, 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(paid_page.messages.len(), 1);

        // 5. Initial page `FLOOD_WAIT_15` surfaces error AND triggers `checkSearchPostsFlood` re-check.
        let flood_before = server
            .rpcs_of::<tl_fns::channels::CheckSearchPostsFlood>()
            .len();
        let err = posts
            .search_query(&rpc, &mut session, "free_topic")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "FLOOD_WAIT_15"
        ));
        assert_eq!(
            server
                .rpcs_of::<tl_fns::channels::CheckSearchPostsFlood>()
                .len(),
            flood_before + 2,
            "initial check + FLOOD_WAIT re-check must both fire"
        );

        let post_reqs: Vec<tl_fns::channels::SearchPosts> = server
            .rpcs_of::<tl_fns::channels::SearchPosts>()
            .into_iter()
            .map(|r| tl_fns::channels::SearchPosts::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(post_reqs[0].hashtag.as_deref(), Some("rust"));
        assert!(post_reqs[0].query.is_none());
        assert_eq!(post_reqs[1].offset_id, 500);
        assert_eq!(post_reqs[2].allow_paid_stars, Some(10));
    })
    .await
    .expect("test timeout");
}

// ============================================================================
// AREA 2: FORUM TOPICS CONFORMANCE TESTS
// ============================================================================

#[tokio::test]
async fn forum_topics_paging_get_by_id_deleted_min_and_channel_forum_missing() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let ch_peer = PeerId::from_channel(tg_data::ChannelId(500));

        let server = ScriptedServer::new(|_, rpc| match rpc.ctor {
            tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetForumTopics::from_bytes(&rpc.body).unwrap();
                if req.q.as_deref() == Some("missing") {
                    return ServerAction::err(400, "CHANNEL_FORUM_MISSING");
                }
                if req.offset_date == 0 {
                    assert_eq!(req.limit, 20, "first page must use kTopicsFirstLoad = 20");
                    assert_eq!(req.offset_id, 0);
                    assert_eq!(req.offset_topic, 0);
                    ServerAction::ok(&tl_enums::messages::ForumTopics::ForumTopics(
                        tl_types::messages::ForumTopics {
                            order_by_create_date: false,
                            count: 3,
                            topics: vec![
                                tl_forum_topic(
                                    tl_peer_channel(500),
                                    1,
                                    "General",
                                    100,
                                    5,
                                    95,
                                    true,
                                    false,
                                ),
                                tl_forum_topic(
                                    tl_peer_channel(500),
                                    20,
                                    "Announcements",
                                    80,
                                    2,
                                    78,
                                    false,
                                    false,
                                ),
                            ],
                            messages: vec![
                                tl_message(
                                    tl_peer_channel(500),
                                    None,
                                    100,
                                    1_700_000_100,
                                    "gen top",
                                ),
                                tl_message(
                                    tl_peer_channel(500),
                                    None,
                                    80,
                                    1_700_000_080,
                                    "ann top",
                                ),
                            ],
                            chats: vec![],
                            users: vec![],
                            pts: 55,
                        },
                    ))
                } else {
                    assert_eq!(
                        req.limit, 500,
                        "subsequent pages must use kTopicsPerPage = 500"
                    );
                    assert_eq!(req.offset_date, 1_700_000_080);
                    assert_eq!(req.offset_id, 80);
                    assert_eq!(req.offset_topic, 20);
                    ServerAction::ok(&tl_enums::messages::ForumTopics::ForumTopics(
                        tl_types::messages::ForumTopics {
                            order_by_create_date: false,
                            count: 3,
                            topics: vec![
                                // `short = true` (`min`) topic for id=20 must NOT clobber unread_count=2!
                                tl_forum_topic(
                                    tl_peer_channel(500),
                                    20,
                                    "Announcements Renamed",
                                    0,
                                    0,
                                    0,
                                    false,
                                    true,
                                ),
                                tl_forum_topic(
                                    tl_peer_channel(500),
                                    30,
                                    "Third",
                                    60,
                                    0,
                                    60,
                                    false,
                                    false,
                                ),
                            ],
                            messages: vec![tl_message(
                                tl_peer_channel(500),
                                None,
                                60,
                                1_700_000_060,
                                "third top",
                            )],
                            chats: vec![],
                            users: vec![],
                            pts: 56,
                        },
                    ))
                }
            }
            tl_fns::messages::GetForumTopicsByID::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetForumTopicsByID::from_bytes(&rpc.body).unwrap();
                assert!(
                    req.topics.len() <= 100,
                    "stale batches must cap at kStalePerRequest=100"
                );
                let topics = if req.topics.len() == 100 {
                    vec![
                        tl_enums::ForumTopic::Deleted(tl_types::ForumTopicDeleted { id: 1 }),
                        tl_enums::ForumTopic::Deleted(tl_types::ForumTopicDeleted { id: 30 }),
                    ]
                } else {
                    vec![]
                };
                ServerAction::ok(&tl_enums::messages::ForumTopics::ForumTopics(
                    tl_types::messages::ForumTopics {
                        order_by_create_date: false,
                        count: topics.len() as i32,
                        topics,
                        messages: vec![],
                        chats: vec![],
                        users: vec![],
                        pts: 57,
                    },
                ))
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();

        // Page 1 (limit=20)
        let p1 = load_forum_topics_page(
            &rpc,
            &mut session,
            Some(&mut updates),
            ch_peer,
            None,
            1_700_000_200,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!p1.all_loaded);
        assert_eq!(p1.next_offset.date, 1_700_000_080);
        assert_eq!(p1.next_offset.id, MsgId(80));
        assert_eq!(p1.next_offset.topic_id, MsgId(20));
        assert_eq!(updates.channel_pts(tg_data::ChannelId(500)), Some(55));

        // Page 2 (limit=500, verifying `short=true` preserves unread_count=2 on topic 20!)
        let p2 = load_forum_topics_page(
            &rpc,
            &mut session,
            Some(&mut updates),
            ch_peer,
            None,
            1_700_000_200,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(p2.next_offset.topic_id, MsgId(30));
        let t20 = session.forum_topic(ch_peer, MsgId(20)).unwrap();
        assert_eq!(t20.title, "Announcements Renamed");
        assert_eq!(
            t20.unread_count,
            Some(2),
            "short/min topic must preserve existing unread_count (`data_forum_topic.cpp:408`)"
        );
        assert_eq!(t20.read_inbox_max_id, MsgId(78));

        // `getForumTopicsByID` batching (105 IDs -> 2 RPCs: 100 + 5) & `ForumTopicDeleted`:
        // General (id=1) MUST be preserved, while non-General (id=30) MUST be deleted!
        let ids: Vec<MsgId> = (1..=105).map(MsgId).collect();
        let (_applied, deleted) = load_forum_topics_by_id(
            &rpc,
            &mut session,
            Some(&mut updates),
            ch_peer,
            &ids,
            1_700_000_200,
        )
        .await
        .unwrap();
        assert_eq!(
            server
                .rpcs_of::<tl_fns::messages::GetForumTopicsByID>()
                .len(),
            2
        );
        assert!(
            session
                .forum_topic(ch_peer, FORUM_GENERAL_TOPIC_ID)
                .is_some()
        );
        assert!(session.forum_topic(ch_peer, MsgId(30)).is_none());
        assert_eq!(deleted, vec![MsgId(30)]);

        // `CHANNEL_FORUM_MISSING` must clear `ChannelFlags::FORUM` (`data_forum.cpp:210-212`).
        let err = load_forum_topics_page(
            &rpc,
            &mut session,
            None,
            ch_peer,
            Some("missing".to_owned()),
            1_700_000_200,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "CHANNEL_FORUM_MISSING"
        ));
        assert!(
            !session
                .channel(tg_data::ChannelId(500))
                .unwrap()
                .flags
                .contains(ChannelFlags::FORUM)
        );
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn forum_topics_crud_pin_reorder_delete_history_and_live_updates() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let ch_peer = PeerId::from_channel(tg_data::ChannelId(500));
        let bot_peer = PeerId::from_user(UserId(400));

        // Seed topics 1, 10, 20.
        {
            let forum = session.ensure_forum(ch_peer);
            let t1 = forum.ensure_topic(MsgId(1));
            t1.title = "General".to_owned();
            t1.last_message_date = 1_700_000_010;
            let t10 = forum.ensure_topic(MsgId(10));
            t10.title = "Alpha".to_owned();
            t10.last_message_date = 1_700_000_020;
            let t20 = forum.ensure_topic(MsgId(20));
            t20.title = "Beta".to_owned();
            t20.last_message_date = 1_700_000_030;
        }

        let delete_step = Arc::new(AtomicU32::new(0));
        let ds = delete_step.clone();
        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 100,
                    qts: 0,
                    date: 1_700_000_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::CreateForumTopic::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![],
                    users: vec![],
                    chats: vec![],
                    date: 1_700_000_100,
                    seq: 0,
                }))
            }
            tl_fns::messages::EditForumTopic::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::EditForumTopic::from_bytes(&rpc.body).unwrap();
                if req.title.as_deref() == Some("unchanged") {
                    ServerAction::err(400, "TOPIC_NOT_MODIFIED")
                } else {
                    ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                        updates: vec![],
                        users: vec![],
                        chats: vec![],
                        date: 1_700_000_101,
                        seq: 0,
                    }))
                }
            }
            tl_fns::messages::UpdatePinnedForumTopic::CONSTRUCTOR_ID
            | tl_fns::messages::ReorderPinnedForumTopics::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![],
                    users: vec![],
                    chats: vec![],
                    date: 1_700_000_102,
                    seq: 0,
                }))
            }
            tl_fns::messages::DeleteTopicHistory::CONSTRUCTOR_ID => {
                let n = ds.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                        tl_types::messages::AffectedHistory {
                            pts: 201,
                            pts_count: 1,
                            offset: 50,
                        },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                        tl_types::messages::AffectedHistory {
                            pts: 202,
                            pts_count: 1,
                            offset: 0,
                        },
                    ))
                }
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();
        updates
            .initialize(&rpc, &mut session, 1_700_000_000_000)
            .await
            .unwrap();
        updates.init_channel_pts(tg_data::ChannelId(500), 200);

        // 1. `createForumTopic`: channel (`title_missing=false`) vs bot (`title_missing=true`).
        let _ = create_forum_topic(
            &rpc,
            &session,
            ch_peer,
            "New Channel Topic".to_owned(),
            Some(0x6F_B9_F0),
            Some(9999),
            111_222,
            None,
        )
        .await
        .unwrap();
        let _ = create_forum_topic(
            &rpc,
            &session,
            bot_peer,
            "Bot Thread".to_owned(),
            Some(0xFF_D6_7E),
            None,
            333_444,
            None,
        )
        .await
        .unwrap();
        let create_reqs: Vec<tl_fns::messages::CreateForumTopic> = server
            .rpcs_of::<tl_fns::messages::CreateForumTopic>()
            .into_iter()
            .map(|r| tl_fns::messages::CreateForumTopic::from_bytes(&r.body).unwrap())
            .collect();
        assert!(!create_reqs[0].title_missing);
        assert_eq!(create_reqs[0].icon_emoji_id, Some(9999));
        assert!(
            create_reqs[1].title_missing,
            "bot forum must set f_title_missing"
        );

        // 2. `editForumTopic`: General omits `icon_emoji_id`; `TOPIC_NOT_MODIFIED` -> Ok(None).
        let _ = edit_forum_topic(
            &rpc,
            &session,
            ch_peer,
            FORUM_GENERAL_TOPIC_ID,
            ForumTopicEdit {
                title: Some("General Renamed".to_owned()),
                icon_emoji_id: Some(8888),
                closed: None,
                hidden: Some(true),
            },
        )
        .await
        .unwrap();
        let not_mod = edit_forum_topic(
            &rpc,
            &session,
            ch_peer,
            MsgId(10),
            ForumTopicEdit {
                title: Some("unchanged".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(not_mod.is_none());
        let edit_reqs: Vec<tl_fns::messages::EditForumTopic> = server
            .rpcs_of::<tl_fns::messages::EditForumTopic>()
            .into_iter()
            .map(|r| tl_fns::messages::EditForumTopic::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(
            edit_reqs[0].icon_emoji_id, None,
            "General topic edit MUST omit icon_emoji_id (`edit_forum_topic_box.cpp:566`)"
        );
        assert_eq!(edit_reqs[0].hidden, Some(true));

        // 3. `updatePinnedForumTopic` & `reorderPinnedForumTopics(force=true)`.
        let _ = update_pinned_forum_topic(&rpc, &session, ch_peer, MsgId(10), true)
            .await
            .unwrap();
        let _ = reorder_pinned_forum_topics(&rpc, &session, ch_peer, &[MsgId(20), MsgId(10)])
            .await
            .unwrap();
        let reorder_req = tl_fns::messages::ReorderPinnedForumTopics::from_bytes(
            &server.rpcs_of::<tl_fns::messages::ReorderPinnedForumTopics>()[0].body,
        )
        .unwrap();
        assert!(
            reorder_req.force,
            "reorderPinnedForumTopics must set force=true"
        );
        assert_eq!(reorder_req.order, vec![20, 10]);

        // 4. `deleteTopicHistory`: loops on `offset > 0` and advances channel PTS 200 -> 202.
        let aff_pages = delete_topic_history(
            &rpc,
            &mut session,
            Some(&mut updates),
            ch_peer,
            MsgId(20),
            1_700_000_200_000,
        )
        .await
        .unwrap();
        assert_eq!(aff_pages.len(), 2);
        assert_eq!(updates.channel_pts(tg_data::ChannelId(500)), Some(202));
        assert!(session.forum_topic(ch_peer, MsgId(20)).is_none());

        // 5. Live MTProto 2.0 server-pushed topic updates.
        let mut rx = rpc.subscribe_updates().unwrap();
        let mut change_rx = session.subscribe();
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::PinnedForumTopic(tl_types::UpdatePinnedForumTopic {
                        pinned: true,
                        peer: tl_peer_channel(500),
                        topic_id: 10,
                    }),
                    // Unloaded topic 99 -> emits ForumTopicReloadNeeded!
                    tl_enums::Update::PinnedForumTopic(tl_types::UpdatePinnedForumTopic {
                        pinned: true,
                        peer: tl_peer_channel(500),
                        topic_id: 99,
                    }),
                    tl_enums::Update::PinnedForumTopics(tl_types::UpdatePinnedForumTopics {
                        peer: tl_peer_channel(500),
                        order: Some(vec![10, 1]),
                    }),
                    // Missing order -> emits ForumTopicsReloadNeeded!
                    tl_enums::Update::PinnedForumTopics(tl_types::UpdatePinnedForumTopics {
                        peer: tl_peer_channel(500),
                        order: None,
                    }),
                    tl_enums::Update::EditForumTopic(tl_types::UpdateEditForumTopic {
                        peer: tl_peer_channel(500),
                        topic_id: 10,
                        title: Some("Alpha Live".to_owned()),
                        icon_emoji_id: Some(777_111),
                        closed: Some(true),
                        hidden: None,
                    }),
                    tl_enums::Update::NotifySettings(tl_types::UpdateNotifySettings {
                        peer: tl_enums::NotifyPeer::NotifyForumTopic(tl_types::NotifyForumTopic {
                            peer: tl_peer_channel(500),
                            top_msg_id: 10,
                        }),
                        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                            tl_types::PeerNotifySettings {
                                show_previews: None,
                                silent: None,
                                mute_until: Some(i32::MAX),
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
                    }),
                ],
                users: vec![],
                chats: vec![],
                date: 1_700_000_250,
                seq: 2,
            }),
        ));

        let pushed = rx.recv().await.unwrap();
        updates
            .handle_updates(&rpc, &mut session, pushed, None, 1_700_000_250_000)
            .await
            .unwrap();
        let evs = updates.drain_events();
        assert!(evs.contains(&UpdateEvent::ForumTopicReloadNeeded {
            peer_id: ch_peer,
            root_id: MsgId(99),
        }));
        assert!(evs.contains(&UpdateEvent::ForumTopicsReloadNeeded { peer_id: ch_peer }));

        let t10 = session.forum_topic(ch_peer, MsgId(10)).unwrap();
        assert_eq!(t10.title, "Alpha Live");
        assert_eq!(t10.icon_emoji_id, Some(DocumentId(777_111)));
        assert!(t10.closed);
        assert!(t10.muted);
        assert!(t10.pinned);
        assert_eq!(
            session.forum(ch_peer).unwrap().pinned_order,
            vec![MsgId(10), MsgId(1)]
        );

        let mut saw_topic_edit = false;
        while let Ok(ce) = change_rx.try_recv() {
            if let ChangeEvent::TopicUpdated {
                peer_id,
                root_id,
                flags,
            } = ce
                && peer_id == ch_peer
                && root_id == MsgId(10)
                && flags.contains(TopicUpdateFlags::TITLE | TopicUpdateFlags::CLOSED)
            {
                saw_topic_edit = true;
            }
        }
        assert!(saw_topic_edit);
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn forum_topics_per_topic_read_state_and_unread_counts() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let ch_peer = PeerId::from_channel(tg_data::ChannelId(500));
        {
            let forum = session.ensure_forum(ch_peer);
            let t10 = forum.ensure_topic(MsgId(10));
            t10.top_message_id = Some(MsgId(50));
            t10.last_known_server_msg_id = MsgId(50);
            t10.read_inbox_max_id = MsgId(45);
            t10.unread_count = Some(3);
            t10.unread_mentions_count = 1;
        }

        let server = ScriptedServer::new(|_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 10,
                    qts: 0,
                    date: 1_700_000_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::ReadDiscussion::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::ReadDiscussion::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.msg_id, 10);
                assert_eq!(req.read_max_id, 50);
                ServerAction::ok(&true)
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();
        updates
            .initialize(&rpc, &mut session, 1_700_000_000_000)
            .await
            .unwrap();
        updates.init_channel_pts(tg_data::ChannelId(500), 10);

        // Incoming topic message increments unread_count from 3 -> 4.
        let mut incoming = tl_message(
            tl_peer_channel(500),
            Some(tl_peer_user(200)),
            51,
            1_700_000_051,
            "new topic reply",
        );
        if let tl_enums::Message::Message(ref mut m) = incoming {
            m.reply_to = Some(tl_enums::MessageReplyHeader::MessageReplyHeader(
                tl_types::MessageReplyHeader {
                    reply_to_scheduled: false,
                    forum_topic: true,
                    quote: false,
                    reply_to_ephemeral: false,
                    reply_to_msg_id: Some(10),
                    reply_to_peer_id: None,
                    reply_from: None,
                    reply_media: None,
                    reply_to_top_id: Some(10),
                    quote_text: None,
                    quote_entities: None,
                    quote_offset: None,
                    todo_item_id: None,
                    poll_option: None,
                    quote_rich: None,
                },
            ));
        }
        updates
            .handle_updates(
                &rpc,
                &mut session,
                tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![tl_enums::Update::NewChannelMessage(
                        tl_types::UpdateNewChannelMessage {
                            message: incoming,
                            pts: 11,
                            pts_count: 1,
                        },
                    )],
                    users: vec![],
                    chats: vec![],
                    date: 1_700_000_051,
                    seq: 2,
                }),
                None,
                1_700_000_051_000,
            )
            .await
            .unwrap();
        let t10 = session.forum_topic(ch_peer, MsgId(10)).unwrap();
        assert_eq!(t10.unread_count, Some(4));
        assert_eq!(t10.top_message_id, Some(MsgId(51)));
        assert_eq!(
            session
                .forum(ch_peer)
                .unwrap()
                .total_unread_state()
                .messages,
            4
        );

        // `messages.readDiscussion` up to 50 (leaves 51 unread -> unread_count unknown until server tells).
        assert!(
            read_topic_discussion(&rpc, &mut session, ch_peer, MsgId(10), MsgId(50))
                .await
                .unwrap()
        );
        assert_eq!(
            session
                .forum_topic(ch_peer, MsgId(10))
                .unwrap()
                .read_inbox_max_id,
            MsgId(50)
        );

        // Live `updateReadChannelDiscussionInbox(read_max_id=51)` & `Outbox(read_max_id=49)`.
        updates
            .handle_updates(
                &rpc,
                &mut session,
                tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![
                        tl_enums::Update::ReadChannelDiscussionInbox(
                            tl_types::UpdateReadChannelDiscussionInbox {
                                channel_id: 500,
                                top_msg_id: 10,
                                read_max_id: 51,
                                broadcast_id: Some(600),
                                broadcast_post: Some(12),
                            },
                        ),
                        tl_enums::Update::ReadChannelDiscussionOutbox(
                            tl_types::UpdateReadChannelDiscussionOutbox {
                                channel_id: 500,
                                top_msg_id: 10,
                                read_max_id: 49,
                            },
                        ),
                    ],
                    users: vec![],
                    chats: vec![],
                    date: 1_700_000_060,
                    seq: 3,
                }),
                None,
                1_700_000_060_000,
            )
            .await
            .unwrap();
        let t10 = session.forum_topic(ch_peer, MsgId(10)).unwrap();
        assert_eq!(t10.read_inbox_max_id, MsgId(51));
        assert_eq!(t10.unread_count, Some(0));
        assert_eq!(t10.read_outbox_max_id, MsgId(49));
    })
    .await
    .expect("test timeout");
}

// ============================================================================
// AREA 3: SAVED MESSAGES DIALOGS & REACTION TAGS CONFORMANCE TESTS
// ============================================================================

#[tokio::test]
async fn saved_dialogs_paging_pinned_monoforum_and_unsupported() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let u200 = PeerId::from_user(UserId(200));
        let u300 = PeerId::from_user(UserId(300));
        let mono_ch = tg_data::ChannelId(500);

        let page_num = Arc::new(AtomicU32::new(0));
        let pn = page_num.clone();
        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::messages::GetPinnedSavedDialogs::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::messages::SavedDialogs::SavedDialogs(
                    tl_types::messages::SavedDialogs {
                        dialogs: vec![tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                            pinned: true,
                            peer: tl_peer_user(200),
                            top_message: 10,
                        })],
                        messages: vec![tl_message(
                            tl_peer_user(100),
                            Some(tl_peer_user(100)),
                            10,
                            1_700_000_100,
                            "pinned saved msg",
                        )],
                        chats: vec![],
                        users: vec![tl_user(200, 2000, "Two", false)],
                    },
                ))
            }
            tl_fns::messages::GetSavedDialogs::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetSavedDialogs::from_bytes(&rpc.body).unwrap();
                assert!(
                    req.exclude_pinned,
                    "messages.getSavedDialogs MUST set exclude_pinned=true (`data_saved_messages.cpp:311`)"
                );
                if req.parent_peer.is_some() {
                    // Monoforum saved dialogs.
                    return ServerAction::ok(&tl_enums::messages::SavedDialogs::SavedDialogs(
                        tl_types::messages::SavedDialogs {
                            dialogs: vec![tl_enums::SavedDialog::MonoForumDialog(
                                tl_types::MonoForumDialog {
                                    unread_mark: true,
                                    nopaid_messages_exception: true,
                                    peer: tl_peer_user(200),
                                    top_message: 77,
                                    read_inbox_max_id: 70,
                                    read_outbox_max_id: 75,
                                    unread_count: 3,
                                    unread_reactions_count: 1,
                                    draft: None,
                                },
                            )],
                            messages: vec![tl_message(
                                tl_peer_channel(500),
                                Some(tl_peer_user(200)),
                                77,
                                1_700_000_777,
                                "monoforum dm",
                            )],
                            chats: vec![],
                            users: vec![],
                        },
                    ));
                }
                let n = pn.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    assert_eq!(req.limit, 20, "first saved dialogs page must use limit=20");
                    ServerAction::ok(&tl_enums::messages::SavedDialogs::Slice(
                        tl_types::messages::SavedDialogsSlice {
                            count: 3,
                            dialogs: vec![tl_enums::SavedDialog::SavedDialog(
                                tl_types::SavedDialog {
                                    pinned: false,
                                    peer: tl_peer_user(300),
                                    top_message: 9,
                                },
                            )],
                            messages: vec![tl_message(
                                tl_peer_user(100),
                                Some(tl_peer_user(100)),
                                9,
                                1_700_000_090,
                                "unpinned saved msg",
                            )],
                            chats: vec![],
                            users: vec![tl_user(300, 3000, "Three", false)],
                        },
                    ))
                } else {
                    assert_eq!(req.limit, 100, "page 2 must use kListPerPage=100");
                    assert_eq!(req.offset_date, 1_700_000_090);
                    assert_eq!(req.offset_id, 9);
                    assert!(matches!(
                        req.offset_peer,
                        tl_enums::InputPeer::User(ref u) if u.user_id == 300
                    ));
                    // Out-of-order date (> prev 1_700_000_090) must stop pagination (`data_saved_messages.cpp:323`).
                    ServerAction::ok(&tl_enums::messages::SavedDialogs::Slice(
                        tl_types::messages::SavedDialogsSlice {
                            count: 3,
                            dialogs: vec![tl_enums::SavedDialog::SavedDialog(
                                tl_types::SavedDialog {
                                    pinned: false,
                                    peer: tl_peer_user(200),
                                    top_message: 11,
                                },
                            )],
                            messages: vec![tl_message(
                                tl_peer_user(100),
                                Some(tl_peer_user(100)),
                                11,
                                1_700_000_999,
                                "out of order",
                            )],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
            }
            tl_fns::messages::GetSavedDialogsByID::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetSavedDialogsByID::from_bytes(&rpc.body).unwrap();
                assert!(req.parent_peer.is_some());
                assert_eq!(req.ids.len(), 1);
                ServerAction::ok(&tl_enums::messages::SavedDialogs::SavedDialogs(
                    tl_types::messages::SavedDialogs {
                        dialogs: vec![],
                        messages: vec![],
                        chats: vec![],
                        users: vec![],
                    },
                ))
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);

        // Page 1 loads pinned + first unpinned page.
        let p1 = load_saved_dialogs_page(&rpc, &mut session, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(p1.pinned_peers, Some(vec![u200]));
        assert_eq!(p1.sublist_peers, vec![u300]);
        assert!(!p1.all_loaded);
        assert!(session.saved_messages().sublist(u200).unwrap().pinned);

        // Page 2 detects out-of-order date and stops pagination.
        let p2 = load_saved_dialogs_page(&rpc, &mut session, None)
            .await
            .unwrap()
            .unwrap();
        assert!(p2.all_loaded);
        assert!(load_saved_dialogs_page(&rpc, &mut session, None).await.unwrap().is_none());

        // Monoforum skips `getPinnedSavedDialogs` and applies `MonoForumDialog` fields.
        let pinned_count_before = server
            .rpcs_of::<tl_fns::messages::GetPinnedSavedDialogs>()
            .len();
        let mp = load_saved_dialogs_page(&rpc, &mut session, Some(mono_ch))
            .await
            .unwrap()
            .unwrap();
        assert!(mp.pinned_peers.is_none());
        assert_eq!(
            server
                .rpcs_of::<tl_fns::messages::GetPinnedSavedDialogs>()
                .len(),
            pinned_count_before
        );
        let mono_sub = session.monoforum(mono_ch).unwrap().sublist(u200).unwrap();
        assert!(mono_sub.unread_mark);
        assert!(mono_sub.nopaid_messages_exception);
        assert_eq!(mono_sub.unread_count, Some(3));
        assert_eq!(mono_sub.read_inbox_max_id, MsgId(70));

        let _ = load_saved_dialogs_by_id(&rpc, &mut session, mono_ch, &[u300])
            .await
            .unwrap();

        // Separate server returning `SAVED_DIALOGS_UNSUPPORTED`.
        let unsupported_server =
            ScriptedServer::new(|_, _| ServerAction::err(400, "SAVED_DIALOGS_UNSUPPORTED"));
        let (_c2, rpc2) = build_client_and_rpc(&unsupported_server);
        let mut s2 = seed_session();
        let err = load_pinned_saved_dialogs(&rpc2, &mut s2).await.unwrap_err();
        assert!(matches!(
            err,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "SAVED_DIALOGS_UNSUPPORTED"
        ));
        assert!(!s2.saved_messages().supported());
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn saved_dialogs_pin_reorder_history_delete_read_and_live_updates() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let u200 = PeerId::from_user(UserId(200));
        let u300 = PeerId::from_user(UserId(300));
        let mono_ch = tg_data::ChannelId(500);
        session.saved_messages_mut().ensure_sublist(u200);
        session.saved_messages_mut().ensure_sublist(u300);

        let del_step = Arc::new(AtomicU32::new(0));
        let ds = del_step.clone();
        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 300,
                    qts: 0,
                    date: 1_700_000_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::ToggleSavedDialogPin::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::ToggleSavedDialogPin::from_bytes(&rpc.body).unwrap();
                assert!(req.pinned);
                ServerAction::ok(&true)
            }
            tl_fns::messages::ReorderPinnedSavedDialogs::CONSTRUCTOR_ID => {
                let req =
                    tl_fns::messages::ReorderPinnedSavedDialogs::from_bytes(&rpc.body).unwrap();
                assert!(req.force, "reorderPinnedSavedDialogs must pass force=true");
                assert_eq!(req.order.len(), 2);
                ServerAction::ok(&true)
            }
            tl_fns::messages::GetSavedHistory::CONSTRUCTOR_ID => ServerAction::ok(
                &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
                    messages: vec![tl_message(
                        tl_peer_user(100),
                        Some(tl_peer_user(100)),
                        55,
                        1_700_000_055,
                        "saved entry",
                    )],
                    topics: vec![],
                    chats: vec![],
                    users: vec![],
                }),
            ),
            tl_fns::messages::DeleteSavedHistory::CONSTRUCTOR_ID => {
                let n = ds.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                        tl_types::messages::AffectedHistory {
                            pts: 301,
                            pts_count: 1,
                            offset: 25,
                        },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::messages::AffectedHistory::AffectedHistory(
                        tl_types::messages::AffectedHistory {
                            pts: 302,
                            pts_count: 1,
                            offset: 0,
                        },
                    ))
                }
            }
            tl_fns::messages::ReadSavedHistory::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::ReadSavedHistory::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.max_id, 77);
                ServerAction::ok(&true)
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();
        updates
            .initialize(&rpc, &mut session, 1_700_000_000_000)
            .await
            .unwrap();

        // Pin & reorder saved dialogs.
        assert!(
            toggle_saved_dialog_pin(&rpc, &mut session, u200, true)
                .await
                .unwrap()
        );
        assert!(
            reorder_pinned_saved_dialogs(&rpc, &mut session, &[u300, u200])
                .await
                .unwrap()
        );
        assert_eq!(session.saved_messages().pinned_order, vec![u300, u200]);

        // `messages.getSavedHistory` pagination rules: Around(0), Around(55), Before(55), After(55).
        let _ = load_saved_history(
            &rpc,
            &mut session,
            None,
            u200,
            MsgId::ZERO,
            LoadDirection::Around,
        )
        .await
        .unwrap();
        let _ = load_saved_history(
            &rpc,
            &mut session,
            None,
            u200,
            MsgId(55),
            LoadDirection::Around,
        )
        .await
        .unwrap();
        let _ = load_saved_history(
            &rpc,
            &mut session,
            None,
            u200,
            MsgId(55),
            LoadDirection::Before,
        )
        .await
        .unwrap();
        let _ = load_saved_history(
            &rpc,
            &mut session,
            Some(mono_ch),
            u200,
            MsgId(55),
            LoadDirection::After,
        )
        .await
        .unwrap();
        let hist_reqs: Vec<tl_fns::messages::GetSavedHistory> = server
            .rpcs_of::<tl_fns::messages::GetSavedHistory>()
            .into_iter()
            .map(|r| tl_fns::messages::GetSavedHistory::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(hist_reqs[0].offset_id, 0);
        assert_eq!(hist_reqs[0].add_offset, 0);
        assert_eq!(hist_reqs[0].limit, 50);
        assert_eq!(hist_reqs[1].offset_id, 55);
        assert_eq!(hist_reqs[1].add_offset, -25);
        assert_eq!(hist_reqs[2].offset_id, 55);
        assert_eq!(hist_reqs[2].add_offset, 0);
        assert_eq!(hist_reqs[3].offset_id, 56);
        assert_eq!(hist_reqs[3].add_offset, -50);
        assert!(hist_reqs[3].parent_peer.is_some());

        // `messages.deleteSavedHistory` loops on `offset > 0` and updates common PTS 300 -> 302.
        let aff = delete_saved_history(
            &rpc,
            &mut session,
            Some(&mut updates),
            None,
            u300,
            MsgId::ZERO,
            None,
            None,
            1_700_000_100_000,
        )
        .await
        .unwrap();
        assert_eq!(aff.len(), 2);
        assert_eq!(updates.pts(), 302);
        assert!(session.saved_messages().sublist(u300).is_none());

        // `messages.readSavedHistory` & live updates (`updateSavedDialogPinned`, `updatePinnedSavedDialogs`,
        // `updateReadMonoForumInbox`, `updateReadMonoForumOutbox`).
        assert!(
            read_saved_history(&rpc, &mut session, mono_ch, u200, MsgId(77))
                .await
                .unwrap()
        );
        let mut rx = rpc.subscribe_updates().unwrap();
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    // Unloaded peer u300 (deleted above) -> triggers SavedPinnedReloadNeeded!
                    tl_enums::Update::SavedDialogPinned(tl_types::UpdateSavedDialogPinned {
                        pinned: true,
                        peer: tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                            peer: tl_peer_user(300),
                        }),
                    }),
                    tl_enums::Update::PinnedSavedDialogs(tl_types::UpdatePinnedSavedDialogs {
                        order: None,
                    }),
                    tl_enums::Update::ReadMonoForumInbox(tl_types::UpdateReadMonoForumInbox {
                        channel_id: 500,
                        saved_peer_id: tl_peer_user(200),
                        read_max_id: 88,
                    }),
                    tl_enums::Update::ReadMonoForumOutbox(tl_types::UpdateReadMonoForumOutbox {
                        channel_id: 500,
                        saved_peer_id: tl_peer_user(200),
                        read_max_id: 85,
                    }),
                ],
                users: vec![],
                chats: vec![],
                date: 1_700_000_200,
                seq: 2,
            }),
        ));
        let pushed = rx.recv().await.unwrap();
        updates
            .handle_updates(&rpc, &mut session, pushed, None, 1_700_000_200_000)
            .await
            .unwrap();
        let evs = updates.drain_events();
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e, UpdateEvent::SavedPinnedReloadNeeded))
                .count(),
            2
        );
        let mono_sub = session.monoforum(mono_ch).unwrap().sublist(u200).unwrap();
        assert_eq!(mono_sub.read_inbox_max_id, MsgId(88));
        assert_eq!(mono_sub.read_outbox_max_id, MsgId(85));
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn saved_reaction_tags_hash_not_modified_error_reset_rename_and_update() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let u200 = PeerId::from_user(UserId(200));
        let fail_next = Arc::new(AtomicU32::new(0));
        let fn_flag = fail_next.clone();

        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 10,
                    qts: 0,
                    date: 1_700_000_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::GetSavedReactionTags::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetSavedReactionTags::from_bytes(&rpc.body).unwrap();
                if fn_flag.swap(0, Ordering::SeqCst) == 1 {
                    return ServerAction::err(400, "SAVED_TAGS_UNAVAILABLE");
                }
                if req.hash == 0x1234_5678 {
                    ServerAction::ok(&tl_enums::messages::SavedReactionTags::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::messages::SavedReactionTags::SavedReactionTags(
                        tl_types::messages::SavedReactionTags {
                            tags: vec![tl_enums::SavedReactionTag::SavedReactionTag(
                                tl_types::SavedReactionTag {
                                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                        emoticon: "⭐".to_owned(),
                                    }),
                                    title: Some("Work".to_owned()),
                                    count: 7,
                                },
                            )],
                            hash: 0x1234_5678,
                        },
                    ))
                }
            }
            tl_fns::messages::UpdateSavedReactionTag::CONSTRUCTOR_ID => ServerAction::ok(&true),
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();
        updates
            .initialize(&rpc, &mut session, 1_700_000_000_000)
            .await
            .unwrap();

        // 1. First fetch sends hash=0, stores hash=0x1234_5678.
        assert!(
            get_saved_reaction_tags(&rpc, &mut session, None)
                .await
                .unwrap()
        );
        assert_eq!(session.saved_reaction_tags().hash_for(None), 0x1234_5678);
        assert_eq!(session.saved_reaction_tags().tags_for(None).len(), 1);

        // 2. Second fetch sends stored hash and receives `NotModified`.
        assert!(
            !get_saved_reaction_tags(&rpc, &mut session, None)
                .await
                .unwrap()
        );

        // 3. Error resets hash to 0 (`data_message_reactions.cpp:1075`).
        fail_next.store(1, Ordering::SeqCst);
        assert!(
            get_saved_reaction_tags(&rpc, &mut session, None)
                .await
                .is_err()
        );
        assert_eq!(session.saved_reaction_tags().hash_for(None), 0);

        // 4. Per-sublist fetch passes `peer = Some(u200)`.
        assert!(
            get_saved_reaction_tags(&rpc, &mut session, Some(u200))
                .await
                .unwrap()
        );

        // 5. `updateSavedReactionTag`: non-empty title sets `f_title`, blank omits `f_title`.
        let star = ReactionId::Emoji("⭐".to_owned());
        assert!(
            update_saved_reaction_tag(&rpc, &mut session, &star, Some("Priority"))
                .await
                .unwrap()
        );
        assert_eq!(
            session.saved_reaction_tags().tags_for(None)[0]
                .title
                .as_deref(),
            Some("Priority")
        );
        assert!(
            update_saved_reaction_tag(&rpc, &mut session, &star, Some("   "))
                .await
                .unwrap()
        );
        assert_eq!(session.saved_reaction_tags().tags_for(None)[0].title, None);

        let tag_upd_reqs: Vec<tl_fns::messages::UpdateSavedReactionTag> = server
            .rpcs_of::<tl_fns::messages::UpdateSavedReactionTag>()
            .into_iter()
            .map(|r| tl_fns::messages::UpdateSavedReactionTag::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(tag_upd_reqs[0].title.as_deref(), Some("Priority"));
        assert_eq!(tag_upd_reqs[1].title, None);

        // 6. Live `updateSavedReactionTags` marks cache dirty and emits event.
        updates
            .handle_updates(
                &rpc,
                &mut session,
                tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                    update: tl_enums::Update::SavedReactionTags,
                    date: 1_700_000_100,
                }),
                None,
                1_700_000_100_000,
            )
            .await
            .unwrap();
        assert!(session.saved_reaction_tags().is_dirty(None));
        assert!(
            updates
                .drain_events()
                .contains(&UpdateEvent::SavedReactionTagsInvalidated)
        );
    })
    .await
    .expect("test timeout");
}

// ============================================================================
// AREA 4: SCHEDULED MESSAGES & QUICK REPLIES CONFORMANCE TESTS
// ============================================================================

#[test]
fn api_hash_and_list_hash_known_answer_vectors() {
    // 1. Exact known-answer check for `Api::HashInit`/`HashUpdate`/`HashFinalize` (`api/api_hash.h:36-70`).
    let mut h = hash_init();
    assert_eq!(h, 0);
    hash_update_u64(&mut h, 1);
    assert_eq!(h, 1);
    // Step 2: h=1 -> (1 ^ 0) = 1 -> (1 ^ (1<<35)) -> ^= >> 4 -> + 2
    let mut expected = 1u64;
    expected ^= expected >> 21;
    expected ^= expected << 35;
    expected ^= expected >> 4;
    expected = expected.wrapping_add(2);
    hash_update_u64(&mut h, 2);
    assert_eq!(h, expected);
    assert_eq!(count_hash([1, 2]), expected);

    // Negative i32 MUST sign-extend to i64 (`HashUpdate(already, int64(value))`).
    let mut h_neg = hash_init();
    hash_update_i32(&mut h_neg, -1);
    assert_eq!(h_neg, u64::MAX);

    // 2. ID range boundary checks (`IsScheduledMsgId` / `IsShortcutMsgId`).
    assert!(!SERVER_MAX_MSG_ID.is_scheduled());
    assert!(!SCHEDULED_MAX_MSG_ID.is_shortcut());
    let local_sched = scheduled_remote_to_local_id(MsgId(42)).unwrap();
    assert_eq!(local_sched, MsgId(SERVER_MAX_MSG_ID.0 + 43));
    assert!(local_sched.is_scheduled());
    assert_eq!(scheduled_local_to_remote_id(local_sched), Some(MsgId(42)));

    let local_sc = shortcut_remote_to_local_id(MsgId(7)).unwrap();
    assert_eq!(local_sc, MsgId(SCHEDULED_MAX_MSG_ID.0 + 8));
    assert!(local_sc.is_shortcut());
    assert_eq!(shortcut_local_to_remote_id(local_sc), Some(MsgId(7)));

    assert!(!has_scheduled_date(
        SCHEDULED_UNTIL_ONLINE_TIMESTAMP,
        1_700_000_000
    ));
    assert!(has_scheduled_date(1_700_000_500, 1_700_000_000));

    // 3. Exact `ScheduledMessages::countListHash` vs `ShortcutMessages::countListHash`:
    //    Scheduled hashes reverse order of `(remote_id: i64, edit_date_or_0: i32, date: i32)`.
    //    Shortcut hashes reverse order of `(remote_id: i64, edit_date_or_0: i32)` WITHOUT `date`!
    let mut session = seed_session();
    let u200 = PeerId::from_user(UserId(200));
    let mut m1 = Message::new_text(
        u200,
        MsgId(10),
        Some(u200),
        1_700_001_000,
        TextWithEntities::plain("a"),
    );
    m1.edit_date = Some(1_700_000_500);
    let m2 = Message::new_text(
        u200,
        MsgId(20),
        Some(u200),
        1_700_002_000,
        TextWithEntities::plain("b"),
    );
    session
        .scheduled_messages_mut()
        .upsert_server_message(u200, m1.clone());
    session
        .scheduled_messages_mut()
        .upsert_server_message(u200, m2.clone());

    let mut expected_sched = hash_init();
    // Reverse order: m2 (date 2000) first, then m1 (date 1000).
    hash_update_i64(&mut expected_sched, 20);
    hash_update_i32(&mut expected_sched, 0);
    hash_update_i32(&mut expected_sched, 1_700_002_000);
    hash_update_i64(&mut expected_sched, 10);
    hash_update_i32(&mut expected_sched, 1_700_000_500);
    hash_update_i32(&mut expected_sched, 1_700_001_000);
    assert_eq!(
        session.scheduled_messages().count_list_hash(u200),
        hash_finalize(expected_sched) as i64
    );

    session.shortcut_messages_mut().upsert_server_message(7, m1);
    session.shortcut_messages_mut().upsert_server_message(7, m2);
    let mut expected_sc = hash_init();
    hash_update_i64(&mut expected_sc, 20);
    hash_update_i32(&mut expected_sc, 0);
    hash_update_i64(&mut expected_sc, 10);
    hash_update_i32(&mut expected_sc, 1_700_000_500);
    assert_eq!(
        session.shortcut_messages().count_list_hash(7),
        hash_finalize(expected_sc) as i64
    );
    assert_ne!(
        session.scheduled_messages().count_list_hash(u200),
        session.shortcut_messages().count_list_hash(7)
    );
}

#[tokio::test]
async fn scheduled_messages_hash_throttle_send_delete_and_live_updates() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let u200 = PeerId::from_user(UserId(200));
        let readonly_bc = PeerId::from_channel(tg_data::ChannelId(600));

        // Expected hash after receiving messages (id=10, date=1_700_001_000) & (id=20, date=1_700_002_000).
        let mut expected_hash = hash_init();
        hash_update_i64(&mut expected_hash, 20);
        hash_update_i32(&mut expected_hash, 0);
        hash_update_i32(&mut expected_hash, 1_700_002_000);
        hash_update_i64(&mut expected_hash, 10);
        hash_update_i32(&mut expected_hash, 0);
        hash_update_i32(&mut expected_hash, 1_700_001_000);
        let expected_hash_i64 = expected_hash as i64;

        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 50,
                    qts: 0,
                    date: 1_700_000_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::GetScheduledHistory::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetScheduledHistory::from_bytes(&rpc.body).unwrap();
                if req.hash == expected_hash_i64 {
                    ServerAction::ok(&tl_enums::messages::Messages::NotModified(
                        tl_types::messages::MessagesNotModified { count: 2 },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::messages::Messages::Messages(
                        tl_types::messages::Messages {
                            messages: vec![
                                tl_message(
                                    tl_peer_user(200),
                                    Some(tl_peer_user(100)),
                                    20,
                                    1_700_002_000,
                                    "later",
                                ),
                                tl_message(
                                    tl_peer_user(200),
                                    Some(tl_peer_user(100)),
                                    10,
                                    1_700_001_000,
                                    "sooner",
                                ),
                            ],
                            topics: vec![],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
            }
            tl_fns::messages::SendScheduledMessages::CONSTRUCTOR_ID
            | tl_fns::messages::DeleteScheduledMessages::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![],
                    users: vec![],
                    chats: vec![],
                    date: 1_700_000_100,
                    seq: 0,
                }))
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();
        updates
            .initialize(&rpc, &mut session, 1_700_000_000_000)
            .await
            .unwrap();

        // 1. Broadcast channel without `can_post_messages` MUST skip RPC (`scheduled_messages.cpp:491`).
        assert_eq!(
            get_scheduled_history(&rpc, &mut session, readonly_bc, 10_000, false)
                .await
                .unwrap(),
            ScheduledHistoryOutcome::Skipped
        );
        assert_eq!(
            server
                .rpcs_of::<tl_fns::messages::GetScheduledHistory>()
                .len(),
            0
        );

        // 2. First fetch sends `hash = 0` and loads 2 items under local scheduled IDs.
        assert_eq!(
            get_scheduled_history(&rpc, &mut session, u200, 10_000, false)
                .await
                .unwrap(),
            ScheduledHistoryOutcome::Updated(2)
        );
        let local_10 = scheduled_remote_to_local_id(MsgId(10)).unwrap();
        let local_20 = scheduled_remote_to_local_id(MsgId(20)).unwrap();
        assert!(
            session
                .scheduled_messages()
                .lookup_local(u200, local_10)
                .is_some()
        );
        assert!(
            session
                .scheduled_messages()
                .lookup_local(u200, local_20)
                .is_some()
        );

        // 3. Within 60s cooldown -> skipped (0 RPCs).
        assert_eq!(
            get_scheduled_history(&rpc, &mut session, u200, 10_000 + 30_000, false)
                .await
                .unwrap(),
            ScheduledHistoryOutcome::Skipped
        );
        assert_eq!(
            server
                .rpcs_of::<tl_fns::messages::GetScheduledHistory>()
                .len(),
            1
        );

        // 4. After 60s cooldown -> sends exact computed `countListHash` and gets `NotModified`!
        assert_eq!(
            get_scheduled_history(
                &rpc,
                &mut session,
                u200,
                10_000 + SCHEDULED_REQUEST_TIME_LIMIT_MS,
                false
            )
            .await
            .unwrap(),
            ScheduledHistoryOutcome::NotModified
        );
        let sched_reqs: Vec<tl_fns::messages::GetScheduledHistory> = server
            .rpcs_of::<tl_fns::messages::GetScheduledHistory>()
            .into_iter()
            .map(|r| tl_fns::messages::GetScheduledHistory::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(sched_reqs[0].hash, 0);
        assert_eq!(sched_reqs[1].hash, expected_hash_i64);

        // 5. `sendScheduledMessages` sorts by ascending date and converts local -> remote IDs.
        let _ = send_scheduled_messages(&rpc, &session, u200, &[local_20, local_10])
            .await
            .unwrap();
        let send_req = tl_fns::messages::SendScheduledMessages::from_bytes(
            &server.rpcs_of::<tl_fns::messages::SendScheduledMessages>()[0].body,
        )
        .unwrap();
        assert_eq!(
            send_req.id,
            vec![10, 20],
            "must sort by ascending scheduled date"
        );

        // 6. `deleteScheduledMessages` converts local -> remote IDs.
        let _ = delete_scheduled_messages(&rpc, &session, u200, &[local_20])
            .await
            .unwrap();
        let del_req = tl_fns::messages::DeleteScheduledMessages::from_bytes(
            &server.rpcs_of::<tl_fns::messages::DeleteScheduledMessages>()[0].body,
        )
        .unwrap();
        assert_eq!(del_req.id, vec![20]);

        // 7. Live pushed `updateNewScheduledMessage` (with `applyConvertToScheduledOnSend`) +
        //    `updateDeleteScheduledMessages` (with `sent_messages` mapping).
        let local_pending_id = MsgId::client_by_index(1).unwrap();
        let mut pending_msg = Message::new_text(
            u200,
            local_pending_id,
            Some(PeerId::from_user(UserId(100))),
            1_700_000_100,
            TextWithEntities::plain("converted"),
        );
        pending_msg.flags.insert(MessageFlags::BEING_SENT);
        session.insert_message(pending_msg);
        session.register_random_id(777_888, FullMsgId::new(u200, local_pending_id));

        let mut rx = rpc.subscribe_updates().unwrap();
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                        id: 30,
                        random_id: 777_888,
                    }),
                    tl_enums::Update::NewScheduledMessage(tl_types::UpdateNewScheduledMessage {
                        message: tl_message(
                            tl_peer_user(200),
                            Some(tl_peer_user(100)),
                            30,
                            SCHEDULED_UNTIL_ONLINE_TIMESTAMP,
                            "when online",
                        ),
                    }),
                    tl_enums::Update::DeleteScheduledMessages(
                        tl_types::UpdateDeleteScheduledMessages {
                            peer: tl_peer_user(200),
                            messages: vec![10, 20],
                            sent_messages: Some(vec![5010]),
                        },
                    ),
                ],
                users: vec![],
                chats: vec![],
                date: 1_700_000_200,
                seq: 2,
            }),
        ));
        let pushed = rx.recv().await.unwrap();
        updates
            .handle_updates(&rpc, &mut session, pushed, None, 1_700_000_200_000)
            .await
            .unwrap();

        // Pending regular message was destroyed and converted to scheduled ID 30.
        assert!(
            session
                .message(FullMsgId::new(u200, local_pending_id))
                .is_none()
        );
        let local_30 = scheduled_remote_to_local_id(MsgId(30)).unwrap();
        assert_eq!(
            session
                .scheduled_messages()
                .lookup_local(u200, local_30)
                .unwrap()
                .date,
            SCHEDULED_UNTIL_ONLINE_TIMESTAMP
        );
        // 10 & 20 were deleted, and 10 emitted `ScheduledMessagesSent { sent_id: Some(5010) }`.
        assert!(
            session
                .scheduled_messages()
                .lookup_remote(u200, MsgId(10))
                .is_none()
        );
        assert!(
            session
                .scheduled_messages()
                .lookup_remote(u200, MsgId(20))
                .is_none()
        );
        let evs = updates.drain_events();
        assert!(evs.contains(&UpdateEvent::SentConvertedToScheduled {
            peer_id: u200,
            scheduled_id: local_30,
        }));
        assert!(evs.iter().any(|e| matches!(
            e,
            UpdateEvent::ScheduledMessagesSent { peer_id, items }
                if *peer_id == u200 && items.len() == 1 && items[0].sent_id == Some(MsgId(5010))
        )));
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn quick_replies_hash_messages_send_edit_delete_and_live_updates() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut session = seed_session();
        let u200 = PeerId::from_user(UserId(200));

        // Expected `ShortcutMessages::countListHash` for (id=5, edit_date=None) and (id=6, edit_date=Some(1_700_000_555)).
        let mut expected_qr_hash = hash_init();
        hash_update_i64(&mut expected_qr_hash, 6);
        hash_update_i32(&mut expected_qr_hash, 1_700_000_555);
        hash_update_i64(&mut expected_qr_hash, 5);
        hash_update_i32(&mut expected_qr_hash, 0);
        let expected_qr_hash_i64 = expected_qr_hash as i64;

        let server = ScriptedServer::new(move |_, rpc| match rpc.ctor {
            tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 10,
                    qts: 0,
                    date: 1_700_000_000,
                    seq: 1,
                    unread_count: 0,
                }))
            }
            tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetQuickReplies::from_bytes(&rpc.body).unwrap();
                if req.hash == 42 {
                    ServerAction::ok(&tl_enums::messages::QuickReplies::NotModified)
                } else {
                    let mut m5 = tl_message(
                        tl_peer_user(100),
                        Some(tl_peer_user(100)),
                        5,
                        1_700_000_005,
                        "Hello there!",
                    );
                    if let tl_enums::Message::Message(ref mut inner) = m5 {
                        inner.quick_reply_shortcut_id = Some(1);
                    }
                    ServerAction::ok(&tl_enums::messages::QuickReplies::QuickReplies(
                        tl_types::messages::QuickReplies {
                            quick_replies: vec![tl_enums::QuickReply::QuickReply(
                                tl_types::QuickReply {
                                    shortcut_id: 1,
                                    shortcut: "hello".to_owned(),
                                    top_message: 5,
                                    count: 1,
                                },
                            )],
                            messages: vec![m5],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
            }
            tl_fns::messages::GetQuickReplyMessages::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetQuickReplyMessages::from_bytes(&rpc.body).unwrap();
                if req.hash == expected_qr_hash_i64 {
                    ServerAction::ok(&tl_enums::messages::Messages::NotModified(
                        tl_types::messages::MessagesNotModified { count: 2 },
                    ))
                } else {
                    let mut m5 = tl_message(
                        tl_peer_user(100),
                        Some(tl_peer_user(100)),
                        5,
                        1_700_000_005,
                        "Hello there!",
                    );
                    let mut m6 = tl_message(
                        tl_peer_user(100),
                        Some(tl_peer_user(100)),
                        6,
                        1_700_000_006,
                        "How can I help?",
                    );
                    if let tl_enums::Message::Message(ref mut inner) = m5 {
                        inner.quick_reply_shortcut_id = Some(1);
                    }
                    if let tl_enums::Message::Message(ref mut inner) = m6 {
                        inner.quick_reply_shortcut_id = Some(1);
                        inner.edit_date = Some(1_700_000_555);
                    }
                    ServerAction::ok(&tl_enums::messages::Messages::Messages(
                        tl_types::messages::Messages {
                            messages: vec![m5, m6],
                            topics: vec![],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
            }
            tl_fns::messages::SendQuickReplyMessages::CONSTRUCTOR_ID
            | tl_fns::messages::DeleteQuickReplyMessages::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![],
                    users: vec![],
                    chats: vec![],
                    date: 1_700_000_100,
                    seq: 0,
                }))
            }
            tl_fns::messages::EditQuickReplyShortcut::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::EditQuickReplyShortcut::from_bytes(&rpc.body).unwrap();
                if req.shortcut_id < 0 {
                    ServerAction::err(400, "SHORTCUT_ID_INVALID")
                } else {
                    ServerAction::ok(&true)
                }
            }
            tl_fns::messages::DeleteQuickReplyShortcut::CONSTRUCTOR_ID => ServerAction::ok(&true),
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);
        let mut updates = UpdatesHandler::new();
        updates
            .initialize(&rpc, &mut session, 1_700_000_000_000)
            .await
            .unwrap();

        // 1. `messages.getQuickReplies` + `NotModified`.
        assert_eq!(
            get_quick_replies(&rpc, &mut session).await.unwrap(),
            QuickRepliesOutcome::Updated(1)
        );
        assert_eq!(
            session.shortcut_messages().lookup_shortcut_id("HELLO"),
            Some(1)
        );
        session.shortcut_messages_mut().set_shortcuts_hash(42);
        assert_eq!(
            get_quick_replies(&rpc, &mut session).await.unwrap(),
            QuickRepliesOutcome::NotModified
        );

        // 2. `messages.getQuickReplyMessages`: hash, 60s throttle, then exact `count_list_hash` -> `NotModified`.
        assert_eq!(
            get_quick_reply_messages(&rpc, &mut session, 1, 10_000, false)
                .await
                .unwrap(),
            QuickReplyMessagesOutcome::Updated(2)
        );
        assert_eq!(
            get_quick_reply_messages(&rpc, &mut session, 1, 10_000 + 20_000, false)
                .await
                .unwrap(),
            QuickReplyMessagesOutcome::Skipped
        );
        assert_eq!(
            get_quick_reply_messages(&rpc, &mut session, 1, 10_000 + 60_000, false)
                .await
                .unwrap(),
            QuickReplyMessagesOutcome::NotModified
        );
        let qr_reqs: Vec<tl_fns::messages::GetQuickReplyMessages> = server
            .rpcs_of::<tl_fns::messages::GetQuickReplyMessages>()
            .into_iter()
            .map(|r| tl_fns::messages::GetQuickReplyMessages::from_bytes(&r.body).unwrap())
            .collect();
        assert_eq!(qr_reqs.len(), 2);
        assert_eq!(qr_reqs[1].hash, expected_qr_hash_i64);

        // 3. `sendQuickReplyMessages` & `deleteQuickReplyMessages` convert local shortcut IDs -> remote IDs.
        let local_5 = shortcut_remote_to_local_id(MsgId(5)).unwrap();
        let local_6 = shortcut_remote_to_local_id(MsgId(6)).unwrap();
        assert!(local_5.0 > SCHEDULED_MAX_MSG_ID.0 && local_5.0 < SHORTCUT_MAX_MSG_ID.0);
        let _ =
            send_quick_reply_messages(&rpc, &session, u200, 1, &[local_5, local_6], vec![11, 22])
                .await
                .unwrap();
        let _ = delete_quick_reply_messages(&rpc, &session, 1, &[local_6])
            .await
            .unwrap();
        let send_qr_req = tl_fns::messages::SendQuickReplyMessages::from_bytes(
            &server.rpcs_of::<tl_fns::messages::SendQuickReplyMessages>()[0].body,
        )
        .unwrap();
        assert_eq!(send_qr_req.id, vec![5, 6]);
        assert_eq!(send_qr_req.random_id, vec![11, 22]);

        // 4. Local placeholder shortcut: `SHORTCUT_OCCUPIED` check + `SHORTCUT_ID_INVALID` local fallback.
        let local_sc_id = session
            .shortcut_messages_mut()
            .emplace_shortcut("draft_sc".to_owned());
        assert!(local_sc_id < 0);
        let occupied_err = edit_quick_reply_shortcut(&rpc, &mut session, local_sc_id, "hello")
            .await
            .unwrap_err();
        assert!(matches!(
            occupied_err,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "SHORTCUT_OCCUPIED"
        ));
        assert!(
            edit_quick_reply_shortcut(&rpc, &mut session, local_sc_id, "bye")
                .await
                .unwrap()
        );
        assert_eq!(
            session
                .shortcut_messages()
                .shortcut(local_sc_id)
                .unwrap()
                .name,
            "bye"
        );

        // 5. Live updates: `updateNewQuickReply` migrates `local_sc_id ("bye") -> 2`,
        //    `updateQuickReplies` with missing top_message -> `QuickRepliesReloadNeeded`,
        //    `updateQuickReplyMessage` on unloaded shortcut 3 -> `QuickReplyMessagesReloadNeeded`,
        //    `updateDeleteQuickReplyMessages` -> `QuickReplyMessagesReloadNeeded`,
        //    `updateDeleteQuickReply(2)` -> `ShortcutIdChanged { old_id: 2, new_id: 0 }`.
        let mut msg_sc3 = tl_message(
            tl_peer_user(100),
            Some(tl_peer_user(100)),
            99,
            1_700_000_099,
            "brand new sc3",
        );
        if let tl_enums::Message::Message(ref mut inner) = msg_sc3 {
            inner.quick_reply_shortcut_id = Some(3);
        }
        let mut rx = rpc.subscribe_updates().unwrap();
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::NewQuickReply(tl_types::UpdateNewQuickReply {
                        quick_reply: tl_enums::QuickReply::QuickReply(tl_types::QuickReply {
                            shortcut_id: 2,
                            shortcut: "bye".to_owned(),
                            top_message: 50,
                            count: 1,
                        }),
                    }),
                    tl_enums::Update::QuickReplies(tl_types::UpdateQuickReplies {
                        quick_replies: vec![
                            tl_enums::QuickReply::QuickReply(tl_types::QuickReply {
                                shortcut_id: 1,
                                shortcut: "hello".to_owned(),
                                top_message: 5,
                                count: 2,
                            }),
                            tl_enums::QuickReply::QuickReply(tl_types::QuickReply {
                                shortcut_id: 2,
                                shortcut: "bye".to_owned(),
                                top_message: 50,
                                count: 1,
                            }),
                        ],
                    }),
                    tl_enums::Update::QuickReplyMessage(tl_types::UpdateQuickReplyMessage {
                        message: msg_sc3,
                    }),
                    tl_enums::Update::DeleteQuickReplyMessages(
                        tl_types::UpdateDeleteQuickReplyMessages {
                            shortcut_id: 1,
                            messages: vec![6],
                        },
                    ),
                    tl_enums::Update::DeleteQuickReply(tl_types::UpdateDeleteQuickReply {
                        shortcut_id: 2,
                    }),
                ],
                users: vec![],
                chats: vec![],
                date: 1_700_000_300,
                seq: 2,
            }),
        ));
        let pushed = rx.recv().await.unwrap();
        updates
            .handle_updates(&rpc, &mut session, pushed, None, 1_700_000_300_000)
            .await
            .unwrap();
        let evs = updates.drain_events();
        assert!(
            evs.contains(&UpdateEvent::ShortcutIdChanged(tg_data::ShortcutIdChange {
                old_id: local_sc_id,
                new_id: 2,
            }))
        );
        assert!(evs.contains(&UpdateEvent::QuickRepliesReloadNeeded));
        assert!(evs.contains(&UpdateEvent::QuickReplyMessagesReloadNeeded { shortcut_id: 3 }));
        assert!(evs.contains(&UpdateEvent::QuickReplyMessagesReloadNeeded { shortcut_id: 1 }));
        assert!(
            evs.contains(&UpdateEvent::ShortcutIdChanged(tg_data::ShortcutIdChange {
                old_id: 2,
                new_id: 0,
            }))
        );
        assert!(session.shortcut_messages().shortcut(2).is_none());
        assert_eq!(session.shortcut_messages().messages(1).len(), 1);

        // Cleanup shortcut 1.
        assert!(
            delete_quick_reply_shortcut(&rpc, &mut session, 1)
                .await
                .unwrap()
        );
        assert!(session.shortcut_messages().shortcut(1).is_none());
    })
    .await
    .expect("test timeout");
}

#[tokio::test]
async fn history_parse_zero_anchor_and_error_handling_conformance() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Verify `parse_history_result` bug fix: `message_id == 0` with a partial `Slice`
        //    MUST NOT mark `loaded_at_top() == true` (range must be `[min_id, SERVER_MAX_MSG_ID]`).
        let mut session = seed_session();
        let ch_peer = PeerId::from_channel(tg_data::ChannelId(500));
        let slice = parse_history_result(
            MsgId::ZERO,
            LoadDirection::Before,
            tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
                inexact: false,
                count: 500,
                next_rate: None,
                offset_id_offset: None,
                search_flood: None,
                messages: vec![
                    tl_message(tl_peer_channel(500), None, 100, 1_700_000_100, "newest"),
                    tl_message(tl_peer_channel(500), None, 99, 1_700_000_099, "prev"),
                ],
                topics: vec![tl_forum_topic(
                    tl_peer_channel(500),
                    10,
                    "Embedded Topic",
                    100,
                    1,
                    99,
                    false,
                    false,
                )],
                chats: vec![],
                users: vec![],
            }),
        );
        assert_eq!(slice.no_skip_range, MsgRange::new(MsgId(99), SERVER_MAX_MSG_ID));
        slice.apply_to_session(&mut session, ch_peer);
        let hist = session.history(ch_peer).unwrap();
        assert!(hist.loaded_ranges.loaded_at_bottom());
        assert!(
            !hist.loaded_ranges.loaded_at_top(),
            "partial slice from offset_id=0 must NOT falsely mark loaded_at_top"
        );
        assert_eq!(
            session.forum_topic(ch_peer, MsgId(10)).unwrap().title,
            "Embedded Topic"
        );

        // 2. Verify protocol error propagation (`PREMIUM_ACCOUNT_REQUIRED`, `TOPIC_CLOSED`,
        //    `SCHEDULE_TOO_MUCH`, `CHAT_ADMIN_REQUIRED`, `FLOOD_PREMIUM_WAIT_12`).
        let server = ScriptedServer::new(|_, rpc| match rpc.ctor {
            tl_fns::messages::UpdateSavedReactionTag::CONSTRUCTOR_ID => {
                ServerAction::err(403, "PREMIUM_ACCOUNT_REQUIRED")
            }
            tl_fns::messages::EditForumTopic::CONSTRUCTOR_ID => {
                ServerAction::err(400, "TOPIC_CLOSED")
            }
            tl_fns::messages::SendScheduledMessages::CONSTRUCTOR_ID => {
                ServerAction::err(400, "SCHEDULE_TOO_MUCH")
            }
            tl_fns::messages::UpdatePinnedForumTopic::CONSTRUCTOR_ID => {
                ServerAction::err(400, "CHAT_ADMIN_REQUIRED")
            }
            tl_fns::channels::CheckSearchPostsFlood::CONSTRUCTOR_ID => {
                ServerAction::err(420, "FLOOD_PREMIUM_WAIT_12")
            }
            other => panic!("unexpected ctor: 0x{other:08x}"),
        });
        let (_client, rpc) = build_client_and_rpc(&server);

        let e_prem = update_saved_reaction_tag(
            &rpc,
            &mut session,
            &ReactionId::Emoji("🔥".to_owned()),
            Some("Tag"),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            e_prem,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "PREMIUM_ACCOUNT_REQUIRED"
        ));

        let e_closed = edit_forum_topic(
            &rpc,
            &session,
            ch_peer,
            MsgId(10),
            ForumTopicEdit {
                title: Some("Nope".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            e_closed,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "TOPIC_CLOSED"
        ));

        let e_sched = send_scheduled_messages(&rpc, &session, ch_peer, &[MsgId(1)])
            .await
            .unwrap_err();
        assert!(matches!(
            e_sched,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "SCHEDULE_TOO_MUCH"
        ));

        let e_admin = update_pinned_forum_topic(&rpc, &session, ch_peer, MsgId(10), true)
            .await
            .unwrap_err();
        assert!(matches!(
            e_admin,
            ApiError::Invocation(InvocationError::Rpc(ref e)) if e.name == "CHAT_ADMIN_REQUIRED"
        ));

        let mut posts = PostsSearchController::new();
        let e_flood_prem = posts.check_flood(&rpc, Some("q")).await.unwrap_err();
        assert!(matches!(
            e_flood_prem,
            ApiError::Invocation(InvocationError::Rpc(ref e))
                if e.name == "FLOOD_PREMIUM_WAIT_12" && e.flood_wait_seconds() == Some(12)
        ));
    })
    .await
    .expect("test timeout");
}
