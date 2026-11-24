//! Byte-level tdesktop conformance tests for Channels & Admin protocol (`tg-api`,
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

use tg_api::channels::{
    ADMIN_LOG_FIRST_PAGE, AdminLogEventsFilter, AdminLogQuery, ChannelParticipantKind,
    ChannelParticipantsFilter, ChannelParticipantsPage, ChatInviteImportersQuery,
    EXPORTED_INVITES_FIRST_PAGE, EXPORTED_INVITES_PER_PAGE, EditChatInviteOptions,
    EditChatInviteResult, ExportChatInviteOptions, ExportedChatInvitesQuery,
    INVITE_IMPORTERS_FIRST_PAGE, INVITE_IMPORTERS_PER_PAGE, api_hash_finalize, api_hash_init,
    api_hash_update_i32, count_hash_i64, edit_admin, edit_banned, edit_exported_chat_invite,
    export_chat_invite, get_admin_log, get_channel_recommendations, get_chat_invite_importers,
    get_exported_chat_invites, get_participant, get_participants, hide_all_chat_join_requests,
    hide_chat_join_request, kicked_restrictions_for_peer, normalize_ban_until_date,
    read_channel_history, read_channel_message_contents, read_common_message_contents,
    toggle_join_request, toggle_join_to_send, toggle_signatures, toggle_slow_mode,
};
use tg_api::convert::{
    admin_rights_from_tl, admin_rights_to_tl, apply_tl_chat, apply_tl_chat_full, apply_tl_user,
    banned_rights_to_tl,
};
use tg_api::{
    ApiCredentials, ApiError, CHANNEL_GET_DIFF_LIMIT, InvocationError, UpdateEvent, UpdatesHandler,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    ChannelFlags, ChannelId, ChatAdminRights, ChatBannedRights, ChatId, ChatRestrictionsInfo,
    FullMsgId, Message, MessageFlags, MsgId, PeerId, Session, UserId,
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

struct ScriptedServerInner {
    rsa_pub: PublicKey,
    rsa_n: Vec<u8>,
    rsa_d: Vec<u8>,
    next_conn_id: AtomicU64,
    dh_counter: AtomicU64,
    keys: Mutex<HashMap<u64, ServerKeyEntry>>,
    rpcs: Mutex<Vec<RpcRecord>>,
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

    fn spawn_connection(&self, _conn_id: u64, mut peer: FakeServerPeer) {
        let server = self.clone();
        let (_push_tx, mut push_rx) = mpsc::unbounded_channel::<tl_enums::Updates>();

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
                    _ = push_rx.recv() => {}
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
        _protocol_dc_id: i16,
        _secret: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn PacketConnection>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            let conn_id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
            let (client_conn, server_peer) = FakeConnection::pair();
            self.spawn_connection(conn_id, server_peer);
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

fn tl_user(id: i64, access_hash: i64, first: &str) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: false,
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

fn tl_channel(
    id: i64,
    access_hash: i64,
    title: &str,
    megagroup: bool,
    forum: bool,
) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: !megagroup,
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
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(50),
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

fn tl_basic_chat(id: i64, title: &str) -> tl_enums::Chat {
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
        date: 1_750_000_000,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    })
}

fn tl_channel_msg(channel_id: i64, id: i32, text: &str) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: true,
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
        guestchat_via_from: None,
        id,
        from_id: None,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: 1_750_000_000 + id,
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

fn tl_invite_exported(
    link: &str,
    permanent: bool,
    revoked: bool,
    date: i32,
) -> tl_enums::ExportedChatInvite {
    tl_enums::ExportedChatInvite::ChatInviteExported(tl_types::ChatInviteExported {
        revoked,
        permanent,
        request_needed: false,
        link: link.to_owned(),
        admin_id: 1000,
        date,
        start_date: None,
        expire_date: None,
        usage_limit: None,
        usage: Some(3),
        requested: Some(1),
        subscription_expired: None,
        title: Some("Main".to_owned()),
        subscription_pricing: None,
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![],
        users: vec![],
        chats: vec![],
        date: 1_750_000_000,
        seq: 0,
    })
}

fn seed_session() -> Session {
    let mut session = Session::new(UserId::from_tl(1000));
    apply_tl_user(&mut session, &tl_user(1000, 10000, "Admin"));
    apply_tl_user(&mut session, &tl_user(2001, 20010, "Alice"));
    apply_tl_user(&mut session, &tl_user(2002, 20020, "Bob"));
    apply_tl_user(&mut session, &tl_user(2003, 20030, "GuardBot"));
    apply_tl_chat(
        &mut session,
        &tl_channel(3001, 30010, "Supergroup", true, true),
    );
    apply_tl_chat(
        &mut session,
        &tl_channel(3002, 30020, "OtherChan", false, false),
    );
    apply_tl_chat(&mut session, &tl_basic_chat(4001, "BasicGroup"));
    session
}

// ============================================================================
// 1. Rights bitmasks, until_date, Api::CountHash, ChannelFlags::FORUM, ChannelFull slowmode
// ============================================================================

#[test]
fn rights_bitmasks_api_hash_forum_and_channel_full_slowmode_conformance() {
    // 1. All 19 Layer 233 ChatAdminRights round-trip bit-for-bit on the wire.
    let all_admin = ChatAdminRights::all();
    let tl_admin = admin_rights_to_tl(all_admin);
    let wire_admin = tl_admin.to_bytes();
    let decoded_admin = tl_enums::ChatAdminRights::from_bytes(&wire_admin).unwrap();
    assert_eq!(admin_rights_from_tl(&decoded_admin), all_admin);

    // 2. `banned_rights_to_tl` MUST NEVER set legacy `send_messages`/`send_media` bits
    //    (matching `RestrictionsToMTP` in `data/data_chat_participant_status.cpp:147-177`).
    let kicked_user = kicked_restrictions_for_peer(PeerId::from_user(UserId::from_tl(2001)));
    assert_eq!(kicked_user.until, 0);
    assert!(kicked_user.flags.contains(ChatBannedRights::VIEW_MESSAGES));
    assert!(kicked_user.flags.contains(ChatBannedRights::EMBED_LINKS));
    let tl_enums::ChatBannedRights::ChatBannedRights(tl_ku) = banned_rights_to_tl(kicked_user);
    assert!(tl_ku.view_messages);
    assert!(
        !tl_ku.send_messages,
        "legacy send_messages must never be set"
    );
    assert!(!tl_ku.send_media, "legacy send_media must never be set");
    assert!(
        tl_ku.send_stickers
            && tl_ku.send_gifs
            && tl_ku.send_games
            && tl_ku.send_inline
            && tl_ku.embed_links
    );

    // Non-user peer kick only sets VIEW_MESSAGES (`data/data_channel.cpp:495-514`).
    let kicked_chan = kicked_restrictions_for_peer(PeerId::from_channel(ChannelId::from_tl(3002)));
    assert_eq!(kicked_chan.flags, ChatBannedRights::VIEW_MESSAGES);
    assert_eq!(kicked_chan.until, 0);

    // `until_date` normalization: 0, negative, i32::MAX -> 0 (forever); finite kept.
    assert_eq!(normalize_ban_until_date(0), 0);
    assert_eq!(normalize_ban_until_date(-10), 0);
    assert_eq!(normalize_ban_until_date(i32::MAX), 0);
    assert_eq!(normalize_ban_until_date(1_750_086_400), 1_750_086_400);

    // 3. `Api::CountHash` matches `api/api_hash.h:36-70` (including i32 sign extension).
    assert_eq!(count_hash_i64([]), 0);
    assert_eq!(count_hash_i64([42]), 42);
    let mut h = api_hash_init();
    api_hash_update_i32(&mut h, -1);
    assert_eq!(api_hash_finalize(h), -1);
    let h_pair = count_hash_i64([1001, 1002]);
    let mut manual = 0u64;
    for v in [1001u64, 1002u64] {
        manual ^= manual >> 21;
        manual ^= manual << 35;
        manual ^= manual >> 4;
        manual = manual.wrapping_add(v);
    }
    assert_eq!(h_pair, manual as i64);

    // 4. `ChannelFlags::FORUM` only set when `d.forum && d.megagroup` (`data_session.cpp:1209`).
    let mut session = seed_session();
    apply_tl_chat(
        &mut session,
        &tl_channel(3099, 30990, "BroadcastWithForumBit", false, true),
    );
    assert!(
        !session
            .channel(ChannelId::from_tl(3099))
            .unwrap()
            .flags
            .contains(ChannelFlags::FORUM)
    );
    assert!(
        session
            .channel(ChannelId::from_tl(3001))
            .unwrap()
            .flags
            .contains(ChannelFlags::FORUM)
    );

    // 5. `ChannelFull` with `slowmode_seconds: None` clears stale slowmode value to 0.
    let make_full = |slowmode: Option<i32>| {
        tl_enums::ChatFull::ChannelFull(tl_types::ChannelFull {
            can_view_participants: true,
            can_set_username: false,
            can_set_stickers: false,
            hidden_prehistory: false,
            can_set_location: false,
            has_scheduled: false,
            can_view_stats: false,
            blocked: false,
            can_delete_channel: false,
            antispam: false,
            participants_hidden: false,
            translations_disabled: false,
            stories_pinned_available: false,
            view_forum_as_messages: false,
            restricted_sponsored: false,
            can_view_revenue: false,
            paid_media_allowed: false,
            can_view_stars_revenue: false,
            paid_reactions_available: false,
            stargifts_available: false,
            paid_messages_available: false,
            has_welcome_messages: false,
            id: 3001,
            about: "test".to_owned(),
            participants_count: Some(50),
            admins_count: Some(2),
            kicked_count: Some(0),
            banned_count: Some(0),
            online_count: None,
            read_inbox_max_id: 1,
            read_outbox_max_id: 1,
            unread_count: 0,
            chat_photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
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
            exported_invite: None,
            bot_info: vec![],
            migrated_from_chat_id: None,
            migrated_from_max_id: None,
            pinned_msg_id: None,
            stickerset: None,
            available_min_id: None,
            folder_id: None,
            linked_chat_id: None,
            location: None,
            slowmode_seconds: slowmode,
            slowmode_next_send_date: None,
            stats_dc: None,
            pts: 10,
            call: None,
            ttl_period: None,
            pending_suggestions: None,
            groupcall_default_join_as: None,
            theme_emoticon: None,
            requests_pending: None,
            recent_requesters: None,
            default_send_as: None,
            available_reactions: None,
            reactions_limit: None,
            stories: None,
            wallpaper: None,
            boosts_applied: slowmode.map(|_| 3),
            boosts_unrestrict: slowmode.map(|_| 2),
            emojiset: None,
            bot_verification: None,
            stargifts_count: None,
            send_paid_messages_stars: None,
            main_tab: None,
            guard_bot_id: None,
            share_theme: None,
        })
    };
    apply_tl_chat_full(&mut session, &make_full(Some(30)));
    let mg = session
        .channel(ChannelId::from_tl(3001))
        .unwrap()
        .mg_info
        .as_ref()
        .unwrap();
    assert_eq!(mg.slowmode_seconds, 30);
    assert_eq!(mg.boosts_applied, 3);
    assert_eq!(mg.boosts_unrestrict, 2);

    apply_tl_chat_full(&mut session, &make_full(None));
    let mg = session
        .channel(ChannelId::from_tl(3001))
        .unwrap()
        .mg_info
        .as_ref()
        .unwrap();
    assert_eq!(mg.slowmode_seconds, 0);
    assert_eq!(mg.boosts_applied, 0);
    assert_eq!(mg.boosts_unrestrict, 0);
}

// ============================================================================
// 2. Participants, editAdmin, editBanned, adminLog, toggles, join requests, invites, reads, recs
// ============================================================================

#[tokio::test]
async fn channels_admin_rpcs_over_mtproto_wire() {
    let expected_hash = count_hash_i64([1000, 2001]);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::channels::GetParticipants::CONSTRUCTOR_ID => {
            let req = tl_fns::channels::GetParticipants::from_bytes(&rpc.body).unwrap();
            if req.hash == expected_hash {
                return ServerAction::ok(&tl_enums::channels::ChannelParticipants::NotModified);
            }
            let participants = if req.offset == 0 {
                vec![
                    tl_enums::ChannelParticipant::Creator(tl_types::ChannelParticipantCreator {
                        user_id: 1000,
                        admin_rights: admin_rights_to_tl(ChatAdminRights::all()),
                        rank: Some("Owner".to_owned()),
                    }),
                    tl_enums::ChannelParticipant::Admin(tl_types::ChannelParticipantAdmin {
                        can_edit: true,
                        self_: false,
                        user_id: 2001,
                        inviter_id: Some(1000),
                        promoted_by: 1000,
                        date: 1_750_000_001,
                        admin_rights: admin_rights_to_tl(
                            ChatAdminRights::BAN_USERS | ChatAdminRights::DELETE_MESSAGES,
                        ),
                        rank: Some("Mod".to_owned()),
                    }),
                ]
            } else {
                vec![tl_enums::ChannelParticipant::Banned(
                    tl_types::ChannelParticipantBanned {
                        left: false,
                        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 2002 }),
                        kicked_by: 1000,
                        date: 1_750_000_002,
                        banned_rights: banned_rights_to_tl(ChatRestrictionsInfo::new(
                            ChatBannedRights::SEND_PLAIN,
                            1_750_086_400,
                        )),
                        rank: None,
                    },
                )]
            };
            ServerAction::ok(
                &tl_enums::channels::ChannelParticipants::ChannelParticipants(
                    tl_types::channels::ChannelParticipants {
                        count: 3,
                        participants,
                        chats: vec![],
                        users: vec![tl_user(2002, 20020, "Bob")],
                    },
                ),
            )
        }
        x if x == tl_fns::channels::GetParticipant::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::channels::ChannelParticipant::ChannelParticipant(
                tl_types::channels::ChannelParticipant {
                    participant: tl_enums::ChannelParticipant::Self_(
                        tl_types::ChannelParticipantSelf {
                            via_request: true,
                            user_id: 1000,
                            inviter_id: 2001,
                            date: 1_750_000_000,
                            subscription_until_date: None,
                            rank: Some("Founder".to_owned()),
                        },
                    ),
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        x if x == tl_fns::channels::EditAdmin::CONSTRUCTOR_ID
            || x == tl_fns::channels::EditBanned::CONSTRUCTOR_ID
            || x == tl_fns::channels::ToggleSlowMode::CONSTRUCTOR_ID
            || x == tl_fns::channels::ToggleSignatures::CONSTRUCTOR_ID
            || x == tl_fns::channels::ToggleJoinToSend::CONSTRUCTOR_ID
            || x == tl_fns::channels::ToggleJoinRequest::CONSTRUCTOR_ID
            || x == tl_fns::messages::HideChatJoinRequest::CONSTRUCTOR_ID
            || x == tl_fns::messages::HideAllChatJoinRequests::CONSTRUCTOR_ID =>
        {
            ServerAction::ok(&empty_updates())
        }
        x if x == tl_fns::channels::GetAdminLog::CONSTRUCTOR_ID => {
            let req = tl_fns::channels::GetAdminLog::from_bytes(&rpc.body).unwrap();
            let ev_id = if req.max_id == 0 { 500 } else { 400 };
            ServerAction::ok(&tl_enums::channels::AdminLogResults::AdminLogResults(
                tl_types::channels::AdminLogResults {
                    events: vec![tl_enums::ChannelAdminLogEvent::ChannelAdminLogEvent(
                        tl_types::ChannelAdminLogEvent {
                            id: ev_id,
                            date: 1_750_000_000,
                            user_id: 1000,
                            action: tl_enums::ChannelAdminLogEventAction::ToggleSlowMode(
                                tl_types::ChannelAdminLogEventActionToggleSlowMode {
                                    prev_value: 0,
                                    new_value: 30,
                                },
                            ),
                        },
                    )],
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        x if x == tl_fns::messages::GetChatInviteImporters::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetChatInviteImporters::from_bytes(&rpc.body).unwrap();
            let (uid, date) = if req.offset_date == 0 {
                (2001, 1_750_000_100)
            } else {
                (2002, 1_750_000_050)
            };
            ServerAction::ok(
                &tl_enums::messages::ChatInviteImporters::ChatInviteImporters(
                    tl_types::messages::ChatInviteImporters {
                        count: 2,
                        importers: vec![tl_enums::ChatInviteImporter::ChatInviteImporter(
                            tl_types::ChatInviteImporter {
                                requested: req.requested,
                                via_chatlist: false,
                                user_id: uid,
                                date,
                                about: Some("Let me in".to_owned()),
                                approved_by: None,
                            },
                        )],
                        users: vec![],
                    },
                ),
            )
        }
        x if x == tl_fns::messages::ExportChatInvite::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_invite_exported("https://t.me/+newlink1", false, false, 1_750_000_200),
        ),
        x if x == tl_fns::messages::EditExportedChatInvite::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::EditExportedChatInvite::from_bytes(&rpc.body).unwrap();
            if req.revoked {
                ServerAction::ok(&tl_enums::messages::ExportedChatInvite::Replaced(
                    tl_types::messages::ExportedChatInviteReplaced {
                        invite: tl_invite_exported(&req.link, true, true, 1_750_000_100),
                        new_invite: tl_invite_exported(
                            "https://t.me/+replaced2",
                            true,
                            false,
                            1_750_000_300,
                        ),
                        users: vec![],
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::messages::ExportedChatInvite::ExportedChatInvite(
                    tl_types::messages::ExportedChatInvite {
                        invite: tl_invite_exported(&req.link, false, false, 1_750_000_200),
                        users: vec![],
                    },
                ))
            }
        }
        x if x == tl_fns::messages::GetExportedChatInvites::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetExportedChatInvites::from_bytes(&rpc.body).unwrap();
            let inv = if req.offset_date.is_none() {
                tl_invite_exported("https://t.me/+inv1", true, req.revoked, 1_750_000_200)
            } else {
                tl_invite_exported("https://t.me/+inv2", false, req.revoked, 1_750_000_100)
            };
            ServerAction::ok(
                &tl_enums::messages::ExportedChatInvites::ExportedChatInvites(
                    tl_types::messages::ExportedChatInvites {
                        count: 2,
                        invites: vec![inv],
                        users: vec![],
                    },
                ),
            )
        }
        x if x == tl_fns::channels::ReadHistory::CONSTRUCTOR_ID
            || x == tl_fns::channels::ReadMessageContents::CONSTRUCTOR_ID =>
        {
            ServerAction::ok(&true)
        }
        x if x == tl_fns::messages::ReadMessageContents::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::messages::AffectedMessages::AffectedMessages(
                tl_types::messages::AffectedMessages {
                    pts: 25,
                    pts_count: 1,
                },
            ))
        }
        x if x == tl_fns::channels::GetChannelRecommendations::CONSTRUCTOR_ID => {
            let req = tl_fns::channels::GetChannelRecommendations::from_bytes(&rpc.body).unwrap();
            if req.channel.is_some() {
                ServerAction::ok(&tl_enums::messages::Chats::Slice(
                    tl_types::messages::ChatsSlice {
                        count: 12,
                        chats: vec![tl_channel(3050, 30500, "Rec1", false, false)],
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::messages::Chats::Chats(
                    tl_types::messages::Chats {
                        chats: vec![tl_channel(3051, 30510, "GlobalRec", false, false)],
                    },
                ))
            }
        }
        other => ServerAction::err(400, format!("UNEXPECTED_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = seed_session();
    let cid = ChannelId::from_tl(3001);
    let cpeer = PeerId::from_channel(cid);

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. `channels.getParticipants`: page 0, page 1, NotModified with hash, plus all filter shapes.
        let p0 = get_participants(
            &rpc,
            &mut session,
            cid,
            &ChannelParticipantsFilter::Recent,
            0,
            2,
            0,
        )
        .await
        .unwrap();
        match p0 {
            ChannelParticipantsPage::Participants {
                count,
                participants,
            } => {
                assert_eq!(count, 3);
                assert_eq!(participants.len(), 2);
                assert_eq!(participants[0].kind, ChannelParticipantKind::Creator);
                assert_eq!(participants[1].kind, ChannelParticipantKind::Admin);
                assert!(participants[1].can_edit);
                assert_eq!(participants[1].rank.as_deref(), Some("Mod"));
            }
            _ => panic!("expected Participants"),
        }
        let p1 = get_participants(
            &rpc,
            &mut session,
            cid,
            &ChannelParticipantsFilter::Banned {
                query: "bob".to_owned(),
            },
            2,
            200,
            0,
        )
        .await
        .unwrap();
        match p1 {
            ChannelParticipantsPage::Participants { participants, .. } => {
                assert_eq!(participants[0].kind, ChannelParticipantKind::Restricted);
                assert_eq!(participants[0].restrictions.until, 1_750_086_400);
            }
            _ => panic!("expected Participants page 1"),
        }
        let nm = get_participants(
            &rpc,
            &mut session,
            cid,
            &ChannelParticipantsFilter::Admins,
            0,
            200,
            expected_hash,
        )
        .await
        .unwrap();
        assert_eq!(nm, ChannelParticipantsPage::NotModified);

        // Exercise remaining filter constructors on wire.
        for f in [
            ChannelParticipantsFilter::Kicked {
                query: String::new(),
            },
            ChannelParticipantsFilter::Bots,
            ChannelParticipantsFilter::Search {
                query: "al".to_owned(),
            },
            ChannelParticipantsFilter::Contacts {
                query: String::new(),
            },
            ChannelParticipantsFilter::Mentions {
                query: Some("a".to_owned()),
                top_msg_id: Some(MsgId(42)),
            },
        ] {
            let _ = get_participants(&rpc, &mut session, cid, &f, 0, 50, 0)
                .await
                .unwrap();
        }

        // 2. `channels.getParticipant`.
        let self_part = get_participant(
            &rpc,
            &mut session,
            cid,
            PeerId::from_user(UserId::from_tl(1000)),
        )
        .await
        .unwrap();
        assert!(self_part.is_self && self_part.via_request);
        assert_eq!(self_part.rank.as_deref(), Some("Founder"));

        // 3. `channels.editAdmin`: promote with rank, clear rank (`Some("")`), leave unchanged (`None`).
        edit_admin(
            &rpc,
            &session,
            cid,
            UserId::from_tl(2001),
            ChatAdminRights::BAN_USERS | ChatAdminRights::MANAGE_TOPICS,
            Some("Moderator"),
        )
        .await
        .unwrap();
        edit_admin(
            &rpc,
            &session,
            cid,
            UserId::from_tl(2001),
            ChatAdminRights::empty(),
            Some(""),
        )
        .await
        .unwrap();
        edit_admin(
            &rpc,
            &session,
            cid,
            UserId::from_tl(2001),
            ChatAdminRights::PIN_MESSAGES,
            None,
        )
        .await
        .unwrap();

        // 4. `channels.editBanned`: kick user, kick non-user channel, temporary restrict, unban.
        edit_banned(
            &rpc,
            &session,
            cid,
            PeerId::from_user(UserId::from_tl(2002)),
            kicked_restrictions_for_peer(PeerId::from_user(UserId::from_tl(2002))),
        )
        .await
        .unwrap();
        edit_banned(
            &rpc,
            &session,
            cid,
            PeerId::from_channel(ChannelId::from_tl(3002)),
            kicked_restrictions_for_peer(PeerId::from_channel(ChannelId::from_tl(3002))),
        )
        .await
        .unwrap();
        edit_banned(
            &rpc,
            &session,
            cid,
            PeerId::from_user(UserId::from_tl(2002)),
            ChatRestrictionsInfo::new(ChatBannedRights::SEND_PLAIN, 1_750_086_400),
        )
        .await
        .unwrap();

        // 5. `channels.getAdminLog`: page 0 (`max_id=0`, limit=100, filter + admins) -> page 1 (`max_id=500`).
        let log0 = get_admin_log(
            &rpc,
            &mut session,
            cid,
            &AdminLogQuery {
                query: "slow".to_owned(),
                events_filter: Some(
                    AdminLogEventsFilter::SETTINGS
                        | AdminLogEventsFilter::PROMOTE
                        | AdminLogEventsFilter::EDIT_RANK,
                ),
                admins: vec![UserId::from_tl(1000)],
                max_id: 0,
                min_id: 0,
                limit: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(log0.min_event_id, Some(500));
        let log1 = get_admin_log(
            &rpc,
            &mut session,
            cid,
            &AdminLogQuery {
                max_id: log0.min_event_id.unwrap(),
                ..AdminLogQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(log1.min_event_id, Some(400));

        // 6. Toggles: slowMode, signatures (Layer 233 flags), joinToSend, joinRequest (Layer 233 flags).
        toggle_slow_mode(&rpc, &session, cid, 30).await.unwrap();
        toggle_signatures(&rpc, &session, cid, true, true)
            .await
            .unwrap();
        toggle_signatures(&rpc, &session, cid, false, true)
            .await
            .unwrap();
        toggle_join_to_send(&rpc, &session, cid, true)
            .await
            .unwrap();
        toggle_join_request(&rpc, &session, cid, true, true, Some(UserId::from_tl(2003)))
            .await
            .unwrap();

        // 7. Join requests: hide one + hide all (with link).
        hide_chat_join_request(&rpc, &session, cpeer, UserId::from_tl(2001), true)
            .await
            .unwrap();
        hide_all_chat_join_requests(&rpc, &session, cpeer, false, Some("https://t.me/+inv1"))
            .await
            .unwrap();

        // 8. `messages.getChatInviteImporters` paging (first page limit 10 -> next page limit 50).
        let imp1 = get_chat_invite_importers(
            &rpc,
            &mut session,
            cpeer,
            &ChatInviteImportersQuery {
                requested: true,
                link: Some("https://t.me/+inv1".to_owned()),
                ..ChatInviteImportersQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(imp1.count, 2);
        let (off_date, off_user) = imp1.next_offset.unwrap();
        let imp2 = get_chat_invite_importers(
            &rpc,
            &mut session,
            cpeer,
            &ChatInviteImportersQuery {
                requested: true,
                link: Some("https://t.me/+inv1".to_owned()),
                offset_date: off_date,
                offset_user: Some(off_user),
                ..ChatInviteImportersQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(imp2.importers[0].user_id, UserId::from_tl(2002));

        // 9. `exportChatInvite` (request_needed strips usage_limit), `editExportedChatInvite` (Edited + Replaced),
        //    `getExportedChatInvites` paging (offset_date + offset_link shared flag, limit 10 -> 50).
        let exp = export_chat_invite(
            &rpc,
            &session,
            cpeer,
            &ExportChatInviteOptions {
                request_needed: true,
                usage_limit: Some(10),
                expire_date: Some(1_750_086_400),
                title: Some("VIP".to_owned()),
                ..ExportChatInviteOptions::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(exp.link, "https://t.me/+newlink1");

        let edited = edit_exported_chat_invite(
            &rpc,
            &mut session,
            cpeer,
            "https://t.me/+newlink1".to_owned(),
            &EditChatInviteOptions {
                title: Some("VIP 2".to_owned()),
                ..EditChatInviteOptions::default()
            },
        )
        .await
        .unwrap();
        assert!(matches!(edited, EditChatInviteResult::Edited(Some(_))));

        let replaced = edit_exported_chat_invite(
            &rpc,
            &mut session,
            cpeer,
            "https://t.me/+inv1".to_owned(),
            &EditChatInviteOptions {
                revoked: true,
                ..EditChatInviteOptions::default()
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            replaced,
            EditChatInviteResult::Replaced {
                old_invite: Some(ref old),
                new_invite: Some(ref new),
            } if old.revoked && new.link == "https://t.me/+replaced2"
        ));

        let inv_p1 = get_exported_chat_invites(
            &rpc,
            &mut session,
            cpeer,
            &ExportedChatInvitesQuery {
                admin_id: UserId::from_tl(1000),
                revoked: false,
                offset: None,
                limit: 0,
            },
        )
        .await
        .unwrap();
        let inv_p2 = get_exported_chat_invites(
            &rpc,
            &mut session,
            cpeer,
            &ExportedChatInvitesQuery {
                admin_id: UserId::from_tl(1000),
                revoked: false,
                offset: inv_p1.next_offset,
                limit: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(inv_p2.invites[0].link, "https://t.me/+inv2");

        // 10. `channels.readHistory`, `channels.readMessageContents`, `messages.readMessageContents`,
        //     `channels.getChannelRecommendations`.
        assert!(
            read_channel_history(&rpc, &session, cid, MsgId(99))
                .await
                .unwrap()
        );
        assert!(
            read_channel_message_contents(&rpc, &session, cid, &[MsgId(10), MsgId(11)])
                .await
                .unwrap()
        );
        let aff = read_common_message_contents(&rpc, &[MsgId(7), MsgId(8)])
            .await
            .unwrap();
        let tl_enums::messages::AffectedMessages::AffectedMessages(aff) = aff;
        assert_eq!(aff.pts, 25);

        let rec_slice = get_channel_recommendations(&rpc, &mut session, Some(cid))
            .await
            .unwrap();
        assert_eq!(
            rec_slice.peers,
            vec![PeerId::from_channel(ChannelId::from_tl(3050))]
        );
        assert_eq!(rec_slice.total_count, 12);
        assert_eq!(rec_slice.more, 11);

        let rec_global = get_channel_recommendations(&rpc, &mut session, None)
            .await
            .unwrap();
        assert_eq!(
            rec_global.peers,
            vec![PeerId::from_channel(ChannelId::from_tl(3051))]
        );
        assert_eq!(rec_global.more, 0);
    })
    .await
    .expect("channels_admin_rpcs timed out");

    // Wire assertions on recorded RPC payloads.
    let edit_admins = server.rpcs_of::<tl_fns::channels::EditAdmin>();
    assert_eq!(edit_admins.len(), 3);
    assert_eq!(
        tl_fns::channels::EditAdmin::from_bytes(&edit_admins[0].body)
            .unwrap()
            .rank
            .as_deref(),
        Some("Moderator")
    );
    assert_eq!(
        tl_fns::channels::EditAdmin::from_bytes(&edit_admins[1].body)
            .unwrap()
            .rank
            .as_deref(),
        Some(""),
        "clearing rank / demoting must explicitly send Some(\"\")"
    );
    assert_eq!(
        tl_fns::channels::EditAdmin::from_bytes(&edit_admins[2].body)
            .unwrap()
            .rank,
        None
    );

    let sigs = server.rpcs_of::<tl_fns::channels::ToggleSignatures>();
    let sig0 = tl_fns::channels::ToggleSignatures::from_bytes(&sigs[0].body).unwrap();
    let sig1 = tl_fns::channels::ToggleSignatures::from_bytes(&sigs[1].body).unwrap();
    assert!(sig0.signatures_enabled && sig0.profiles_enabled);
    assert!(!sig1.signatures_enabled && !sig1.profiles_enabled);

    let jreq = tl_fns::channels::ToggleJoinRequest::from_bytes(
        &server.rpcs_of::<tl_fns::channels::ToggleJoinRequest>()[0].body,
    )
    .unwrap();
    assert!(jreq.enabled && jreq.apply_to_invites && jreq.guard_bot.is_some());

    let exp_wire = tl_fns::messages::ExportChatInvite::from_bytes(
        &server.rpcs_of::<tl_fns::messages::ExportChatInvite>()[0].body,
    )
    .unwrap();
    assert!(exp_wire.request_needed && exp_wire.usage_limit.is_none());

    let inv_reqs = server.rpcs_of::<tl_fns::messages::GetExportedChatInvites>();
    let inv0 = tl_fns::messages::GetExportedChatInvites::from_bytes(&inv_reqs[0].body).unwrap();
    let inv1 = tl_fns::messages::GetExportedChatInvites::from_bytes(&inv_reqs[1].body).unwrap();
    assert_eq!(inv0.limit, EXPORTED_INVITES_FIRST_PAGE);
    assert!(inv0.offset_date.is_none() && inv0.offset_link.is_none());
    assert_eq!(inv1.limit, EXPORTED_INVITES_PER_PAGE);
    assert_eq!(inv1.offset_date, Some(1_750_000_200));
    assert_eq!(inv1.offset_link.as_deref(), Some("https://t.me/+inv1"));

    let imp_reqs = server.rpcs_of::<tl_fns::messages::GetChatInviteImporters>();
    let imp0 = tl_fns::messages::GetChatInviteImporters::from_bytes(&imp_reqs[0].body).unwrap();
    let imp1 = tl_fns::messages::GetChatInviteImporters::from_bytes(&imp_reqs[1].body).unwrap();
    assert_eq!(imp0.limit, INVITE_IMPORTERS_FIRST_PAGE);
    assert_eq!(imp1.limit, INVITE_IMPORTERS_PER_PAGE);

    let log_req0 = tl_fns::channels::GetAdminLog::from_bytes(
        &server.rpcs_of::<tl_fns::channels::GetAdminLog>()[0].body,
    )
    .unwrap();
    assert_eq!(log_req0.limit, ADMIN_LOG_FIRST_PAGE);
    assert!(log_req0.events_filter.is_some());
    assert_eq!(log_req0.admins.as_ref().map(Vec::len), Some(1));
}

// ============================================================================
// 3. `updates.getChannelDifference`, `updateChannelTooLong`, `channelDifferenceTooLong`
//    with `requestChannelRangeDifference`, and `updatePendingJoinRequests`
// ============================================================================

#[tokio::test]
async fn channel_difference_too_long_runs_range_difference_and_pending_join_requests() {
    let cid = ChannelId::from_tl(3001);
    let peer_id = PeerId::from_channel(cid);

    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        x if x == tl_fns::updates::GetChannelDifference::CONSTRUCTOR_ID => {
            let req = tl_fns::updates::GetChannelDifference::from_bytes(&rpc.body).unwrap();
            match req.filter {
                // Main diff request -> return `channelDifferenceTooLong` jumping pts 10 -> 500
                tl_enums::ChannelMessagesFilter::Empty => {
                    assert_eq!(req.limit, CHANNEL_GET_DIFF_LIMIT);
                    assert_eq!(req.pts, 10);
                    ServerAction::ok(&tl_enums::updates::ChannelDifference::TooLong(
                        tl_types::updates::ChannelDifferenceTooLong {
                            r#final: true,
                            timeout: Some(30),
                            dialog: tl_enums::Dialog::Dialog(tl_types::Dialog {
                                pinned: false,
                                unread_mark: false,
                                view_forum_as_messages: false,
                                peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                                    channel_id: 3001,
                                }),
                                top_message: 50,
                                read_inbox_max_id: 40,
                                read_outbox_max_id: 40,
                                unread_count: 10,
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
                                pts: Some(500),
                                draft: None,
                                folder_id: None,
                                ttl_period: None,
                            }),
                            messages: vec![tl_channel_msg(3001, 50, "latest post 50")],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
                // Follow-up range difference (`requestChannelRangeDifference`) for loaded [10, 12]
                tl_enums::ChannelMessagesFilter::ChannelMessagesFilter(f) => {
                    assert!(req.force, "range diff must set force=true");
                    assert_eq!(req.pts, 10, "range diff must use pre-TooLong old_pts");
                    assert_eq!(f.ranges.len(), 1);
                    let tl_enums::MessageRange::MessageRange(r) = &f.ranges[0];
                    assert_eq!((r.min_id, r.max_id), (10, 12));
                    assert_eq!(req.limit, 3);
                    // Report msg 11 edited and msg 12 deleted!
                    ServerAction::ok(&tl_enums::updates::ChannelDifference::ChannelDifference(
                        tl_types::updates::ChannelDifference {
                            r#final: true,
                            pts: 500,
                            timeout: None,
                            new_messages: vec![],
                            other_updates: vec![
                                tl_enums::Update::EditChannelMessage(
                                    tl_types::UpdateEditChannelMessage {
                                        message: tl_channel_msg(3001, 11, "edited post 11"),
                                        pts: 11,
                                        pts_count: 1,
                                    },
                                ),
                                tl_enums::Update::DeleteChannelMessages(
                                    tl_types::UpdateDeleteChannelMessages {
                                        channel_id: 3001,
                                        messages: vec![12],
                                        pts: 12,
                                        pts_count: 1,
                                    },
                                ),
                            ],
                            chats: vec![],
                            users: vec![],
                        },
                    ))
                }
            }
        }
        other => ServerAction::err(400, format!("UNEXPECTED_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = seed_session();
    // Seed loaded history slice [10, 11, 12] so `range_for_difference_request()` returns [10, 13).
    let mut seed_msgs = Vec::new();
    for id in 10..=12 {
        let mut msg = Message::new_text(
            peer_id,
            MsgId(id),
            None,
            1_750_000_000 + id as i32,
            tg_data::TextWithEntities::plain(format!("old post {id}")),
        );
        msg.flags = MessageFlags::HAS_VIEWS;
        seed_msgs.push(msg);
    }
    session.add_history_slice(
        peer_id,
        seed_msgs,
        tg_data::MsgRange::new(MsgId(10), MsgId(13)),
        None,
    );

    let mut handler = UpdatesHandler::new();
    handler
        .state_machine_mut()
        .init_state(10, 1_750_000_000, 0, 1, 1_000);
    handler.init_channel_pts(cid, 10);

    tokio::time::timeout(TEST_TIMEOUT, async {
        // Trigger via `updateChannelTooLong` with pts=500 -> forces `updates.getChannelDifference`.
        let too_long_upd = tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![
                tl_enums::Update::ChannelTooLong(tl_types::UpdateChannelTooLong {
                    channel_id: 3001,
                    pts: Some(500),
                }),
                tl_enums::Update::PendingJoinRequests(tl_types::UpdatePendingJoinRequests {
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 3001 }),
                    requests_pending: 4,
                    recent_requesters: vec![2001, 2002],
                }),
                tl_enums::Update::PendingJoinRequests(tl_types::UpdatePendingJoinRequests {
                    peer: tl_enums::Peer::Chat(tl_types::PeerChat { chat_id: 4001 }),
                    requests_pending: 2,
                    recent_requesters: vec![2001],
                }),
            ],
            users: vec![],
            chats: vec![],
            date: 1_750_000_100,
            seq: 0,
        });

        handler
            .handle_updates(&rpc, &mut session, too_long_upd, None, 20_000)
            .await
            .unwrap();
    })
    .await
    .expect("channel_difference_too_long test timed out");

    // Verify both main diff AND range diff were sent over encrypted MTProto.
    let diff_rpcs = server.rpcs_of::<tl_fns::updates::GetChannelDifference>();
    assert_eq!(diff_rpcs.len(), 2);

    // Channel pts advanced to 500; snapshot msg 50 inserted; range diff edited msg 11 and deleted msg 12!
    assert_eq!(handler.channel_pts(cid), Some(500));
    assert!(
        session
            .message(FullMsgId::new(peer_id, MsgId(50)))
            .is_some()
    );
    let msg11 = session.message(FullMsgId::new(peer_id, MsgId(11))).unwrap();
    assert_eq!(msg11.text().unwrap().text, "edited post 11");
    assert!(
        session
            .message(FullMsgId::new(peer_id, MsgId(12)))
            .is_none()
    );

    // `ChannelTooLong` event emitted.
    let events = handler.drain_events();
    assert!(events.iter().any(|e| matches!(
        e,
        UpdateEvent::ChannelTooLong { channel_id, pts: Some(500) } if *channel_id == cid
    )));

    // `updatePendingJoinRequests` updated both Channel and Chat.
    let ch = session.channel(cid).unwrap();
    assert_eq!(ch.pending_requests_count, 4);
    assert_eq!(
        ch.recent_requesters,
        vec![UserId::from_tl(2001), UserId::from_tl(2002)]
    );
    let chat = session.chat(ChatId::from_tl(4001)).unwrap();
    assert_eq!(chat.pending_requests_count, 2);
    assert_eq!(chat.recent_requesters, vec![UserId::from_tl(2001)]);
}

// ============================================================================
// 4. Non-final `updates.getChannelDifference` slice loop & admin RPC error paths
// ============================================================================

#[tokio::test]
async fn channel_difference_non_final_loop_and_admin_error_paths() {
    let cid = ChannelId::from_tl(3001);
    let peer_id = PeerId::from_channel(cid);

    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        x if x == tl_fns::updates::GetChannelDifference::CONSTRUCTOR_ID => {
            let req = tl_fns::updates::GetChannelDifference::from_bytes(&rpc.body).unwrap();
            if req.pts == 20 {
                ServerAction::ok(&tl_enums::updates::ChannelDifference::ChannelDifference(
                    tl_types::updates::ChannelDifference {
                        r#final: false,
                        pts: 21,
                        timeout: None,
                        new_messages: vec![tl_channel_msg(3001, 101, "slice 1")],
                        other_updates: vec![],
                        chats: vec![],
                        users: vec![],
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::updates::ChannelDifference::ChannelDifference(
                    tl_types::updates::ChannelDifference {
                        r#final: true,
                        pts: 22,
                        timeout: Some(15),
                        new_messages: vec![tl_channel_msg(3001, 102, "slice 2")],
                        other_updates: vec![],
                        chats: vec![],
                        users: vec![],
                    },
                ))
            }
        }
        x if x == tl_fns::channels::EditAdmin::CONSTRUCTOR_ID => {
            ServerAction::err(403, "CHAT_ADMIN_REQUIRED")
        }
        x if x == tl_fns::channels::GetParticipant::CONSTRUCTOR_ID => {
            ServerAction::err(400, "USER_NOT_PARTICIPANT")
        }
        x if x == tl_fns::messages::EditExportedChatInvite::CONSTRUCTOR_ID => {
            ServerAction::err(400, "INVITE_HASH_EXPIRED")
        }
        other => ServerAction::err(400, format!("UNEXPECTED_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = seed_session();
    let mut handler = UpdatesHandler::new();
    handler
        .state_machine_mut()
        .init_state(10, 1_750_000_000, 0, 1, 1_000);
    handler.init_channel_pts(cid, 20);

    tokio::time::timeout(TEST_TIMEOUT, async {
        // Trigger non-final channel difference loop (pts 20 -> 21 (final=false) -> 22 (final=true)).
        let gap = tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::ChannelTooLong(
                tl_types::UpdateChannelTooLong {
                    channel_id: 3001,
                    pts: Some(22),
                },
            )],
            users: vec![],
            chats: vec![],
            date: 1_750_000_200,
            seq: 0,
        });
        handler
            .handle_updates(&rpc, &mut session, gap, None, 30_000)
            .await
            .unwrap();
        assert_eq!(handler.channel_pts(cid), Some(22));
        assert!(session.message(FullMsgId::new(peer_id, MsgId(101))).is_some());
        assert!(session.message(FullMsgId::new(peer_id, MsgId(102))).is_some());

        // Error paths propagate cleanly.
        let e1 = edit_admin(
            &rpc,
            &session,
            cid,
            UserId::from_tl(2001),
            ChatAdminRights::BAN_USERS,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            e1,
            ApiError::Invocation(InvocationError::Rpc(ref r)) if r.code == 403 && r.name == "CHAT_ADMIN_REQUIRED"
        ));

        let e2 = get_participant(&rpc, &mut session, cid, PeerId::from_user(UserId::from_tl(2002)))
            .await
            .unwrap_err();
        assert!(matches!(
            e2,
            ApiError::Invocation(InvocationError::Rpc(ref r)) if r.code == 400 && r.name == "USER_NOT_PARTICIPANT"
        ));

        let e3 = edit_exported_chat_invite(
            &rpc,
            &mut session,
            peer_id,
            "https://t.me/+expired".to_owned(),
            &EditChatInviteOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            e3,
            ApiError::Invocation(InvocationError::Rpc(ref r)) if r.code == 400 && r.name == "INVITE_HASH_EXPIRED"
        ));
    })
    .await
    .expect("non_final_loop_and_errors timed out");

    assert_eq!(
        server
            .rpcs_of::<tl_fns::updates::GetChannelDifference>()
            .len(),
        2
    );
}
