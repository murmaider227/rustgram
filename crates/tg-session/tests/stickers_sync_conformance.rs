//! Byte-level tdesktop conformance tests for hash-synced server lists:
//! stickers, masks, custom emoji, featured/recent/faved stickers, saved GIFs,
//! reactions, emoji statuses & groups, emoji keywords, app config, peer colors,
//! wallpapers, cloud themes, attach menu bots, premium promo, saved ringtones,
//! quick replies, live update handling, refresh throttles, sticker set loading
//! & mutations, and `tg-storage` cache persistence.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses obviously fake credentials (`api_id = 1`, `api_hash = "0000...0000"`),
//!   synthetic peer/user/document IDs, and an ephemeral test-only RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{
    APP_CONFIG_REFRESH_MS, ApiCredentials, ApiError, AppConfigValue, CLOUD_RECENT_ATTACHED_SET_ID,
    CLOUD_RECENT_SET_ID, CLOUD_THEME_FORMAT, CachedListsSync, CachedStickerSet, DEFAULT_SET_ID,
    EMOJI_KEYWORDS_REFRESH_MS, EMOJI_STATUSES_REFRESH_MS, FAVED_SET_ID, InstallStickerSetOutcome,
    PEER_COLORS_REFRESH_MS, QUICK_REPLIES_REFRESH_LIMIT_MS, REACTIONS_FULL_LIST_REFRESH_MS,
    RECENT_EMOJI_STATUSES_DEBOUNCE_MS, RECENT_REACTIONS_DEBOUNCE_MS, RECENT_REACTIONS_LIMIT,
    STICKERS_UPDATE_TIMEOUT_MS, StickerSetFlags, StickerSetSpec, StickersSyncManager, StickersType,
    TOP_REACTIONS_DELAY_MS, TOP_REACTIONS_LIMIT, UpdateEvent, count_document_vector_hash,
    count_hash_i32, count_hash_u32, count_hash_u64, hash_finalize, hash_init, hash_update_i32,
    hash_update_i64, hash_update_u32, hash_update_u64, tl_sticker_set_hash_to_u64,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{DocumentId, ReactionId, Session, UserId};
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
use tg_storage::account::{AccountStorage, StoredStickerSet, StoredStickerSetsFile};
use tg_storage::domain::DomainStorage;
use tg_storage::serialize::document::{DocumentType, SerializedDocument, StickerSetType};
use tg_storage::serialize::image_location::{
    DownloadLocation, ImageLocation, StorageFileLocation, StorageFileLocationType,
};
use tg_storage::tdf::generate_key;
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
// TL Fixture Builders (Layer 233)
// ============================================================================

fn tl_doc(id: i64) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id.wrapping_mul(17),
        file_reference: vec![0xAA, 0xBB],
        date: 1_700_000_000,
        mime_type: "image/webp".into(),
        size: 1024,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![],
    })
}

#[allow(clippy::too_many_arguments)]
fn tl_sticker_set_header(
    id: i64,
    access_hash: i64,
    hash: i32,
    short_name: &str,
    count: i32,
    installed_date: Option<i32>,
    archived: bool,
    masks: bool,
    emojis: bool,
) -> tl_enums::StickerSet {
    tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
        archived,
        official: false,
        masks,
        emojis,
        text_color: false,
        channel_emoji_status: false,
        creator: false,
        installed_date,
        id,
        access_hash,
        title: format!("Pack {short_name}"),
        short_name: short_name.to_owned(),
        thumbs: None,
        thumb_dc_id: None,
        thumb_version: None,
        thumb_document_id: None,
        count,
        hash,
    })
}

fn tl_full_sticker_set(
    set: tl_enums::StickerSet,
    doc_ids: &[i64],
    emoticon: &str,
) -> tl_enums::messages::StickerSet {
    tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
        set,
        packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: emoticon.to_owned(),
            documents: doc_ids.to_vec(),
        })],
        keywords: vec![],
        documents: doc_ids.iter().copied().map(tl_doc).collect(),
    })
}

fn tl_covered(set: tl_enums::StickerSet, cover_doc_id: i64) -> tl_enums::StickerSetCovered {
    tl_enums::StickerSetCovered::StickerSetCovered(tl_types::StickerSetCovered {
        set,
        cover: tl_doc(cover_doc_id),
    })
}

fn tl_user(id: i64, access_hash: i64, first: &str, bot: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
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
        bot_attach_menu: bot,
        premium: false,
        attach_menu_enabled: bot,
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

fn tl_config() -> tl_enums::Config {
    tl_enums::Config::Config(tl_types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: false,
        revoke_pm_inbox: false,
        blocked_mode: false,
        force_try_ipv6: false,
        date: 1_700_000_000,
        expires: 1_700_003_600,
        test_mode: false,
        this_dc: 2,
        dc_options: vec![],
        dc_txt_domain_name: "apv3.stel.com".into(),
        chat_size_max: 200,
        megagroup_size_max: 200_000,
        forwarded_count_max: 100,
        online_update_period_ms: 210_000,
        offline_blur_timeout_ms: 5_000,
        offline_idle_timeout_ms: 30_000,
        online_cloud_timeout_ms: 300_000,
        notify_cloud_delay_ms: 30_000,
        notify_default_delay_ms: 1_500,
        push_chat_period_ms: 60_000,
        push_chat_limit: 2,
        edit_time_limit: 172_800,
        revoke_time_limit: 172_800,
        revoke_pm_time_limit: 172_800,
        rating_e_decay: 2_419_200,
        stickers_recent_limit: 30,
        channels_read_media_period: 604_800,
        tmp_sessions: None,
        call_receive_timeout_ms: 20_000,
        call_ring_timeout_ms: 90_000,
        call_connect_timeout_ms: 30_000,
        call_packet_timeout_ms: 10_000,
        me_url_prefix: "https://t.me/".into(),
        autoupdate_url_prefix: None,
        gif_search_username: Some("gif".into()),
        venue_search_username: None,
        img_search_username: None,
        static_maps_provider: None,
        caption_length_max: 1024,
        message_length_max: 4096,
        webfile_dc_id: 4,
        suggested_lang_code: Some("en".into()),
        lang_pack_version: Some(1),
        base_lang_pack_version: Some(1),
        reactions_default: None,
        autologin_token: None,
    })
}

fn tl_available_reaction(
    emoji: &str,
    title: &str,
    inactive: bool,
    premium: bool,
) -> tl_enums::AvailableReaction {
    tl_enums::AvailableReaction::AvailableReaction(tl_types::AvailableReaction {
        inactive,
        premium,
        reaction: emoji.to_owned(),
        title: title.to_owned(),
        static_icon: tl_doc(101),
        appear_animation: tl_doc(102),
        select_animation: tl_doc(103),
        activate_animation: tl_doc(104),
        effect_animation: tl_doc(105),
        around_animation: None,
        center_icon: None,
    })
}

fn tl_theme(id: i64, slug: &str, title: &str, emoticon: Option<&str>) -> tl_enums::Theme {
    tl_enums::Theme::Theme(tl_types::Theme {
        creator: false,
        default: false,
        for_chat: emoticon.is_some(),
        id,
        access_hash: id * 13,
        slug: slug.to_owned(),
        title: title.to_owned(),
        document: None,
        settings: None,
        emoticon: emoticon.map(str::to_owned),
        installs_count: Some(42),
    })
}

// ============================================================================
// 1. Hand-computed tdesktop 64-bit Hash Conformance Tests (`api/api_hash.*`)
// ============================================================================

#[test]
fn hash_algorithm_hand_computed_vectors_and_list_hashes() {
    // 1a. Empty list -> Api::HashInit() == 0.
    assert_eq!(hash_init(), 0);
    assert_eq!(hash_finalize(hash_init()), 0);
    assert_eq!(count_hash_u64([]), 0);

    // 1b. Single element [x]: starting from 0, shifts/xors stay 0, so result == x.
    assert_eq!(
        count_hash_u64([0x1234_5678_9ABC_DEF0]),
        0x1234_5678_9ABC_DEF0
    );

    // 1c. Hand-computed [1u64, 2u64] from api/api_hash.h:40-45:
    //   step 1 (value=1): h = 1
    //   step 2 (value=2):
    //     h ^= h >> 21 -> 1
    //     h ^= h << 35 -> 0x0000_0008_0000_0001
    //     h ^= h >> 4  -> 0x0000_0008_8000_0001
    //     h += 2       -> 0x0000_0008_8000_0003 (36_507_222_019)
    assert_eq!(count_hash_u64([1, 2]), 0x0000_0008_8000_0003);

    // 1d. Hand-computed [1u64, 2u64, 3u64] from api/api_hash.h:40-45:
    //   starting h = 0x0000_0008_8000_0003:
    //     h ^= h >> 21 -> 0x0000_0008_8000_4403
    //     h ^= h << 35 -> 0x0002_2010_8000_4403
    //     h ^= h >> 4  -> 0x0002_0211_8800_4043
    //     h += 3       -> 0x0002_0211_8800_4046 (565_224_272_838_726)
    assert_eq!(count_hash_u64([1, 2, 3]), 0x0002_0211_8800_4046);

    // 1e. Signed int32 sign-extension (`api/api_hash.h:55-57`: HashUpdate(already, int64(value))):
    //   [-1i32, 1i32]:
    //     step 1 (-1i32 -> 0xFFFF_FFFF_FFFF_FFFF): h = 0xFFFF_FFFF_FFFF_FFFF
    //     step 2 (+1i32):
    //       h ^= h >> 21 -> 0xFFFF_F800_0000_0000
    //       h ^= h << 35 -> 0xFFFF_F800_0000_0000 (shift clears all bits)
    //       h ^= h >> 4  -> 0xF000_0780_0000_0000
    //       h += 1       -> 0xF000_0780_0000_0001
    assert_eq!(count_hash_i32([-1, 1]), 0xF000_0780_0000_0001);
    // Unsigned u32::MAX zero-extends (`api/api_hash.h:51-53`), producing a distinct value!
    assert_ne!(count_hash_u32([u32::MAX, 1]), count_hash_i32([-1, 1]));

    // Verify all HashUpdate overloads match directly.
    let mut h_i64 = hash_init();
    hash_update_i64(&mut h_i64, -1);
    hash_update_i64(&mut h_i64, 1);
    assert_eq!(hash_finalize(h_i64), 0xF000_0780_0000_0001);

    let mut h_i32 = hash_init();
    hash_update_i32(&mut h_i32, -1);
    hash_update_i32(&mut h_i32, 1);
    assert_eq!(hash_finalize(h_i32), 0xF000_0780_0000_0001);

    let mut h_u32 = hash_init();
    hash_update_u32(&mut h_u32, 1);
    hash_update_u32(&mut h_u32, 2);
    assert_eq!(hash_finalize(h_u32), 0x0000_0008_8000_0003);

    // 1f. Negative StickerSet.hash (int32) sign-extends to u64 (`data_stickers.cpp:1565`).
    assert_eq!(tl_sticker_set_hash_to_u64(-1), u64::MAX);
    assert_eq!(tl_sticker_set_hash_to_u64(-42), (-42i64) as u64);

    // 1g. StickersSyncManager list-specific hashes (`api/api_hash.cpp:38-137`).
    let mut mgr = StickersSyncManager::new();
    let mk_set = |id: u64, hash: i32, flags: StickerSetFlags| CachedStickerSet {
        id,
        access_hash: id.wrapping_mul(10),
        hash: tl_sticker_set_hash_to_u64(hash),
        title: format!("S{id}"),
        short_name: format!("s{id}"),
        count: 1,
        flags,
        install_date: 1_700_000_000,
        thumbnail_document_id: 0,
        stickers: vec![id.wrapping_mul(100)],
        covers: vec![],
        dates: vec![],
        emoji: Default::default(),
    };

    // Installed stickers order: [set 100 (hash=-1), set 101 (ARCHIVED, skipped),
    //   set 102 (SPECIAL, skipped), set 103 (hash=1)].
    // Effective hashed values: [-1i32 (sign-extended), 1i32] -> 0xF000_0780_0000_0001.
    mgr.sets
        .insert(100, mk_set(100, -1, StickerSetFlags::INSTALLED));
    mgr.sets.insert(
        101,
        mk_set(
            101,
            999,
            StickerSetFlags::INSTALLED | StickerSetFlags::ARCHIVED,
        ),
    );
    mgr.sets.insert(
        102,
        mk_set(
            102,
            888,
            StickerSetFlags::INSTALLED | StickerSetFlags::SPECIAL,
        ),
    );
    mgr.sets
        .insert(103, mk_set(103, 1, StickerSetFlags::INSTALLED));
    mgr.sets_order = vec![100, 101, 102, 103];
    assert_eq!(mgr.count_stickers_hash(false), 0xF000_0780_0000_0001);
    assert_eq!(mgr.count_stickers_hash(true), 0xF000_0780_0000_0001);

    // Inserting DEFAULT_SET_ID (0) marks outdated: check_outdated_info=true returns 0,
    // check_outdated_info=false skips DEFAULT_SET_ID and keeps 0xF000_0780_0000_0001.
    mgr.sets.insert(
        DEFAULT_SET_ID,
        mk_set(DEFAULT_SET_ID, 77, StickerSetFlags::OFFICIAL),
    );
    mgr.sets_order.insert(0, DEFAULT_SET_ID);
    assert_eq!(mgr.count_stickers_hash(true), 0);
    assert_eq!(mgr.count_stickers_hash(false), 0xF000_0780_0000_0001);

    // Masks & custom emoji hashes use their respective orders.
    mgr.sets.insert(
        200,
        mk_set(200, 1, StickerSetFlags::INSTALLED | StickerSetFlags::MASKS),
    );
    mgr.sets.insert(
        201,
        mk_set(201, 2, StickerSetFlags::INSTALLED | StickerSetFlags::MASKS),
    );
    mgr.mask_sets_order = vec![200, 201];
    assert_eq!(mgr.count_masks_hash(true), 0x0000_0008_8000_0003);

    mgr.sets.insert(
        300,
        mk_set(300, 1, StickerSetFlags::INSTALLED | StickerSetFlags::EMOJI),
    );
    mgr.sets.insert(
        301,
        mk_set(301, 2, StickerSetFlags::INSTALLED | StickerSetFlags::EMOJI),
    );
    mgr.sets.insert(
        302,
        mk_set(302, 3, StickerSetFlags::INSTALLED | StickerSetFlags::EMOJI),
    );
    mgr.emoji_sets_order = vec![300, 301, 302];
    assert_eq!(mgr.count_custom_emoji_hash(true), 0x0002_0211_8800_4046);

    // Featured hash (`api/api_hash.cpp:63-78`):
    //   set 10 (UNREAD) -> HashUpdate(10), HashUpdate(1)
    //   set 20 (read)   -> HashUpdate(20)
    //   exact 64-bit result = 0x0014_140D_D002_829F (5_651_549_091_168_927)
    mgr.sets.insert(
        10,
        mk_set(10, 0, StickerSetFlags::FEATURED | StickerSetFlags::UNREAD),
    );
    mgr.sets
        .insert(20, mk_set(20, 0, StickerSetFlags::FEATURED));
    mgr.featured_sets_order = vec![10, 20];
    assert_eq!(mgr.count_featured_stickers_hash(), 0x0014_140D_D002_829F);

    // Clearing UNREAD changes hash to CountHash([10, 20]):
    let mut expected_read = hash_init();
    hash_update_u64(&mut expected_read, 10);
    hash_update_u64(&mut expected_read, 20);
    mgr.sets.get_mut(&10).unwrap().flags &= !StickerSetFlags::UNREAD;
    assert_eq!(mgr.count_featured_stickers_hash(), expected_read);

    // Featured custom emoji hash uses featured_emoji_sets_order.
    mgr.sets.insert(
        30,
        mk_set(
            30,
            0,
            StickerSetFlags::FEATURED | StickerSetFlags::EMOJI | StickerSetFlags::UNREAD,
        ),
    );
    mgr.featured_emoji_sets_order = vec![30];
    assert_eq!(mgr.count_featured_emoji_hash(), count_hash_u64([30, 1]));

    // Recent (regular vs attached), faved, and saved GIFs hashes.
    assert_eq!(mgr.count_recent_stickers_hash(false), 0);
    assert_eq!(mgr.count_recent_stickers_hash(true), 0);
    assert_eq!(mgr.count_faved_stickers_hash(), 0);
    assert_eq!(mgr.count_saved_gifs_hash(), 0);

    let mut recent = mk_set(CLOUD_RECENT_SET_ID, 0, StickerSetFlags::SPECIAL);
    recent.stickers = vec![1, 2];
    mgr.sets.insert(CLOUD_RECENT_SET_ID, recent);
    let mut attached = mk_set(CLOUD_RECENT_ATTACHED_SET_ID, 0, StickerSetFlags::SPECIAL);
    attached.stickers = vec![1, 2, 3];
    mgr.sets.insert(CLOUD_RECENT_ATTACHED_SET_ID, attached);
    let mut faved = mk_set(FAVED_SET_ID, 0, StickerSetFlags::SPECIAL);
    faved.stickers = vec![500];
    mgr.sets.insert(FAVED_SET_ID, faved);
    mgr.saved_gifs = vec![1, 2];

    assert_eq!(mgr.count_recent_stickers_hash(false), 0x0000_0008_8000_0003);
    assert_eq!(mgr.count_recent_stickers_hash(true), 0x0002_0211_8800_4046);
    assert_eq!(mgr.count_faved_stickers_hash(), 500);
    assert_eq!(mgr.count_saved_gifs_hash(), 0x0000_0008_8000_0003);
    assert_eq!(count_document_vector_hash(&[1, 2]), 0x0000_0008_8000_0003);
}

// ============================================================================
// 2. Stickers / Masks / Emoji / Featured / Recent / Faved / SavedGifs
//    Sync, NotModified, Hash Changes, and 1-Hour Refresh Throttle over MTProto
// ============================================================================

#[tokio::test]
async fn stickers_masks_emoji_featured_recent_faved_gifs_sync_not_modified_and_timing_over_wire() {
    #[derive(Default)]
    struct ServerState {
        sticker_version: u32,
        mask_version: u32,
        emoji_version: u32,
        featured_version: u32,
        featured_emoji_version: u32,
        recent_version: u32,
        recent_attached_version: u32,
        faved_version: u32,
        gifs_version: u32,
    }

    let state = Arc::new(Mutex::new(ServerState::default()));
    let st_srv = Arc::clone(&state);

    let server = ScriptedServer::new(move |_srv, rpc| {
        let st = st_srv.lock().unwrap();
        match rpc.ctor {
            x if x == tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetAllStickers::from_bytes(&rpc.body).unwrap();
                let sets = if st.sticker_version == 0 {
                    vec![
                        tl_sticker_set_header(
                            1001,
                            11,
                            -1,
                            "pack_a",
                            1,
                            Some(100),
                            false,
                            false,
                            false,
                        ),
                        tl_sticker_set_header(
                            1002,
                            22,
                            1,
                            "pack_b",
                            1,
                            Some(200),
                            false,
                            false,
                            false,
                        ),
                    ]
                } else {
                    vec![tl_sticker_set_header(
                        1003,
                        33,
                        77,
                        "pack_c",
                        1,
                        Some(300),
                        false,
                        false,
                        false,
                    )]
                };
                let expected = if st.sticker_version == 0 {
                    count_hash_i32([-1, 1]) as i64
                } else {
                    count_hash_i32([77]) as i64
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::AllStickers::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::messages::AllStickers::AllStickers(
                        tl_types::messages::AllStickers {
                            hash: expected,
                            sets,
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetMaskStickers::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetMaskStickers::from_bytes(&rpc.body).unwrap();
                let expected = if st.mask_version == 0 {
                    count_hash_i32([1, 2]) as i64
                } else {
                    count_hash_i32([9]) as i64
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::AllStickers::NotModified)
                } else {
                    let sets = if st.mask_version == 0 {
                        vec![
                            tl_sticker_set_header(
                                2001,
                                21,
                                1,
                                "mask_a",
                                1,
                                Some(100),
                                false,
                                true,
                                false,
                            ),
                            tl_sticker_set_header(
                                2002,
                                22,
                                2,
                                "mask_b",
                                1,
                                Some(100),
                                false,
                                true,
                                false,
                            ),
                        ]
                    } else {
                        vec![tl_sticker_set_header(
                            2003,
                            23,
                            9,
                            "mask_c",
                            1,
                            Some(100),
                            false,
                            true,
                            false,
                        )]
                    };
                    ServerAction::ok(&tl_enums::messages::AllStickers::AllStickers(
                        tl_types::messages::AllStickers {
                            hash: expected,
                            sets,
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetEmojiStickers::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetEmojiStickers::from_bytes(&rpc.body).unwrap();
                let expected = if st.emoji_version == 0 {
                    count_hash_i32([10, 20]) as i64
                } else {
                    count_hash_i32([30]) as i64
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::AllStickers::NotModified)
                } else {
                    let sets = if st.emoji_version == 0 {
                        vec![
                            tl_sticker_set_header(
                                3001,
                                31,
                                10,
                                "emo_a",
                                1,
                                Some(100),
                                false,
                                false,
                                true,
                            ),
                            tl_sticker_set_header(
                                3002,
                                32,
                                20,
                                "emo_b",
                                1,
                                Some(100),
                                false,
                                false,
                                true,
                            ),
                        ]
                    } else {
                        vec![tl_sticker_set_header(
                            3003,
                            33,
                            30,
                            "emo_c",
                            1,
                            Some(100),
                            false,
                            false,
                            true,
                        )]
                    };
                    ServerAction::ok(&tl_enums::messages::AllStickers::AllStickers(
                        tl_types::messages::AllStickers {
                            hash: expected,
                            sets,
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetFeaturedStickers::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetFeaturedStickers::from_bytes(&rpc.body).unwrap();
                // set 10 (unread) + set 20 (read) => 0x0014_140D_D002_829F.
                let expected = if st.featured_version == 0 {
                    0x0014_140D_D002_829Fu64 as i64
                } else {
                    count_hash_u64([30]) as i64
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::FeaturedStickers::NotModified(
                        tl_types::messages::FeaturedStickersNotModified { count: 2 },
                    ))
                } else if st.featured_version == 0 {
                    ServerAction::ok(&tl_enums::messages::FeaturedStickers::FeaturedStickers(
                        tl_types::messages::FeaturedStickers {
                            premium: false,
                            hash: expected,
                            count: 2,
                            sets: vec![
                                tl_covered(
                                    tl_sticker_set_header(
                                        10, 100, 1, "feat_10", 1, None, false, false, false,
                                    ),
                                    9010,
                                ),
                                tl_covered(
                                    tl_sticker_set_header(
                                        20, 200, 2, "feat_20", 1, None, false, false, false,
                                    ),
                                    9020,
                                ),
                            ],
                            unread: vec![10],
                        },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::messages::FeaturedStickers::FeaturedStickers(
                        tl_types::messages::FeaturedStickers {
                            premium: false,
                            hash: expected,
                            count: 1,
                            sets: vec![tl_covered(
                                tl_sticker_set_header(
                                    30, 300, 3, "feat_30", 1, None, false, false, false,
                                ),
                                9030,
                            )],
                            unread: vec![],
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetFeaturedEmojiStickers::CONSTRUCTOR_ID => {
                let req =
                    tl_fns::messages::GetFeaturedEmojiStickers::from_bytes(&rpc.body).unwrap();
                let expected = if st.featured_emoji_version == 0 {
                    count_hash_u64([40, 1]) as i64
                } else {
                    count_hash_u64([41]) as i64
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::FeaturedStickers::NotModified(
                        tl_types::messages::FeaturedStickersNotModified { count: 1 },
                    ))
                } else if st.featured_emoji_version == 0 {
                    ServerAction::ok(&tl_enums::messages::FeaturedStickers::FeaturedStickers(
                        tl_types::messages::FeaturedStickers {
                            premium: false,
                            hash: expected,
                            count: 1,
                            sets: vec![tl_covered(
                                tl_sticker_set_header(
                                    40, 400, 4, "femo_40", 1, None, false, false, true,
                                ),
                                9040,
                            )],
                            unread: vec![40],
                        },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::messages::FeaturedStickers::FeaturedStickers(
                        tl_types::messages::FeaturedStickers {
                            premium: false,
                            hash: expected,
                            count: 1,
                            sets: vec![tl_covered(
                                tl_sticker_set_header(
                                    41, 410, 5, "femo_41", 1, None, false, false, true,
                                ),
                                9041,
                            )],
                            unread: vec![],
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetRecentStickers::from_bytes(&rpc.body).unwrap();
                let (docs, expected) = if req.attached {
                    if st.recent_attached_version == 0 {
                        (vec![11, 12], count_hash_u64([11, 12]) as i64)
                    } else {
                        (vec![13], count_hash_u64([13]) as i64)
                    }
                } else if st.recent_version == 0 {
                    (vec![1, 2], 0x0000_0008_8000_0003u64 as i64)
                } else {
                    (vec![3], 3)
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::RecentStickers::NotModified)
                } else {
                    let dates: Vec<i32> = docs.iter().map(|&d| 1_700_000_000 + d as i32).collect();
                    ServerAction::ok(&tl_enums::messages::RecentStickers::RecentStickers(
                        tl_types::messages::RecentStickers {
                            hash: expected,
                            packs: vec![],
                            stickers: docs.into_iter().map(tl_doc).collect(),
                            dates,
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetFavedStickers::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetFavedStickers::from_bytes(&rpc.body).unwrap();
                let (docs, expected) = if st.faved_version == 0 {
                    (vec![50, 60], count_hash_u64([50, 60]) as i64)
                } else {
                    (vec![70], 70)
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::FavedStickers::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::messages::FavedStickers::FavedStickers(
                        tl_types::messages::FavedStickers {
                            hash: expected,
                            packs: vec![],
                            stickers: docs.into_iter().map(tl_doc).collect(),
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetSavedGifs::from_bytes(&rpc.body).unwrap();
                let (docs, expected) = if st.gifs_version == 0 {
                    (vec![1, 2], 0x0000_0008_8000_0003u64 as i64)
                } else {
                    (vec![99], 99)
                };
                if req.hash == expected {
                    ServerAction::ok(&tl_enums::messages::SavedGifs::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::messages::SavedGifs::SavedGifs(
                        tl_types::messages::SavedGifs {
                            hash: expected,
                            gifs: docs.into_iter().map(tl_doc).collect(),
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetStickerSet::from_bytes(&rpc.body).unwrap();
                let (id, access_hash) = match req.stickerset {
                    tl_enums::InputStickerSet::ID(s) => (s.id, s.access_hash),
                    other => panic!("unexpected getStickerSet spec: {other:?}"),
                };
                let (hash, masks, emojis) = match id {
                    1001 => (-1, false, false),
                    1002 => (1, false, false),
                    1003 => (77, false, false),
                    2001 => (1, true, false),
                    2002 => (2, true, false),
                    2003 => (9, true, false),
                    3001 => (10, false, true),
                    3002 => (20, false, true),
                    3003 => (30, false, true),
                    10 => (1, false, false),
                    20 => (2, false, false),
                    30 => (3, false, false),
                    40 => (4, false, true),
                    41 => (5, false, true),
                    _ => (0, false, false),
                };
                let installed = if id >= 1000 { Some(100) } else { None };
                let header = tl_sticker_set_header(
                    id,
                    access_hash,
                    hash,
                    &format!("s{id}"),
                    1,
                    installed,
                    false,
                    masks,
                    emojis,
                );
                ServerAction::ok(&tl_full_sticker_set(header, &[id * 10], "😀"))
            }
            other => panic!("unexpected RPC ctor 0x{other:08x}"),
        }
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut mgr = StickersSyncManager::new();

    let t0 = 10_000_000u64;
    // Initial sync of all 8 lists.
    mgr.update_stickers(&rpc, t0).await.unwrap();
    mgr.update_masks(&rpc, t0).await.unwrap();
    mgr.update_custom_emoji(&rpc, t0).await.unwrap();
    mgr.update_saved_gifs(&rpc, t0).await.unwrap();

    assert_eq!(mgr.sets_order, vec![1001, 1002]);
    assert_eq!(mgr.mask_sets_order, vec![2001, 2002]);
    assert_eq!(mgr.emoji_sets_order, vec![3001, 3002]);
    assert_eq!(mgr.featured_sets_order, vec![10, 20]);
    assert_eq!(mgr.featured_sets_unread_count, 1);
    assert_eq!(mgr.featured_emoji_sets_order, vec![40]);
    assert_eq!(mgr.sets[&CLOUD_RECENT_SET_ID].stickers, vec![1, 2]);
    assert_eq!(
        mgr.sets[&CLOUD_RECENT_ATTACHED_SET_ID].stickers,
        vec![11, 12]
    );
    assert_eq!(mgr.sets[&FAVED_SET_ID].stickers, vec![50, 60]);
    assert_eq!(mgr.saved_gifs, vec![1, 2]);

    // Verify 1-hour throttle: at t0 + 3_599_999 ms, NO RPCs are sent!
    let count_before = server.rpcs().len();
    let t_almost = t0 + STICKERS_UPDATE_TIMEOUT_MS - 1;
    mgr.update_stickers(&rpc, t_almost).await.unwrap();
    mgr.update_masks(&rpc, t_almost).await.unwrap();
    mgr.update_custom_emoji(&rpc, t_almost).await.unwrap();
    mgr.update_saved_gifs(&rpc, t_almost).await.unwrap();
    assert_eq!(
        server.rpcs().len(),
        count_before,
        "must not refetch before kUpdateTimeout (3_600_000 ms)"
    );

    // At t0 + 3_600_000 ms: every list sends its exact computed hash, receives *NotModified,
    // and keeps the entire cached state intact.
    let t1 = t0 + STICKERS_UPDATE_TIMEOUT_MS;
    mgr.update_stickers(&rpc, t1).await.unwrap();
    mgr.update_masks(&rpc, t1).await.unwrap();
    mgr.update_custom_emoji(&rpc, t1).await.unwrap();
    mgr.update_saved_gifs(&rpc, t1).await.unwrap();

    // Check exact wire hashes sent on the second request for every endpoint.
    let all_reqs = server.rpcs_of::<tl_fns::messages::GetAllStickers>();
    assert_eq!(all_reqs.len(), 2);
    assert_eq!(
        tl_fns::messages::GetAllStickers::from_bytes(&all_reqs[0].body)
            .unwrap()
            .hash,
        0
    );
    assert_eq!(
        tl_fns::messages::GetAllStickers::from_bytes(&all_reqs[1].body)
            .unwrap()
            .hash,
        0xF000_0780_0000_0001u64 as i64
    );

    let feat_reqs = server.rpcs_of::<tl_fns::messages::GetFeaturedStickers>();
    assert_eq!(feat_reqs.len(), 2);
    assert_eq!(
        tl_fns::messages::GetFeaturedStickers::from_bytes(&feat_reqs[1].body)
            .unwrap()
            .hash,
        0x0014_140D_D002_829Fu64 as i64
    );

    let gif_reqs = server.rpcs_of::<tl_fns::messages::GetSavedGifs>();
    assert_eq!(gif_reqs.len(), 2);
    assert_eq!(
        tl_fns::messages::GetSavedGifs::from_bytes(&gif_reqs[1].body)
            .unwrap()
            .hash,
        0x0000_0008_8000_0003u64 as i64
    );

    // Cache preserved across *NotModified!
    assert_eq!(mgr.sets_order, vec![1001, 1002]);
    assert_eq!(mgr.mask_sets_order, vec![2001, 2002]);
    assert_eq!(mgr.emoji_sets_order, vec![3001, 3002]);
    assert_eq!(mgr.featured_sets_order, vec![10, 20]);
    assert_eq!(mgr.featured_emoji_sets_order, vec![40]);
    assert_eq!(mgr.sets[&CLOUD_RECENT_SET_ID].stickers, vec![1, 2]);
    assert_eq!(
        mgr.sets[&CLOUD_RECENT_ATTACHED_SET_ID].stickers,
        vec![11, 12]
    );
    assert_eq!(mgr.sets[&FAVED_SET_ID].stickers, vec![50, 60]);
    assert_eq!(mgr.saved_gifs, vec![1, 2]);

    // Advance server versions so hashes change; at t2 = t1 + 1h, every cache is replaced!
    {
        let mut st = state.lock().unwrap();
        st.sticker_version = 1;
        st.mask_version = 1;
        st.emoji_version = 1;
        st.featured_version = 1;
        st.featured_emoji_version = 1;
        st.recent_version = 1;
        st.recent_attached_version = 1;
        st.faved_version = 1;
        st.gifs_version = 1;
    }
    let t2 = t1 + STICKERS_UPDATE_TIMEOUT_MS;
    mgr.update_stickers(&rpc, t2).await.unwrap();
    mgr.update_masks(&rpc, t2).await.unwrap();
    mgr.update_custom_emoji(&rpc, t2).await.unwrap();
    mgr.update_saved_gifs(&rpc, t2).await.unwrap();

    assert_eq!(mgr.sets_order, vec![1003]);
    assert_eq!(mgr.mask_sets_order, vec![2003]);
    assert_eq!(mgr.emoji_sets_order, vec![3003]);
    assert_eq!(mgr.featured_sets_order, vec![30]);
    assert_eq!(mgr.featured_sets_unread_count, 0);
    assert_eq!(mgr.featured_emoji_sets_order, vec![41]);
    assert_eq!(mgr.sets[&CLOUD_RECENT_SET_ID].stickers, vec![3]);
    assert_eq!(mgr.sets[&CLOUD_RECENT_ATTACHED_SET_ID].stickers, vec![13]);
    assert_eq!(mgr.sets[&FAVED_SET_ID].stickers, vec![70]);
    assert_eq!(mgr.saved_gifs, vec![99]);

    // Forced recent/faved refresh sends hash = 0 (`apiwrap.cpp:3341, 3376`).
    mgr.request_recent_stickers(&rpc, None, false, t2 + 10)
        .await
        .unwrap();
    mgr.request_faved_stickers(&rpc, None, t2 + 10)
        .await
        .unwrap();
    let last_recent = server
        .rpcs_of::<tl_fns::messages::GetRecentStickers>()
        .pop()
        .unwrap();
    assert_eq!(
        tl_fns::messages::GetRecentStickers::from_bytes(&last_recent.body)
            .unwrap()
            .hash,
        0
    );
    let last_faved = server
        .rpcs_of::<tl_fns::messages::GetFavedStickers>()
        .pop()
        .unwrap();
    assert_eq!(
        tl_fns::messages::GetFavedStickers::from_bytes(&last_faved.body)
            .unwrap()
            .hash,
        0
    );
}

// ============================================================================
// 3. Server-Hashed / Versioned Lists: Reactions, Emoji Statuses & Groups,
//    Emoji Keywords, AppConfig, PeerColors, WallPapers, Themes,
//    AttachMenuBots, PremiumPromo, SavedRingtones, QuickReplies
// ============================================================================

#[tokio::test]
async fn reactions_emoji_statuses_groups_keywords_and_server_hashed_lists_over_wire() {
    #[derive(Default)]
    struct ListsServerState {
        top_rev: u32,
        recent_rev: u32,
        avail_rev: u32,
        fail_avail_once: bool,
        def_status_rev: u32,
        ch_status_rev: u32,
        groups_rev: u32,
        kw_bad_lang: bool,
        kw_rev: u32,
        app_cfg_rev: u32,
        peer_colors_rev: u32,
        wallpapers_rev: u32,
        themes_rev: u32,
        attach_rev: u32,
        ringtones_rev: u32,
        quick_replies_not_modified: bool,
    }

    let state = Arc::new(Mutex::new(ListsServerState::default()));
    let st_srv = Arc::clone(&state);

    let server = ScriptedServer::new(move |_srv, rpc| {
        let mut st = st_srv.lock().unwrap();
        match rpc.ctor {
            x if x == tl_fns::messages::GetTopReactions::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetTopReactions::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.limit, TOP_REACTIONS_LIMIT);
                let h = if st.top_rev == 0 { 111 } else { 222 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::messages::Reactions::NotModified)
                } else {
                    let emoji = if st.top_rev == 0 { "👍" } else { "🔥" };
                    ServerAction::ok(&tl_enums::messages::Reactions::Reactions(
                        tl_types::messages::Reactions {
                            hash: h,
                            reactions: vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                emoticon: emoji.into(),
                            })],
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetRecentReactions::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetRecentReactions::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.limit, RECENT_REACTIONS_LIMIT);
                let h = if st.recent_rev == 0 { 333 } else { 444 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::messages::Reactions::NotModified)
                } else {
                    let reactions = if st.recent_rev == 0 {
                        vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                            emoticon: "❤️".into(),
                        })]
                    } else {
                        vec![tl_enums::Reaction::CustomEmoji(
                            tl_types::ReactionCustomEmoji { document_id: 9001 },
                        )]
                    };
                    ServerAction::ok(&tl_enums::messages::Reactions::Reactions(
                        tl_types::messages::Reactions { hash: h, reactions },
                    ))
                }
            }
            x if x == tl_fns::messages::GetAvailableReactions::CONSTRUCTOR_ID => {
                if st.fail_avail_once {
                    st.fail_avail_once = false;
                    return ServerAction::err(400, "REACTIONS_UNAVAILABLE");
                }
                let req = tl_fns::messages::GetAvailableReactions::from_bytes(&rpc.body).unwrap();
                let h = if st.avail_rev == 0 { 555 } else { 666 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::messages::AvailableReactions::NotModified)
                } else {
                    let reactions = if st.avail_rev == 0 {
                        vec![
                            tl_available_reaction("👍", "Thumbs Up", false, false),
                            tl_available_reaction("💩", "Poop", true, false),
                        ]
                    } else {
                        vec![tl_available_reaction("🎉", "Party", false, true)]
                    };
                    ServerAction::ok(&tl_enums::messages::AvailableReactions::AvailableReactions(
                        tl_types::messages::AvailableReactions { hash: h, reactions },
                    ))
                }
            }
            x if x == tl_fns::account::GetDefaultEmojiStatuses::CONSTRUCTOR_ID => {
                let req = tl_fns::account::GetDefaultEmojiStatuses::from_bytes(&rpc.body).unwrap();
                let h = if st.def_status_rev == 0 { 701 } else { 702 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::account::EmojiStatuses::NotModified)
                } else {
                    let doc_id = if st.def_status_rev == 0 { 8001 } else { 8002 };
                    ServerAction::ok(&tl_enums::account::EmojiStatuses::EmojiStatuses(
                        tl_types::account::EmojiStatuses {
                            hash: h,
                            statuses: vec![tl_enums::EmojiStatus::EmojiStatus(
                                tl_types::EmojiStatus {
                                    document_id: doc_id,
                                    until: None,
                                },
                            )],
                        },
                    ))
                }
            }
            x if x == tl_fns::account::GetChannelDefaultEmojiStatuses::CONSTRUCTOR_ID => {
                let req =
                    tl_fns::account::GetChannelDefaultEmojiStatuses::from_bytes(&rpc.body).unwrap();
                let h = if st.ch_status_rev == 0 { 711 } else { 712 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::account::EmojiStatuses::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::account::EmojiStatuses::EmojiStatuses(
                        tl_types::account::EmojiStatuses {
                            hash: h,
                            statuses: vec![tl_enums::EmojiStatus::EmojiStatus(
                                tl_types::EmojiStatus {
                                    document_id: if st.ch_status_rev == 0 { 8101 } else { 8102 },
                                    until: None,
                                },
                            )],
                        },
                    ))
                }
            }
            x if x == tl_fns::account::GetRecentEmojiStatuses::CONSTRUCTOR_ID
                || x == tl_fns::account::GetCollectibleEmojiStatuses::CONSTRUCTOR_ID =>
            {
                let hash = if x == tl_fns::account::GetRecentEmojiStatuses::CONSTRUCTOR_ID {
                    tl_fns::account::GetRecentEmojiStatuses::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                } else {
                    tl_fns::account::GetCollectibleEmojiStatuses::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                };
                if hash == 750 {
                    ServerAction::ok(&tl_enums::account::EmojiStatuses::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::account::EmojiStatuses::EmojiStatuses(
                        tl_types::account::EmojiStatuses {
                            hash: 750,
                            statuses: vec![tl_enums::EmojiStatus::EmojiStatus(
                                tl_types::EmojiStatus {
                                    document_id: 8500,
                                    until: Some(1_800_000_000),
                                },
                            )],
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetEmojiGroups::CONSTRUCTOR_ID
                || x == tl_fns::messages::GetEmojiStatusGroups::CONSTRUCTOR_ID
                || x == tl_fns::messages::GetEmojiStickerGroups::CONSTRUCTOR_ID
                || x == tl_fns::messages::GetEmojiProfilePhotoGroups::CONSTRUCTOR_ID =>
            {
                let hash = if x == tl_fns::messages::GetEmojiGroups::CONSTRUCTOR_ID {
                    tl_fns::messages::GetEmojiGroups::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                } else if x == tl_fns::messages::GetEmojiStatusGroups::CONSTRUCTOR_ID {
                    tl_fns::messages::GetEmojiStatusGroups::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                } else if x == tl_fns::messages::GetEmojiStickerGroups::CONSTRUCTOR_ID {
                    tl_fns::messages::GetEmojiStickerGroups::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                } else {
                    tl_fns::messages::GetEmojiProfilePhotoGroups::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                };
                let target_h = if st.groups_rev == 0 { 901 } else { 902 };
                if hash == target_h {
                    ServerAction::ok(&tl_enums::messages::EmojiGroups::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::messages::EmojiGroups::EmojiGroups(
                        tl_types::messages::EmojiGroups {
                            hash: target_h,
                            groups: vec![
                                tl_enums::EmojiGroup::EmojiGroup(tl_types::EmojiGroup {
                                    title: if st.groups_rev == 0 {
                                        "Love".into()
                                    } else {
                                        "Party".into()
                                    },
                                    icon_emoji_id: 42,
                                    emoticons: vec!["❤️".into()],
                                }),
                                tl_enums::EmojiGroup::Premium(tl_types::EmojiGroupPremium {
                                    title: "Premium".into(),
                                    icon_emoji_id: 43,
                                }),
                            ],
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetEmojiKeywords::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetEmojiKeywords::from_bytes(&rpc.body).unwrap();
                ServerAction::ok(&tl_enums::EmojiKeywordsDifference::EmojiKeywordsDifference(
                    tl_types::EmojiKeywordsDifference {
                        lang_code: req.lang_code,
                        from_version: 0,
                        version: 10,
                        keywords: vec![
                            tl_enums::EmojiKeyword::EmojiKeyword(tl_types::EmojiKeyword {
                                keyword: "Heart".into(),
                                emoticons: vec!["❤️".into(), "💙".into()],
                            }),
                            tl_enums::EmojiKeyword::EmojiKeyword(tl_types::EmojiKeyword {
                                keyword: "fire".into(),
                                emoticons: vec!["🔥".into()],
                            }),
                        ],
                    },
                ))
            }
            x if x == tl_fns::messages::GetEmojiKeywordsDifference::CONSTRUCTOR_ID => {
                let req =
                    tl_fns::messages::GetEmojiKeywordsDifference::from_bytes(&rpc.body).unwrap();
                if st.kw_bad_lang {
                    return ServerAction::ok(
                        &tl_enums::EmojiKeywordsDifference::EmojiKeywordsDifference(
                            tl_types::EmojiKeywordsDifference {
                                lang_code: "zz".into(),
                                from_version: req.from_version,
                                version: 99,
                                keywords: vec![],
                            },
                        ),
                    );
                }
                if st.kw_rev == 0 {
                    // Unchanged version -> empty diff.
                    ServerAction::ok(&tl_enums::EmojiKeywordsDifference::EmojiKeywordsDifference(
                        tl_types::EmojiKeywordsDifference {
                            lang_code: req.lang_code,
                            from_version: req.from_version,
                            version: req.from_version,
                            keywords: vec![],
                        },
                    ))
                } else {
                    // Delete one emoticon from "heart", delete all emoticons from "fire", add "rocket".
                    ServerAction::ok(&tl_enums::EmojiKeywordsDifference::EmojiKeywordsDifference(
                        tl_types::EmojiKeywordsDifference {
                            lang_code: req.lang_code,
                            from_version: req.from_version,
                            version: 11,
                            keywords: vec![
                                tl_enums::EmojiKeyword::Deleted(tl_types::EmojiKeywordDeleted {
                                    keyword: "heart".into(),
                                    emoticons: vec!["💙".into()],
                                }),
                                tl_enums::EmojiKeyword::Deleted(tl_types::EmojiKeywordDeleted {
                                    keyword: "fire".into(),
                                    emoticons: vec!["🔥".into()],
                                }),
                                tl_enums::EmojiKeyword::EmojiKeyword(tl_types::EmojiKeyword {
                                    keyword: "rocket".into(),
                                    emoticons: vec!["🚀".into()],
                                }),
                            ],
                        },
                    ))
                }
            }
            x if x == tl_fns::help::GetAppConfig::CONSTRUCTOR_ID => {
                let req = tl_fns::help::GetAppConfig::from_bytes(&rpc.body).unwrap();
                let h = if st.app_cfg_rev == 0 { 1001 } else { 1002 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::help::AppConfig::NotModified)
                } else {
                    let quote_max = if st.app_cfg_rev == 0 { 1024.0 } else { 2048.0 };
                    ServerAction::ok(&tl_enums::help::AppConfig::AppConfig(
                        tl_types::help::AppConfig {
                            hash: h,
                            config: tl_enums::JSONValue::JsonObject(tl_types::JsonObject {
                                value: vec![tl_enums::JSONObjectValue::JsonObjectValue(
                                    tl_types::JsonObjectValue {
                                        key: "quote_length_max".into(),
                                        value: tl_enums::JSONValue::JsonNumber(
                                            tl_types::JsonNumber { value: quote_max },
                                        ),
                                    },
                                )],
                            }),
                        },
                    ))
                }
            }
            x if x == tl_fns::help::GetPeerColors::CONSTRUCTOR_ID
                || x == tl_fns::help::GetPeerProfileColors::CONSTRUCTOR_ID =>
            {
                let req_hash = if x == tl_fns::help::GetPeerColors::CONSTRUCTOR_ID {
                    tl_fns::help::GetPeerColors::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                } else {
                    tl_fns::help::GetPeerProfileColors::from_bytes(&rpc.body)
                        .unwrap()
                        .hash
                };
                let h = if st.peer_colors_rev == 0 { 2001 } else { 2002 };
                if req_hash == h {
                    ServerAction::ok(&tl_enums::help::PeerColors::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::help::PeerColors::PeerColors(
                        tl_types::help::PeerColors {
                            hash: h,
                            colors: vec![tl_enums::help::PeerColorOption::PeerColorOption(
                                tl_types::help::PeerColorOption {
                                    hidden: false,
                                    color_id: if st.peer_colors_rev == 0 { 7 } else { 9 },
                                    colors: None,
                                    dark_colors: None,
                                    channel_min_level: Some(3),
                                    group_min_level: Some(5),
                                },
                            )],
                        },
                    ))
                }
            }
            x if x == tl_fns::account::GetWallPapers::CONSTRUCTOR_ID => {
                let req = tl_fns::account::GetWallPapers::from_bytes(&rpc.body).unwrap();
                let h = if st.wallpapers_rev == 0 { 3001 } else { 3002 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::account::WallPapers::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::account::WallPapers::WallPapers(
                        tl_types::account::WallPapers {
                            hash: h,
                            wallpapers: vec![tl_enums::WallPaper::NoFile(
                                tl_types::WallPaperNoFile {
                                    id: if st.wallpapers_rev == 0 { 11 } else { 22 },
                                    default: true,
                                    dark: false,
                                    settings: None,
                                },
                            )],
                        },
                    ))
                }
            }
            x if x == tl_fns::account::GetThemes::CONSTRUCTOR_ID => {
                let req = tl_fns::account::GetThemes::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.format, CLOUD_THEME_FORMAT);
                let h = if st.themes_rev == 0 { 4001 } else { 4002 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::account::Themes::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::account::Themes::Themes(
                        tl_types::account::Themes {
                            hash: h,
                            themes: vec![tl_theme(
                                if st.themes_rev == 0 { 1 } else { 2 },
                                "day",
                                "Day",
                                None,
                            )],
                        },
                    ))
                }
            }
            x if x == tl_fns::account::GetChatThemes::CONSTRUCTOR_ID => {
                let req = tl_fns::account::GetChatThemes::from_bytes(&rpc.body).unwrap();
                if req.hash == 4100 {
                    ServerAction::ok(&tl_enums::account::Themes::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::account::Themes::Themes(
                        tl_types::account::Themes {
                            hash: 4100,
                            themes: vec![tl_theme(10, "love", "Love", Some("❤️"))],
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetAttachMenuBots::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetAttachMenuBots::from_bytes(&rpc.body).unwrap();
                let h = if st.attach_rev == 0 { 5001 } else { 5002 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::AttachMenuBots::NotModified)
                } else {
                    let bot_id = if st.attach_rev == 0 { 7001 } else { 7002 };
                    ServerAction::ok(&tl_enums::AttachMenuBots::AttachMenuBots(
                        tl_types::AttachMenuBots {
                            hash: h,
                            bots: vec![tl_enums::AttachMenuBot::AttachMenuBot(
                                tl_types::AttachMenuBot {
                                    inactive: false,
                                    has_settings: false,
                                    request_write_access: false,
                                    show_in_attach_menu: true,
                                    show_in_side_menu: true,
                                    side_menu_disclaimer_needed: false,
                                    bot_id,
                                    short_name: format!("bot{bot_id}"),
                                    peer_types: None,
                                    icons: vec![],
                                },
                            )],
                            users: vec![tl_user(bot_id, bot_id * 9, "AttachBot", true)],
                        },
                    ))
                }
            }
            x if x == tl_fns::help::GetPremiumPromo::CONSTRUCTOR_ID => ServerAction::ok(
                &tl_enums::help::PremiumPromo::PremiumPromo(tl_types::help::PremiumPromo {
                    status_text: "Go Premium".into(),
                    status_entities: vec![],
                    video_sections: vec!["more_upload".into()],
                    videos: vec![tl_doc(9901)],
                    period_options: vec![
                        tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(
                            tl_types::PremiumSubscriptionOption {
                                current: false,
                                can_purchase_upgrade: false,
                                transaction: None,
                                months: 1,
                                currency: "USD".into(),
                                amount: 499,
                                bot_url: "https://t.me/PremiumBot".into(),
                                store_product: None,
                            },
                        ),
                    ],
                    users: vec![tl_user(777_000, 123, "Telegram", false)],
                }),
            ),
            x if x == tl_fns::account::GetSavedRingtones::CONSTRUCTOR_ID => {
                let req = tl_fns::account::GetSavedRingtones::from_bytes(&rpc.body).unwrap();
                let h = if st.ringtones_rev == 0 { 6001 } else { 6002 };
                if req.hash == h {
                    ServerAction::ok(&tl_enums::account::SavedRingtones::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::account::SavedRingtones::SavedRingtones(
                        tl_types::account::SavedRingtones {
                            hash: h,
                            ringtones: vec![tl_doc(if st.ringtones_rev == 0 {
                                6101
                            } else {
                                6102
                            })],
                        },
                    ))
                }
            }
            x if x == tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID => {
                if st.quick_replies_not_modified {
                    ServerAction::ok(&tl_enums::messages::QuickReplies::NotModified)
                } else {
                    ServerAction::ok(&tl_enums::messages::QuickReplies::QuickReplies(
                        tl_types::messages::QuickReplies {
                            quick_replies: vec![tl_enums::QuickReply::QuickReply(
                                tl_types::QuickReply {
                                    shortcut_id: 1,
                                    shortcut: "hello".into(),
                                    top_message: 55,
                                    count: 1,
                                },
                            )],
                            messages: vec![],
                            chats: vec![],
                            users: vec![tl_user(8801, 99, "BizUser", false)],
                        },
                    ))
                }
            }
            other => panic!("unexpected RPC ctor 0x{other:08x}"),
        }
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut session = Session::new(UserId(1));
    let mut sync = CachedListsSync::new();
    let t0 = 1_000_000u64;

    // 1. Reactions: top, recent, available (+ throttle, NotModified, changed hash, error reset).
    assert!(sync.reactions.refresh_top(&rpc, t0, false).await.unwrap());
    assert_eq!(sync.reactions.top_hash, 111);
    assert_eq!(sync.reactions.top, vec![ReactionId::Emoji("👍".into())]);
    // Throttled within 60s (`kTopRequestDelay`).
    assert!(
        !sync
            .reactions
            .refresh_top(&rpc, t0 + TOP_REACTIONS_DELAY_MS - 1, false)
            .await
            .unwrap()
    );
    // At +60s -> NotModified keeps cache.
    assert!(
        sync.reactions
            .refresh_top(&rpc, t0 + TOP_REACTIONS_DELAY_MS, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.reactions.top, vec![ReactionId::Emoji("👍".into())]);
    // Hash changed -> replaces cache.
    state.lock().unwrap().top_rev = 1;
    assert!(
        sync.reactions
            .refresh_top(&rpc, t0 + 2 * TOP_REACTIONS_DELAY_MS, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.reactions.top_hash, 222);
    assert_eq!(sync.reactions.top, vec![ReactionId::Emoji("🔥".into())]);

    sync.reactions.refresh_recent(&rpc).await.unwrap();
    assert_eq!(sync.reactions.recent_hash, 333);
    assert_eq!(sync.reactions.recent, vec![ReactionId::Emoji("❤️".into())]);
    sync.reactions.refresh_recent(&rpc).await.unwrap(); // NotModified
    assert_eq!(sync.reactions.recent, vec![ReactionId::Emoji("❤️".into())]);
    state.lock().unwrap().recent_rev = 1;
    sync.reactions.refresh_recent(&rpc).await.unwrap();
    assert_eq!(sync.reactions.recent_hash, 444);
    assert_eq!(
        sync.reactions.recent,
        vec![ReactionId::Custom(DocumentId(9001))]
    );

    assert!(
        sync.reactions
            .refresh_available(&rpc, t0, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.reactions.available_hash, 555);
    assert_eq!(sync.reactions.available.len(), 2);
    assert_eq!(sync.reactions.active.len(), 1);
    assert!(
        !sync
            .reactions
            .refresh_available(&rpc, t0 + REACTIONS_FULL_LIST_REFRESH_MS - 1, false)
            .await
            .unwrap()
    );
    assert!(
        sync.reactions
            .refresh_available(&rpc, t0 + REACTIONS_FULL_LIST_REFRESH_MS, false)
            .await
            .unwrap()
    ); // NotModified
    assert_eq!(sync.reactions.available_hash, 555);
    // Failure resets available_hash to 0 (`data_message_reactions.cpp:1029`).
    state.lock().unwrap().fail_avail_once = true;
    assert!(
        sync.reactions
            .refresh_available(&rpc, t0, true)
            .await
            .is_err()
    );
    assert_eq!(sync.reactions.available_hash, 0);
    state.lock().unwrap().avail_rev = 1;
    assert!(
        sync.reactions
            .refresh_available(&rpc, t0, true)
            .await
            .unwrap()
    );
    assert_eq!(sync.reactions.available_hash, 666);
    assert_eq!(sync.reactions.active[0].reaction, "🎉");

    // 2. Emoji statuses & emoji groups (all 4 group endpoints).
    assert!(
        sync.emoji_statuses
            .refresh_default(&rpc, t0, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.emoji_statuses.default_hash, 701);
    assert_eq!(sync.emoji_statuses.default_statuses[0].document_id, 8001);
    assert!(
        !sync
            .emoji_statuses
            .refresh_default(&rpc, t0 + EMOJI_STATUSES_REFRESH_MS - 1, false)
            .await
            .unwrap()
    );
    assert!(
        sync.emoji_statuses
            .refresh_default(&rpc, t0 + EMOJI_STATUSES_REFRESH_MS, false)
            .await
            .unwrap()
    ); // NotModified
    assert_eq!(sync.emoji_statuses.default_statuses[0].document_id, 8001);
    state.lock().unwrap().def_status_rev = 1;
    assert!(
        sync.emoji_statuses
            .refresh_default(&rpc, t0 + 2 * EMOJI_STATUSES_REFRESH_MS, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.emoji_statuses.default_hash, 702);
    assert_eq!(sync.emoji_statuses.default_statuses[0].document_id, 8002);

    assert!(
        sync.emoji_statuses
            .refresh_channel_default(&rpc, t0, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.emoji_statuses.channel_default_hash, 711);
    assert_eq!(
        sync.emoji_statuses.channel_default_statuses[0].document_id,
        8101
    );
    sync.emoji_statuses.refresh_recent(&rpc).await.unwrap();
    sync.emoji_statuses
        .refresh_collectibles(&rpc)
        .await
        .unwrap();
    assert_eq!(sync.emoji_statuses.recent_hash, 750);
    assert_eq!(sync.emoji_statuses.collectibles_hash, 750);

    sync.emoji_statuses
        .refresh_emoji_groups(&rpc)
        .await
        .unwrap();
    sync.emoji_statuses
        .refresh_status_groups(&rpc)
        .await
        .unwrap();
    sync.emoji_statuses
        .refresh_sticker_groups(&rpc)
        .await
        .unwrap();
    sync.emoji_statuses
        .refresh_profile_photo_groups(&rpc)
        .await
        .unwrap();
    assert_eq!(sync.emoji_statuses.emoji_groups.hash, 901);
    assert_eq!(sync.emoji_statuses.emoji_groups.groups.len(), 2);
    assert!(sync.emoji_statuses.emoji_groups.groups[1].is_premium);
    // NotModified preserves groups; changed hash replaces them.
    sync.emoji_statuses
        .refresh_emoji_groups(&rpc)
        .await
        .unwrap();
    assert_eq!(sync.emoji_statuses.emoji_groups.groups[0].title, "Love");
    state.lock().unwrap().groups_rev = 1;
    sync.emoji_statuses
        .refresh_emoji_groups(&rpc)
        .await
        .unwrap();
    assert_eq!(sync.emoji_statuses.emoji_groups.hash, 902);
    assert_eq!(sync.emoji_statuses.emoji_groups.groups[0].title, "Party");

    // 3. Emoji keywords: full fetch -> throttle -> empty diff -> incremental diff -> bad lang_code reset.
    assert!(
        sync.emoji_keywords
            .refresh(&rpc, "en", t0, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.emoji_keywords.packs["en"].version, 10);
    assert_eq!(
        sync.emoji_keywords.packs["en"].keywords["heart"],
        vec!["❤️", "💙"]
    );
    assert!(
        !sync
            .emoji_keywords
            .refresh(&rpc, "en", t0 + EMOJI_KEYWORDS_REFRESH_MS - 1, false)
            .await
            .unwrap()
    );
    // At +1h: sends GetEmojiKeywordsDifference(from_version=10), empty diff preserves cache.
    let t_kw1 = t0 + EMOJI_KEYWORDS_REFRESH_MS;
    assert!(
        sync.emoji_keywords
            .refresh(&rpc, "en", t_kw1, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.emoji_keywords.packs["en"].version, 10);
    assert!(
        sync.emoji_keywords.packs["en"]
            .keywords
            .contains_key("fire")
    );
    // Incremental diff deletes "💙" from "heart", removes "fire" completely, adds "rocket".
    state.lock().unwrap().kw_rev = 1;
    let t_kw2 = t_kw1 + EMOJI_KEYWORDS_REFRESH_MS;
    assert!(
        sync.emoji_keywords
            .refresh(&rpc, "en", t_kw2, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.emoji_keywords.packs["en"].version, 11);
    assert_eq!(
        sync.emoji_keywords.packs["en"].keywords["heart"],
        vec!["❤️"]
    );
    assert!(
        !sync.emoji_keywords.packs["en"]
            .keywords
            .contains_key("fire")
    );
    assert_eq!(
        sync.emoji_keywords.packs["en"].keywords["rocket"],
        vec!["🚀"]
    );
    // Mismatched lang_code resets version to 0 without wiping dictionary (`emoji_keywords.cpp:425-431`).
    state.lock().unwrap().kw_bad_lang = true;
    assert!(
        sync.emoji_keywords
            .refresh(&rpc, "en", t_kw2 + 1, true)
            .await
            .unwrap()
    );
    assert_eq!(sync.emoji_keywords.packs["en"].version, 0);
    assert_eq!(
        sync.emoji_keywords.packs["en"].keywords["rocket"],
        vec!["🚀"]
    );

    // 4. AppConfig (`help.getAppConfig` hash, 1h throttle, NotModified, replace).
    assert!(sync.app_config.refresh(&rpc, t0, false).await.unwrap());
    assert_eq!(sync.app_config.hash, 1001);
    assert_eq!(
        sync.app_config.values["quote_length_max"],
        AppConfigValue::Number(1024.0)
    );
    assert!(
        !sync
            .app_config
            .refresh(&rpc, t0 + APP_CONFIG_REFRESH_MS - 1, false)
            .await
            .unwrap()
    );
    assert!(
        sync.app_config
            .refresh(&rpc, t0 + APP_CONFIG_REFRESH_MS, false)
            .await
            .unwrap()
    ); // NotModified
    assert_eq!(
        sync.app_config.values["quote_length_max"],
        AppConfigValue::Number(1024.0)
    );
    state.lock().unwrap().app_cfg_rev = 1;
    assert!(
        sync.app_config
            .refresh(&rpc, t0 + 2 * APP_CONFIG_REFRESH_MS, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.app_config.hash, 1002);
    assert_eq!(
        sync.app_config.values["quote_length_max"],
        AppConfigValue::Number(2048.0)
    );

    // 5. PeerColors & PeerProfileColors (1h throttle, NotModified, replace).
    assert!(sync.peer_colors.refresh(&rpc, t0, false).await.unwrap());
    assert!(
        sync.peer_colors
            .refresh_profile(&rpc, t0, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.peer_colors.hash, 2001);
    assert_eq!(sync.peer_colors.colors[0].color_id, 7);
    assert!(
        !sync
            .peer_colors
            .refresh(&rpc, t0 + PEER_COLORS_REFRESH_MS - 1, false)
            .await
            .unwrap()
    );
    assert!(
        sync.peer_colors
            .refresh(&rpc, t0 + PEER_COLORS_REFRESH_MS, false)
            .await
            .unwrap()
    ); // NotModified
    assert_eq!(sync.peer_colors.colors[0].color_id, 7);
    state.lock().unwrap().peer_colors_rev = 1;
    assert!(
        sync.peer_colors
            .refresh(&rpc, t0 + 2 * PEER_COLORS_REFRESH_MS, false)
            .await
            .unwrap()
    );
    assert_eq!(sync.peer_colors.hash, 2002);
    assert_eq!(sync.peer_colors.colors[0].color_id, 9);

    // 6. WallPapers (`account.getWallPapers`).
    assert!(sync.wallpapers.refresh(&rpc).await.unwrap());
    assert_eq!(sync.wallpapers.hash, 3001);
    assert_eq!(sync.wallpapers.wallpapers[0].id, 11);
    assert!(!sync.wallpapers.refresh(&rpc).await.unwrap()); // NotModified
    assert_eq!(sync.wallpapers.wallpapers[0].id, 11);
    state.lock().unwrap().wallpapers_rev = 1;
    assert!(sync.wallpapers.refresh(&rpc).await.unwrap());
    assert_eq!(sync.wallpapers.hash, 3002);
    assert_eq!(sync.wallpapers.wallpapers[0].id, 22);

    // 7. CloudThemes & ChatThemes (`account.getThemes`, `account.getChatThemes`).
    assert!(sync.themes.refresh(&rpc).await.unwrap());
    assert_eq!(sync.themes.hash, 4001);
    assert!(!sync.themes.refresh(&rpc).await.unwrap()); // NotModified
    state.lock().unwrap().themes_rev = 1;
    assert!(sync.themes.refresh(&rpc).await.unwrap());
    assert_eq!(sync.themes.hash, 4002);
    assert_eq!(sync.themes.themes[0].id, 2);
    assert!(sync.themes.refresh_chat_themes(&rpc).await.unwrap());
    assert!(!sync.themes.refresh_chat_themes(&rpc).await.unwrap()); // NotModified
    assert_eq!(sync.themes.chat_themes[0].emoticon.as_deref(), Some("❤️"));

    // 8. AttachMenuBots (`messages.getAttachMenuBots` + user ingestion).
    assert!(
        sync.attach_menu_bots
            .refresh(&rpc, Some(&mut session))
            .await
            .unwrap()
    );
    assert_eq!(sync.attach_menu_bots.hash, 5001);
    assert_eq!(sync.attach_menu_bots.bots[0].bot_id, 7001);
    assert!(session.user(UserId(7001)).is_some());
    assert!(
        !sync
            .attach_menu_bots
            .refresh(&rpc, Some(&mut session))
            .await
            .unwrap()
    ); // NotModified
    state.lock().unwrap().attach_rev = 1;
    assert!(
        sync.attach_menu_bots
            .refresh(&rpc, Some(&mut session))
            .await
            .unwrap()
    );
    assert_eq!(sync.attach_menu_bots.hash, 5002);
    assert_eq!(sync.attach_menu_bots.bots[0].bot_id, 7002);

    // 9. PremiumPromo (`help.getPremiumPromo` + user ingestion).
    sync.premium_promo
        .refresh(&rpc, Some(&mut session))
        .await
        .unwrap();
    assert_eq!(sync.premium_promo.status_text, "Go Premium");
    assert_eq!(sync.premium_promo.period_options[0].amount, 499);
    assert_eq!(
        sync.premium_promo.video_sections,
        vec![("more_upload".to_owned(), 9901)]
    );
    assert!(session.user(UserId(777_000)).is_some());

    // 10. SavedRingtones (`account.getSavedRingtones`).
    assert!(sync.ringtones.refresh(&rpc).await.unwrap());
    assert_eq!(sync.ringtones.hash, 6001);
    assert_eq!(sync.ringtones.ringtone_ids, vec![6101]);
    assert!(!sync.ringtones.refresh(&rpc).await.unwrap()); // NotModified
    state.lock().unwrap().ringtones_rev = 1;
    assert!(sync.ringtones.refresh(&rpc).await.unwrap());
    assert_eq!(sync.ringtones.hash, 6002);
    assert_eq!(sync.ringtones.ringtone_ids, vec![6102]);

    // 11. QuickReplies (`messages.getQuickReplies`, 60s throttle, NotModified).
    assert!(
        sync.quick_replies
            .refresh(&rpc, Some(&mut session), t0, false)
            .await
            .unwrap()
    );
    assert!(sync.quick_replies.loaded);
    assert_eq!(sync.quick_replies.shortcuts[&1].shortcut, "hello");
    assert!(session.user(UserId(8801)).is_some());
    assert!(
        !sync
            .quick_replies
            .refresh(
                &rpc,
                Some(&mut session),
                t0 + QUICK_REPLIES_REFRESH_LIMIT_MS - 1,
                false
            )
            .await
            .unwrap()
    );
    state.lock().unwrap().quick_replies_not_modified = true;
    assert!(
        sync.quick_replies
            .refresh(
                &rpc,
                Some(&mut session),
                t0 + QUICK_REPLIES_REFRESH_LIMIT_MS,
                false
            )
            .await
            .unwrap()
    );
    assert_eq!(sync.quick_replies.shortcuts[&1].shortcut, "hello");
}

// ============================================================================
// 4. Live Server Updates over MTProto 2.0 (`SessionRuntime` + `UpdatesHandler`
//    + `CachedListsSync`)
// ============================================================================

#[tokio::test]
async fn live_server_updates_dispatch_refetch_and_local_mutations_over_wire() {
    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_700_000_000,
                seq: 1,
                unread_count: 0,
            }))
        }
        x if x == tl_fns::users::GetUsers::CONSTRUCTOR_ID => {
            ServerAction::ok(&vec![tl_user(1, 100, "Self", false)])
        }
        x if x == tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID => {
            let s1 = tl_sticker_set_header(100, 10, 1, "s100", 1, Some(10), false, false, false);
            let s2 = tl_sticker_set_header(200, 20, 2, "s200", 1, Some(20), false, false, false);
            ServerAction::ok(&tl_enums::messages::AllStickers::AllStickers(
                tl_types::messages::AllStickers {
                    hash: count_hash_i32([1, 2]) as i64,
                    sets: vec![s1, s2],
                },
            ))
        }
        x if x == tl_fns::messages::GetMaskStickers::CONSTRUCTOR_ID => {
            let m1 = tl_sticker_set_header(300, 30, 3, "m300", 1, Some(10), false, true, false);
            ServerAction::ok(&tl_enums::messages::AllStickers::AllStickers(
                tl_types::messages::AllStickers {
                    hash: 3,
                    sets: vec![m1],
                },
            ))
        }
        x if x == tl_fns::messages::GetEmojiStickers::CONSTRUCTOR_ID => {
            let e1 = tl_sticker_set_header(400, 40, 4, "e400", 1, Some(10), false, false, true);
            ServerAction::ok(&tl_enums::messages::AllStickers::AllStickers(
                tl_types::messages::AllStickers {
                    hash: 4,
                    sets: vec![e1],
                },
            ))
        }
        x if x == tl_fns::messages::GetFeaturedStickers::CONSTRUCTOR_ID
            || x == tl_fns::messages::GetFeaturedEmojiStickers::CONSTRUCTOR_ID =>
        {
            ServerAction::ok(&tl_enums::messages::FeaturedStickers::NotModified(
                tl_types::messages::FeaturedStickersNotModified { count: 0 },
            ))
        }
        x if x == tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::messages::RecentStickers::RecentStickers(
                tl_types::messages::RecentStickers {
                    hash: 501,
                    packs: vec![],
                    stickers: vec![tl_doc(501)],
                    dates: vec![1_700_000_001],
                },
            ))
        }
        x if x == tl_fns::messages::GetFavedStickers::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::FavedStickers::FavedStickers(tl_types::messages::FavedStickers {
                hash: 601,
                packs: vec![],
                stickers: vec![tl_doc(601)],
            }),
        ),
        x if x == tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::SavedGifs::SavedGifs(tl_types::messages::SavedGifs {
                hash: 701,
                gifs: vec![tl_doc(701)],
            }),
        ),
        x if x == tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetStickerSet::from_bytes(&rpc.body).unwrap();
            let (id, access_hash) = match req.stickerset {
                tl_enums::InputStickerSet::ID(s) => (s.id, s.access_hash),
                _ => (999, 999),
            };
            let (h, masks, emojis) = match id {
                100 => (1, false, false),
                200 => (2, false, false),
                300 => (3, true, false),
                400 => (4, false, true),
                _ => (0, false, false),
            };
            ServerAction::ok(&tl_full_sticker_set(
                tl_sticker_set_header(
                    id,
                    access_hash,
                    h,
                    &format!("s{id}"),
                    1,
                    Some(10),
                    false,
                    masks,
                    emojis,
                ),
                &[id * 10],
                "😀",
            ))
        }
        x if x == tl_fns::messages::GetRecentReactions::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::Reactions::Reactions(tl_types::messages::Reactions {
                hash: 888,
                reactions: vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "🔥".into(),
                })],
            }),
        ),
        x if x == tl_fns::account::GetRecentEmojiStatuses::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::account::EmojiStatuses::EmojiStatuses(tl_types::account::EmojiStatuses {
                hash: 999,
                statuses: vec![tl_enums::EmojiStatus::EmojiStatus(tl_types::EmojiStatus {
                    document_id: 9999,
                    until: None,
                })],
            }),
        ),
        x if x == tl_fns::help::GetConfig::CONSTRUCTOR_ID => ServerAction::ok(&tl_config()),
        x if x == tl_fns::help::GetAppConfig::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::help::AppConfig::AppConfig(tl_types::help::AppConfig {
                hash: 4242,
                config: tl_enums::JSONValue::JsonObject(tl_types::JsonObject {
                    value: vec![tl_enums::JSONObjectValue::JsonObjectValue(
                        tl_types::JsonObjectValue {
                            key: "wallet_available".into(),
                            value: tl_enums::JSONValue::JsonBool(tl_types::JsonBool {
                                value: true,
                            }),
                        },
                    )],
                }),
            }),
        ),
        x if x == tl_fns::messages::GetAttachMenuBots::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::AttachMenuBots::AttachMenuBots(tl_types::AttachMenuBots {
                hash: 3131,
                bots: vec![],
                users: vec![],
            }),
        ),
        x if x == tl_fns::account::GetSavedRingtones::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetSavedRingtones::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(&tl_enums::account::SavedRingtones::SavedRingtones(
                tl_types::account::SavedRingtones {
                    hash: if req.hash == 0 { 7171 } else { req.hash },
                    ringtones: vec![tl_doc(7171)],
                },
            ))
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_client(&server, 2);
    let runtime = SessionRuntime::start_mtproto_on(
        UserId(1),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: true,
            idle_ping_enabled: false,
            ..Default::default()
        },
    )
    .expect("start SessionRuntime");
    let mut events_rx = runtime.subscribe_events();
    tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
        .await
        .unwrap()
        .unwrap();

    let mut sync = CachedListsSync::new();
    let t0 = 5_000_000u64;
    // Prime caches so timestamps are fresh (< 1h).
    sync.stickers.update_stickers(&rpc, t0).await.unwrap();
    sync.stickers.update_masks(&rpc, t0).await.unwrap();
    sync.stickers.update_custom_emoji(&rpc, t0).await.unwrap();
    sync.stickers.update_saved_gifs(&rpc, t0).await.unwrap();
    sync.app_config.refresh(&rpc, t0, false).await.unwrap();
    sync.ringtones.hash = 5555;
    sync.ringtones.ringtone_ids = vec![5555];

    let push_short = |u: tl_enums::Update| {
        assert!(server.push_update_to_dc(
            2,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: u,
                date: 1_700_000_005,
            }),
        ));
    };

    async fn next_event(rx: &mut tokio::sync::broadcast::Receiver<SessionEvent>) -> SessionEvent {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                match rx.recv().await.expect("event") {
                    SessionEvent::Ready => continue,
                    other => return other,
                }
            }
        })
        .await
        .expect("event timeout")
    }

    // 1. updateStickerSetsOrder with known sets [200, 100]: reorders locally with ZERO RPCs!
    let rpcs_before = server.rpcs().len();
    push_short(tl_enums::Update::StickerSetsOrder(
        tl_types::UpdateStickerSetsOrder {
            masks: false,
            emojis: false,
            order: vec![200, 100],
        },
    ));
    let ev = next_event(&mut events_rx).await;
    assert_eq!(
        ev,
        SessionEvent::StickerSetsOrder {
            masks: false,
            emojis: false,
            order: vec![200, 100],
        }
    );
    sync.handle_update_event(
        &rpc,
        None,
        &UpdateEvent::StickerSetsOrder {
            masks: false,
            emojis: false,
            order: vec![200, 100],
        },
        t0 + 10,
    )
    .await
    .unwrap();
    assert_eq!(sync.stickers.sets_order, vec![200, 100]);
    assert_eq!(
        server.rpcs().len(),
        rpcs_before,
        "local reorder must not trigger RPC"
    );

    // 2. updateMoveStickerSetToTop (known set 100 -> index 0): local rotate with ZERO RPCs!
    push_short(tl_enums::Update::MoveStickerSetToTop(
        tl_types::UpdateMoveStickerSetToTop {
            masks: false,
            emojis: false,
            stickerset: 100,
        },
    ));
    let ev = next_event(&mut events_rx).await;
    assert_eq!(
        ev,
        SessionEvent::MoveStickerSetToTop {
            masks: false,
            emojis: false,
            stickerset: 100,
        }
    );
    sync.handle_update_event(
        &rpc,
        None,
        &UpdateEvent::MoveStickerSetToTop {
            masks: false,
            emojis: false,
            stickerset: 100,
        },
        t0 + 20,
    )
    .await
    .unwrap();
    assert_eq!(sync.stickers.sets_order, vec![100, 200]);
    assert_eq!(
        server.rpcs().len(),
        rpcs_before,
        "local move-to-top must not trigger RPC"
    );

    // 3. updateNewStickerSet: inserts at front locally with ZERO RPCs!
    let new_set = tl_full_sticker_set(
        tl_sticker_set_header(
            250,
            25,
            25,
            "s250",
            1,
            Some(1_700_000_010),
            false,
            false,
            false,
        ),
        &[2500],
        "🌟",
    );
    push_short(tl_enums::Update::NewStickerSet(
        tl_types::UpdateNewStickerSet {
            stickerset: new_set.clone(),
        },
    ));
    let ev = next_event(&mut events_rx).await;
    assert!(matches!(ev, SessionEvent::NewStickerSet(_)));
    sync.handle_update_event(
        &rpc,
        None,
        &UpdateEvent::NewStickerSet(Box::new(new_set)),
        t0 + 30,
    )
    .await
    .unwrap();
    assert_eq!(sync.stickers.sets_order, vec![250, 100, 200]);
    assert_eq!(sync.stickers.sets[&250].stickers, vec![2500]);
    assert_eq!(server.rpcs().len(), rpcs_before);

    // Rejects archived or missing installed_date in updateNewStickerSet (`data_stickers.cpp:1768-1776`).
    let archived_new = tl_full_sticker_set(
        tl_sticker_set_header(251, 25, 25, "arch", 1, Some(10), true, false, false),
        &[2510],
        "🌟",
    );
    assert_eq!(
        sync.stickers
            .apply_update_new_sticker_set(&archived_new, 10),
        None
    );
    assert!(!sync.stickers.sets.contains_key(&251));

    // 4. updateStickerSetsOrder with unknown ID 999 -> triggers GetAllStickers refetch!
    let all_before = server.rpcs_of::<tl_fns::messages::GetAllStickers>().len();
    sync.handle_update_event(
        &rpc,
        None,
        &UpdateEvent::StickerSetsOrder {
            masks: false,
            emojis: false,
            order: vec![999, 100, 200],
        },
        t0 + 40,
    )
    .await
    .unwrap();
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetAllStickers>().len(),
        all_before + 1
    );

    // 5. updateStickerSets with masks=true and emojis=true route to respective RPCs!
    let masks_before = server.rpcs_of::<tl_fns::messages::GetMaskStickers>().len();
    let emoji_before = server.rpcs_of::<tl_fns::messages::GetEmojiStickers>().len();
    push_short(tl_enums::Update::StickerSets(tl_types::UpdateStickerSets {
        masks: true,
        emojis: false,
    }));
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::StickerSets {
            masks: true,
            emojis: false,
        }
    );
    sync.handle_update_event(
        &rpc,
        None,
        &UpdateEvent::StickerSets {
            masks: true,
            emojis: false,
        },
        t0 + 50,
    )
    .await
    .unwrap();
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetMaskStickers>().len(),
        masks_before + 1
    );

    push_short(tl_enums::Update::StickerSets(tl_types::UpdateStickerSets {
        masks: false,
        emojis: true,
    }));
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::StickerSets {
            masks: false,
            emojis: true,
        }
    );
    sync.handle_update_event(
        &rpc,
        None,
        &UpdateEvent::StickerSets {
            masks: false,
            emojis: true,
        },
        t0 + 60,
    )
    .await
    .unwrap();
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetEmojiStickers>().len(),
        emoji_before + 1
    );

    // 6. updateRecentStickers & updateFavedStickers: only reset their own timestamp,
    //    NOT re-polling fresh GetAllStickers!
    let all_n = server.rpcs_of::<tl_fns::messages::GetAllStickers>().len();
    let recent_n = server
        .rpcs_of::<tl_fns::messages::GetRecentStickers>()
        .len();
    let faved_n = server.rpcs_of::<tl_fns::messages::GetFavedStickers>().len();
    push_short(tl_enums::Update::RecentStickers);
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::RecentStickers
    );
    sync.handle_update_event(&rpc, None, &UpdateEvent::RecentStickers, t0 + 70)
        .await
        .unwrap();
    assert_eq!(
        server
            .rpcs_of::<tl_fns::messages::GetRecentStickers>()
            .len(),
        recent_n + 1
    );
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetAllStickers>().len(),
        all_n
    );
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetFavedStickers>().len(),
        faved_n
    );

    push_short(tl_enums::Update::FavedStickers);
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::FavedStickers
    );
    sync.handle_update_event(&rpc, None, &UpdateEvent::FavedStickers, t0 + 80)
        .await
        .unwrap();
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetFavedStickers>().len(),
        faved_n + 1
    );
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetAllStickers>().len(),
        all_n
    );

    // 7. updateSavedGifs -> refetches messages.getSavedGifs.
    let gifs_n = server.rpcs_of::<tl_fns::messages::GetSavedGifs>().len();
    push_short(tl_enums::Update::SavedGifs);
    assert_eq!(next_event(&mut events_rx).await, SessionEvent::SavedGifs);
    sync.handle_update_event(&rpc, None, &UpdateEvent::SavedGifs, t0 + 90)
        .await
        .unwrap();
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetSavedGifs>().len(),
        gifs_n + 1
    );

    // 8. updateRecentReactions & updateRecentEmojiStatuses: 10s debounce & coalescing!
    push_short(tl_enums::Update::RecentReactions);
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::RecentReactions
    );
    sync.handle_update_event(&rpc, None, &UpdateEvent::RecentReactions, t0 + 100)
        .await
        .unwrap();
    sync.handle_update_event(&rpc, None, &UpdateEvent::RecentReactions, t0 + 2_000)
        .await
        .unwrap();
    // Before 10s deadline: zero RPCs!
    sync.poll_due(&rpc, t0 + 100 + RECENT_REACTIONS_DEBOUNCE_MS - 1)
        .await
        .unwrap();
    assert_eq!(
        server
            .rpcs_of::<tl_fns::messages::GetRecentReactions>()
            .len(),
        0
    );
    // At 10s deadline: exactly ONE RPC!
    sync.poll_due(&rpc, t0 + 100 + RECENT_REACTIONS_DEBOUNCE_MS)
        .await
        .unwrap();
    assert_eq!(
        server
            .rpcs_of::<tl_fns::messages::GetRecentReactions>()
            .len(),
        1
    );
    assert_eq!(sync.reactions.recent_hash, 888);

    push_short(tl_enums::Update::RecentEmojiStatuses);
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::RecentEmojiStatuses
    );
    sync.handle_update_event(&rpc, None, &UpdateEvent::RecentEmojiStatuses, t0 + 100)
        .await
        .unwrap();
    sync.poll_due(&rpc, t0 + 100 + RECENT_EMOJI_STATUSES_DEBOUNCE_MS - 1)
        .await
        .unwrap();
    assert_eq!(
        server
            .rpcs_of::<tl_fns::account::GetRecentEmojiStatuses>()
            .len(),
        0
    );
    sync.poll_due(&rpc, t0 + 100 + RECENT_EMOJI_STATUSES_DEBOUNCE_MS)
        .await
        .unwrap();
    assert_eq!(
        server
            .rpcs_of::<tl_fns::account::GetRecentEmojiStatuses>()
            .len(),
        1
    );
    assert_eq!(sync.emoji_statuses.recent_hash, 999);

    // 9. updateConfig -> refetches BOTH help.getConfig AND forced help.getAppConfig even within 1h!
    let cfg_n = server.rpcs_of::<tl_fns::help::GetConfig>().len();
    let app_cfg_n = server.rpcs_of::<tl_fns::help::GetAppConfig>().len();
    push_short(tl_enums::Update::Config);
    assert_eq!(next_event(&mut events_rx).await, SessionEvent::Config);
    sync.handle_update_event(&rpc, None, &UpdateEvent::Config, t0 + 200)
        .await
        .unwrap();
    assert_eq!(server.rpcs_of::<tl_fns::help::GetConfig>().len(), cfg_n + 1);
    assert_eq!(
        server.rpcs_of::<tl_fns::help::GetAppConfig>().len(),
        app_cfg_n + 1
    );

    // 10. updateAttachMenuBots -> refetches messages.getAttachMenuBots.
    push_short(tl_enums::Update::AttachMenuBots);
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::AttachMenuBots
    );
    sync.handle_update_event(&rpc, None, &UpdateEvent::AttachMenuBots, t0 + 210)
        .await
        .unwrap();
    assert_eq!(
        server
            .rpcs_of::<tl_fns::messages::GetAttachMenuBots>()
            .len(),
        1
    );

    // 11. updateSavedRingtones -> resets hash = 0, clears cache, refetches GetSavedRingtones(0)!
    push_short(tl_enums::Update::SavedRingtones);
    assert_eq!(
        next_event(&mut events_rx).await,
        SessionEvent::SavedRingtones
    );
    sync.handle_update_event(&rpc, None, &UpdateEvent::SavedRingtones, t0 + 220)
        .await
        .unwrap();
    let ring_reqs = server.rpcs_of::<tl_fns::account::GetSavedRingtones>();
    assert_eq!(ring_reqs.len(), 1);
    assert_eq!(
        tl_fns::account::GetSavedRingtones::from_bytes(&ring_reqs[0].body)
            .unwrap()
            .hash,
        0
    );
    assert_eq!(sync.ringtones.hash, 7171);
    assert_eq!(sync.ringtones.ringtone_ids, vec![7171]);

    // 12. updateUserEmojiStatus -> updates user's emoji_status directly in tg_data::Session!
    push_short(tl_enums::Update::UserEmojiStatus(
        tl_types::UpdateUserEmojiStatus {
            user_id: 1,
            emoji_status: tl_enums::EmojiStatus::EmojiStatus(tl_types::EmojiStatus {
                document_id: 777_888,
                until: Some(1_900_000_000),
            }),
        },
    ));
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let doc_id = runtime
                .session()
                .read()
                .await
                .user(UserId(1))
                .map(|u| u.common.emoji_status.document_id);
            if doc_id == Some(DocumentId(777_888)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("UserEmojiStatus updated in Session");

    runtime.shutdown();
}

// ============================================================================
// 5. Sticker Set Loading (`InputStickerSet*`, `hash`, `NotModified`),
//    `installStickerSet` Archived Handling, `uninstallStickerSet`,
//    `reorderStickerSets`, and `tg-storage` Cache Persistence
// ============================================================================

#[tokio::test]
async fn sticker_set_loading_variants_install_archived_uninstall_reorder_and_storage_over_wire() {
    let fail_reorder = Arc::new(Mutex::new(false));
    let fail_reorder_srv = Arc::clone(&fail_reorder);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetStickerSet::from_bytes(&rpc.body).unwrap();
            let (id, short_name, hash, doc_id) = match &req.stickerset {
                tl_enums::InputStickerSet::ID(s) => (s.id, format!("id_{}", s.id), 1234, s.id * 10),
                tl_enums::InputStickerSet::ShortName(s) => {
                    (5002, s.short_name.clone(), 2345, 50_020)
                }
                tl_enums::InputStickerSet::AnimatedEmoji => {
                    (5003, "anim_emoji".into(), 3456, 50_030)
                }
                tl_enums::InputStickerSet::Dice(d) => {
                    (5004, format!("dice_{}", d.emoticon), 4567, 50_040)
                }
                tl_enums::InputStickerSet::EmojiGenericAnimations => {
                    (5005, "generic".into(), 5678, 50_050)
                }
                tl_enums::InputStickerSet::AnimatedEmojiAnimations => {
                    (5006, "anim_inter".into(), 6789, 50_060)
                }
                tl_enums::InputStickerSet::EmojiDefaultStatuses => {
                    (5007, "def_statuses".into(), 7890, 50_070)
                }
                tl_enums::InputStickerSet::EmojiDefaultTopicIcons => {
                    (5008, "topic_icons".into(), 8901, 50_080)
                }
                tl_enums::InputStickerSet::EmojiChannelDefaultStatuses => {
                    (5009, "ch_statuses".into(), 9012, 50_090)
                }
                tl_enums::InputStickerSet::PremiumGifts => {
                    (5010, "prem_gifts".into(), 1013, 50_100)
                }
                tl_enums::InputStickerSet::TonGifts => (5011, "ton_gifts".into(), 1114, 50_110),
                tl_enums::InputStickerSet::Empty => (5012, "empty".into(), 1215, 50_120),
            };
            if req.hash == hash {
                ServerAction::ok(&tl_enums::messages::StickerSet::NotModified)
            } else {
                let header = tl_sticker_set_header(
                    id,
                    id * 7,
                    hash,
                    &short_name,
                    1,
                    Some(1_700_000_000),
                    false,
                    false,
                    false,
                );
                ServerAction::ok(&tl_full_sticker_set(header, &[doc_id], "🎲"))
            }
        }
        x if x == tl_fns::messages::InstallStickerSet::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::InstallStickerSet::from_bytes(&rpc.body).unwrap();
            match req.stickerset {
                tl_enums::InputStickerSet::ID(s) if s.id == 6001 => {
                    ServerAction::ok(&tl_enums::messages::StickerSetInstallResult::Success)
                }
                _ => {
                    // Return archived regular set (5001) + an emoji set (7001, skipped by tdesktop
                    // `data_stickers.cpp:465-467`).
                    ServerAction::ok(&tl_enums::messages::StickerSetInstallResult::Archive(
                        tl_types::messages::StickerSetInstallResultArchive {
                            sets: vec![
                                tl_covered(
                                    tl_sticker_set_header(
                                        5001,
                                        5001 * 7,
                                        1234,
                                        "old_pack",
                                        1,
                                        None,
                                        true,
                                        false,
                                        false,
                                    ),
                                    50_010,
                                ),
                                tl_covered(
                                    tl_sticker_set_header(
                                        7001,
                                        7001 * 7,
                                        4321,
                                        "old_emoji",
                                        1,
                                        None,
                                        true,
                                        false,
                                        true,
                                    ),
                                    70_010,
                                ),
                            ],
                        },
                    ))
                }
            }
        }
        x if x == tl_fns::messages::UninstallStickerSet::CONSTRUCTOR_ID => {
            ServerAction::Reply(BOOL_TRUE_ID.to_le_bytes().to_vec())
        }
        x if x == tl_fns::messages::ReorderStickerSets::CONSTRUCTOR_ID => {
            if *fail_reorder_srv.lock().unwrap() {
                ServerAction::err(400, "STICKERSET_INVALID")
            } else {
                ServerAction::Reply(BOOL_TRUE_ID.to_le_bytes().to_vec())
            }
        }
        x if x == tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID => {
            let s1 = tl_sticker_set_header(
                6001,
                6001 * 7,
                11,
                "s6001",
                1,
                Some(10),
                false,
                false,
                false,
            );
            let s2 = tl_sticker_set_header(
                6002,
                6002 * 7,
                22,
                "s6002",
                1,
                Some(20),
                false,
                false,
                false,
            );
            ServerAction::ok(&tl_enums::messages::AllStickers::AllStickers(
                tl_types::messages::AllStickers {
                    hash: count_hash_i32([11, 22]) as i64,
                    sets: vec![s1, s2],
                },
            ))
        }
        x if x == tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::messages::RecentStickers::NotModified)
        }
        x if x == tl_fns::messages::GetFavedStickers::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::messages::FavedStickers::NotModified)
        }
        x if x == tl_fns::messages::GetFeaturedStickers::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::messages::FeaturedStickers::NotModified(
                tl_types::messages::FeaturedStickersNotModified { count: 0 },
            ))
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_client(&server, 2);
    let mut mgr = StickersSyncManager::new();
    let t0 = 2_000_000u64;

    // 1. Load every InputStickerSet variant, then re-query with cached hash -> NotModified preserves cache!
    let specs = [
        StickerSetSpec::Id {
            id: 5001,
            access_hash: 5001 * 7,
        },
        StickerSetSpec::ShortName("custom_short".into()),
        StickerSetSpec::AnimatedEmoji,
        StickerSetSpec::Dice("🎲".into()),
        StickerSetSpec::EmojiGenericAnimations,
        StickerSetSpec::AnimatedEmojiAnimations,
        StickerSetSpec::EmojiDefaultStatuses,
        StickerSetSpec::EmojiDefaultTopicIcons,
        StickerSetSpec::EmojiChannelDefaultStatuses,
        StickerSetSpec::PremiumGifts,
        StickerSetSpec::TonGifts,
    ];
    for spec in &specs {
        let loaded_id = mgr
            .get_sticker_set(&rpc, spec, None, t0)
            .await
            .unwrap()
            .expect("first getStickerSet must return full set");
        let stickers_before = mgr.sets[&loaded_id].stickers.clone();
        assert!(!stickers_before.is_empty());

        // Second call uses cached hash -> server answers stickerSetNotModified -> cache preserved!
        let second = mgr.get_sticker_set(&rpc, spec, None, t0 + 1).await.unwrap();
        assert_eq!(second, None, "expected stickerSetNotModified for {spec:?}");
        assert_eq!(mgr.sets[&loaded_id].stickers, stickers_before);
    }

    // 2. installStickerSet: Success vs Archive result handling!
    mgr.sets_order = vec![5001];
    mgr.emoji_sets_order = vec![7001];
    mgr.sets.insert(
        6001,
        CachedStickerSet {
            id: 6001,
            access_hash: 6001 * 7,
            hash: 11,
            title: "Pack 6001".into(),
            short_name: "p6001".into(),
            count: 1,
            flags: StickerSetFlags::FEATURED | StickerSetFlags::UNREAD,
            install_date: 0,
            thumbnail_document_id: 0,
            stickers: vec![60_010],
            covers: vec![],
            dates: vec![],
            emoji: Default::default(),
        },
    );
    let outcome1 = mgr
        .install_sticker_set(
            &rpc,
            &StickerSetSpec::Id {
                id: 6001,
                access_hash: 6001 * 7,
            },
            false,
            t0,
        )
        .await
        .unwrap();
    assert_eq!(outcome1, InstallStickerSetOutcome::Success);
    assert_eq!(mgr.sets_order, vec![6001, 5001]);
    assert!(mgr.sets[&6001].flags.contains(StickerSetFlags::INSTALLED));
    assert!(!mgr.sets[&6001].flags.contains(StickerSetFlags::UNREAD));

    // Install set 6002 -> server returns Archive([5001 (regular), 7001 (emoji)]).
    mgr.sets.insert(
        6002,
        CachedStickerSet {
            id: 6002,
            access_hash: 6002 * 7,
            hash: 22,
            title: "Pack 6002".into(),
            short_name: "p6002".into(),
            count: 1,
            flags: StickerSetFlags::EMPTY,
            install_date: 0,
            thumbnail_document_id: 0,
            stickers: vec![60_020],
            covers: vec![],
            dates: vec![],
            emoji: Default::default(),
        },
    );
    let outcome2 = mgr
        .install_sticker_set(
            &rpc,
            &StickerSetSpec::Id {
                id: 6002,
                access_hash: 6002 * 7,
            },
            false,
            t0 + 1000,
        )
        .await
        .unwrap();
    assert_eq!(outcome2, InstallStickerSetOutcome::Archived(vec![5001]));
    assert_eq!(mgr.sets_order, vec![6002, 6001]);
    assert!(mgr.archived_sets_order.contains(&5001));
    assert!(mgr.sets[&5001].flags.contains(StickerSetFlags::ARCHIVED));
    // Emoji set 7001 was NOT removed from emoji_sets_order (`data_stickers.cpp:465-467`).
    assert_eq!(mgr.emoji_sets_order, vec![7001]);

    // 3. uninstallStickerSet: erases non-featured set (6002), keeps featured set (6001) without INSTALLED!
    assert!(mgr.uninstall_sticker_set(&rpc, 6002, None).await.unwrap());
    assert!(!mgr.sets_order.contains(&6002));
    assert!(!mgr.sets.contains_key(&6002));

    assert!(mgr.uninstall_sticker_set(&rpc, 6001, None).await.unwrap());
    assert!(!mgr.sets_order.contains(&6001));
    assert!(mgr.sets.contains_key(&6001), "featured set stays cached");
    assert!(!mgr.sets[&6001].flags.contains(StickerSetFlags::INSTALLED));

    // 4. reorderStickerSets: flags, <2 skip, and error-triggered refetch!
    assert!(
        !mgr.reorder_sticker_sets(&rpc, StickersType::Stickers, &[6001], t0)
            .await
            .unwrap()
    );
    assert!(
        mgr.reorder_sticker_sets(&rpc, StickersType::Masks, &[2002, 2001], t0)
            .await
            .unwrap()
    );
    assert_eq!(mgr.mask_sets_order, vec![2002, 2001]);
    let reorder_req = tl_fns::messages::ReorderStickerSets::from_bytes(
        &server.rpcs_of::<tl_fns::messages::ReorderStickerSets>()[0].body,
    )
    .unwrap();
    assert!(reorder_req.masks && !reorder_req.emojis);
    assert_eq!(reorder_req.order, vec![2002, 2001]);

    assert!(
        mgr.reorder_sticker_sets(&rpc, StickersType::Emoji, &[3002, 3001], t0)
            .await
            .unwrap()
    );
    let emoji_reorder = tl_fns::messages::ReorderStickerSets::from_bytes(
        &server.rpcs_of::<tl_fns::messages::ReorderStickerSets>()[1].body,
    )
    .unwrap();
    assert!(!emoji_reorder.masks && emoji_reorder.emojis);

    // Reorder failure resets last_stickers_update_ms and refetches GetAllStickers (`apiwrap.cpp:1698-1710`).
    mgr.last_stickers_update_ms = t0;
    *fail_reorder.lock().unwrap() = true;
    let all_before = server.rpcs_of::<tl_fns::messages::GetAllStickers>().len();
    let err = mgr
        .reorder_sticker_sets(&rpc, StickersType::Stickers, &[6002, 6001], t0 + 10)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Invocation(_)));
    assert_eq!(
        server.rpcs_of::<tl_fns::messages::GetAllStickers>().len(),
        all_before + 1
    );
    assert_eq!(mgr.sets_order, vec![6001, 6002]);

    // 5. tg-storage persistence conformance (`storage/storage_account.cpp`).
    let dir = tempfile::tempdir().expect("tempdir");
    let domain = DomainStorage::create_fresh(dir.path(), "data").expect("domain");
    let mut account =
        AccountStorage::open(dir.path(), "data", 0, domain.local_key().clone()).expect("account");

    // 5a. Saved GIFs deduplication on read (`storage_account.cpp:2803-2818`).
    let gif1 = SerializedDocument {
        id: 1,
        access_hash: 11,
        date: 1_700_000_000,
        dc_id: 2,
        size: 512,
        width: 320,
        height: 240,
        doc_type: DocumentType::Animated,
        duration_ms: 1000,
        ..Default::default()
    };
    let gif2 = SerializedDocument {
        id: 2,
        ..gif1.clone()
    };
    account
        .write_saved_gifs(&[gif1.clone(), gif1.clone(), gif2.clone()])
        .expect("write_saved_gifs");
    let reloaded_gifs = account.read_saved_gifs().expect("read_saved_gifs");
    let reloaded_ids: Vec<u64> = reloaded_gifs.iter().map(|d| d.id).collect();
    assert_eq!(reloaded_ids, vec![1, 2]);
    assert_eq!(
        count_document_vector_hash(&reloaded_ids),
        0x0000_0008_8000_0003
    );

    // 5b. Sticker sets: NOT_LOADED flag encodes negative count & deduplicates sticker docs on read.
    let skey = generate_key(account.base_path());
    let doc1 = SerializedDocument {
        id: 5001,
        access_hash: 6001,
        date: 1_700_000_000,
        dc_id: 2,
        size: 1024,
        width: 512,
        height: 512,
        doc_type: DocumentType::Sticker,
        sticker_alt: "🙂".into(),
        sticker_set_type: StickerSetType::Id,
        duration_ms: -1,
        ..Default::default()
    };
    let sets_file = StoredStickerSetsFile {
        sets: vec![
            StoredStickerSet {
                id: 1001,
                access_hash: 2002,
                hash: tl_sticker_set_hash_to_u64(-1),
                title: "Loaded".into(),
                short_name: "loaded".into(),
                count: 2,
                flags: i32::from(StickerSetFlags::INSTALLED.bits()),
                install_date: 1_700_000_000,
                thumbnail_document_id: 5001,
                thumbnail_type: 0,
                thumbnail_location: ImageLocation::default(),
                stickers: vec![doc1.clone(), doc1.clone()], // duplicate doc ID!
                dates: vec![],
                emoji: vec![("🙂".into(), vec![5001])],
            },
            StoredStickerSet {
                id: 1002,
                access_hash: 2003,
                hash: tl_sticker_set_hash_to_u64(1),
                title: "NotLoaded".into(),
                short_name: "not_loaded".into(),
                count: 25, // positive count + NOT_LOADED flag -> must persist as -25!
                flags: i32::from((StickerSetFlags::INSTALLED | StickerSetFlags::NOT_LOADED).bits()),
                install_date: 1_700_000_001,
                thumbnail_document_id: 0,
                thumbnail_type: 0,
                thumbnail_location: ImageLocation::default(),
                stickers: vec![],
                dates: vec![],
                emoji: vec![],
            },
        ],
        order: vec![1001, 1002],
    };
    account
        .write_sticker_sets(skey, &sets_file)
        .expect("write_sticker_sets");
    let reloaded = account.read_sticker_sets(skey).expect("read_sticker_sets");
    assert_eq!(reloaded.sets.len(), 2);
    assert_eq!(
        reloaded.sets[0].stickers.len(),
        1,
        "duplicate sticker doc deduplicated"
    );
    assert_eq!(reloaded.sets[0].hash, u64::MAX);
    assert_eq!(
        reloaded.sets[1].count, -25,
        "NOT_LOADED persisted with negative count"
    );
    assert_eq!(reloaded.order, vec![1001, 1002]);

    // 5c. Reject valid legacy thumbnail location (`storage_account.cpp:2173-2175`).
    let bad_key = generate_key(account.base_path());
    let legacy_thumb_file = StoredStickerSetsFile {
        sets: vec![StoredStickerSet {
            id: 9001,
            access_hash: 1,
            hash: 1,
            title: "Legacy".into(),
            short_name: "legacy".into(),
            count: 0,
            flags: 1,
            install_date: 0,
            thumbnail_document_id: 0,
            thumbnail_type: 0,
            thumbnail_location: ImageLocation {
                file: DownloadLocation::Storage(StorageFileLocation {
                    dc_id: 2,
                    location_type: StorageFileLocationType::Legacy,
                    volume_id: 100,
                    local_id: 200,
                    ..Default::default()
                }),
                width: 100,
                height: 100,
            },
            stickers: vec![],
            dates: vec![],
            emoji: vec![],
        }],
        order: vec![9001],
    };
    account
        .write_sticker_sets(bad_key, &legacy_thumb_file)
        .expect("write legacy");
    assert!(
        account.read_sticker_sets(bad_key).is_err(),
        "valid legacy sticker set thumbnail location must be rejected"
    );
}
