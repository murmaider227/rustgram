//! Byte-level tdesktop conformance suite for WP-66 (Contacts, Spam Bar, Contact Tokens,
//! Fee-Waiver Tokens, Birthdays, Contact Notes, Own/Contact Profile Photos & `help.hidePromoData`)
//! against an in-process scripted fake Telegram MTProto 2.0 server.
//!
//! Covers all 18 WP-66 RPCs end-to-end through `tg-app-core` backend traits -> `LiveSession` ->
//! `tg_app_live::contacts_extras` -> `ErasedRpcClient` -> `MtprotoRpc` -> `tg_mtproto::Client`
//! (encrypted MTProto 2.0 framing) -> `FakeConnection` -> `ScriptedServer`:
//!  1. `contacts.deleteContacts#096a0e00`
//!  2. `contacts.acceptContact#f831a20f`
//!  3. `contacts.exportContactToken#f8654027`
//!  4. `contacts.importContactToken#13005788`
//!  5. `contacts.exportNoPaidMessagesToken#83c586ae`
//!  6. `contacts.importNoPaidMessagesToken#35a7c6ca`
//!  7. `contacts.updateContactNote#139f63fb`
//!  8. `contacts.getBirthdays#daeda864`
//!  9. `contacts.blockFromReplies#29a8962c`
//! 10. `contacts.setBlocked#94c65c76`
//! 11. `contacts.toggleTopPeers#8514bdda`
//! 12. `contacts.getStatuses#c4a353ee`
//! 13. `contacts.getSponsoredPeers#b6c8c393`
//! 14. `messages.reportSpam#cf1592db`
//! 15. `messages.hidePeerSettingsBar#4facb138`
//! 16. `photos.deletePhotos#87cf7f2f`
//! 17. `photos.uploadContactProfilePhoto#e14c4a71`
//! 18. `help.hidePromoData#1e251c95`
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//! Zero network access; synthetic test-only IDs and ephemeral test RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::ApiCredentials;
use tg_api::convert::apply_tl_users_and_chats;
use tg_app_core::core_types::dto::contacts_extras::{
    BlockFromRepliesRequest, ContactProfilePhotoMode, ContactProfilePhotoUpload,
};
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::{EmojiUserpicRequest, InfoBackend, ProfilePhotoRef};
use tg_app_core::settings::backend::SettingsBackend;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_core::stories::backend::StoriesBackend;
use tg_app_live::convert::{from_data_peer, to_data_peer};
use tg_app_live::{LiveSession, start_mtproto_erased_on};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{
    BlockStatus, ChannelId, ContactStatus, PeerBarSettings, PeerId as DataPeerId, PhotoId,
    UserFlags, UserId as DataUserId,
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
use tg_text::{EntityInText, EntityType, TextWithEntities as UiText};
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
    _body: Vec<u8>,
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
    _dc_id: DcId,
    _tx: mpsc::UnboundedSender<tl_enums::Updates>,
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
            .insert(
                conn_id,
                LiveConnPush {
                    _dc_id: dc_id,
                    _tx: push_tx,
                },
            );

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
                                    _body: inner_body,
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
// Synthetic TL Fixtures & Harness
// ============================================================================

fn tl_user(
    id: i64,
    first: &str,
    username: &str,
    is_self: bool,
    contact: bool,
    stars_per_msg: Option<i64>,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact,
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
        apply_min_photo: true,
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
        send_paid_messages_stars: stars_per_msg,
        linked_community_id: None,
    }
}

fn tl_channel(id: i64, title: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast: true,
        verified: true,
        megagroup: false,
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
        participants_count: Some(42),
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

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_500,
        seq: 0,
    })
}

fn default_bootstrap_reply(ctor: u32) -> Option<ServerAction> {
    if ctor == tl_fns::updates::GetState::CONSTRUCTOR_ID {
        return Some(ServerAction::ok(&tl_enums::updates::State::State(
            tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_700_000_000,
                seq: 1,
                unread_count: 0,
            },
        )));
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
                    pts: 10,
                    qts: 0,
                    date: 1_700_000_000,
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
    None
}

struct MtprotoHarness {
    _rt: tokio::runtime::Runtime,
    _client: Client,
    server: ScriptedServer,
    live: LiveSession,
    u2002: PeerId,
    u2003: PeerId,
    ch5001: PeerId,
}

impl MtprotoHarness {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync + 'static,
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let server = ScriptedServer::new(move |srv, req| {
            if let Some(boot) = default_bootstrap_reply(req.ctor) {
                return boot;
            }
            handler(srv, req)
        });
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
                    SessionRuntimeConfig {
                        run_get_difference_on_start: false,
                        load_initial_dialogs: false,
                        fetch_self_user_on_start: false,
                        idle_ping_enabled: false,
                        ..SessionRuntimeConfig::default()
                    },
                )
                .expect("start_mtproto_erased_on"),
            );
            tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
                .await
                .expect("wait_ready timeout")
                .expect("wait_ready ok");
            let live = LiveSession::new(runtime, 660);
            (client, live)
        });

        let self_u = tl_user(1001, "Self", "self_u", true, false, None);
        let u2002_tl = tl_user(2002, "ContactOne", "contact_one", false, true, Some(25));
        let u2003_tl = tl_user(2003, "StrangerTwo", "stranger_two", false, false, None);
        let ch5001_tl = tl_channel(5001, "PromoChan");

        live.write_session(|session| {
            apply_tl_users_and_chats(
                session,
                &[
                    tl_enums::User::User(self_u),
                    tl_enums::User::User(u2002_tl),
                    tl_enums::User::User(u2003_tl),
                ],
                &[tl_enums::Chat::Channel(ch5001_tl)],
            );
            if let Some(u) = session.user_mut(DataUserId(1001)) {
                u.common.userpic_photo_id = PhotoId::from_tl(88_001);
            }
            if let Some(p) = session.peer_mut(DataPeerId::from_user(DataUserId(2003))) {
                p.common_mut().bar_settings = PeerBarSettings::REPORT_SPAM
                    | PeerBarSettings::ADD_CONTACT
                    | PeerBarSettings::SHARE_CONTACT;
            }
            if let Some(p) = session.peer_mut(DataPeerId::from_channel(ChannelId(5001))) {
                p.common_mut().bar_settings = PeerBarSettings::REPORT_SPAM;
            }
        });

        Self {
            _rt: rt,
            _client: client,
            server,
            live,
            u2002: from_data_peer(DataPeerId::from_user(DataUserId(2002))),
            u2003: from_data_peer(DataPeerId::from_user(DataUserId(2003))),
            ch5001: from_data_peer(DataPeerId::from_channel(ChannelId(5001))),
        }
    }

    fn read_session<R>(&self, f: impl FnOnce(&tg_data::Session) -> R) -> R {
        self._rt.block_on(async {
            let guard = self.live.runtime().session().read().await;
            f(&guard)
        })
    }
}

#[test]
fn wp66_all_18_rpcs_exact_tl_bytes_over_encrypted_mtproto_2() {
    let mut h = MtprotoHarness::start(|_srv, req| match req.ctor {
        c if c == tl_fns::contacts::DeleteContacts::CONSTRUCTOR_ID => {
            ServerAction::ok(&empty_updates())
        }
        c if c == tl_fns::contacts::AcceptContact::CONSTRUCTOR_ID => {
            ServerAction::ok(&empty_updates())
        }
        c if c == tl_fns::messages::ReportSpam::CONSTRUCTOR_ID => ServerAction::ok(&true),
        c if c == tl_fns::messages::HidePeerSettingsBar::CONSTRUCTOR_ID => ServerAction::ok(&true),
        c if c == tl_fns::contacts::ExportContactToken::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::ExportedContactToken::ExportedContactToken(tl_types::ExportedContactToken {
                url: "https://t.me/contact/mtproto_tok".to_owned(),
                expires: 1_900_000_000,
            }),
        ),
        c if c == tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::User::User(tl_user(2099, "Imported", "imp_user", false, false, None)),
        ),
        c if c == tl_fns::contacts::ExportNoPaidMessagesToken::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::ExportedNoPaidMessagesToken::ExportedNoPaidMessagesToken(
                tl_types::ExportedNoPaidMessagesToken {
                    token: "waiver_tok".to_owned(),
                    url: "https://t.me/self_u?free=waiver_tok".to_owned(),
                    expires: Some(1_900_003_600),
                },
            ),
        ),
        c if c == tl_fns::contacts::ImportNoPaidMessagesToken::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::User::User(tl_user(
                2002,
                "ContactOne",
                "contact_one",
                false,
                true,
                None,
            )))
        }
        c if c == tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID => ServerAction::ok(&true),
        c if c == tl_fns::contacts::GetBirthdays::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::contacts::ContactBirthdays::ContactBirthdays(
                tl_types::contacts::ContactBirthdays {
                    contacts: vec![tl_enums::ContactBirthday::ContactBirthday(
                        tl_types::ContactBirthday {
                            contact_id: 2002,
                            birthday: tl_enums::Birthday::Birthday(tl_types::Birthday {
                                day: 24,
                                month: 11,
                                year: Some(1992),
                            }),
                        },
                    )],
                    users: vec![tl_enums::User::User(tl_user(
                        2002,
                        "ContactOne",
                        "contact_one",
                        false,
                        true,
                        None,
                    ))],
                },
            ))
        }
        c if c == tl_fns::contacts::BlockFromReplies::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::PeerBlocked(tl_types::UpdatePeerBlocked {
                    blocked: true,
                    blocked_my_stories_from: false,
                    peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 2003 }),
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_600,
                seq: 0,
            }))
        }
        c if c == tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID => ServerAction::ok(&true),
        c if c == tl_fns::contacts::ToggleTopPeers::CONSTRUCTOR_ID => ServerAction::ok(&true),
        c if c == tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID => {
            ServerAction::ok(&vec![tl_enums::ContactStatus::ContactStatus(
                tl_types::ContactStatus {
                    user_id: 2002,
                    status: tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
                        expires: now_unix_secs() + 600,
                    }),
                },
            )])
        }
        c if c == tl_fns::contacts::GetSponsoredPeers::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::contacts::SponsoredPeers::SponsoredPeers(
                tl_types::contacts::SponsoredPeers {
                    peers: vec![tl_enums::SponsoredPeer::SponsoredPeer(
                        tl_types::SponsoredPeer {
                            random_id: vec![0xAA, 0xBB],
                            peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                                channel_id: 5001,
                            }),
                            sponsor_info: Some("Ad Info".to_owned()),
                            additional_info: None,
                        },
                    )],
                    chats: vec![tl_enums::Chat::Channel(tl_channel(5001, "PromoChan"))],
                    users: Vec::new(),
                },
            ))
        }
        c if c == tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID => {
            ServerAction::ok(&vec![88_001i64])
        }
        c if c == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID => ServerAction::ok(&true),
        c if c == tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
                photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 99_001 }),
                users: Vec::new(),
            }))
        }
        c if c == tl_fns::help::HidePromoData::CONSTRUCTOR_ID => ServerAction::ok(&true),
        other => ServerAction::err(400, format!("UNEXPECTED_CTOR_0x{other:08x}")),
    });

    // 1. contacts.deleteContacts
    InfoBackend::delete_contact(&mut h.live, h.u2002).expect("delete_contact");
    let del_rec = h.server.rpcs_of::<tl_fns::contacts::DeleteContacts>();
    assert_eq!(del_rec.len(), 1);
    assert_eq!(
        del_rec[0].body,
        tl_fns::contacts::DeleteContacts {
            id: vec![tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 2002,
                access_hash: 2002 * 29 + 11,
            })],
        }
        .to_bytes()
    );
    h.read_session(|s| {
        assert_eq!(
            s.user(DataUserId(2002)).unwrap().contact_status,
            ContactStatus::NotContact
        );
    });

    // 2. contacts.acceptContact
    HistoryBackend::accept_contact(&mut h.live, h.u2003).expect("accept_contact");
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::AcceptContact>()[0].body,
        tl_fns::contacts::AcceptContact {
            id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 2003,
                access_hash: 2003 * 29 + 11,
            }),
        }
        .to_bytes()
    );
    h.read_session(|s| {
        assert!(
            s.peer(to_data_peer(h.u2003))
                .unwrap()
                .common()
                .bar_settings
                .is_empty()
        );
    });

    // 3. messages.reportSpam
    assert!(HistoryBackend::report_spam(&mut h.live, h.ch5001).expect("report_spam"));
    assert_eq!(
        h.server.rpcs_of::<tl_fns::messages::ReportSpam>()[0].body,
        tl_fns::messages::ReportSpam {
            peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                channel_id: 5001,
                access_hash: 5001 * 17 + 5,
            }),
        }
        .to_bytes()
    );
    h.read_session(|s| {
        assert!(
            s.peer(to_data_peer(h.ch5001))
                .unwrap()
                .common()
                .bar_settings
                .is_empty()
        );
    });

    // 4. messages.hidePeerSettingsBar
    assert!(HistoryBackend::hide_peer_settings_bar(&mut h.live, h.u2003).expect("hide_bar"));
    assert_eq!(
        h.server.rpcs_of::<tl_fns::messages::HidePeerSettingsBar>()[0].body,
        tl_fns::messages::HidePeerSettingsBar {
            peer: tl_enums::InputPeer::User(tl_types::InputPeerUser {
                user_id: 2003,
                access_hash: 2003 * 29 + 11,
            }),
        }
        .to_bytes()
    );

    // 5. contacts.exportContactToken
    let tok = InfoBackend::export_contact_token(&h.live).expect("export_contact_token");
    assert_eq!(tok.url, "https://t.me/contact/mtproto_tok");
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::ExportContactToken>()[0].body,
        tl_fns::contacts::ExportContactToken {}.to_bytes()
    );

    // 6. contacts.importContactToken
    let imp = InfoBackend::import_contact_token(&mut h.live, "mtproto_tok").expect("import_tok");
    assert_eq!(imp, from_data_peer(DataPeerId::from_user(DataUserId(2099))));
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::ImportContactToken>()[0].body,
        tl_fns::contacts::ImportContactToken {
            token: "mtproto_tok".to_owned(),
        }
        .to_bytes()
    );

    // 7. contacts.exportNoPaidMessagesToken
    let fee_tok =
        InfoBackend::export_no_paid_messages_token(&h.live, false, true).expect("export_fee_tok");
    assert_eq!(fee_tok.token, "waiver_tok");
    assert_eq!(
        h.server
            .rpcs_of::<tl_fns::contacts::ExportNoPaidMessagesToken>()[0]
            .body,
        tl_fns::contacts::ExportNoPaidMessagesToken {
            revoke: false,
            temporary: true,
        }
        .to_bytes()
    );

    // 8. contacts.importNoPaidMessagesToken
    let fee_res = InfoBackend::import_no_paid_messages_token(&mut h.live, h.u2002, "waiver_tok")
        .expect("import_fee_tok");
    assert!(fee_res.price_removed);
    assert_eq!(
        h.server
            .rpcs_of::<tl_fns::contacts::ImportNoPaidMessagesToken>()[0]
            .body,
        tl_fns::contacts::ImportNoPaidMessagesToken {
            user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 2002,
                access_hash: 2002 * 29 + 11,
            }),
            token: "waiver_tok".to_owned(),
        }
        .to_bytes()
    );

    // 9. contacts.updateContactNote
    let note = UiText::with_entities(
        "Core dev",
        vec![EntityInText::new(EntityType::Italic, 0, 4)],
    );
    assert!(InfoBackend::update_contact_note(&mut h.live, h.u2002, note.clone()).expect("note"));
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::UpdateContactNote>()[0].body,
        tl_fns::contacts::UpdateContactNote {
            id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 2002,
                access_hash: 2002 * 29 + 11,
            }),
            note: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Core dev".to_owned(),
                entities: vec![tl_enums::MessageEntity::Italic(
                    tl_types::MessageEntityItalic {
                        offset: 0,
                        length: 4,
                    },
                )],
            }),
        }
        .to_bytes()
    );
    assert_eq!(InfoBackend::contact_note(&h.live, h.u2002), Some(note));

    // 10. contacts.getBirthdays
    let bdays = DialogsBackend::contact_birthdays(&h.live, true).expect("birthdays");
    assert_eq!(bdays.len(), 1);
    assert_eq!(bdays[0].birthday.day, 24);
    assert_eq!(bdays[0].birthday.month, 11);
    assert_eq!(bdays[0].birthday.year, Some(1992));
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::GetBirthdays>()[0].body,
        tl_fns::contacts::GetBirthdays {}.to_bytes()
    );

    // 11. contacts.blockFromReplies
    HistoryBackend::block_from_replies(
        &mut h.live,
        BlockFromRepliesRequest {
            msg_id: MsgId(512),
            delete_message: true,
            delete_history: true,
            report_spam: false,
        },
    )
    .expect("block_from_replies");
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::BlockFromReplies>()[0].body,
        tl_fns::contacts::BlockFromReplies {
            delete_message: true,
            delete_history: true,
            report_spam: false,
            msg_id: 512,
        }
        .to_bytes()
    );
    h.read_session(|s| {
        assert_eq!(
            s.peer(to_data_peer(h.u2003)).unwrap().common().block_status,
            BlockStatus::Blocked
        );
    });

    // 12. contacts.setBlocked (both `my_stories_from = true` and `false`)
    let u2002_data = DataPeerId::from_user(DataUserId(2002));
    assert!(
        StoriesBackend::save_stories_blocklist(&mut h.live, &[u2002_data])
            .expect("save_stories_blocklist")
    );
    assert!(
        SettingsBackend::set_blocked_peers(&mut h.live, &[h.u2002.0], false)
            .expect("set_blocked_peers")
    );
    let sb = h.server.rpcs_of::<tl_fns::contacts::SetBlocked>();
    assert_eq!(sb.len(), 2);
    assert_eq!(
        sb[0].body,
        tl_fns::contacts::SetBlocked {
            my_stories_from: true,
            id: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                user_id: 2002,
                access_hash: 2002 * 29 + 11,
            })],
            limit: 1,
        }
        .to_bytes()
    );
    assert_eq!(
        sb[1].body,
        tl_fns::contacts::SetBlocked {
            my_stories_from: false,
            id: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                user_id: 2002,
                access_hash: 2002 * 29 + 11,
            })],
            limit: 1,
        }
        .to_bytes()
    );

    // 13. contacts.toggleTopPeers
    assert!(DialogsBackend::toggle_top_peers(&mut h.live, false).expect("toggle_top_peers"));
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::ToggleTopPeers>()[0].body,
        tl_fns::contacts::ToggleTopPeers { enabled: false }.to_bytes()
    );

    // 14. contacts.getStatuses
    let st = DialogsBackend::refresh_contact_statuses(&mut h.live).expect("get_statuses");
    assert_eq!(st.len(), 1);
    assert!(st[0].is_online);
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::GetStatuses>()[0].body,
        tl_fns::contacts::GetStatuses {}.to_bytes()
    );

    // 15. contacts.getSponsoredPeers
    let sp = DialogsBackend::sponsored_peers(&h.live, "promo").expect("get_sponsored_peers");
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0].peer_id, h.ch5001);
    assert_eq!(sp[0].random_id, vec![0xAA, 0xBB]);
    assert_eq!(
        h.server.rpcs_of::<tl_fns::contacts::GetSponsoredPeers>()[0].body,
        tl_fns::contacts::GetSponsoredPeers {
            q: "promo".to_owned(),
        }
        .to_bytes()
    );

    // 16. photos.deletePhotos
    let deleted_photos = InfoBackend::delete_profile_photos(
        &mut h.live,
        &[ProfilePhotoRef {
            id: 88_001,
            access_hash: 777_001,
            file_reference: vec![9, 8, 7],
        }],
    )
    .expect("delete_profile_photos");
    assert_eq!(deleted_photos, vec![88_001]);
    assert_eq!(
        h.server.rpcs_of::<tl_fns::photos::DeletePhotos>()[0].body,
        tl_fns::photos::DeletePhotos {
            id: vec![tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                id: 88_001,
                access_hash: 777_001,
                file_reference: vec![9, 8, 7],
            })],
        }
        .to_bytes()
    );
    h.read_session(|s| {
        assert_eq!(
            s.user(DataUserId(1001)).unwrap().common.userpic_photo_id,
            PhotoId::ZERO
        );
    });

    // 17. photos.uploadContactProfilePhoto (video + video_start_ts, emoji_markup suggest, and clearPersonal)
    let pid_vid = InfoBackend::upload_contact_profile_photo(
        &mut h.live,
        h.u2002,
        ContactProfilePhotoUpload::video_photo(
            ContactProfilePhotoMode::Save,
            "clip.mp4",
            vec![0, 1, 2, 3],
            Some(1.5),
        ),
    )
    .expect("upload video contact photo");
    assert_eq!(pid_vid, 99_001);
    let pid_suggest = InfoBackend::upload_contact_profile_photo(
        &mut h.live,
        h.u2002,
        ContactProfilePhotoUpload::emoji_markup(
            ContactProfilePhotoMode::Suggest,
            EmojiUserpicRequest {
                emoji_id: 42_4242,
                emoji_glyph: "🔥".to_owned(),
                background_colors: vec![0x112233],
            },
        ),
    )
    .expect("suggest emoji contact photo");
    assert_eq!(pid_suggest, 99_001);
    let _ = InfoBackend::clear_contact_profile_photo(&mut h.live, h.u2002)
        .expect("clear personal photo");

    let up_rpcs = h
        .server
        .rpcs_of::<tl_fns::photos::UploadContactProfilePhoto>();
    assert_eq!(up_rpcs.len(), 3);
    let decoded_vid =
        tl_fns::photos::UploadContactProfilePhoto::from_bytes(&up_rpcs[0].body).unwrap();
    assert!(decoded_vid.save && !decoded_vid.suggest);
    assert!(decoded_vid.video.is_some());
    assert_eq!(decoded_vid.video_start_ts, Some(1.5));
    assert_eq!(
        up_rpcs[1].body,
        tl_fns::photos::UploadContactProfilePhoto {
            suggest: true,
            save: false,
            user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 2002,
                access_hash: 2002 * 29 + 11,
            }),
            file: None,
            video: None,
            video_start_ts: None,
            video_emoji_markup: Some(tl_enums::VideoSize::EmojiMarkup(
                tl_types::VideoSizeEmojiMarkup {
                    emoji_id: 42_4242,
                    background_colors: vec![0x112233],
                }
            )),
        }
        .to_bytes()
    );
    assert_eq!(
        up_rpcs[2].body,
        tl_fns::photos::UploadContactProfilePhoto {
            suggest: false,
            save: true,
            user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 2002,
                access_hash: 2002 * 29 + 11,
            }),
            file: None,
            video: None,
            video_start_ts: None,
            video_emoji_markup: None,
        }
        .to_bytes()
    );
    h.read_session(|s| {
        assert!(
            !s.user(DataUserId(2002))
                .unwrap()
                .flags
                .contains(UserFlags::PERSONAL_PHOTO)
        );
    });

    // 18. help.hidePromoData
    assert!(DialogsBackend::hide_promo_data(&mut h.live, h.ch5001).expect("hide_promo_data"));
    assert_eq!(
        h.server.rpcs_of::<tl_fns::help::HidePromoData>()[0].body,
        tl_fns::help::HidePromoData {
            peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                channel_id: 5001,
                access_hash: 5001 * 17 + 5,
            }),
        }
        .to_bytes()
    );
}
