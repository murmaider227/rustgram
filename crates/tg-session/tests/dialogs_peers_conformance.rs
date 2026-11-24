//! Byte-level tdesktop conformance tests for chat list / dialogs loading,
//! peers, contacts, privacy rules, peer cache persistence, and 64-bit hashes
//! (`tg-data`, `tg-api`, `tg-storage`, `tg-session`) against an in-process
//! scripted fake Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses obviously fake credentials (`api_id = 1`, `api_hash = "0000...0000"`),
//!   synthetic peer/user IDs, synthetic names, and an ephemeral test-only RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::convert::{
    apply_tl_chat, apply_tl_dialog_in_folder, apply_tl_message, apply_tl_peer_dialogs_for_folder,
    apply_tl_user, apply_tl_users_and_chats, input_channel_for, input_peer_for, input_user_for,
};
use tg_api::{
    ApiCredentials, BLOCKED_FIRST_SLICE, BLOCKED_PER_PAGE, BlockedPeersManager,
    ContactsFetchResult, ContactsManager, DIALOGS_FIRST_LOAD, DIALOGS_PER_PAGE, DialogsLoader,
    PrivacyExceptions, PrivacyKey, PrivacyOption, PrivacyRule, UpdateEvent, UpdatesHandler,
    UserPrivacyManager, count_contacts_hash, count_hash_i32, count_hash_i64, count_hash_u64,
    hash_finalize, hash_init, hash_update_i32, hash_update_i64, hash_update_u32, hash_update_u64,
    privacy_key_from_tl, privacy_key_to_tl, privacy_rules_from_tl, privacy_rules_to_tl,
    resolve_username,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    ARCHIVE_FOLDER_ID, BlockStatus, ChannelFlags, ChannelId, ChatBannedRights, ChatFilterFlags,
    ChatFlags, ChatId, ContactStatus, DialogKey, DocumentId, FullMsgId, MsgId, PeerId,
    PersistedUpdatesState, PhotoId, Session, UserFlags, UserId,
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
use tg_storage::APP_VERSION;
use tg_storage::account::{
    AccountStorage, PeerTrustFlags, RecentHashtagsAndBots, TrustedPeersData,
};
use tg_storage::peer_id::PeerId as StoragePeerId;
use tg_storage::qdatastream::{QDataStreamReader, QDataStreamWriter};
use tg_storage::serialize::image_location::ImageLocation;
use tg_storage::serialize::peer::{
    PEER_SERIALIZE_VERSION, PEER_SERIALIZE_VERSION_TAG, SerializedChannel, SerializedChat,
    SerializedPeer, SerializedPeerKind, SerializedUser,
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

    fn bool_true() -> Self {
        Self::Reply(BOOL_TRUE_ID.to_le_bytes().to_vec())
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
    #[allow(dead_code)]
    dc_id: DcId,
    #[allow(dead_code)]
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

// ============================================================================
// Synthetic TL Fixture Builders (Layer 233)
// ============================================================================

fn tl_user_base(id: i64, access_hash: i64, first: &str, min: bool) -> tl_types::User {
    tl_types::User {
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
    }
}

fn tl_user(id: i64, access_hash: i64, first: &str, min: bool) -> tl_enums::User {
    tl_enums::User::User(tl_user_base(id, access_hash, first, min))
}

fn tl_self_user(id: i64, access_hash: i64) -> tl_enums::User {
    let mut u = tl_user_base(id, access_hash, "Self Account", false);
    u.self_ = true;
    u.contact = false;
    u.mutual_contact = false;
    tl_enums::User::User(u)
}

fn tl_chat_base(id: i64, title: &str) -> tl_types::Chat {
    tl_types::Chat {
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
    }
}

fn tl_chat(id: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Chat(tl_chat_base(id, title))
}

fn tl_channel_base(
    id: i64,
    access_hash: i64,
    title: &str,
    broadcast: bool,
    min: bool,
) -> tl_types::Channel {
    tl_types::Channel {
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
    }
}

fn tl_channel(
    id: i64,
    access_hash: i64,
    title: &str,
    broadcast: bool,
    min: bool,
) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_channel_base(id, access_hash, title, broadcast, min))
}

fn tl_notify_settings() -> tl_enums::PeerNotifySettings {
    tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
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
    })
}

fn tl_dialog(
    peer: tl_enums::Peer,
    top_message: i32,
    read_inbox_max_id: i32,
    read_outbox_max_id: i32,
    unread_count: i32,
    pinned: bool,
    folder_id: Option<i32>,
) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned,
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
        notify_settings: tl_notify_settings(),
        pts: None,
        draft: None,
        folder_id,
        ttl_period: None,
    })
}

fn tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from: Option<tl_enums::Peer>,
    out: bool,
    date: i32,
    text: &str,
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

fn tl_state(pts: i32, qts: i32, date: i32, seq: i32) -> tl_enums::updates::State {
    tl_enums::updates::State::State(tl_types::updates::State {
        pts,
        qts,
        date,
        seq,
        unread_count: 0,
    })
}

fn tl_peer_user(user_id: i64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser { user_id })
}

fn tl_peer_chat(chat_id: i64) -> tl_enums::Peer {
    tl_enums::Peer::Chat(tl_types::PeerChat { chat_id })
}

fn tl_peer_channel(channel_id: i64) -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id })
}

fn tl_text(s: &str) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: s.to_owned(),
        entities: vec![],
    })
}

fn tl_profile_photo(photo_id: i64, personal: bool) -> tl_enums::UserProfilePhoto {
    tl_enums::UserProfilePhoto::UserProfilePhoto(tl_types::UserProfilePhoto {
        has_video: false,
        personal,
        photo_id,
        stripped_thumb: None,
        dc_id: 2,
    })
}

fn init_updates(pts: i32, qts: i32, date: i32, seq: i32) -> UpdatesHandler {
    let mut updates = UpdatesHandler::new();
    updates.restore_state(
        &PersistedUpdatesState {
            pts,
            qts,
            date,
            seq,
            channel_pts: Default::default(),
        },
        1_000,
    );
    updates
}

async fn apply_updates(
    updates: &mut UpdatesHandler,
    rpc: &MtprotoRpc,
    session: &mut Session,
    upd: tl_enums::Updates,
) -> Vec<UpdateEvent> {
    tokio::time::timeout(
        TEST_TIMEOUT,
        updates.handle_updates(rpc, session, upd, None, 2_000),
    )
    .await
    .expect("handle_updates timeout")
    .expect("handle_updates rpc");
    updates.drain_events()
}

// ============================================================================
// 1. Chat list loading, pagination, pinned order, folders, filters
// ============================================================================

#[tokio::test]
async fn dialogs_pagination_offset_pinned_order_and_end_of_list_conformance() {
    let page_calls = Arc::new(AtomicUsize::new(0));
    let page_calls_srv = Arc::clone(&page_calls);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetDialogs::from_bytes(&rpc.body).unwrap();
            let call = page_calls_srv.fetch_add(1, Ordering::SeqCst);
            assert!(
                req.exclude_pinned,
                "getDialogs must set exclude_pinned=true"
            );
            assert_eq!(req.folder_id, Some(0), "main list passes folder_id=Some(0)");
            assert_eq!(req.hash, 0, "getDialogs always sends hash=0 like tdesktop");

            if call == 0 {
                assert_eq!(req.limit, DIALOGS_FIRST_LOAD);
                assert_eq!(req.offset_date, 0);
                assert_eq!(req.offset_id, 0);
                assert!(matches!(req.offset_peer, tl_enums::InputPeer::Empty));

                // Page 1: regular dialog (User 101), regular Self dialog (User 1),
                // plus trailing Dialog::Folder(archive=1) which MUST be skipped
                // when computing next pagination offset (`apiwrap.cpp:1075-1103`).
                let resp = tl_enums::messages::Dialogs::Slice(tl_types::messages::DialogsSlice {
                    count: 3,
                    dialogs: vec![
                        tl_dialog(tl_peer_user(101), 50, 48, 50, 2, false, Some(0)),
                        tl_dialog(tl_peer_user(1), 40, 40, 40, 0, false, Some(0)),
                        tl_enums::Dialog::Folder(tl_types::DialogFolder {
                            pinned: true,
                            folder: tl_enums::Folder::Folder(tl_types::Folder {
                                autofill_new_broadcasts: false,
                                autofill_public_groups: false,
                                autofill_new_correspondents: false,
                                id: ARCHIVE_FOLDER_ID,
                                title: "Archive".to_owned(),
                                photo: None,
                            }),
                            peer: tl_peer_user(301),
                            top_message: 999,
                            unread_muted_peers_count: 1,
                            unread_unmuted_peers_count: 2,
                            unread_muted_messages_count: 3,
                            unread_unmuted_messages_count: 4,
                        }),
                    ],
                    messages: vec![
                        tl_message(
                            50,
                            tl_peer_user(101),
                            Some(tl_peer_user(101)),
                            false,
                            1_700_000_500,
                            "msg 50",
                        ),
                        tl_message(
                            40,
                            tl_peer_user(1),
                            Some(tl_peer_user(1)),
                            true,
                            1_700_000_400,
                            "saved note",
                        ),
                        tl_message(
                            999,
                            tl_peer_user(301),
                            Some(tl_peer_user(301)),
                            false,
                            1_700_000_999,
                            "archived top",
                        ),
                    ],
                    chats: vec![],
                    users: vec![
                        tl_self_user(1, 0x1111),
                        tl_user(101, 0x1010, "Peer One", false),
                        tl_user(301, 0x3010, "Peer Archived", false),
                    ],
                });
                ServerAction::ok(&resp)
            } else if call == 1 {
                // Page 2 MUST use limit=500 and offset from Self dialog (NOT Dialog::Folder!),
                // with InputPeer::Self_ (`data/data_peer.cpp:2090-2092`).
                assert_eq!(req.limit, DIALOGS_PER_PAGE);
                assert_eq!(req.offset_date, 1_700_000_400);
                assert_eq!(req.offset_id, 40);
                assert!(
                    matches!(req.offset_peer, tl_enums::InputPeer::Self_),
                    "self offset peer must serialize as InputPeer::Self_, got {:?}",
                    req.offset_peer
                );

                // Final page returns `messages.dialogs` (end-of-list).
                let resp = tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                    dialogs: vec![tl_dialog(tl_peer_user(102), 30, 30, 30, 0, false, Some(0))],
                    messages: vec![tl_message(
                        30,
                        tl_peer_user(102),
                        Some(tl_peer_user(102)),
                        false,
                        1_700_000_300,
                        "msg 30",
                    )],
                    chats: vec![],
                    users: vec![tl_user(102, 0x1020, "Peer Two", false)],
                });
                ServerAction::ok(&resp)
            } else {
                panic!("unexpected 3rd getDialogs call after messages.dialogs end-of-list");
            }
        }
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetPinnedDialogs::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.folder_id, 0);
            // Server sends pinned dialogs in order [201, 202, 203].
            let resp =
                tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                    dialogs: vec![
                        tl_dialog(tl_peer_user(201), 10, 10, 10, 0, true, Some(0)),
                        tl_dialog(tl_peer_user(202), 11, 11, 11, 0, true, Some(0)),
                        tl_dialog(tl_peer_user(203), 12, 12, 12, 0, true, Some(0)),
                    ],
                    messages: vec![
                        tl_message(
                            10,
                            tl_peer_user(201),
                            Some(tl_peer_user(201)),
                            false,
                            1_700_000_100,
                            "pin 1",
                        ),
                        tl_message(
                            11,
                            tl_peer_user(202),
                            Some(tl_peer_user(202)),
                            false,
                            1_700_000_110,
                            "pin 2",
                        ),
                        tl_message(
                            12,
                            tl_peer_user(203),
                            Some(tl_peer_user(203)),
                            false,
                            1_700_000_120,
                            "pin 3",
                        ),
                    ],
                    chats: vec![],
                    users: vec![
                        tl_user(201, 0x2010, "Pin Alpha", false),
                        tl_user(202, 0x2020, "Pin Beta", false),
                        tl_user(203, 0x2030, "Pin Gamma", false),
                    ],
                    state: tl_state(10, 0, 1_700_000_500, 1),
                });
            ServerAction::ok(&resp)
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut loader = DialogsLoader::new();

    // Load page 1 (+ pinned dialogs).
    let page1 = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_page(&rpc, 0))
        .await
        .expect("page1 timeout")
        .expect("page1 rpc")
        .expect("page1 present");
    assert!(!page1.folder_done);
    page1.apply_to_session(&mut session, 1_700_000_600);

    // Pinned order MUST match exact server order [201, 202, 203] (`data/data_session.cpp:2801-2810`).
    let pinned_order: Vec<PeerId> = session
        .main_list()
        .pinned()
        .order()
        .iter()
        .filter_map(|k| match k {
            DialogKey::History(pid) => Some(*pid),
            _ => None,
        })
        .collect();
    assert_eq!(
        pinned_order,
        vec![
            PeerId::from_user(UserId(201)),
            PeerId::from_user(UserId(202)),
            PeerId::from_user(UserId(203)),
        ]
    );

    // Archive folder entry from Dialog::Folder must be populated with cloud unread counts.
    let arch_unread = session.archive().unread_state();
    assert_eq!(arch_unread.messages, 7); // 3 muted + 4 unmuted
    assert_eq!(arch_unread.chats, 3); // 1 muted + 2 unmuted
    assert!(
        session
            .main_list()
            .indexed()
            .contains(DialogKey::Folder(ARCHIVE_FOLDER_ID))
    );

    // Load page 2 (end-of-list).
    let page2 = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_page(&rpc, 0))
        .await
        .expect("page2 timeout")
        .expect("page2 rpc")
        .expect("page2 present");
    assert!(page2.folder_done);
    page2.apply_to_session(&mut session, 1_700_000_600);
    assert!(session.main_list().loaded());

    // 3rd call must be a no-op (no extra RPCs).
    let page3 = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_page(&rpc, 0))
        .await
        .expect("page3 timeout")
        .expect("page3 rpc");
    assert!(page3.is_none());
    assert_eq!(server.rpcs_of::<tl_fns::messages::GetDialogs>().len(), 2);
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetPinnedDialogs>().len(),
        1
    );

    // Stale read_inbox_max_id protection (`history/history.cpp:3676-3680`):
    // Peer 101 currently has inbox_read_before = 49 (read_inbox_max_id=48) and unread_count = 2.
    let p101 = PeerId::from_user(UserId(101));
    assert_eq!(session.history(p101).unwrap().unread_count, Some(2));
    assert_eq!(
        session.history(p101).unwrap().inbox_read_before,
        Some(MsgId(49))
    );
    // Re-applying an older snapshot with read_inbox_max_id=45 and unread_count=5 must NOT regress it.
    let stale_dialog = tl_dialog(tl_peer_user(101), 50, 45, 50, 5, false, Some(0));
    apply_tl_dialog_in_folder(&mut session, &stale_dialog, Some(0), 1_700_000_600);
    assert_eq!(session.history(p101).unwrap().unread_count, Some(2));
    assert_eq!(
        session.history(p101).unwrap().inbox_read_before,
        Some(MsgId(49))
    );
}

#[tokio::test]
async fn dialogs_archive_folder_inheritance_stale_pins_empty_slice_and_not_modified_conformance() {
    let archive_calls = Arc::new(AtomicUsize::new(0));
    let archive_calls_srv = Arc::clone(&archive_calls);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetDialogs::from_bytes(&rpc.body).unwrap();
            if req.folder_id == Some(ARCHIVE_FOLDER_ID) {
                let c = archive_calls_srv.fetch_add(1, Ordering::SeqCst);
                if c == 0 {
                    // Archive dialog intentionally omits `folder_id: None` — must inherit folder 1!
                    let resp =
                        tl_enums::messages::Dialogs::Slice(tl_types::messages::DialogsSlice {
                            count: 1,
                            dialogs: vec![tl_dialog(tl_peer_user(401), 70, 70, 70, 0, false, None)],
                            messages: vec![tl_message(
                                70,
                                tl_peer_user(401),
                                Some(tl_peer_user(401)),
                                false,
                                1_700_000_700,
                                "arch 1",
                            )],
                            chats: vec![],
                            users: vec![tl_user(401, 0x4010, "Arch Peer", false)],
                        });
                    ServerAction::ok(&resp)
                } else {
                    // Empty slice terminates pagination cleanly (`apiwrap.cpp:1097-1103`).
                    let resp =
                        tl_enums::messages::Dialogs::Slice(tl_types::messages::DialogsSlice {
                            count: 1,
                            dialogs: vec![],
                            messages: vec![],
                            chats: vec![],
                            users: vec![],
                        });
                    ServerAction::ok(&resp)
                }
            } else {
                // Main list returns `messages.dialogsNotModified`.
                let resp = tl_enums::messages::Dialogs::NotModified(
                    tl_types::messages::DialogsNotModified { count: 7 },
                );
                ServerAction::ok(&resp)
            }
        }
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetPinnedDialogs::from_bytes(&rpc.body).unwrap();
            if req.folder_id == ARCHIVE_FOLDER_ID {
                // Pinned archive dialog also omits `folder_id: None` — must land in folder 1.
                let resp =
                    tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                        dialogs: vec![tl_dialog(tl_peer_user(402), 71, 71, 71, 0, true, None)],
                        messages: vec![tl_message(
                            71,
                            tl_peer_user(402),
                            Some(tl_peer_user(402)),
                            false,
                            1_700_000_710,
                            "arch pin",
                        )],
                        chats: vec![],
                        users: vec![tl_user(402, 0x4020, "Arch Pin", false)],
                        state: tl_state(10, 0, 1_700_000_710, 1),
                    });
                ServerAction::ok(&resp)
            } else {
                let resp =
                    tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                        dialogs: vec![],
                        messages: vec![],
                        chats: vec![],
                        users: vec![],
                        state: tl_state(10, 0, 1_700_000_710, 1),
                    });
                ServerAction::ok(&resp)
            }
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut loader = DialogsLoader::new();

    // Seed a stale pinned peer in archive folder 1 to verify clearPinnedChats on reload.
    let stale_pid = PeerId::from_user(UserId(499));
    apply_tl_user(&mut session, &tl_user(499, 0x4990, "Stale Arch Pin", false));
    session.set_history_folder(stale_pid, Some(ARCHIVE_FOLDER_ID));
    session.set_pinned_in_folder(DialogKey::History(stale_pid), true, true);
    assert_eq!(
        session.archive().chats_list.pinned().order(),
        &[DialogKey::History(stale_pid)]
    );

    // Page 1 of archive folder.
    let p1 = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_page(&rpc, ARCHIVE_FOLDER_ID))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    p1.apply_to_session(&mut session, 1_700_000_720);

    let p401 = PeerId::from_user(UserId(401));
    let p402 = PeerId::from_user(UserId(402));
    assert_eq!(
        session.history(p401).unwrap().folder,
        Some(Some(ARCHIVE_FOLDER_ID))
    );
    assert_eq!(
        session.history(p402).unwrap().folder,
        Some(Some(ARCHIVE_FOLDER_ID))
    );
    // Stale pin 499 MUST have been cleared and replaced by server pin 402.
    assert_eq!(
        session.archive().chats_list.pinned().order(),
        &[DialogKey::History(p402)]
    );

    // Page 2 returns empty slice -> finishes archive folder cleanly.
    let p2 = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_page(&rpc, ARCHIVE_FOLDER_ID))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(p2.folder_done);
    p2.apply_to_session(&mut session, 1_700_000_720);
    assert!(session.archive().chats_list.loaded());

    // Main folder receives `messages.dialogsNotModified(count = 7)` -> finishes cleanly.
    let main_nm = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_page(&rpc, 0))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(main_nm.folder_done);
    assert_eq!(main_nm.total_count, 7);
    main_nm.apply_to_session(&mut session, 1_700_000_720);
    assert!(session.main_list().loaded());
}

#[tokio::test]
async fn dialogs_updates_folder_peers_pinned_unread_and_read_history_conformance() {
    let pinned_refetches = Arc::new(AtomicUsize::new(0));
    let pinned_refetches_srv = Arc::clone(&pinned_refetches);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetPinnedDialogs::from_bytes(&rpc.body).unwrap();
            let idx = pinned_refetches_srv.fetch_add(1, Ordering::SeqCst);
            if req.folder_id == 0 {
                // Return [502, 501] on refetch.
                let resp =
                    tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                        dialogs: vec![
                            tl_dialog(tl_peer_user(502), 20, 20, 20, 0, true, Some(0)),
                            tl_dialog(tl_peer_user(501), 10, 5, 4, 5, true, Some(0)),
                        ],
                        messages: vec![
                            tl_message(
                                20,
                                tl_peer_user(502),
                                Some(tl_peer_user(502)),
                                false,
                                1_700_000_200,
                                "m20",
                            ),
                            tl_message(
                                10,
                                tl_peer_user(501),
                                Some(tl_peer_user(501)),
                                false,
                                1_700_000_100,
                                "m10",
                            ),
                        ],
                        chats: vec![],
                        users: vec![
                            tl_user(501, 0x5010, "Peer 501", false),
                            tl_user(502, 0x5020, "Peer 502", false),
                        ],
                        state: tl_state(20 + idx as i32, 0, 1_700_000_300, 1),
                    });
                ServerAction::ok(&resp)
            } else {
                let resp =
                    tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                        dialogs: vec![tl_dialog(tl_peer_user(503), 30, 30, 30, 0, true, Some(1))],
                        messages: vec![tl_message(
                            30,
                            tl_peer_user(503),
                            Some(tl_peer_user(503)),
                            false,
                            1_700_000_300,
                            "m30",
                        )],
                        chats: vec![],
                        users: vec![tl_user(503, 0x5030, "Peer 503", false)],
                        state: tl_state(25, 0, 1_700_000_300, 1),
                    });
                ServerAction::ok(&resp)
            }
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut updates = init_updates(10, 0, 1_700_000_100, 1);

    // Seed peers 501, 502, and channel 700.
    apply_tl_users_and_chats(
        &mut session,
        &[
            tl_user(501, 0x5010, "Peer 501", false),
            tl_user(502, 0x5020, "Peer 502", false),
        ],
        &[tl_channel(700, 0x7000, "Chan 700", true, false)],
    );
    let p501 = PeerId::from_user(UserId(501));
    let p502 = PeerId::from_user(UserId(502));
    let ch700 = PeerId::from_channel(ChannelId(700));
    apply_tl_dialog_in_folder(
        &mut session,
        &tl_dialog(tl_peer_user(501), 10, 5, 4, 5, true, Some(0)),
        Some(0),
        1_700_000_100,
    );
    apply_tl_dialog_in_folder(
        &mut session,
        &tl_dialog(tl_peer_user(502), 20, 20, 20, 0, false, Some(0)),
        Some(0),
        1_700_000_100,
    );
    apply_tl_dialog_in_folder(
        &mut session,
        &tl_dialog(tl_peer_channel(700), 100, 90, 90, 10, false, Some(0)),
        Some(0),
        1_700_000_100,
    );
    assert_eq!(
        session.main_list().pinned().order(),
        &[DialogKey::History(p501)]
    );

    // 1) updateFolderPeers: move pinned p501 to archive (folder 1) + ignore unloaded peer 599.
    let upd_folder = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::FolderPeers(tl_types::UpdateFolderPeers {
            folder_peers: vec![
                tl_enums::FolderPeer::FolderPeer(tl_types::FolderPeer {
                    peer: tl_peer_user(501),
                    folder_id: ARCHIVE_FOLDER_ID,
                }),
                tl_enums::FolderPeer::FolderPeer(tl_types::FolderPeer {
                    peer: tl_peer_user(599),
                    folder_id: ARCHIVE_FOLDER_ID,
                }),
            ],
            pts: 11,
            pts_count: 1,
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_105,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_folder).await;
    // Moving p501 to archive MUST unpin it from main list (`history/history.cpp:2528-2534`).
    assert!(session.main_list().pinned().order().is_empty());
    assert_eq!(
        session.history(p501).unwrap().folder,
        Some(Some(ARCHIVE_FOLDER_ID))
    );
    assert!(session.history(PeerId::from_user(UserId(599))).is_none());

    // 2) updateDialogPinned: pin p501 inside archive folder 1 and p502 in main (zero RPCs).
    let upd_pin = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::DialogPinned(tl_types::UpdateDialogPinned {
                pinned: true,
                folder_id: Some(ARCHIVE_FOLDER_ID),
                peer: tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                    peer: tl_peer_user(501),
                }),
            }),
            tl_enums::Update::DialogPinned(tl_types::UpdateDialogPinned {
                pinned: true,
                folder_id: None,
                peer: tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                    peer: tl_peer_user(502),
                }),
            }),
        ],
        users: vec![],
        chats: vec![],
        date: 1_700_000_110,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_pin).await;
    assert_eq!(
        session.archive().chats_list.pinned().order(),
        &[DialogKey::History(p501)]
    );
    assert_eq!(
        session.main_list().pinned().order(),
        &[DialogKey::History(p502)]
    );
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetPinnedDialogs>().len(),
        0
    );

    // 3) updatePinnedDialogs WITH order: moves p501 back to main & sets exact order [p501, p502] without RPC.
    let upd_pins_with_order = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::PinnedDialogs(
            tl_types::UpdatePinnedDialogs {
                folder_id: None,
                order: Some(vec![
                    tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                        peer: tl_peer_user(501),
                    }),
                    tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                        peer: tl_peer_user(502),
                    }),
                ]),
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_115,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_pins_with_order).await;
    assert_eq!(
        session.main_list().pinned().order(),
        &[DialogKey::History(p501), DialogKey::History(p502)]
    );
    assert_eq!(session.history(p501).unwrap().folder, Some(None));
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetPinnedDialogs>().len(),
        0
    );

    // 4) updatePinnedDialogs WITHOUT order (order: None) MUST trigger messages.getPinnedDialogs(0)!
    let upd_pins_no_order = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::PinnedDialogs(
            tl_types::UpdatePinnedDialogs {
                folder_id: None,
                order: None,
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_120,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_pins_no_order).await;
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetPinnedDialogs>().len(),
        1
    );
    assert_eq!(
        session.main_list().pinned().order(),
        &[DialogKey::History(p502), DialogKey::History(p501)]
    );

    // 5) updateDialogPinned for an UNLOADED peer (503 in archive) OR a nested folder in archive
    // both return `done = false` and trigger `messages.getPinnedDialogs(1)` (`api/api_updates.cpp:2497-2526`).
    let upd_pin_unloaded = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DialogPinned(
            tl_types::UpdateDialogPinned {
                pinned: true,
                folder_id: Some(ARCHIVE_FOLDER_ID),
                peer: tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                    peer: tl_peer_user(503),
                }),
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_125,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_pin_unloaded).await;
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetPinnedDialogs>().len(),
        2
    );
    assert_eq!(
        session.archive().chats_list.pinned().order(),
        &[DialogKey::History(PeerId::from_user(UserId(503)))]
    );

    let upd_pin_nested_folder = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DialogPinned(
            tl_types::UpdateDialogPinned {
                pinned: true,
                folder_id: Some(ARCHIVE_FOLDER_ID),
                peer: tl_enums::DialogPeer::Folder(tl_types::DialogPeerFolder {
                    folder_id: ARCHIVE_FOLDER_ID,
                }),
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_126,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_pin_nested_folder).await;
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetPinnedDialogs>().len(),
        3
    );

    // 6) updateDialogUnreadMark: marks p502 unread, ignores Folder and unloaded peer 598.
    let upd_unread = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::DialogUnreadMark(tl_types::UpdateDialogUnreadMark {
                unread: true,
                peer: tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                    peer: tl_peer_user(502),
                }),
                saved_peer_id: None,
            }),
            tl_enums::Update::DialogUnreadMark(tl_types::UpdateDialogUnreadMark {
                unread: true,
                peer: tl_enums::DialogPeer::Folder(tl_types::DialogPeerFolder {
                    folder_id: ARCHIVE_FOLDER_ID,
                }),
                saved_peer_id: None,
            }),
            tl_enums::Update::DialogUnreadMark(tl_types::UpdateDialogUnreadMark {
                unread: true,
                peer: tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer {
                    peer: tl_peer_user(598),
                }),
                saved_peer_id: None,
            }),
        ],
        users: vec![],
        chats: vec![],
        date: 1_700_000_130,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_unread).await;
    assert!(session.history(p502).unwrap().unread_mark);
    assert!(session.history(PeerId::from_user(UserId(598))).is_none());

    // 7) updateReadHistoryInbox / Outbox / ReadChannelInbox update unread counters and read watermarks.
    let upd_reads = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::ReadHistoryInbox(tl_types::UpdateReadHistoryInbox {
                folder_id: None,
                peer: tl_peer_user(501),
                top_msg_id: None,
                max_id: 9,
                still_unread_count: 1,
                pts: 12,
                pts_count: 1,
            }),
            tl_enums::Update::ReadHistoryOutbox(tl_types::UpdateReadHistoryOutbox {
                peer: tl_peer_user(501),
                max_id: 8,
                pts: 13,
                pts_count: 1,
            }),
            tl_enums::Update::ReadChannelInbox(tl_types::UpdateReadChannelInbox {
                folder_id: None,
                channel_id: 700,
                max_id: 97,
                still_unread_count: 3,
                pts: 50,
            }),
        ],
        users: vec![],
        chats: vec![],
        date: 1_700_000_135,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_reads).await;
    assert_eq!(session.history(p501).unwrap().unread_count, Some(1));
    assert_eq!(
        session.history(p501).unwrap().inbox_read_before,
        Some(MsgId(10))
    );
    assert_eq!(
        session.history(p501).unwrap().outbox_read_before,
        Some(MsgId(9))
    );
    assert_eq!(session.history(ch700).unwrap().unread_count, Some(3));
    assert_eq!(
        session.history(ch700).unwrap().inbox_read_before,
        Some(MsgId(98))
    );
}

#[tokio::test]
async fn dialog_filters_chatlists_exceptions_and_updates_conformance() {
    let filter_get_calls = Arc::new(AtomicUsize::new(0));
    let filter_get_calls_srv = Arc::clone(&filter_get_calls);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID => {
            let call = filter_get_calls_srv.fetch_add(1, Ordering::SeqCst);
            let filters = if call == 0 {
                // Initial load: Default + Filter 2 (normal with pinned [601, 602] & include [603]) + Chatlist 3.
                vec![
                    tl_enums::DialogFilter::Default,
                    tl_enums::DialogFilter::DialogFilter(tl_types::DialogFilter {
                        contacts: true,
                        non_contacts: false,
                        groups: true,
                        broadcasts: false,
                        bots: false,
                        exclude_muted: false,
                        exclude_read: false,
                        exclude_archived: true,
                        title_noanimate: false,
                        id: 2,
                        title: tl_text("Work"),
                        emoticon: None,
                        color: Some(1),
                        pinned_peers: vec![
                            tl_enums::InputPeer::User(tl_types::InputPeerUser {
                                user_id: 601,
                                access_hash: 0x6010,
                            }),
                            tl_enums::InputPeer::User(tl_types::InputPeerUser {
                                user_id: 602,
                                access_hash: 0x6020,
                            }),
                        ],
                        include_peers: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                            user_id: 603,
                            access_hash: 0x6030,
                        })],
                        exclude_peers: vec![],
                    }),
                    tl_enums::DialogFilter::Chatlist(tl_types::DialogFilterChatlist {
                        has_my_invites: true,
                        title_noanimate: false,
                        id: 3,
                        title: tl_text("Shared List"),
                        emoticon: None,
                        color: Some(2),
                        pinned_peers: vec![],
                        include_peers: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                            user_id: 601,
                            access_hash: 0x6010,
                        })],
                    }),
                ]
            } else {
                // Subsequent reload after updateDialogFilterOrder mismatch / updateDialogFilters:
                // Filter 3 removed, only Filter 2 remains!
                vec![
                    tl_enums::DialogFilter::Default,
                    tl_enums::DialogFilter::DialogFilter(tl_types::DialogFilter {
                        contacts: true,
                        non_contacts: false,
                        groups: true,
                        broadcasts: false,
                        bots: false,
                        exclude_muted: false,
                        exclude_read: false,
                        exclude_archived: true,
                        title_noanimate: false,
                        id: 2,
                        title: tl_text("Work Reloaded"),
                        emoticon: None,
                        color: Some(1),
                        pinned_peers: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                            user_id: 601,
                            access_hash: 0x6010,
                        })],
                        include_peers: vec![],
                        exclude_peers: vec![],
                    }),
                ]
            };
            let resp = tl_enums::messages::DialogFilters::DialogFilters(
                tl_types::messages::DialogFilters {
                    tags_enabled: true,
                    filters,
                },
            );
            ServerAction::ok(&resp)
        }
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID => {
            let resp = tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: vec![tl_dialog(tl_peer_user(601), 1, 1, 1, 0, false, Some(0))],
                messages: vec![tl_message(
                    1,
                    tl_peer_user(601),
                    Some(tl_peer_user(601)),
                    false,
                    1_700_000_001,
                    "hi",
                )],
                chats: vec![],
                users: vec![tl_user(601, 0x6010, "Peer 601", false)],
            });
            ServerAction::ok(&resp)
        }
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID => {
            let resp =
                tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                    dialogs: vec![],
                    messages: vec![],
                    chats: vec![],
                    users: vec![],
                    state: tl_state(10, 0, 1_700_000_001, 1),
                });
            ServerAction::ok(&resp)
        }
        tl_fns::messages::GetPeerDialogs::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetPeerDialogs::from_bytes(&rpc.body).unwrap();
            // Peer 601 was already loaded in main dialogs; only 602 & 603 should be requested.
            assert_eq!(req.peers.len(), 2);
            let resp =
                tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                    dialogs: vec![
                        tl_dialog(tl_peer_user(602), 2, 2, 2, 0, false, Some(0)),
                        tl_dialog(tl_peer_user(603), 3, 3, 3, 0, false, Some(0)),
                    ],
                    messages: vec![
                        tl_message(
                            2,
                            tl_peer_user(602),
                            Some(tl_peer_user(602)),
                            false,
                            1_700_000_002,
                            "m2",
                        ),
                        tl_message(
                            3,
                            tl_peer_user(603),
                            Some(tl_peer_user(603)),
                            false,
                            1_700_000_003,
                            "m3",
                        ),
                    ],
                    chats: vec![],
                    users: vec![
                        tl_user(602, 0x6020, "Peer 602", false),
                        tl_user(603, 0x6030, "Peer 603", false),
                    ],
                    state: tl_state(10, 0, 1_700_000_003, 1),
                });
            ServerAction::ok(&resp)
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut loader = DialogsLoader::new();
    let mut updates = init_updates(10, 0, 1_700_000_003, 1);

    // Load main list so `load_next_filter_exceptions` is unblocked.
    let main_page = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_page(&rpc, 0))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    main_page.apply_to_session(&mut session, 1_700_000_010);

    // Load dialog filters + apply to session.
    tokio::time::timeout(TEST_TIMEOUT, loader.load_dialog_filters(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert!(loader.tags_enabled());
    assert!(loader.archive_needed()); // Chatlist implies archive needed
    loader.apply_filters_to_session(&mut session);
    assert!(session.filters().tags_enabled());
    assert_eq!(session.filters().list().len(), 2); // Filter 2 + Chatlist 3 (Default skipped in list)
    assert!(
        session
            .filters()
            .get(3)
            .unwrap()
            .flags
            .contains(ChatFilterFlags::CHATLIST)
    );
    assert!(
        session
            .filters()
            .get(3)
            .unwrap()
            .flags
            .contains(ChatFilterFlags::HAS_MY_LINKS)
    );

    // Fetch missing filter exception peers (602, 603) via `messages.getPeerDialogs`.
    let exc_resp = tokio::time::timeout(TEST_TIMEOUT, loader.load_next_filter_exceptions(&rpc))
        .await
        .unwrap()
        .unwrap()
        .expect("exception peer dialogs");
    apply_tl_peer_dialogs_for_folder(&mut session, &exc_resp, None, false, 1_700_000_010);
    // Re-apply filters so pinned indices are hydrated on loaded histories 601 & 602.
    loader.apply_filters_to_session(&mut session);
    let p601 = PeerId::from_user(UserId(601));
    let p602 = PeerId::from_user(UserId(602));
    assert!(session.history(p601).unwrap().pinned_index(2) > 0);
    assert!(session.history(p602).unwrap().pinned_index(2) > 0);

    // updateDialogFilter: shrink pinned_peers of filter 2 to only [601] -> p602's cached pin MUST be evicted!
    let upd_shrink = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DialogFilter(
            tl_types::UpdateDialogFilter {
                id: 2,
                filter: Some(tl_enums::DialogFilter::DialogFilter(
                    tl_types::DialogFilter {
                        contacts: true,
                        non_contacts: false,
                        groups: true,
                        broadcasts: false,
                        bots: false,
                        exclude_muted: false,
                        exclude_read: false,
                        exclude_archived: true,
                        title_noanimate: false,
                        id: 2,
                        title: tl_text("Work"),
                        emoticon: None,
                        color: Some(1),
                        pinned_peers: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                            user_id: 601,
                            access_hash: 0x6010,
                        })],
                        include_peers: vec![],
                        exclude_peers: vec![],
                    },
                )),
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_020,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_shrink).await;
    assert!(session.history(p601).unwrap().pinned_index(2) > 0);
    assert_eq!(
        session.history(p602).unwrap().pinned_index(2),
        0,
        "evicted filter pin must clear History::pinned_index(filter_id)"
    );

    // Valid updateDialogFilterOrder ([3, 2]) reorders without RPC.
    let upd_order_ok = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DialogFilterOrder(
            tl_types::UpdateDialogFilterOrder { order: vec![3, 2] },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_025,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_order_ok).await;
    let ids: Vec<i32> = session.filters().list().iter().map(|f| f.id).collect();
    assert_eq!(ids, vec![3, 2]);
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetDialogFilters>().len(),
        1
    );

    // Mismatched updateDialogFilterOrder ([99, 2]) MUST trigger `messages.getDialogFilters` reload
    // and prune filter 3 via `sync_filters_from_list` (`data/data_chat_filters.cpp:536-541, 799-835`).
    let upd_order_bad = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DialogFilterOrder(
            tl_types::UpdateDialogFilterOrder { order: vec![99, 2] },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_030,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_order_bad).await;
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetDialogFilters>().len(),
        2
    );
    assert_eq!(session.filters().list().len(), 1);
    assert_eq!(
        session.filters().get(2).unwrap().title.text,
        "Work Reloaded"
    );
    assert!(session.filters().get(3).is_none());

    // updateDialogFilters triggers another reload (`data/data_chat_filters.cpp:534-535`).
    let upd_reload = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DialogFilters],
        users: vec![],
        chats: vec![],
        date: 1_700_000_035,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_reload).await;
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetDialogFilters>().len(),
        3
    );
}

// ============================================================================
// 2. Peers: min protection, *FromMessage InputPeers, Empty/Forbidden, updates, migration, storage
// ============================================================================

#[test]
fn peers_min_protection_and_from_message_input_peers_conformance() {
    let mut session = Session::new(UserId(1));

    // --- Part A: Full User must NOT be clobbered by a `min = true` User ---
    let mut full_u = tl_user_base(1001, 0x1111_2222_3333_4444, "FullFirst", false);
    full_u.last_name = Some("FullLast".to_owned());
    full_u.phone = Some("0000001001".to_owned());
    full_u.username = Some("full_user".to_owned());
    full_u.usernames = Some(vec![
        tl_enums::Username::Username(tl_types::Username {
            editable: true,
            active: true,
            deletable: false,
            expired: false,
            username: "full_user".to_owned(),
        }),
        tl_enums::Username::Username(tl_types::Username {
            editable: false,
            active: true,
            deletable: false,
            expired: false,
            username: "collectible_one".to_owned(),
        }),
    ]);
    full_u.photo = Some(tl_profile_photo(555_001, true)); // personal=true -> DISCARD_MIN_PHOTO
    full_u.status = Some(tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
        expires: 1_700_009_000,
    }));
    full_u.stories_max_id = Some(tl_enums::RecentStory::RecentStory(tl_types::RecentStory {
        live: false,
        max_id: Some(77),
    }));
    full_u.restriction_reason = Some(vec![tl_enums::RestrictionReason::RestrictionReason(
        tl_types::RestrictionReason {
            platform: "all".to_owned(),
            reason: "policy".to_owned(),
            text: "Restricted".to_owned(),
        },
    )]);
    apply_tl_user(&mut session, &tl_enums::User::User(full_u));

    let mut min_u = tl_user_base(1001, 0x9999_9999, "MinFirst", true);
    min_u.last_name = Some("MinLast".to_owned());
    min_u.phone = Some("0000009999".to_owned());
    min_u.username = Some("min_user".to_owned());
    min_u.photo = Some(tl_profile_photo(555_999, false));
    min_u.apply_min_photo = true; // Must STILL be discarded because full_u set DISCARD_MIN_PHOTO (personal=true)!
    min_u.contact = false;
    min_u.stories_max_id = None;
    min_u.restriction_reason = None;
    apply_tl_user(&mut session, &tl_enums::User::User(min_u));

    let u1001 = session.user(UserId(1001)).unwrap();
    assert_eq!(u1001.access_hash, 0x1111_2222_3333_4444);
    assert_eq!(u1001.first_name, "FullFirst");
    assert_eq!(u1001.last_name, "FullLast");
    assert_eq!(u1001.phone, "0000001001");
    assert_eq!(u1001.usernames.editable_username(), "full_user");
    assert_eq!(u1001.common.userpic_photo_id, PhotoId(555_001));
    assert!(u1001.flags.contains(UserFlags::DISCARD_MIN_PHOTO));
    assert_eq!(u1001.contact_status, ContactStatus::Contact);
    assert!(u1001.flags.contains(UserFlags::HAS_ACTIVE_STORIES));
    assert_eq!(u1001.common.unavailable_reasons.len(), 1);

    // Verify apply_min_photo=true on the full user clears DISCARD_MIN_PHOTO so a subsequent
    // min user CAN update the photo (`data/data_session.cpp:803-805, 899-905`).
    let mut full_u2 = tl_user_base(1002, 0x2222, "PhotoUser", false);
    full_u2.apply_min_photo = true;
    full_u2.photo = Some(tl_profile_photo(600_001, false));
    apply_tl_user(&mut session, &tl_enums::User::User(full_u2));
    let mut min_u2 = tl_user_base(1002, 0x7777, "PhotoUserMin", true);
    min_u2.photo = Some(tl_profile_photo(600_002, false));
    apply_tl_user(&mut session, &tl_enums::User::User(min_u2));
    let u1002 = session.user(UserId(1002)).unwrap();
    assert_eq!(u1002.access_hash, 0x2222);
    assert_eq!(u1002.common.userpic_photo_id, PhotoId(600_002));

    // Non-min user without `usernames` clears previously stored collectible usernames (`data/data_session.cpp:907-917`).
    let mut refreshed_u = tl_user_base(1001, 0x1111_2222_3333_4444, "FullFirst", false);
    refreshed_u.username = Some("only_primary".to_owned());
    refreshed_u.usernames = None;
    apply_tl_user(&mut session, &tl_enums::User::User(refreshed_u));
    let u1001_after = session.user(UserId(1001)).unwrap();
    assert_eq!(u1001_after.usernames.editable_username(), "only_primary");
    assert_eq!(
        u1001_after.usernames.usernames(),
        &["only_primary".to_owned()]
    );

    // --- Part B: Full Channel must NOT be clobbered by a `min = true` Channel ---
    let mut full_ch = tl_channel_base(2001, 0xAAAA_BBBB, "Full Megagroup", false, false);
    full_ch.creator = true;
    full_ch.date = 1_700_001_234;
    full_ch.stories_hidden = true;
    full_ch.stories_max_id = Some(tl_enums::RecentStory::RecentStory(tl_types::RecentStory {
        live: false,
        max_id: Some(88),
    }));
    full_ch.restriction_reason = Some(vec![tl_enums::RestrictionReason::RestrictionReason(
        tl_types::RestrictionReason {
            platform: "all".to_owned(),
            reason: "policy".to_owned(),
            text: "Chan Restricted".to_owned(),
        },
    )]);
    apply_tl_chat(&mut session, &tl_enums::Chat::Channel(full_ch));

    let mut min_ch = tl_channel_base(2001, 0xDEAD_BEEF, "Min Megagroup", false, true);
    min_ch.creator = false;
    min_ch.date = 1_600_000_000;
    min_ch.stories_hidden = false;
    min_ch.stories_hidden_min = true; // Must NOT overwrite non-min stories_hidden!
    min_ch.stories_max_id = None;
    min_ch.restriction_reason = None;
    apply_tl_chat(&mut session, &tl_enums::Chat::Channel(min_ch));

    let ch2001 = session.channel(ChannelId(2001)).unwrap();
    assert_eq!(ch2001.access_hash, 0xAAAA_BBBB);
    assert!(ch2001.flags.contains(ChannelFlags::CREATOR));
    assert_eq!(ch2001.date, 1_700_001_234);
    assert!(ch2001.flags.contains(ChannelFlags::STORIES_HIDDEN));
    assert!(ch2001.flags.contains(ChannelFlags::HAS_ACTIVE_STORIES));
    assert_eq!(ch2001.common.unavailable_reasons.len(), 1);

    // Broadcast channel with `forum = true` must NOT set `ChannelFlags::FORUM` (requires `megagroup`, `data/data_session.cpp:1141`).
    let mut bcast_forum = tl_channel_base(2002, 0xCCCC, "Broadcast", true, false);
    bcast_forum.forum = true;
    apply_tl_chat(&mut session, &tl_enums::Chat::Channel(bcast_forum));
    assert!(
        !session
            .channel(ChannelId(2002))
            .unwrap()
            .flags
            .contains(ChannelFlags::FORUM)
    );

    // --- Part C: Min-only User & Channel -> InputPeer*FromMessage / InputUserFromMessage / InputChannelFromMessage ---
    let min_only_user = tl_user(3001, 0x0, "MinOnly User", true);
    let min_only_chan = tl_channel(3002, 0x0, "MinOnly Chan", true, true);
    apply_tl_user(&mut session, &min_only_user);
    apply_tl_chat(&mut session, &min_only_chan);

    // Message inside loaded container megagroup 2001 authored by min-only user 3001.
    let container_msg = tl_message(
        42,
        tl_peer_channel(2001),
        Some(tl_peer_user(3001)),
        false,
        1_700_002_000,
        "from min user",
    );
    apply_tl_message(&mut session, &container_msg);

    // Message inside loaded container megagroup 2001 referencing min-only channel 3002 as sender.
    let container_msg_ch = tl_message(
        43,
        tl_peer_channel(2001),
        Some(tl_peer_channel(3002)),
        false,
        1_700_002_001,
        "from min channel",
    );
    apply_tl_message(&mut session, &container_msg_ch);

    assert_eq!(
        session.message_with_peer(PeerId::from_user(UserId(3001))),
        Some(FullMsgId::new(
            PeerId::from_channel(ChannelId(2001)),
            MsgId(42)
        ))
    );

    // Expect InputPeer::UserFromMessage & InputUser::FromMessage!
    let expected_container = tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
        channel_id: 2001,
        access_hash: 0xAAAA_BBBB,
    });
    assert_eq!(
        input_peer_for(&session, PeerId::from_user(UserId(3001))),
        tl_enums::InputPeer::UserFromMessage(tl_types::InputPeerUserFromMessage {
            peer: Box::new(expected_container.clone()),
            msg_id: 42,
            user_id: 3001,
        })
    );
    assert_eq!(
        input_user_for(&session, UserId(3001)),
        tl_enums::InputUser::FromMessage(tl_types::InputUserFromMessage {
            peer: expected_container.clone(),
            msg_id: 42,
            user_id: 3001,
        })
    );
    assert_eq!(
        input_peer_for(&session, PeerId::from_channel(ChannelId(3002))),
        tl_enums::InputPeer::ChannelFromMessage(tl_types::InputPeerChannelFromMessage {
            peer: Box::new(expected_container.clone()),
            msg_id: 43,
            channel_id: 3002,
        })
    );
    assert_eq!(
        input_channel_for(&session, ChannelId(3002)),
        tl_enums::InputChannel::FromMessage(tl_types::InputChannelFromMessage {
            peer: expected_container,
            msg_id: 43,
            channel_id: 3002,
        })
    );

    // Once full peers arrive, InputPeer upgrades to normal InputPeer::User / Channel.
    apply_tl_user(
        &mut session,
        &tl_user(3001, 0x3001_ABCD, "Now Full User", false),
    );
    apply_tl_chat(
        &mut session,
        &tl_channel(3002, 0x3002_ABCD, "Now Full Chan", true, false),
    );
    assert_eq!(
        input_peer_for(&session, PeerId::from_user(UserId(3001))),
        tl_enums::InputPeer::User(tl_types::InputPeerUser {
            user_id: 3001,
            access_hash: 0x3001_ABCD,
        })
    );
    assert_eq!(
        input_peer_for(&session, PeerId::from_channel(ChannelId(3002))),
        tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
            channel_id: 3002,
            access_hash: 0x3002_ABCD,
        })
    );
}

#[test]
fn peers_empty_and_forbidden_conformance() {
    let mut session = Session::new(UserId(1));

    // Seed full user, chat, and channel.
    let mut u = tl_user_base(4001, 0x4001_5555, "Active", false);
    u.last_name = Some("User".to_owned());
    u.username = Some("active_u".to_owned());
    u.phone = Some("0000004001".to_owned());
    apply_tl_user(&mut session, &tl_enums::User::User(u));

    apply_tl_chat(&mut session, &tl_chat(4002, "Active Group"));

    let mut ch = tl_channel_base(4003, 0x4003_1111, "Active Megagroup", false, false);
    ch.participants_count = Some(250);
    ch.date = 1_700_005_000;
    apply_tl_chat(&mut session, &tl_enums::Chat::Channel(ch));

    // Apply User::Empty (`data/data_session.cpp:686-701`).
    apply_tl_user(
        &mut session,
        &tl_enums::User::Empty(tl_types::UserEmpty { id: 4001 }),
    );
    let u4001 = session.user(UserId(4001)).unwrap();
    assert_eq!(u4001.first_name, "Deleted Account");
    assert!(u4001.last_name.is_empty());
    assert!(u4001.phone.is_empty());
    assert!(u4001.usernames.usernames().is_empty());
    assert_eq!(u4001.common.userpic_photo_id, PhotoId::ZERO);
    assert!(u4001.flags.contains(UserFlags::DELETED));

    // Apply Chat::Forbidden (`data/data_session.cpp:1051-1067`).
    apply_tl_chat(
        &mut session,
        &tl_enums::Chat::Forbidden(tl_types::ChatForbidden {
            id: 4002,
            title: "Forbidden Group".to_owned(),
        }),
    );
    let c4002 = session.chat(ChatId(4002)).unwrap();
    assert_eq!(c4002.common.name, "Forbidden Group");
    assert_eq!(c4002.count, -1);
    assert_eq!(c4002.date, 0);
    assert!(c4002.flags.contains(ChatFlags::FORBIDDEN));

    // Apply Chat::ChannelForbidden (`data/data_session.cpp:1266-1308`).
    apply_tl_chat(
        &mut session,
        &tl_enums::Chat::ChannelForbidden(tl_types::ChannelForbidden {
            broadcast: false,
            megagroup: true,
            monoforum: false,
            id: 4003,
            access_hash: 0x4003_9999,
            title: "Banned Group".to_owned(),
            until_date: Some(1_700_099_999),
        }),
    );
    let ch4003 = session.channel(ChannelId(4003)).unwrap();
    assert_eq!(ch4003.common.name, "Banned Group");
    assert_eq!(ch4003.access_hash, 0x4003_9999);
    assert!(ch4003.flags.contains(ChannelFlags::FORBIDDEN));
    assert!(ch4003.flags.contains(ChannelFlags::MEGAGROUP));
    assert_eq!(ch4003.date, 0);
    assert_eq!(ch4003.members_count, 0);
    assert_eq!(ch4003.restrictions.until, 1_700_099_999);
    assert!(
        ch4003
            .restrictions
            .flags
            .contains(ChatBannedRights::VIEW_MESSAGES)
    );
}

#[tokio::test]
async fn peers_updates_and_chat_migration_conformance() {
    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::channels::GetChannels::CONSTRUCTOR_ID => {
            let req = tl_fns::channels::GetChannels::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.id.len(), 1);
            match &req.id[0] {
                tl_enums::InputChannel::InputChannel(ic) => {
                    assert_eq!(ic.channel_id, 5010);
                    assert_eq!(ic.access_hash, 0x5010_AAAA);
                }
                other => panic!("unexpected InputChannel: {other:?}"),
            }
            let resp = tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
                chats: vec![tl_channel(
                    5010,
                    0x5010_AAAA,
                    "Refetched Channel Title",
                    true,
                    false,
                )],
            });
            ServerAction::ok(&resp)
        }
        tl_fns::messages::GetFullChat::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetFullChat::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.chat_id, 5020);
            let mut refreshed_chat = tl_chat_base(5020, "Group 5020 Resynced");
            refreshed_chat.version = 5;
            refreshed_chat.participants_count = 2;
            let resp = tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
                full_chat: tl_enums::ChatFull::ChatFull(tl_types::ChatFull {
                    can_set_username: false,
                    has_scheduled: false,
                    translations_disabled: false,
                    has_welcome_messages: false,
                    id: 5020,
                    about: "Resynced".to_owned(),
                    participants: tl_enums::ChatParticipants::ChatParticipants(
                        tl_types::ChatParticipants {
                            chat_id: 5020,
                            participants: vec![
                                tl_enums::ChatParticipant::Creator(
                                    tl_types::ChatParticipantCreator {
                                        user_id: 1,
                                        rank: None,
                                    },
                                ),
                                tl_enums::ChatParticipant::ChatParticipant(
                                    tl_types::ChatParticipant {
                                        user_id: 5001,
                                        inviter_id: 1,
                                        date: 1_700_000_000,
                                        rank: None,
                                    },
                                ),
                            ],
                            version: 5,
                        },
                    ),
                    chat_photo: None,
                    notify_settings: tl_notify_settings(),
                    exported_invite: None,
                    bot_info: None,
                    pinned_msg_id: None,
                    folder_id: None,
                    call: None,
                    ttl_period: None,
                    groupcall_default_join_as: None,
                    theme_emoticon: None,
                    requests_pending: None,
                    recent_requesters: None,
                    available_reactions: None,
                    reactions_limit: None,
                }),
                chats: vec![tl_enums::Chat::Chat(refreshed_chat)],
                users: vec![tl_user(5001, 0x5001, "Member One", false)],
            });
            ServerAction::ok(&resp)
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut updates = init_updates(10, 0, 1_700_000_000, 1);

    // Seed self user 1, non-contact user 5001, member 5002, channel 5010, basic chat 5020.
    let mut u5001 = tl_user_base(5001, 0x5001, "OldFirst", false);
    u5001.contact = false;
    u5001.mutual_contact = false;
    apply_tl_users_and_chats(
        &mut session,
        &[
            tl_self_user(1, 0x1111),
            tl_enums::User::User(u5001),
            tl_user(5002, 0x5002, "Member Two", false),
        ],
        &[
            tl_channel(5010, 0x5010_AAAA, "Old Channel Title", true, false),
            tl_chat(5020, "Group 5020"),
        ],
    );

    // 1) updateUserName, updateUserPhone, updateUserStatus (loaded vs unloaded!), updateUserEmojiStatus.
    let upd_user = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::UserName(tl_types::UpdateUserName {
                user_id: 5001,
                first_name: "NewFirst".to_owned(),
                last_name: "NewLast".to_owned(),
                usernames: vec![tl_enums::Username::Username(tl_types::Username {
                    editable: true,
                    active: true,
                    deletable: false,
                    expired: false,
                    username: "new_handle".to_owned(),
                })],
            }),
            tl_enums::Update::UserPhone(tl_types::UpdateUserPhone {
                user_id: 5001,
                phone: "0000005001".to_owned(),
            }),
            tl_enums::Update::UserStatus(tl_types::UpdateUserStatus {
                user_id: 5001,
                status: tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
                    expires: 1_700_005_000,
                }),
            }),
            // Unloaded user 5999 must NOT create a phantom entry (`api/api_updates.cpp:2133-2142`).
            tl_enums::Update::UserStatus(tl_types::UpdateUserStatus {
                user_id: 5999,
                status: tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
                    expires: 1_700_005_000,
                }),
            }),
            tl_enums::Update::UserEmojiStatus(tl_types::UpdateUserEmojiStatus {
                user_id: 5001,
                emoji_status: tl_enums::EmojiStatus::EmojiStatus(tl_types::EmojiStatus {
                    document_id: 777_888,
                    until: Some(1_700_099_000),
                }),
            }),
        ],
        users: vec![],
        chats: vec![],
        date: 1_700_000_100,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_user).await;

    let u = session.user(UserId(5001)).unwrap();
    assert_eq!(u.first_name, "NewFirst");
    assert_eq!(u.last_name, "NewLast");
    assert_eq!(u.usernames.editable_username(), "new_handle");
    assert_eq!(u.phone, "0000005001");
    assert_eq!(u.common.emoji_status.document_id, DocumentId(777_888));
    assert!(
        session.user(UserId(5999)).is_none(),
        "unloaded user must not be created"
    );

    // 2) updateChannel -> triggers `channels.getChannels` refetch (`api/api_updates.cpp:2539-2562`).
    let upd_ch = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::Channel(tl_types::UpdateChannel {
            channel_id: 5010,
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_110,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_ch).await;
    assert_eq!(server.rpcs_of::<tl_fns::channels::GetChannels>().len(), 1);
    assert_eq!(
        session.channel(ChannelId(5010)).unwrap().common.name,
        "Refetched Channel Title"
    );

    // 3) updateChatParticipants (v1) -> Add (v2) -> Delete (v3) -> TooOld (v2 ignored) -> Skipped (v5 -> getFullChat!).
    let upd_parts = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::ChatParticipants(tl_types::UpdateChatParticipants {
                participants: tl_enums::ChatParticipants::ChatParticipants(
                    tl_types::ChatParticipants {
                        chat_id: 5020,
                        participants: vec![tl_enums::ChatParticipant::Creator(
                            tl_types::ChatParticipantCreator {
                                user_id: 1,
                                rank: None,
                            },
                        )],
                        version: 1,
                    },
                ),
            }),
            tl_enums::Update::ChatParticipantAdd(tl_types::UpdateChatParticipantAdd {
                chat_id: 5020,
                user_id: 5001,
                inviter_id: 1,
                date: 1_700_000_115,
                version: 2,
            }),
            tl_enums::Update::ChatParticipantAdd(tl_types::UpdateChatParticipantAdd {
                chat_id: 5020,
                user_id: 5002,
                inviter_id: 1,
                date: 1_700_000_116,
                version: 3,
            }),
            tl_enums::Update::ChatParticipantDelete(tl_types::UpdateChatParticipantDelete {
                chat_id: 5020,
                user_id: 5002,
                version: 4,
            }),
            // Stale version 2 -> TooOld, ignored.
            tl_enums::Update::ChatParticipantDelete(tl_types::UpdateChatParticipantDelete {
                chat_id: 5020,
                user_id: 5001,
                version: 2,
            }),
        ],
        users: vec![],
        chats: vec![],
        date: 1_700_000_120,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_parts).await;
    let chat = session.chat(ChatId(5020)).unwrap();
    assert_eq!(chat.version, 4);
    assert!(chat.participants.contains(&UserId(1)));
    assert!(chat.participants.contains(&UserId(5001)));
    assert!(!chat.participants.contains(&UserId(5002)));
    assert_eq!(server.rpcs_of::<tl_fns::messages::GetFullChat>().len(), 0);

    // Version jump 4 -> 6 (Skipped) invalidates participants and triggers `messages.getFullChat(5020)`.
    let upd_gap = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::ChatParticipantAdd(
            tl_types::UpdateChatParticipantAdd {
                chat_id: 5020,
                user_id: 5003,
                inviter_id: 1,
                date: 1_700_000_125,
                version: 6,
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_125,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_gap).await;
    assert_eq!(server.rpcs_of::<tl_fns::messages::GetFullChat>().len(), 1);
    assert_eq!(
        session.chat(ChatId(5020)).unwrap().common.name,
        "Group 5020 Resynced"
    );

    // 4) Chat -> Channel migration (`migrated_to`): preserves existing channel access_hash
    // and hides the basic chat from main dialog list when target megagroup `am_in()`.
    let chat_pid = PeerId::from_chat(ChatId(5020));
    let target_ch_id = ChannelId(5030);
    apply_tl_chat(
        &mut session,
        &tl_channel(5030, 0x5030_BEEF, "Migrated Supergroup", false, false),
    );
    apply_tl_dialog_in_folder(
        &mut session,
        &tl_dialog(tl_peer_chat(5020), 10, 10, 10, 0, false, Some(0)),
        Some(0),
        1_700_000_130,
    );
    assert!(
        session
            .main_list()
            .indexed()
            .contains(DialogKey::History(chat_pid))
    );

    let mut migrated_chat = tl_chat_base(5020, "Group 5020 Resynced");
    migrated_chat.deactivated = true;
    migrated_chat.migrated_to = Some(tl_enums::InputChannel::InputChannel(
        tl_types::InputChannel {
            channel_id: 5030,
            access_hash: 0x0, // Must NOT overwrite already-known 0x5030_BEEF (`data/data_session.cpp:1016-1018`)!
        },
    ));
    apply_tl_chat(&mut session, &tl_enums::Chat::Chat(migrated_chat));

    assert_eq!(
        session.channel(target_ch_id).unwrap().access_hash,
        0x5030_BEEF
    );
    assert_eq!(
        session.chat(ChatId(5020)).unwrap().migrated_to,
        Some(target_ch_id)
    );
    assert_eq!(
        session.channel(target_ch_id).unwrap().migrated_from(),
        Some(ChatId(5020))
    );
    assert!(
        !session
            .main_list()
            .indexed()
            .contains(DialogKey::History(chat_pid)),
        "migrated basic chat must be hidden from dialog list when target megagroup am_in()"
    );
}

#[test]
fn storage_peer_cache_persistence_roundtrip_conformance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let local_key = AuthKey::new([0x3Au8; 256]);

    // 1) Direct v3 binary layout round-trip for User, Chat, and Channel peers.
    let peers = vec![
        SerializedPeer {
            id: StoragePeerId::from_user(5_000_000_123),
            userpic_photo_id: 99_001,
            userpic_location: ImageLocation::default(),
            userpic_has_video: true,
            kind: SerializedPeerKind::User(SerializedUser {
                first_name: "Cached".to_owned(),
                last_name: "Peer".to_owned(),
                phone: String::new(),
                username: "cached_peer".to_owned(),
                access_hash: 0x1122_3344_5566_7788,
                flags: 0x15,
                bot_inline_placeholder: String::new(),
                lastseen: 0x42,
                is_contact: true,
                bot_info_version: -1,
                supports_guest_chat: false,
            }),
        },
        SerializedPeer {
            id: StoragePeerId::from_chat(888_001),
            userpic_photo_id: 0,
            userpic_location: ImageLocation::default(),
            userpic_has_video: false,
            kind: SerializedPeerKind::Chat(SerializedChat {
                name: "Cached Group".to_owned(),
                count: 12,
                date: 1_700_000_000,
                version: 4,
                creator_id: 5_000_000_123,
                flags: 0x01,
                invite_link: String::new(),
            }),
        },
        SerializedPeer {
            id: StoragePeerId::from_channel(2_000_000_999),
            userpic_photo_id: 99_002,
            userpic_location: ImageLocation::default(),
            userpic_has_video: false,
            kind: SerializedPeerKind::Channel(SerializedChannel {
                name: "Cached Channel".to_owned(),
                access_hash: 0xCAFE_BABE_DEAD_BEEF,
                date: 1_700_000_100,
                flags: 0x04,
                invite_link: String::new(),
            }),
        },
    ];

    let mut w = QDataStreamWriter::new();
    for p in &peers {
        p.write_to_stream(&mut w);
    }
    let raw = w.into_bytes();

    // Verify exact tdesktop v3 header tag + version on first record.
    let mut header_r = QDataStreamReader::new(&raw);
    let _peer_id_wire = header_r.read_u64().unwrap();
    assert_eq!(header_r.read_u64().unwrap(), PEER_SERIALIZE_VERSION_TAG);
    assert_eq!(header_r.read_i32().unwrap(), PEER_SERIALIZE_VERSION);

    let mut r = QDataStreamReader::new(&raw);
    for expected in &peers {
        let decoded =
            SerializedPeer::read_from_stream(APP_VERSION, &mut r).expect("decode SerializedPeer");
        assert_eq!(&decoded, expected);
    }

    // 2) Full on-disk encrypted AccountStorage round-trip (inline_bots + trusted_peers).
    let mut storage = AccountStorage::open(dir.path(), "data", 0, local_key.clone())
        .expect("open account storage");
    let recent = RecentHashtagsAndBots {
        write_hashtags: vec![("rust".to_owned(), 3)],
        search_hashtags: vec![],
        inline_bots: vec![peers[0].clone()],
    };
    storage
        .write_recent_hashtags_and_bots(&recent)
        .expect("write recent bots");

    let mut trusted = TrustedPeersData::default();
    trusted.trusted_peers.insert(
        StoragePeerId::from_user(5_000_000_123),
        PeerTrustFlags(PeerTrustFlags::PAYMENT),
    );
    storage
        .write_trusted_peers(&trusted)
        .expect("write trusted peers");

    // Reopen from disk and verify exact peer persistence.
    let reloaded_storage =
        AccountStorage::open(dir.path(), "data", 0, local_key).expect("reopen account storage");
    let loaded_recent = reloaded_storage
        .read_recent_hashtags_and_bots()
        .expect("read recent bots");
    assert_eq!(loaded_recent.inline_bots, vec![peers[0].clone()]);

    let loaded_trusted = reloaded_storage
        .read_trusted_peers()
        .expect("read trusted peers");
    assert_eq!(loaded_trusted, trusted);
}

// ============================================================================
// 3. Contacts, username resolution caching, blocked peers pagination, privacy
// ============================================================================

#[tokio::test]
async fn contacts_get_contacts_hash_and_not_modified_conformance() {
    let expected_hash: i64 = 0x2725_A747_DC17_9FF8_u64 as i64;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_srv = Arc::clone(&calls);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::contacts::GetContacts::CONSTRUCTOR_ID => {
            let req = tl_fns::contacts::GetContacts::from_bytes(&rpc.body).unwrap();
            let c = calls_srv.fetch_add(1, Ordering::SeqCst);
            if c == 0 {
                assert_eq!(req.hash, 0, "initial contacts.getContacts must send hash=0");
                // Return unsorted contacts including >32-bit user ID 5_000_000_123 and self=1001.
                let resp = tl_enums::contacts::Contacts::Contacts(tl_types::contacts::Contacts {
                    contacts: vec![
                        tl_enums::Contact::Contact(tl_types::Contact {
                            user_id: 5_000_000_123,
                            mutual: true,
                        }),
                        tl_enums::Contact::Contact(tl_types::Contact {
                            user_id: 2002,
                            mutual: false,
                        }),
                        tl_enums::Contact::Contact(tl_types::Contact {
                            user_id: 1001,
                            mutual: true,
                        }),
                        tl_enums::Contact::Contact(tl_types::Contact {
                            user_id: 3003,
                            mutual: true,
                        }),
                    ],
                    saved_count: 4,
                    users: vec![
                        tl_self_user(1001, 0x1001),
                        tl_user(2002, 0x2002, "Contact Two", false),
                        tl_user(3003, 0x3003, "Contact Three", false),
                        tl_user(5_000_000_123, 0x5000, "Contact Wide", false),
                    ],
                });
                ServerAction::ok(&resp)
            } else {
                // Second call MUST send exact tdesktop 64-bit CountContactsHash!
                assert_eq!(
                    req.hash, expected_hash,
                    "wire contacts.getContacts hash must match tdesktop CountContactsHash"
                );
                ServerAction::ok(&tl_enums::contacts::Contacts::NotModified)
            }
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1001));
    let mut contacts = ContactsManager::new();
    assert_eq!(contacts.current_hash(), 0);

    let first = tokio::time::timeout(TEST_TIMEOUT, contacts.fetch_contacts(&rpc, &mut session))
        .await
        .unwrap()
        .unwrap();
    match first {
        ContactsFetchResult::Updated {
            contact_user_ids,
            saved_count,
        } => {
            assert_eq!(contact_user_ids.len(), 4);
            assert_eq!(saved_count, 4);
        }
        ContactsFetchResult::NotModified => panic!("expected Updated on first fetch"),
    }
    assert!(contacts.is_loaded());
    assert!(session.contacts_loaded());
    assert_eq!(
        session.user(UserId(1001)).unwrap().contact_status,
        ContactStatus::Contact,
        "self user in contacts list must be marked as Contact (apiwrap.cpp:941-948)"
    );
    assert_eq!(contacts.current_hash(), expected_hash);

    // Second fetch sends exact 64-bit hash and preserves cached state on contactsNotModified.
    let second = tokio::time::timeout(TEST_TIMEOUT, contacts.fetch_contacts(&rpc, &mut session))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second, ContactsFetchResult::NotModified);
    assert_eq!(
        contacts.cached_contact_ids(),
        &[1001, 2002, 3003, 5_000_000_123]
    );
    assert_eq!(contacts.saved_count(), 4);
}

#[tokio::test]
async fn contacts_resolve_username_caching_and_invalidation_conformance() {
    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID => {
            let req = tl_fns::contacts::ResolveUsername::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.username.to_ascii_lowercase(), "peeralpha");
            let mut u = tl_user_base(7001, 0x7001_CAFE, "Peer Alpha", false);
            u.username = Some("PeerAlpha".to_owned());
            let resp =
                tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
                    peer: tl_peer_user(7001),
                    chats: vec![],
                    users: vec![tl_enums::User::User(u)],
                });
            ServerAction::ok(&resp)
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut updates = init_updates(10, 0, 1_700_000_000, 1);

    // 1) Cache miss -> sends contacts.resolveUsername with '@' stripped.
    let r1 = tokio::time::timeout(
        TEST_TIMEOUT,
        resolve_username(&rpc, &mut session, "@PeerAlpha", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r1.peer_id, PeerId::from_user(UserId(7001)));
    assert!(!r1.from_cache);
    assert_eq!(
        server.rpcs_of::<tl_fns::contacts::ResolveUsername>().len(),
        1
    );

    // 2) Cache hit (case-insensitive + whitespace) -> ZERO new RPCs!
    let r2 = tokio::time::timeout(
        TEST_TIMEOUT,
        resolve_username(&rpc, &mut session, "  peeralpha ", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r2.peer_id, PeerId::from_user(UserId(7001)));
    assert!(r2.from_cache);
    assert_eq!(
        server.rpcs_of::<tl_fns::contacts::ResolveUsername>().len(),
        1
    );

    // 3) Non-empty referer MUST bypass local cache (`window_session_controller.cpp:585`).
    let r3 = tokio::time::timeout(
        TEST_TIMEOUT,
        resolve_username(&rpc, &mut session, "peeralpha", Some("ref_123")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r3.peer_id, PeerId::from_user(UserId(7001)));
    assert!(!r3.from_cache);
    let rpcs = server.rpcs_of::<tl_fns::contacts::ResolveUsername>();
    assert_eq!(rpcs.len(), 2);
    let req2 = tl_fns::contacts::ResolveUsername::from_bytes(&rpcs[1].body).unwrap();
    assert_eq!(req2.referer.as_deref(), Some("ref_123"));

    // 4) updateUserName changes username -> old handle no longer cached, new handle cached.
    let upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::UserName(tl_types::UpdateUserName {
            user_id: 7001,
            first_name: "Peer Alpha".to_owned(),
            last_name: String::new(),
            usernames: vec![tl_enums::Username::Username(tl_types::Username {
                editable: true,
                active: true,
                deletable: false,
                expired: false,
                username: "peerbeta".to_owned(),
            })],
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_050,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd).await;
    assert!(session.peer_by_username("peeralpha").is_none());
    let r4 = tokio::time::timeout(
        TEST_TIMEOUT,
        resolve_username(&rpc, &mut session, "@PeerBeta", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r4.peer_id, PeerId::from_user(UserId(7001)));
    assert!(r4.from_cache);
    assert_eq!(
        server.rpcs_of::<tl_fns::contacts::ResolveUsername>().len(),
        2
    );
}

#[tokio::test]
async fn contacts_get_blocked_pagination_and_update_peer_blocked_conformance() {
    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::contacts::GetBlocked::CONSTRUCTOR_ID => {
            let req = tl_fns::contacts::GetBlocked::from_bytes(&rpc.body).unwrap();
            assert!(!req.my_stories_from);
            if req.offset == 0 {
                assert_eq!(req.limit, BLOCKED_FIRST_SLICE);
                let blocked: Vec<_> = (0..16i64)
                    .map(|i| {
                        tl_enums::PeerBlocked::PeerBlocked(tl_types::PeerBlocked {
                            peer_id: tl_peer_user(8000 + i),
                            date: 1_700_000_000 + i as i32,
                        })
                    })
                    .collect();
                let users: Vec<_> = (0..16i64)
                    .map(|i| tl_user(8000 + i, 0x8000 + i, &format!("Blocked {i}"), false))
                    .collect();
                let resp = tl_enums::contacts::Blocked::Slice(tl_types::contacts::BlockedSlice {
                    count: 20,
                    blocked,
                    chats: vec![],
                    users,
                });
                ServerAction::ok(&resp)
            } else {
                assert_eq!(req.offset, 16);
                assert_eq!(req.limit, BLOCKED_PER_PAGE);
                let blocked: Vec<_> = (16..20i64)
                    .map(|i| {
                        tl_enums::PeerBlocked::PeerBlocked(tl_types::PeerBlocked {
                            peer_id: tl_peer_user(8000 + i),
                            date: 1_700_000_000 + i as i32,
                        })
                    })
                    .collect();
                let users: Vec<_> = (16..20i64)
                    .map(|i| tl_user(8000 + i, 0x8000 + i, &format!("Blocked {i}"), false))
                    .collect();
                let resp = tl_enums::contacts::Blocked::Blocked(tl_types::contacts::Blocked {
                    blocked,
                    chats: vec![],
                    users,
                });
                ServerAction::ok(&resp)
            }
        }
        tl_fns::contacts::Block::CONSTRUCTOR_ID | tl_fns::contacts::Unblock::CONSTRUCTOR_ID => {
            ServerAction::bool_true()
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut blocked_mgr = BlockedPeersManager::new();
    let mut updates = init_updates(10, 0, 1_700_000_000, 1);

    // Page 1: offset 0, limit 16 -> blockedSlice(count=20).
    let p1 = tokio::time::timeout(TEST_TIMEOUT, blocked_mgr.load_page(&rpc, &mut session, 0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p1.list.len(), 16);
    assert_eq!(p1.total, 20);
    assert!(!blocked_mgr.is_complete());

    // Page 2: offset 16, limit 40 -> blocked(4 remaining).
    let p2 = tokio::time::timeout(TEST_TIMEOUT, blocked_mgr.load_page(&rpc, &mut session, 16))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p2.list.len(), 4);
    assert!(blocked_mgr.is_complete());
    assert_eq!(blocked_mgr.slice().unwrap().list.len(), 20);
    assert_eq!(
        session.user(UserId(8019)).unwrap().common.block_status,
        BlockStatus::Blocked
    );

    // Unblock peer 8000 via RPC.
    let p8000 = PeerId::from_user(UserId(8000));
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, blocked_mgr.unblock(&rpc, &mut session, p8000))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        session.user(UserId(8000)).unwrap().common.block_status,
        BlockStatus::NotBlocked
    );
    assert!(
        !session
            .user(UserId(8000))
            .unwrap()
            .flags
            .contains(UserFlags::BLOCKED)
    );

    // Re-block peer 8000 via `updatePeerBlocked` through UpdatesHandler.
    let upd_block = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::PeerBlocked(tl_types::UpdatePeerBlocked {
            blocked: true,
            blocked_my_stories_from: false,
            peer_id: tl_peer_user(8000),
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_500,
        seq: 0,
    });
    apply_updates(&mut updates, &rpc, &mut session, upd_block).await;
    assert_eq!(
        session.user(UserId(8000)).unwrap().common.block_status,
        BlockStatus::Blocked
    );
    assert!(
        session
            .user(UserId(8000))
            .unwrap()
            .flags
            .contains(UserFlags::BLOCKED)
    );
}

#[tokio::test]
async fn account_privacy_get_set_roundtrip_and_update_privacy_conformance() {
    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        tl_fns::account::GetPrivacy::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetPrivacy::from_bytes(&rpc.body).unwrap();
            match req.key {
                tl_enums::InputPrivacyKey::StatusTimestamp => {
                    let resp = tl_enums::account::PrivacyRules::PrivacyRules(
                        tl_types::account::PrivacyRules {
                            rules: vec![
                                tl_enums::PrivacyRule::PrivacyValueAllowUsers(
                                    tl_types::PrivacyValueAllowUsers { users: vec![9001] },
                                ),
                                tl_enums::PrivacyRule::PrivacyValueAllowContacts,
                            ],
                            chats: vec![],
                            users: vec![tl_user(9001, 0x9001, "Allowed Peer", false)],
                        },
                    );
                    ServerAction::ok(&resp)
                }
                tl_enums::InputPrivacyKey::ChatInvite => {
                    // Refetch response triggered by `updatePrivacy` when chat 9050 was unloaded.
                    let resp = tl_enums::account::PrivacyRules::PrivacyRules(
                        tl_types::account::PrivacyRules {
                            rules: vec![
                                tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(
                                    tl_types::PrivacyValueAllowChatParticipants {
                                        chats: vec![9050],
                                    },
                                ),
                                tl_enums::PrivacyRule::PrivacyValueDisallowAll,
                            ],
                            chats: vec![tl_chat(9050, "Unloaded Privacy Chat")],
                            users: vec![],
                        },
                    );
                    ServerAction::ok(&resp)
                }
                other => ServerAction::err(400, format!("UNEXPECTED_PRIVACY_KEY_{other:?}")),
            }
        }
        tl_fns::account::SetPrivacy::CONSTRUCTOR_ID => {
            let req = tl_fns::account::SetPrivacy::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.key, tl_enums::InputPrivacyKey::StatusTimestamp);
            // Verify exact tdesktop `RulesToTL` ordering (`api/api_user_privacy.cpp:28-106`):
            // 1. AllowUsers, 2. AllowChatParticipants, 3. AllowPremium, 4. AllowBots,
            // 5. DisallowUsers, 6. DisallowChatParticipants, 7. DisallowBots, 8. Option.
            assert_eq!(req.rules.len(), 8);
            assert!(matches!(
                req.rules[0],
                tl_enums::InputPrivacyRule::InputPrivacyValueAllowUsers(_)
            ));
            assert!(matches!(
                req.rules[1],
                tl_enums::InputPrivacyRule::InputPrivacyValueAllowChatParticipants(_)
            ));
            assert!(matches!(
                req.rules[2],
                tl_enums::InputPrivacyRule::InputPrivacyValueAllowPremium
            ));
            assert!(matches!(
                req.rules[3],
                tl_enums::InputPrivacyRule::InputPrivacyValueAllowBots
            ));
            assert!(matches!(
                req.rules[4],
                tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(_)
            ));
            assert!(matches!(
                req.rules[5],
                tl_enums::InputPrivacyRule::InputPrivacyValueDisallowChatParticipants(_)
            ));
            assert!(matches!(
                req.rules[6],
                tl_enums::InputPrivacyRule::InputPrivacyValueDisallowBots
            ));
            assert!(matches!(
                req.rules[7],
                tl_enums::InputPrivacyRule::InputPrivacyValueAllowContacts
            ));

            // Echo back equivalent TL `PrivacyRule`s.
            let resp =
                tl_enums::account::PrivacyRules::PrivacyRules(tl_types::account::PrivacyRules {
                    rules: vec![
                        tl_enums::PrivacyRule::PrivacyValueAllowUsers(
                            tl_types::PrivacyValueAllowUsers { users: vec![9001] },
                        ),
                        tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(
                            tl_types::PrivacyValueAllowChatParticipants { chats: vec![9010] },
                        ),
                        tl_enums::PrivacyRule::PrivacyValueAllowPremium,
                        tl_enums::PrivacyRule::PrivacyValueAllowBots,
                        tl_enums::PrivacyRule::PrivacyValueDisallowUsers(
                            tl_types::PrivacyValueDisallowUsers { users: vec![9002] },
                        ),
                        tl_enums::PrivacyRule::PrivacyValueDisallowChatParticipants(
                            tl_types::PrivacyValueDisallowChatParticipants { chats: vec![9020] },
                        ),
                        tl_enums::PrivacyRule::PrivacyValueDisallowBots,
                        tl_enums::PrivacyRule::PrivacyValueAllowContacts,
                    ],
                    chats: vec![
                        tl_channel(9010, 0x9010, "Allowed Chan", false, false),
                        tl_chat(9020, "Disallowed Chat"),
                    ],
                    users: vec![
                        tl_user(9001, 0x9001, "Allowed Peer", false),
                        tl_user(9002, 0x9002, "Disallowed Peer", false),
                    ],
                });
            ServerAction::ok(&resp)
        }
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_{other:#x}")),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut privacy = UserPrivacyManager::new();
    let mut updates = init_updates(10, 0, 1_700_000_000, 1);

    // 1) All 14 Layer 233 privacy keys map cleanly.
    let all_keys = [
        PrivacyKey::PhoneNumber,
        PrivacyKey::AddedByPhone,
        PrivacyKey::LastSeen,
        PrivacyKey::Calls,
        PrivacyKey::Invites,
        PrivacyKey::CallsPeer2Peer,
        PrivacyKey::Forwards,
        PrivacyKey::ProfilePhoto,
        PrivacyKey::Voices,
        PrivacyKey::About,
        PrivacyKey::Birthday,
        PrivacyKey::GiftsAutoSave,
        PrivacyKey::NoPaidMessages,
        PrivacyKey::SavedMusic,
    ];
    for key in all_keys {
        let _ = privacy_key_to_tl(key);
    }
    assert_eq!(
        privacy_key_from_tl(&tl_enums::PrivacyKey::StatusTimestamp),
        Some(PrivacyKey::LastSeen)
    );

    // 2) Implicit `Nobody` when server sends empty rule list, and first-occurrence conflict precedence.
    let empty_parsed = privacy_rules_from_tl(&[], &session);
    assert_eq!(empty_parsed.option, PrivacyOption::Nobody);
    let conflict_parsed = privacy_rules_from_tl(
        &[
            tl_enums::PrivacyRule::PrivacyValueAllowUsers(tl_types::PrivacyValueAllowUsers {
                users: vec![9001],
            }),
            tl_enums::PrivacyRule::PrivacyValueDisallowUsers(tl_types::PrivacyValueDisallowUsers {
                users: vec![9001, 9002],
            }),
            tl_enums::PrivacyRule::PrivacyValueAllowContacts,
            tl_enums::PrivacyRule::PrivacyValueAllowAll,
        ],
        &session,
    );
    assert_eq!(conflict_parsed.option, PrivacyOption::Contacts);
    assert_eq!(
        conflict_parsed.always.peers,
        vec![PeerId::from_user(UserId(9001))]
    );
    assert_eq!(
        conflict_parsed.never.peers,
        vec![PeerId::from_user(UserId(9002))]
    );

    // 3) account.getPrivacy -> loads rule & user 9001.
    let loaded = tokio::time::timeout(
        TEST_TIMEOUT,
        privacy.reload(&rpc, &mut session, PrivacyKey::LastSeen),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loaded.option, PrivacyOption::Contacts);
    assert_eq!(loaded.always.peers, vec![PeerId::from_user(UserId(9001))]);

    // 4) account.setPrivacy full round-trip with users, channel, basic chat, premiums, miniapps.
    apply_tl_users_and_chats(
        &mut session,
        &[tl_user(9002, 0x9002, "Disallowed Peer", false)],
        &[
            tl_channel(9010, 0x9010, "Allowed Chan", false, false),
            tl_chat(9020, "Disallowed Chat"),
        ],
    );
    let complex_rule = PrivacyRule {
        option: PrivacyOption::Contacts,
        always: PrivacyExceptions {
            peers: vec![
                PeerId::from_user(UserId(9001)),
                PeerId::from_channel(ChannelId(9010)),
            ],
            premiums: true,
            miniapps: true,
        },
        never: PrivacyExceptions {
            peers: vec![
                PeerId::from_user(UserId(9002)),
                PeerId::from_chat(ChatId(9020)),
            ],
            premiums: false,
            miniapps: true,
        },
        ignore_always: false,
        ignore_never: false,
    };
    let saved = tokio::time::timeout(
        TEST_TIMEOUT,
        privacy.save(&rpc, &mut session, PrivacyKey::LastSeen, &complex_rule),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(saved, complex_rule);
    assert_eq!(privacy_rules_to_tl(&saved, &session).len(), 8);

    // 5) updatePrivacy with ALL chat exceptions already loaded -> applies directly (NO getPrivacy RPC).
    let get_before = server.rpcs_of::<tl_fns::account::GetPrivacy>().len();
    let upd_direct = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::Privacy(tl_types::UpdatePrivacy {
            key: tl_enums::PrivacyKey::PhoneCall,
            rules: vec![
                tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(
                    tl_types::PrivacyValueAllowChatParticipants { chats: vec![9020] },
                ),
                tl_enums::PrivacyRule::PrivacyValueDisallowAll,
            ],
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_200,
        seq: 0,
    });
    let evs_direct = apply_updates(&mut updates, &rpc, &mut session, upd_direct).await;
    assert_eq!(
        server.rpcs_of::<tl_fns::account::GetPrivacy>().len(),
        get_before
    );
    assert!(evs_direct.iter().any(|e| matches!(
        e,
        UpdateEvent::PrivacyUpdated {
            key: PrivacyKey::Calls,
            rule,
        } if rule.option == PrivacyOption::Nobody
            && rule.always.peers == vec![PeerId::from_chat(ChatId(9020))]
    )));

    // 6) updatePrivacy with UNLOADED chat 9050 -> MUST trigger `account.getPrivacy(ChatInvite)`!
    let upd_refetch = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::Privacy(tl_types::UpdatePrivacy {
            key: tl_enums::PrivacyKey::ChatInvite,
            rules: vec![
                tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(
                    tl_types::PrivacyValueAllowChatParticipants { chats: vec![9050] },
                ),
                tl_enums::PrivacyRule::PrivacyValueDisallowAll,
            ],
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_210,
        seq: 0,
    });
    let evs_refetch = apply_updates(&mut updates, &rpc, &mut session, upd_refetch).await;
    assert_eq!(
        server.rpcs_of::<tl_fns::account::GetPrivacy>().len(),
        get_before + 1
    );
    assert!(session.chat_loaded(ChatId(9050)));
    assert!(evs_refetch.iter().any(|e| matches!(
        e,
        UpdateEvent::PrivacyUpdated {
            key: PrivacyKey::Invites,
            rule,
        } if rule.option == PrivacyOption::Nobody
            && rule.always.peers == vec![PeerId::from_chat(ChatId(9050))]
    )));
}

// ============================================================================
// 4. Exact 64-bit hashes (`api/api_hash.h:36-70`)
// ============================================================================

#[test]
fn api_hash_exact_tdesktop_vectors_conformance() {
    // Empty sequence -> 0.
    assert_eq!(hash_finalize(hash_init()), 0);
    assert_eq!(count_hash_u64([]), 0);
    assert_eq!(count_contacts_hash(&[]), 0);

    // Single value [100] -> 0x64.
    assert_eq!(count_hash_u64([100]), 0x64);

    // Two values [100, 200] -> 0x0000_0312_0000_012A.
    assert_eq!(count_hash_u64([100, 200]), 0x0000_0312_0000_012A_u64);

    // Signed i32 [-1, 1]: tdesktop widens int32 -> int64 -> uint64 (sign-extended!).
    assert_eq!(count_hash_i32([-1, 1]), 0xF000_0780_0000_0001_u64);
    let mut h_i32 = hash_init();
    hash_update_i32(&mut h_i32, -1);
    hash_update_i32(&mut h_i32, 1);
    assert_eq!(hash_finalize(h_i32), 0xF000_0780_0000_0001_u64);

    // Unsigned u32 [0xFFFF_FFFF, 1]: zero-extended to u64, distinct from i32(-1)!
    let mut h_u32 = hash_init();
    hash_update_u32(&mut h_u32, u32::MAX);
    hash_update_u32(&mut h_u32, 1);
    assert_eq!(hash_finalize(h_u32), 0xF000_3C00_F000_0781_u64);

    // Streaming i64 / u64 equivalence.
    let docs_u64 = [
        1_234_567_890_123_456_789_u64,
        9_876_543_210_987_654_321_u64,
        42_u64,
    ];
    assert_eq!(count_hash_u64(docs_u64), 0x56A5_3B04_A03A_7E14_u64);
    let docs_i64: Vec<i64> = docs_u64.iter().map(|&v| v as i64).collect();
    assert_eq!(count_hash_i64(docs_i64), 0x56A5_3B04_A03A_7E14_u64);
    let mut h_stream = hash_init();
    hash_update_u64(&mut h_stream, docs_u64[0]);
    hash_update_i64(&mut h_stream, docs_u64[1] as i64);
    hash_update_u64(&mut h_stream, docs_u64[2]);
    assert_eq!(hash_finalize(h_stream), 0x56A5_3B04_A03A_7E14_u64);

    // Contacts hash sorts ascending + deduplicates + handles >32-bit user IDs.
    assert_eq!(
        count_contacts_hash(&[5_000_000_123, 2002, 1001, 3003, 2002]),
        0x2725_A747_DC17_9FF8_u64 as i64
    );
}
