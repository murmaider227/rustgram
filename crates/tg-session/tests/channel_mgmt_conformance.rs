//! Byte-level tdesktop conformance tests for WP-65 Channel & Supergroup Management RPCs
//! (`tg-api::channel_mgmt`, `tg-data`, `tg-session`, `tg-tl`) against an in-process scripted
//! fake MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Covers all 15 WP-65 RPCs:
//! 1. `channels.checkUsername#10e6bd2c`
//! 2. `channels.togglePreHistoryHidden#eabbb94c`
//! 3. `channels.getGroupsForDiscussion#f5dad378`
//! 4. `channels.convertToGigagroup#0b290c69`
//! 5. `channels.deactivateAllUsernames#0a245dd3`
//! 6. `channels.toggleForum#a2952b54`
//! 7. `channels.toggleParticipantsHidden#6a6e7854`
//! 8. `channels.toggleViewForumAsMessages#9738bb15`
//! 9. `channels.setBoostsToUnblockRestrictions#ad399cee`
//! 10. `channels.setStickers#ea8ca4f9`
//! 11. `channels.setEmojiStickers#3cd930b7`
//! 12. `channels.updatePaidMessagesPrice#4b12327b`
//! 13. `channels.toggleAutotranslation#167fc0a1`
//! 14. `messages.deleteChat#5bd0ee50` (+ `PEER_ID_INVALID` self-join retry)
//! 15. `messages.getAdminsWithInvites#3920e6ef`
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses synthetic credentials (`api_id = 1`, `api_hash = "0000...0000"`), synthetic peer IDs,
//!   and an ephemeral test-only RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::ApiCredentials;
use tg_api::channel_mgmt::{
    ChannelStickerSetInput, ChannelUsernameCheckResult, DeleteChatOutcome, check_channel_username,
    convert_to_gigagroup, deactivate_all_usernames, delete_chat, get_admins_with_invites,
    get_groups_for_discussion, set_boosts_to_unblock_restrictions, set_emoji_stickers,
    set_stickers, toggle_autotranslation, toggle_forum, toggle_participants_hidden,
    toggle_pre_history_hidden, toggle_view_forum_as_messages, update_paid_messages_price,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatFlags, ChatId, PeerId, PeerUpdateFlags, Session,
    UserId, Username,
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
    BOOL_FALSE_ID, BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, RawObject,
    Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::mpsc;

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

    fn all_rpcs(&self) -> Vec<RpcRecord> {
        self.inner.rpcs.lock().unwrap().clone()
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

fn make_mtproto_rpc(server: &ScriptedServer) -> (Client, MtprotoRpc) {
    let mut cfg: ClientConfig = build_mtproto_client_config(
        &ApiCredentials {
            api_id: FAKE_API_ID,
            api_hash: FAKE_API_HASH.to_owned(),
        },
        2 as DcId,
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

fn make_session() -> (Session, ChannelId, ChannelId, ChatId) {
    let mut session = Session::new(UserId(100));
    let mg_id = ChannelId(3001);
    let bc_id = ChannelId(3002);
    let chat_id = ChatId(4001);

    let mut mg = Channel::new(mg_id);
    mg.set_name("Test Megagroup", "testmg");
    mg.set_usernames(&[
        Username {
            username: "testmg".to_owned(),
            active: true,
            editable: true,
        },
        Username {
            username: "frag_mg".to_owned(),
            active: true,
            editable: false,
        },
    ]);
    mg.access_hash = 0x1020_3040_5060_7080;
    mg.flags = ChannelFlags::MEGAGROUP | ChannelFlags::CREATOR | ChannelFlags::USERNAME;
    session.set_peer_access_hash(PeerId::from_channel(mg_id), mg.access_hash as i64);
    session.upsert_channel(mg, PeerUpdateFlags::empty());

    let mut bc = Channel::new(bc_id);
    bc.set_name("Test Broadcast", "testbc");
    bc.access_hash = 0x0102_0304_0506_0708;
    bc.flags = ChannelFlags::BROADCAST | ChannelFlags::CREATOR | ChannelFlags::USERNAME;
    session.set_peer_access_hash(PeerId::from_channel(bc_id), bc.access_hash as i64);
    session.upsert_channel(bc, PeerUpdateFlags::empty());

    let mut chat = Chat::new(chat_id);
    chat.set_name("Test Basic Chat");
    chat.flags = ChatFlags::CREATOR;
    session.upsert_chat(chat, PeerUpdateFlags::empty());

    (session, mg_id, bc_id, chat_id)
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_000,
        seq: 0,
    })
}

fn tl_channel(id: i64, access_hash: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: false,
        verified: false,
        megagroup: true,
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
        participants_count: Some(12),
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

#[tokio::test]
async fn channels_check_username_wire_bytes_and_error_variants() {
    let server = ScriptedServer::new(|_srv, rpc| {
        if rpc.ctor == tl_fns::channels::CheckUsername::CONSTRUCTOR_ID {
            let req = tl_fns::channels::CheckUsername::from_bytes(&rpc.body).unwrap();
            match req.username.as_str() {
                "good_channel" => ServerAction::ok(&true),
                "taken_chan" => ServerAction::err(400, "USERNAME_OCCUPIED"),
                "too_many_chan" => ServerAction::err(400, "CHANNELS_ADMIN_PUBLIC_TOO_MUCH"),
                "fragment_chan" => ServerAction::err(400, "USERNAME_PURCHASE_AVAILABLE"),
                "na_group_chan" => ServerAction::err(400, "CHANNEL_PUBLIC_GROUP_NA"),
                "bad_chan_name" => ServerAction::err(400, "USERNAME_INVALID"),
                "testmg" => ServerAction::ok(&false),
                _ => ServerAction::ok(&false),
            }
        } else {
            ServerAction::err(400, "UNEXPECTED_RPC")
        }
    });
    let (_client, rpc) = make_mtproto_rpc(&server);
    let (session, mg_id, _bc_id, _chat_id) = make_session();

    tokio::time::timeout(TEST_TIMEOUT, async {
        assert_eq!(
            check_channel_username(&rpc, &session, Some(mg_id), "@good_channel")
                .await
                .unwrap(),
            ChannelUsernameCheckResult::Available
        );
        assert_eq!(
            check_channel_username(&rpc, &session, Some(mg_id), "testmg")
                .await
                .unwrap(),
            ChannelUsernameCheckResult::Available,
            "current editable username must be treated as Available even when server returns false"
        );
        assert_eq!(
            check_channel_username(&rpc, &session, Some(mg_id), "taken_chan")
                .await
                .unwrap(),
            ChannelUsernameCheckResult::Occupied
        );
        assert_eq!(
            check_channel_username(&rpc, &session, Some(mg_id), "too_many_chan")
                .await
                .unwrap(),
            ChannelUsernameCheckResult::TooManyPublicChannels
        );
        assert_eq!(
            check_channel_username(&rpc, &session, Some(mg_id), "fragment_chan")
                .await
                .unwrap(),
            ChannelUsernameCheckResult::PurchaseAvailable
        );
        assert_eq!(
            check_channel_username(&rpc, &session, Some(mg_id), "na_group_chan")
                .await
                .unwrap(),
            ChannelUsernameCheckResult::PublicGroupNotAvailable
        );
        assert_eq!(
            check_channel_username(&rpc, &session, None, "bad_chan_name")
                .await
                .unwrap(),
            ChannelUsernameCheckResult::Invalid
        );
    })
    .await
    .expect("timeout");

    let calls = server.rpcs_of::<tl_fns::channels::CheckUsername>();
    assert_eq!(calls.len(), 7);

    // Exact TL wire bytes check for first call: ctor `0x10e6bd2c`, `InputChannel` ctor `0xf35aec28`,
    // channel_id = 3001, access_hash = 0x1020304050607080, username = "good_channel".
    let expected_first = tl_fns::channels::CheckUsername {
        channel: tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
            channel_id: 3001,
            access_hash: 0x1020_3040_5060_7080,
        }),
        username: "good_channel".to_owned(),
    }
    .to_bytes();
    assert_eq!(calls[0].body, expected_first);

    // Exact TL wire bytes check for `channel_id: None` -> `InputChannel::Empty` (`0xee8c1e86`).
    let expected_empty = tl_fns::channels::CheckUsername {
        channel: tl_enums::InputChannel::Empty,
        username: "bad_chan_name".to_owned(),
    }
    .to_bytes();
    assert_eq!(calls[6].body, expected_empty);
}

#[tokio::test]
async fn all_12_other_channel_management_rpcs_exact_tl_bytes() {
    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        c if c == tl_fns::channels::TogglePreHistoryHidden::CONSTRUCTOR_ID => {
            let req = tl_fns::channels::TogglePreHistoryHidden::from_bytes(&rpc.body).unwrap();
            if req.enabled {
                ServerAction::ok(&empty_updates())
            } else {
                ServerAction::err(400, "CHAT_NOT_MODIFIED")
            }
        }
        c if c == tl_fns::channels::GetGroupsForDiscussion::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
                chats: vec![tl_channel(9901, 7777, "Candidate Group")],
            }),
        ),
        c if c == tl_fns::channels::ConvertToGigagroup::CONSTRUCTOR_ID
            || c == tl_fns::channels::ToggleForum::CONSTRUCTOR_ID
            || c == tl_fns::channels::ToggleParticipantsHidden::CONSTRUCTOR_ID
            || c == tl_fns::channels::ToggleViewForumAsMessages::CONSTRUCTOR_ID
            || c == tl_fns::channels::SetBoostsToUnblockRestrictions::CONSTRUCTOR_ID
            || c == tl_fns::channels::UpdatePaidMessagesPrice::CONSTRUCTOR_ID
            || c == tl_fns::channels::ToggleAutotranslation::CONSTRUCTOR_ID =>
        {
            ServerAction::ok(&empty_updates())
        }
        c if c == tl_fns::channels::DeactivateAllUsernames::CONSTRUCTOR_ID
            || c == tl_fns::channels::SetStickers::CONSTRUCTOR_ID
            || c == tl_fns::channels::SetEmojiStickers::CONSTRUCTOR_ID =>
        {
            ServerAction::ok(&true)
        }
        _ => ServerAction::err(400, "UNEXPECTED_RPC"),
    });

    let (_client, rpc) = make_mtproto_rpc(&server);
    let (mut session, mg_id, bc_id, _chat_id) = make_session();

    let mg_input = tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
        channel_id: 3001,
        access_hash: 0x1020_3040_5060_7080,
    });
    let bc_input = tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
        channel_id: 3002,
        access_hash: 0x0102_0304_0506_0708,
    });

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 2. `channels.togglePreHistoryHidden` (true + CHAT_NOT_MODIFIED on false).
        assert!(
            toggle_pre_history_hidden(&rpc, &session, mg_id, true)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            toggle_pre_history_hidden(&rpc, &session, mg_id, false)
                .await
                .unwrap()
                .is_none()
        );

        // 3. `channels.getGroupsForDiscussion`.
        let groups = get_groups_for_discussion(&rpc, &mut session).await.unwrap();
        assert_eq!(groups, vec![PeerId::from_channel(ChannelId(9901))]);
        assert_eq!(
            session.channel(ChannelId(9901)).unwrap().common.name,
            "Candidate Group"
        );

        // 4. `channels.convertToGigagroup`.
        assert!(
            convert_to_gigagroup(&rpc, &session, mg_id)
                .await
                .unwrap()
                .is_some()
        );

        // 5. `channels.deactivateAllUsernames`.
        assert!(
            deactivate_all_usernames(&rpc, &mut session, mg_id)
                .await
                .unwrap()
        );
        assert_eq!(
            session.channel(mg_id).unwrap().usernames.usernames(),
            &["testmg".to_owned()]
        );

        // 6. `channels.toggleForum` (enable with tabs, then disable forcing tabs & sidebar false).
        assert!(
            toggle_forum(&rpc, &session, mg_id, true, true, false)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            toggle_forum(&rpc, &session, mg_id, false, true, true)
                .await
                .unwrap()
                .is_some()
        );

        // 7. `channels.toggleParticipantsHidden`.
        assert!(
            toggle_participants_hidden(&rpc, &session, mg_id, true)
                .await
                .unwrap()
                .is_some()
        );

        // 8. `channels.toggleViewForumAsMessages`.
        assert!(
            toggle_view_forum_as_messages(&rpc, &session, mg_id, true)
                .await
                .unwrap()
                .is_some()
        );

        // 9. `channels.setBoostsToUnblockRestrictions`.
        assert!(
            set_boosts_to_unblock_restrictions(&rpc, &session, mg_id, 5)
                .await
                .unwrap()
                .is_some()
        );

        // 10. `channels.setStickers` (ID + Empty).
        assert!(
            set_stickers(
                &rpc,
                &session,
                mg_id,
                &ChannelStickerSetInput::Id {
                    id: 700,
                    access_hash: 701,
                }
            )
            .await
            .unwrap()
        );
        assert!(
            set_stickers(&rpc, &session, mg_id, &ChannelStickerSetInput::Empty)
                .await
                .unwrap()
        );

        // 11. `channels.setEmojiStickers` (ShortName).
        assert!(
            set_emoji_stickers(
                &rpc,
                &session,
                mg_id,
                &ChannelStickerSetInput::ShortName("pack_slug".to_owned())
            )
            .await
            .unwrap()
        );

        // 12. `channels.updatePaidMessagesPrice` (broadcast with flag bit 0, megagroup forced 0).
        assert!(
            update_paid_messages_price(&rpc, &session, bc_id, true, 50)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            update_paid_messages_price(&rpc, &session, mg_id, true, 15)
                .await
                .unwrap()
                .is_some()
        );

        // 13. `channels.toggleAutotranslation`.
        assert!(
            toggle_autotranslation(&rpc, &session, bc_id, true)
                .await
                .unwrap()
                .is_some()
        );
    })
    .await
    .expect("timeout");

    // Assert exact TL wire bytes for every single RPC sent over MTProto 2.0.
    let pre_calls = server.rpcs_of::<tl_fns::channels::TogglePreHistoryHidden>();
    assert_eq!(pre_calls.len(), 2);
    assert_eq!(
        pre_calls[0].body,
        tl_fns::channels::TogglePreHistoryHidden {
            channel: mg_input.clone(),
            enabled: true,
        }
        .to_bytes()
    );
    assert_eq!(
        u32::from_le_bytes(
            pre_calls[0].body[pre_calls[0].body.len() - 4..]
                .try_into()
                .unwrap()
        ),
        BOOL_TRUE_ID
    );
    assert_eq!(
        u32::from_le_bytes(
            pre_calls[1].body[pre_calls[1].body.len() - 4..]
                .try_into()
                .unwrap()
        ),
        BOOL_FALSE_ID
    );

    let disc_calls = server.rpcs_of::<tl_fns::channels::GetGroupsForDiscussion>();
    assert_eq!(disc_calls.len(), 1);
    assert_eq!(disc_calls[0].body, 0xf5da_d378u32.to_le_bytes());

    let giga_calls = server.rpcs_of::<tl_fns::channels::ConvertToGigagroup>();
    assert_eq!(giga_calls.len(), 1);
    assert_eq!(
        giga_calls[0].body,
        tl_fns::channels::ConvertToGigagroup {
            channel: mg_input.clone(),
        }
        .to_bytes()
    );

    let deact_calls = server.rpcs_of::<tl_fns::channels::DeactivateAllUsernames>();
    assert_eq!(deact_calls.len(), 1);
    assert_eq!(
        deact_calls[0].body,
        tl_fns::channels::DeactivateAllUsernames {
            channel: mg_input.clone(),
        }
        .to_bytes()
    );

    let forum_calls = server.rpcs_of::<tl_fns::channels::ToggleForum>();
    assert_eq!(forum_calls.len(), 2);
    assert_eq!(
        forum_calls[0].body,
        tl_fns::channels::ToggleForum {
            channel: mg_input.clone(),
            enabled: true,
            tabs: true,
            sidebar: false,
        }
        .to_bytes()
    );
    assert_eq!(
        forum_calls[1].body,
        tl_fns::channels::ToggleForum {
            channel: mg_input.clone(),
            enabled: false,
            tabs: false,
            sidebar: false,
        }
        .to_bytes()
    );

    let part_calls = server.rpcs_of::<tl_fns::channels::ToggleParticipantsHidden>();
    assert_eq!(part_calls.len(), 1);
    assert_eq!(
        part_calls[0].body,
        tl_fns::channels::ToggleParticipantsHidden {
            channel: mg_input.clone(),
            enabled: true,
        }
        .to_bytes()
    );

    let view_calls = server.rpcs_of::<tl_fns::channels::ToggleViewForumAsMessages>();
    assert_eq!(view_calls.len(), 1);
    assert_eq!(
        view_calls[0].body,
        tl_fns::channels::ToggleViewForumAsMessages {
            channel: mg_input.clone(),
            enabled: true,
        }
        .to_bytes()
    );

    let boost_calls = server.rpcs_of::<tl_fns::channels::SetBoostsToUnblockRestrictions>();
    assert_eq!(boost_calls.len(), 1);
    assert_eq!(
        boost_calls[0].body,
        tl_fns::channels::SetBoostsToUnblockRestrictions {
            channel: mg_input.clone(),
            boosts: 5,
        }
        .to_bytes()
    );

    let sticker_calls = server.rpcs_of::<tl_fns::channels::SetStickers>();
    assert_eq!(sticker_calls.len(), 2);
    assert_eq!(
        sticker_calls[0].body,
        tl_fns::channels::SetStickers {
            channel: mg_input.clone(),
            stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: 700,
                access_hash: 701,
            }),
        }
        .to_bytes()
    );
    assert_eq!(
        sticker_calls[1].body,
        tl_fns::channels::SetStickers {
            channel: mg_input.clone(),
            stickerset: tl_enums::InputStickerSet::Empty,
        }
        .to_bytes()
    );

    let emoji_calls = server.rpcs_of::<tl_fns::channels::SetEmojiStickers>();
    assert_eq!(emoji_calls.len(), 1);
    assert_eq!(
        emoji_calls[0].body,
        tl_fns::channels::SetEmojiStickers {
            channel: mg_input.clone(),
            stickerset: tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                short_name: "pack_slug".to_owned(),
            }),
        }
        .to_bytes()
    );

    let price_calls = server.rpcs_of::<tl_fns::channels::UpdatePaidMessagesPrice>();
    assert_eq!(price_calls.len(), 2);
    assert_eq!(
        price_calls[0].body,
        tl_fns::channels::UpdatePaidMessagesPrice {
            broadcast_messages_allowed: true,
            channel: bc_input.clone(),
            send_paid_messages_stars: 50,
        }
        .to_bytes()
    );
    // flags word at bytes [4..8] must be 1 for broadcast, 0 for megagroup.
    assert_eq!(
        u32::from_le_bytes(price_calls[0].body[4..8].try_into().unwrap()),
        1
    );
    assert_eq!(
        u32::from_le_bytes(price_calls[1].body[4..8].try_into().unwrap()),
        0
    );
    assert_eq!(
        price_calls[1].body,
        tl_fns::channels::UpdatePaidMessagesPrice {
            broadcast_messages_allowed: false,
            channel: mg_input,
            send_paid_messages_stars: 15,
        }
        .to_bytes()
    );

    let auto_calls = server.rpcs_of::<tl_fns::channels::ToggleAutotranslation>();
    assert_eq!(auto_calls.len(), 1);
    assert_eq!(
        auto_calls[0].body,
        tl_fns::channels::ToggleAutotranslation {
            channel: bc_input,
            enabled: true,
        }
        .to_bytes()
    );
}

#[tokio::test]
async fn messages_delete_chat_and_get_admins_with_invites_exact_tl_bytes() {
    let delete_attempts = Arc::new(AtomicU64::new(0));
    let attempts_clone = Arc::clone(&delete_attempts);
    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::messages::DeleteChat::CONSTRUCTOR_ID => {
            let attempt = attempts_clone.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                ServerAction::err(400, "PEER_ID_INVALID")
            } else {
                ServerAction::ok(&true)
            }
        }
        c if c == tl_fns::messages::AddChatUser::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::InvitedUsers::InvitedUsers(tl_types::messages::InvitedUsers {
                updates: empty_updates(),
                missing_invitees: Vec::new(),
            }),
        ),
        c if c == tl_fns::messages::GetAdminsWithInvites::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::ChatAdminsWithInvites::ChatAdminsWithInvites(
                tl_types::messages::ChatAdminsWithInvites {
                    admins: vec![tl_enums::ChatAdminWithInvites::ChatAdminWithInvites(
                        tl_types::ChatAdminWithInvites {
                            admin_id: 404,
                            invites_count: 7,
                            revoked_invites_count: 2,
                        },
                    )],
                    users: vec![tl_enums::User::User(tl_types::User {
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
                        id: 404,
                        access_hash: Some(404_001),
                        first_name: Some("Admin Beta".to_owned()),
                        last_name: None,
                        username: Some("admin_beta".to_owned()),
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
                    })],
                },
            ),
        ),
        _ => ServerAction::err(400, "UNEXPECTED_RPC"),
    });

    let (_client, rpc) = make_mtproto_rpc(&server);
    let (mut session, mg_id, _bc_id, chat_id) = make_session();

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 14. `messages.deleteChat` with `PEER_ID_INVALID` auto-join + retry.
        let outcome = delete_chat(&rpc, chat_id).await.unwrap();
        assert!(matches!(
            outcome,
            DeleteChatOutcome::JoinedAndDeleted { .. }
        ));

        // 15. `messages.getAdminsWithInvites`.
        let admins = get_admins_with_invites(&rpc, &mut session, PeerId::from_channel(mg_id))
            .await
            .unwrap();
        assert_eq!(admins.len(), 1);
        assert_eq!(admins[0].admin_id, UserId(404));
        assert_eq!(admins[0].invites_count, 7);
        assert_eq!(admins[0].revoked_invites_count, 2);
        assert_eq!(
            session.user(UserId(404)).unwrap().usernames.username(),
            "admin_beta"
        );
    })
    .await
    .expect("timeout");

    let all = server.all_rpcs();
    assert_eq!(all.len(), 4);
    assert_eq!(
        all[0].body,
        tl_fns::messages::DeleteChat { chat_id: 4001 }.to_bytes()
    );
    assert_eq!(
        all[1].body,
        tl_fns::messages::AddChatUser {
            chat_id: 4001,
            user_id: tl_enums::InputUser::Self_,
            fwd_limit: 0,
        }
        .to_bytes()
    );
    assert_eq!(
        all[2].body,
        tl_fns::messages::DeleteChat { chat_id: 4001 }.to_bytes()
    );
    assert_eq!(
        all[3].body,
        tl_fns::messages::GetAdminsWithInvites {
            peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                channel_id: 3001,
                access_hash: 0x1020_3040_5060_7080,
            }),
        }
        .to_bytes()
    );
}
