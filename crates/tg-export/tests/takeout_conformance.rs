//! Byte-level tdesktop conformance suite for Data Export (Takeout) (`tg-export`) driven
//! end-to-end against an in-process scripted fake MTProto 2.0 server (`tg-mtproto::Client`
//! over `FakeConnection` / `TransportFactory` with full DH + AES-256-IGE encryption).
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233):
//! - `Telegram/SourceFiles/export/export_api_wrap.h` / `export_api_wrap.cpp`
//! - `Telegram/SourceFiles/export/export_controller.cpp`
//! - `Telegram/SourceFiles/export/export_settings.cpp`
//!
//! Verifies:
//! 1. `account.initTakeoutSession` flags (`contacts`, `message_*`, `files`, `file_max_size`
//!    omitted at `>= 4000 MiB`, `files = false` when `size_limit == 0` without Userpics/Stories).
//! 2. Exact `invokeWithTakeout` + `invokeWithMessagesRange` wire wrapping over MTProto 2.0,
//!    including empty `messages.getSplitRanges` fallback (`[1, i32::MAX]`), `useOnlyLastSplit`
//!    for channel-only exports, and reverse split traversal for `messages.getDialogs`.
//! 3. `TAKEOUT_INIT_DELAY_X` (420) and `TAKEOUT_INVALID` (400) error surfacing and parsing.
//! 4. `TAKEOUT_FILE_EMPTY` handling for `inputTakeoutFileLocation` (`other_data.json`).
//! 5. Chunked `upload.getFile` (`flags = 0`, 128 KiB chunks):
//!    - Exact multiple of 128 KiB (256 KiB) stops cleanly without an over-EOF request.
//!    - Non-multiple (300 KiB) downloads in 3 chunks (128 KiB + 128 KiB + 44 KiB).
//!    - Multi-DC `FILE_MIGRATE_4` redirect onto real guest DC 4 over MTProto 2.0.
//!    - Mid-file `FILE_REFERENCE_EXPIRED` (at `offset = 128 KiB`) refreshes via
//!      `messages.getMessages` (wrapped in `invokeWithTakeout` + `invokeWithMessagesRange`)
//!      and resumes at `offset = 128 KiB` without discarding chunk 0.
//!    - Channel `FILE_REFERENCE_EXPIRED` refreshes via `channels.getMessages` wrapped in
//!      `invokeWithTakeout` ONLY (no `invokeWithMessagesRange`, `export_api_wrap.cpp:3708`).
//!    - Failed file-reference refresh / `VERSION_INVALID` / `LOCATION_NOT_AVAILABLE` gracefully
//!      marks only that file `Unavailable` instead of aborting the export.
//!    - `LoadedFileCache` deduplicates media with identical `(dc_id, kind, thumb, id)` even when
//!      `file_reference` bytes differ (`ComputeLocationKey`).
//! 6. `CHANNEL_PRIVATE` fallback to `messages.search(from_id = inputPeerSelf)` (including
//!    mid-history fallback), `single_peer_from` `offset_date = single_peer_from - 1`,
//!    `photos.getUserPhotos` `max_id` pagination, `contacts.getTopPeers` count-based pagination,
//!    `account.finishTakeoutSession(success = true / false)`, and HTML+JSON output integrity.
//!
//! Security & test hygiene:
//! - Zero network access: all MTProto 2.0 traffic runs in-process over `FakeConnection`.
//! - No real accounts, phone numbers, credentials, or API secrets; uses synthetic test values
//!   (`api_id = 1`) and an ephemeral test-only RSA keypair.

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_export::api_wrap::{
    ExportRpc, FILE_CHUNK_SIZE, MESSAGES_SLICE_LIMIT, RpcCallError, TlExportSource,
};
use tg_export::controller::{ExportController, ExportState};
use tg_export::output::OutputFormat;
use tg_export::settings::{
    Environment, ExportTypes, MAX_FILE_SIZE, MediaSettings, MediaTypes, Settings,
};
use tg_export::stand_in::{InputFileLocation, InputPeer};
use tg_mtproto::dc_options::DcId;
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, InvocationError, PacketConnection,
    TransportFactory,
};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    Boxed, Cursor, Deserializable, Identifiable, LAYER, RawObject, RemoteCall, Serializable,
    enums as tl_e, functions as tl_f, types as tl_t,
};

// ============================================================================
// Ephemeral test-only RSA keypair & helpers (zero network / zero real secrets)
// ============================================================================

const FAKE_API_ID: i32 = 1;
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

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(1);

fn temp_dir(label: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "tg_export_conf_{label}_{}_{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn dir_with_slash(dir: &Path) -> String {
    let mut s = dir.to_string_lossy().replace('\\', "/");
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

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
        .expect("valid test RSA key")
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
// In-Process Scripted Fake MTProto 2.0 Server + `MtprotoExportRpc` Bridge
// ============================================================================

#[derive(Debug, Clone)]
struct TakeoutWireCall {
    dc_id: DcId,
    takeout_id: Option<i64>,
    split_range: Option<(i32, i32)>,
    inner_ctor: u32,
    inner_body: Vec<u8>,
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

type WireHandler = Arc<dyn Fn(&TakeoutWireCall) -> ServerAction + Send + Sync>;

#[derive(Debug, Clone)]
struct ServerKeyEntry {
    key: AuthKey,
    server_salt: u64,
}

struct ScriptedMtprotoServerInner {
    rsa_pub: PublicKey,
    rsa_n: Vec<u8>,
    rsa_d: Vec<u8>,
    next_conn_id: AtomicU64,
    dh_counter: AtomicU64,
    keys: Mutex<HashMap<u64, ServerKeyEntry>>,
    calls: Mutex<Vec<TakeoutWireCall>>,
    handler: WireHandler,
}

#[derive(Clone)]
struct ScriptedMtprotoServer {
    inner: Arc<ScriptedMtprotoServerInner>,
}

impl ScriptedMtprotoServer {
    fn new<F>(handler: F) -> Self
    where
        F: Fn(&TakeoutWireCall) -> ServerAction + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(ScriptedMtprotoServerInner {
                rsa_pub: test_rsa_public_key(),
                rsa_n: hex_decode(TEST_RSA_N_HEX),
                rsa_d: hex_decode(TEST_RSA_D_HEX),
                next_conn_id: AtomicU64::new(1),
                dh_counter: AtomicU64::new(1),
                keys: Mutex::new(HashMap::new()),
                calls: Mutex::new(Vec::new()),
                handler: Arc::new(handler),
            }),
        }
    }

    fn recorded_calls(&self) -> Vec<TakeoutWireCall> {
        self.inner.calls.lock().unwrap().clone()
    }

    fn create_export_rpc(&self, main_dc: DcId) -> MtprotoExportRpc {
        let cfg = ClientConfig {
            api_id: FAKE_API_ID,
            main_dc_id: main_dc,
            proxy: ProxyData::none(),
            use_pfs: false,
            ping_interval: Duration::from_secs(3600),
            max_auto_flood_wait_secs: Some(0),
            ..ClientConfig::default()
        };
        let (client, _updates_rx) = Client::with_transport(cfg, Arc::new(self.clone()));
        client
            .dc_options()
            .add_public_key(self.inner.rsa_pub.clone());
        MtprotoExportRpc { client }
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
        assert_eq!(data_with_hash.len(), 224);
        let mut data_with_padding = data_with_hash[0..192].to_vec();
        data_with_padding.reverse();
        let expected_hash = sha256(&[&temp_key, &data_with_padding]);
        assert_eq!(&data_with_hash[192..224], &expected_hash);
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
            let server_msg_counter = Arc::new(AtomicU64::new(1));
            let server_seq_counter = Arc::new(Mutex::new(0u32));
            let mut sent_new_session = false;
            let tx = peer.tx.clone();

            let next_server_msg_id = |counter: &AtomicU64, is_reply: bool| -> u64 {
                let secs = now_unix_secs().max(1_700_000_000) as u64;
                let low_bits = if is_reply { 1 } else { 3 };
                let c = counter.fetch_add(1, Ordering::Relaxed);
                (secs << 32) | ((c << 2) | low_bits)
            };

            let send_encrypted =
                |tx: &tokio::sync::mpsc::UnboundedSender<Result<Vec<u8>, TransportError>>,
                 key: &AuthKey,
                 salt: u64,
                 session_id: u64,
                 msg_id: u64,
                 content_related: bool,
                 seq_lock: &Mutex<u32>,
                 body: &[u8]| {
                    let mut guard = seq_lock.lock().unwrap();
                    let seq_no = if content_related {
                        let s = *guard * 2 + 1;
                        *guard += 1;
                        s
                    } else {
                        *guard * 2
                    };
                    drop(guard);
                    let pkt = build_encrypted_packet(
                        salt,
                        session_id,
                        msg_id,
                        seq_no,
                        body,
                        key,
                        Side::Server,
                    );
                    let _ = tx.send(Ok(pkt));
                };

            while let Some(raw) = peer.rx.recv().await {
                if raw.len() < 8 {
                    break;
                }
                let auth_key_id = u64::from_le_bytes(raw[0..8].try_into().unwrap());
                if auth_key_id == 0 {
                    assert!(raw.len() >= 20);
                    let msg_len = u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
                    let body = &raw[20..20 + msg_len];
                    let ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());

                    if ctor == mtp_functions::ReqPqMulti::CONSTRUCTOR_ID {
                        let req = mtp_functions::ReqPqMulti::from_bytes(body).unwrap();
                        let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                        let mut server_nonce = [0u8; 16];
                        server_nonce[0..8].copy_from_slice(&seq.to_le_bytes());
                        server_nonce[8..16]
                            .copy_from_slice(&0xA55A_C33C_1234_5678u64.to_le_bytes());
                        let res_pq = mtp_enums::ResPQ::ResPQ(mtp_types::ResPQ {
                            nonce: req.nonce,
                            server_nonce,
                            pq: vec![0x01, 0x87],
                            server_public_key_fingerprints: vec![
                                server.inner.rsa_pub.fingerprint() as i64,
                            ],
                        });
                        let out_id = next_server_msg_id(&server_msg_counter, true);
                        let _ = tx.send(Ok(encode_unencrypted_packet(out_id, &res_pq.to_bytes())));
                    } else if ctor == mtp_functions::ReqDHParams::CONSTRUCTOR_ID {
                        let req = mtp_functions::ReqDHParams::from_bytes(body).unwrap();
                        let plain = server.decrypt_rsa_pad(&req.encrypted_data);
                        let mut cur = Cursor::new(&plain);
                        let pq_inner = mtp_enums::PQInnerData::deserialize(&mut cur).unwrap();
                        let new_nonce = match pq_inner {
                            mtp_enums::PQInnerData::Dc(d) => d.new_nonce,
                            mtp_enums::PQInnerData::TempDc(d) => d.new_nonce,
                            other => panic!("unexpected PQInnerData: {other:?}"),
                        };
                        let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                        let mut a_srv = [0x42u8; 256];
                        a_srv[0..8].copy_from_slice(&seq.to_be_bytes());
                        a_srv[248..256].copy_from_slice(&(seq ^ 0x55AA_55AA).to_be_bytes());
                        let g_a =
                            pad_be_256(&mod_exp(&[3], &a_srv, &GOOD_PRIME).expect("g^a mod p"));

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
                        let (aes_key, aes_iv) = dh_server_aes_key_iv(&new_nonce, &req.server_nonce);
                        let encrypted_answer =
                            aes_ige_encrypt(&answer_with_hash, &aes_key, &aes_iv);
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
                        let out_id = next_server_msg_id(&server_msg_counter, true);
                        let _ = tx.send(Ok(encode_unencrypted_packet(out_id, &resp.to_bytes())));
                    } else if ctor == mtp_functions::SetClientDHParams::CONSTRUCTOR_ID {
                        let req = mtp_functions::SetClientDHParams::from_bytes(body).unwrap();
                        let pending = dh_pending.take().unwrap();
                        let dec =
                            aes_ige_decrypt(&req.encrypted_data, &pending.aes_key, &pending.aes_iv);
                        let mut cur = Cursor::new(&dec[20..]);
                        let mtp_enums::ClientDHInnerData::ClientDHInnerData(cdh) =
                            mtp_enums::ClientDHInnerData::deserialize(&mut cur).unwrap();
                        let shared = pad_be_256(
                            &mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME).expect("g_b^a mod p"),
                        );
                        let auth_key = AuthKey::new(shared);
                        let nn_lo = u64::from_le_bytes(pending.new_nonce[0..8].try_into().unwrap());
                        let sn_lo =
                            u64::from_le_bytes(pending.server_nonce[0..8].try_into().unwrap());
                        let server_salt = nn_lo ^ sn_lo;
                        let nh1 = dh_new_nonce_hash(&pending.new_nonce, 1, &auth_key.aux_hash());
                        server.inner.keys.lock().unwrap().insert(
                            auth_key.key_id_u64(),
                            ServerKeyEntry {
                                key: auth_key,
                                server_salt,
                            },
                        );
                        let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(mtp_types::DhGenOk {
                            nonce: pending.nonce,
                            server_nonce: pending.server_nonce,
                            new_nonce_hash1: nh1,
                        });
                        let out_id = next_server_msg_id(&server_msg_counter, true);
                        let _ = tx.send(Ok(encode_unencrypted_packet(out_id, &ok.to_bytes())));
                    }
                    continue;
                }

                // Encrypted MTProto 2.0 packet.
                let entry = server.inner.keys.lock().unwrap().get(&auth_key_id).cloned();
                let Some(entry) = entry else {
                    let _ = tx.send(Err(TransportError::ServerError(
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
                        .expect("unpack msg_container")
                        .into_iter()
                        .map(|m| (m.msg_id as u64, m.body.0))
                        .collect()
                } else {
                    vec![(dec.msg_id, dec.body.clone())]
                };

                for (msg_id, raw_msg) in inner_msgs {
                    let mut budget = 2 * 1024 * 1024;
                    let body = if raw_msg.len() >= 4
                        && u32::from_le_bytes(raw_msg[0..4].try_into().unwrap())
                            == GzipPacked::CONSTRUCTOR_ID
                    {
                        unwrap_gzip_layers(&raw_msg, &mut budget, 0).expect("unpack gzip")
                    } else {
                        raw_msg
                    };
                    if body.len() < 4 {
                        continue;
                    }
                    let outer_ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());

                    if outer_ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                        let ping = mtp_functions::Ping::from_bytes(&body).unwrap();
                        if !sent_new_session {
                            sent_new_session = true;
                            let ns = mtp_enums::NewSession::Created(mtp_types::NewSessionCreated {
                                first_msg_id: msg_id as i64,
                                unique_id: conn_id as i64,
                                server_salt: entry.server_salt as i64,
                            });
                            let ns_id = next_server_msg_id(&server_msg_counter, false);
                            send_encrypted(
                                &tx,
                                &entry.key,
                                entry.server_salt,
                                session_id,
                                ns_id,
                                true,
                                &server_seq_counter,
                                &ns.to_bytes(),
                            );
                        }
                        let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                            msg_id: msg_id as i64,
                            ping_id: ping.ping_id,
                        });
                        let p_id = next_server_msg_id(&server_msg_counter, true);
                        send_encrypted(
                            &tx,
                            &entry.key,
                            entry.server_salt,
                            session_id,
                            p_id,
                            false,
                            &server_seq_counter,
                            &pong.to_bytes(),
                        );
                        continue;
                    }
                    if outer_ctor == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID {
                        let ping = mtp_functions::PingDelayDisconnect::from_bytes(&body).unwrap();
                        let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                            msg_id: msg_id as i64,
                            ping_id: ping.ping_id,
                        });
                        let p_id = next_server_msg_id(&server_msg_counter, true);
                        send_encrypted(
                            &tx,
                            &entry.key,
                            entry.server_salt,
                            session_id,
                            p_id,
                            false,
                            &server_seq_counter,
                            &pong.to_bytes(),
                        );
                        continue;
                    }
                    if outer_ctor == mtp_types::MsgsAck::CONSTRUCTOR_ID {
                        continue;
                    }
                    if outer_ctor == mtp_functions::GetFutureSalts::CONSTRUCTOR_ID {
                        let now = now_unix_secs();
                        let salts = mtp_enums::FutureSalts::FutureSalts(mtp_types::FutureSalts {
                            req_msg_id: msg_id as i64,
                            now,
                            salts: vec![mtp_types::FutureSalt {
                                valid_since: now - 60,
                                valid_until: now + 3600,
                                salt: entry.server_salt as i64,
                            }],
                        });
                        let s_id = next_server_msg_id(&server_msg_counter, true);
                        send_encrypted(
                            &tx,
                            &entry.key,
                            entry.server_salt,
                            session_id,
                            s_id,
                            false,
                            &server_seq_counter,
                            &salts.to_bytes(),
                        );
                        continue;
                    }

                    // Unwrap `invokeWithLayer(initConnection(...))` if present.
                    let after_layer = if body.len() >= 4
                        && u32::from_le_bytes(body[0..4].try_into().unwrap())
                            == tl_f::InvokeWithLayer::<RawObject>::CONSTRUCTOR_ID
                    {
                        let mut cur = Cursor::new(&body[4..]);
                        let layer = i32::deserialize(&mut cur).expect("invokeWithLayer.layer");
                        assert_eq!(layer, LAYER);
                        let rest = &body[body.len() - cur.remaining()..];
                        let ic_ctor = u32::from_le_bytes(rest[0..4].try_into().unwrap());
                        assert_eq!(ic_ctor, tl_f::InitConnection::<RawObject>::CONSTRUCTOR_ID);
                        let mut icur = Cursor::new(&rest[4..]);
                        let flags =
                            i32::deserialize(&mut icur).expect("initConnection.flags") as u32;
                        let _api_id = i32::deserialize(&mut icur).unwrap();
                        let _device = String::deserialize(&mut icur).unwrap();
                        let _sys_ver = String::deserialize(&mut icur).unwrap();
                        let _app_ver = String::deserialize(&mut icur).unwrap();
                        let _sys_lang = String::deserialize(&mut icur).unwrap();
                        let _lang_pack = String::deserialize(&mut icur).unwrap();
                        let _lang_code = String::deserialize(&mut icur).unwrap();
                        if (flags & (1 << 0)) != 0 {
                            let _ = tl_e::InputClientProxy::deserialize(&mut icur).unwrap();
                        }
                        if (flags & (1 << 1)) != 0 {
                            let _ = tl_e::JSONValue::deserialize(&mut icur).unwrap();
                        }
                        rest[rest.len() - icur.remaining()..].to_vec()
                    } else {
                        body
                    };

                    // Unwrap `InvokeWithTakeout` and `InvokeWithMessagesRange` wrappers,
                    // recording exact outer/inner envelope metadata on the wire!
                    let mut cur = Cursor::new(&after_layer);
                    let mut takeout_id = None;
                    let mut split_range = None;

                    let peek_u32 = |c: &Cursor<'_>, buf: &[u8]| -> Option<u32> {
                        let rem = c.remaining();
                        if rem < 4 {
                            return None;
                        }
                        let pos = buf.len() - rem;
                        Some(u32::from_le_bytes([
                            buf[pos],
                            buf[pos + 1],
                            buf[pos + 2],
                            buf[pos + 3],
                        ]))
                    };

                    if peek_u32(&cur, &after_layer)
                        == Some(tl_f::InvokeWithTakeout::<RawObject>::CONSTRUCTOR_ID)
                    {
                        let _ = cur.read_u32().unwrap();
                        takeout_id = Some(i64::deserialize(&mut cur).unwrap());
                    }
                    if peek_u32(&cur, &after_layer)
                        == Some(tl_f::InvokeWithMessagesRange::<RawObject>::CONSTRUCTOR_ID)
                    {
                        let _ = cur.read_u32().unwrap();
                        let tl_e::MessageRange::MessageRange(r) =
                            tl_e::MessageRange::deserialize(&mut cur).unwrap();
                        split_range = Some((r.min_id, r.max_id));
                    }

                    let inner_start = after_layer.len() - cur.remaining();
                    let inner_body = after_layer[inner_start..].to_vec();
                    if inner_body.len() < 4 {
                        continue;
                    }
                    let inner_ctor = u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                    let wire_call = TakeoutWireCall {
                        dc_id,
                        takeout_id,
                        split_range,
                        inner_ctor,
                        inner_body,
                    };
                    server.inner.calls.lock().unwrap().push(wire_call.clone());

                    let action = (server.inner.handler)(&wire_call);
                    let payload = match action {
                        ServerAction::Reply(bytes) => bytes,
                        ServerAction::RpcError(code, msg) => {
                            mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                error_code: code,
                                error_message: msg,
                            })
                            .to_bytes()
                        }
                    };
                    let rpc_res = Boxed(RpcResult {
                        req_msg_id: msg_id as i64,
                        result: RawObject(payload),
                    });
                    let r_id = next_server_msg_id(&server_msg_counter, true);
                    send_encrypted(
                        &tx,
                        &entry.key,
                        entry.server_salt,
                        session_id,
                        r_id,
                        true,
                        &server_seq_counter,
                        &rpc_res.to_bytes(),
                    );
                }
            }
        });
    }
}

impl TransportFactory for ScriptedMtprotoServer {
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

/// Bridges a real `tg_mtproto::Client` directly into `tg_export::ExportRpc` (matching
/// `LiveExportRpc` in `tg-app-live/src/export.rs`).
struct MtprotoExportRpc {
    client: Client,
}

impl ExportRpc for MtprotoExportRpc {
    async fn invoke<R: RemoteCall + Identifiable + Send + Sync>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, RpcCallError> {
        let res = if dc_id > 0 {
            self.client.invoke_in_dc(dc_id, request).await
        } else {
            self.client.invoke(request).await
        };
        res.map_err(|err| match err {
            InvocationError::Rpc(rpc) => RpcCallError::new(
                rpc.code,
                rpc.name.clone(),
                format!("RPC {} {}", rpc.code, rpc.name),
            ),
            other => RpcCallError::new(-1, "TRANSPORT_ERROR", other.to_string()),
        })
    }
}

// ============================================================================
// Synthetic TL fixtures (no real names, phone numbers, or accounts)
// ============================================================================

fn tl_user(id: i64, first: &str, is_self: bool) -> tl_e::User {
    tl_e::User::User(tl_t::User {
        self_: is_self,
        contact: !is_self,
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
        access_hash: Some(id * 19),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: Some(format!("acct_{id}")),
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

fn tl_channel(id: i64, title: &str, broadcast: bool) -> tl_e::Chat {
    tl_e::Chat::Channel(tl_t::Channel {
        creator: false,
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
        access_hash: Some(id * 23),
        title: title.to_owned(),
        username: None,
        photo: tl_e::ChatPhoto::Empty,
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

fn tl_notify_empty() -> tl_e::PeerNotifySettings {
    tl_e::PeerNotifySettings::PeerNotifySettings(tl_t::PeerNotifySettings {
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

fn tl_dialog(peer: tl_e::Peer, top_message: i32) -> tl_e::Dialog {
    tl_e::Dialog::Dialog(tl_t::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer,
        top_message,
        read_inbox_max_id: top_message,
        read_outbox_max_id: top_message,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: tl_notify_empty(),
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    })
}

fn tl_message(
    id: i32,
    date: i32,
    peer_id: tl_e::Peer,
    from_id: Option<tl_e::Peer>,
    text: &str,
    media: Option<tl_e::MessageMedia>,
) -> tl_e::Message {
    tl_e::Message::Message(tl_t::Message {
        out: true,
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
        from_id,
        from_boosts_applied: None,
        from_rank: None,
        peer_id,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media,
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

fn tl_doc_media(
    id: i64,
    size: i64,
    dc_id: i32,
    fref: &[u8],
    file_name: &str,
) -> tl_e::MessageMedia {
    tl_e::MessageMedia::Document(tl_t::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: false,
        document: Some(tl_e::Document::Document(tl_t::Document {
            id,
            access_hash: id * 13,
            file_reference: fref.to_vec(),
            date: 1_700_000_050,
            mime_type: "application/octet-stream".to_owned(),
            size,
            thumbs: None,
            video_thumbs: None,
            dc_id,
            attributes: vec![tl_e::DocumentAttribute::Filename(
                tl_t::DocumentAttributeFilename {
                    file_name: file_name.to_owned(),
                },
            )],
        })),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    })
}

fn tl_photo_media(id: i64, size: i32, dc_id: i32, fref: &[u8]) -> tl_e::MessageMedia {
    tl_e::MessageMedia::Photo(tl_t::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(tl_e::Photo::Photo(tl_t::Photo {
            has_stickers: false,
            id,
            access_hash: id * 11,
            file_reference: fref.to_vec(),
            date: 1_700_000_060,
            sizes: vec![tl_e::PhotoSize::PhotoSize(tl_t::PhotoSize {
                r#type: "x".to_owned(),
                w: 120,
                h: 90,
                size,
            })],
            video_sizes: None,
            dc_id,
        })),
        ttl_seconds: None,
        video: None,
    })
}

// ============================================================================
// Test 1: `account.initTakeoutSession` flags & `MAX_FILE_SIZE` threshold
// ============================================================================

#[test]
fn init_takeout_session_flags_match_tdesktop() {
    // 1. At `MAX_FILE_SIZE` (4000 MiB), `file_max_size` MUST be omitted (`None`), matching
    //    `export_api_wrap.cpp:1080` (`hasFiles && sizeLimit < kFileMaxSize`).
    let at_max = Settings {
        types: ExportTypes::PERSONAL_CHATS | ExportTypes::PRIVATE_GROUPS | ExportTypes::CONTACTS,
        media: MediaSettings {
            types: MediaTypes::PHOTO | MediaTypes::FILE,
            size_limit: MAX_FILE_SIZE,
        },
        ..Default::default()
    };
    let req_max = TlExportSource::<MtprotoExportRpc>::build_init_takeout_request(&at_max);
    assert!(req_max.contacts);
    assert!(req_max.message_users);
    assert!(req_max.message_chats);
    assert!(req_max.message_megagroups);
    assert!(!req_max.message_channels);
    assert!(req_max.files);
    assert_eq!(req_max.file_max_size, None);

    // 2. Below `MAX_FILE_SIZE`, `file_max_size` carries the exact limit.
    let sub_max = Settings {
        types: ExportTypes::PUBLIC_GROUPS | ExportTypes::PUBLIC_CHANNELS,
        media: MediaSettings {
            types: MediaTypes::VIDEO,
            size_limit: 16 * 1024 * 1024,
        },
        ..Default::default()
    };
    let req_sub = TlExportSource::<MtprotoExportRpc>::build_init_takeout_request(&sub_max);
    assert!(!req_sub.contacts);
    assert!(!req_sub.message_users);
    assert!(!req_sub.message_chats);
    assert!(req_sub.message_megagroups);
    assert!(req_sub.message_channels);
    assert!(req_sub.files);
    assert_eq!(req_sub.file_max_size, Some(16 * 1024 * 1024));

    // 3. `media.types` non-empty with `size_limit == 0` and no Userpics/Stories -> `files = false`.
    let zero_limit = Settings {
        types: ExportTypes::PERSONAL_CHATS,
        media: MediaSettings {
            types: MediaTypes::PHOTO,
            size_limit: 0,
        },
        ..Default::default()
    };
    let req_zero = TlExportSource::<MtprotoExportRpc>::build_init_takeout_request(&zero_limit);
    assert!(!req_zero.files);
    assert_eq!(req_zero.file_max_size, None);

    // 4. Single-peer channel sets `message_megagroups + message_channels` and `contacts = false`.
    let single_ch = Settings {
        types: ExportTypes::CONTACTS | ExportTypes::PERSONAL_CHATS,
        single_peer: InputPeer::Channel(tl_t::InputPeerChannel {
            channel_id: 9001,
            access_hash: 9002,
        }),
        media: MediaSettings {
            types: MediaTypes::empty(),
            size_limit: 0,
        },
        ..Default::default()
    };
    let req_ch = TlExportSource::<MtprotoExportRpc>::build_init_takeout_request(&single_ch);
    assert!(!req_ch.contacts);
    assert!(!req_ch.message_users);
    assert!(!req_ch.message_chats);
    assert!(req_ch.message_megagroups);
    assert!(req_ch.message_channels);
    assert!(!req_ch.files);
}

// ============================================================================
// Test 2: End-to-End Encrypted MTProto 2.0 Takeout (HTML + JSON, Exact-Multiple
//         256 KiB + Non-Multiple 300 KiB Chunking, FILE_MIGRATE_4, Mid-File
//         FILE_REFERENCE_EXPIRED Resume, LoadedFileCache Ref-Agnostic Dedup,
//         TAKEOUT_FILE_EMPTY, CHANNEL_PRIVATE Fallback, FinishTakeoutSession)
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtproto2_end_to_end_takeout_conformance_and_html_json_integrity() {
    let dir = temp_dir("mtproto2_e2e");
    let takeout_id = 91_234_567_i64;

    // 256 KiB exact multiple of 128 KiB -> MUST stop after 2 chunks (offset 0 and 128 KiB)
    // WITHOUT sending a 3rd over-EOF chunk request!
    let exact_256k: Vec<u8> = (0..(2 * FILE_CHUNK_SIZE as usize))
        .map(|i| ((i * 7 + 3) % 251) as u8)
        .collect();
    // 300 KiB non-multiple -> 3 chunks (128 KiB + 128 KiB + 44 KiB) with mid-file
    // FILE_REFERENCE_EXPIRED at chunk 1 (offset = 128 KiB) AND FILE_MIGRATE_4!
    let file_300k: Vec<u8> = (0..(300 * 1024))
        .map(|i| ((i * 13 + 5) % 241) as u8)
        .collect();
    let photo_bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 42, 43];

    #[derive(Default)]
    struct ScenarioState {
        file_300k_offsets: Vec<(i32, i64, bool, Vec<u8>)>, // (dc_id, offset, precise, fref)
        exact_256k_offsets: Vec<i64>,
        photo_fetches: usize,
        mid_file_ref_expired_once: bool,
        chan_history_calls: usize,
        chan_ref_refresh_split_range: Option<Option<(i32, i32)>>,
        dialog_splits_queried: Vec<(i32, i32)>,
        finish_success: Option<bool>,
    }

    let state = Arc::new(Mutex::new(ScenarioState::default()));
    let state_srv = Arc::clone(&state);
    let exact_256k_srv = exact_256k.clone();
    let file_300k_srv = file_300k.clone();
    let photo_bytes_srv = photo_bytes.clone();

    let server = ScriptedMtprotoServer::new(move |call: &TakeoutWireCall| {
        let mut st = state_srv.lock().unwrap();
        let self_u = tl_user(1001, "SelfTester", true);
        let peer_u = tl_user(1002, "PeerOne", false);
        let priv_ch = tl_channel(3009, "Private Feed", true);

        match call.inner_ctor {
            c if c == tl_f::users::GetUsers::CONSTRUCTOR_ID => {
                assert_eq!(
                    call.takeout_id, None,
                    "initial users.getUsers([inputUserSelf]) must precede takeout session"
                );
                ServerAction::ok(&vec![self_u])
            }
            c if c == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, None);
                ServerAction::ok(&tl_e::account::Takeout::Takeout(tl_t::account::Takeout {
                    id: takeout_id,
                }))
            }
            c if c == tl_f::account::FinishTakeoutSession::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                let req =
                    tl_f::account::FinishTakeoutSession::from_bytes(&call.inner_body).unwrap();
                st.finish_success = Some(req.success);
                ServerAction::ok(&true)
            }
            c if c == tl_f::messages::GetSplitRanges::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                assert_eq!(call.split_range, None);
                ServerAction::ok(&vec![
                    tl_e::MessageRange::MessageRange(tl_t::MessageRange {
                        min_id: 1,
                        max_id: 500,
                    }),
                    tl_e::MessageRange::MessageRange(tl_t::MessageRange {
                        min_id: 501,
                        max_id: 1000,
                    }),
                ])
            }
            c if c == tl_f::channels::GetLeftChannels::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                ServerAction::ok(&tl_e::messages::Chats::Chats(tl_t::messages::Chats {
                    chats: Vec::new(),
                }))
            }
            c if c == tl_f::messages::GetDialogs::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                let range = call.split_range.expect("getDialogs must wrap split range");
                let req = tl_f::messages::GetDialogs::from_bytes(&call.inner_body).unwrap();
                if req.limit > 1 {
                    st.dialog_splits_queried.push(range);
                }
                if range == (1, 500) {
                    let top_u = tl_message(
                        5,
                        1_700_000_005,
                        tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                        Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                        "top",
                        None,
                    );
                    let top_ch = tl_message(
                        10,
                        1_700_000_010,
                        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3009 }),
                        Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                        "chan top",
                        None,
                    );
                    ServerAction::ok(&tl_e::messages::Dialogs::Dialogs(tl_t::messages::Dialogs {
                        dialogs: vec![
                            tl_dialog(tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }), 5),
                            tl_dialog(
                                tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3009 }),
                                10,
                            ),
                        ],
                        messages: vec![top_u, top_ch],
                        chats: vec![priv_ch],
                        users: vec![self_u, peer_u],
                    }))
                } else {
                    ServerAction::ok(&tl_e::messages::Dialogs::Dialogs(tl_t::messages::Dialogs {
                        dialogs: Vec::new(),
                        messages: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                    }))
                }
            }
            c if c == tl_f::messages::GetHistory::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                assert_eq!(call.split_range, Some((1, 500)));
                let req = tl_f::messages::GetHistory::from_bytes(&call.inner_body).unwrap();
                if matches!(req.peer, InputPeer::Channel(_)) {
                    st.chan_history_calls += 1;
                    // Count check (limit == 1) succeeds, then slice fetch triggers CHANNEL_PRIVATE
                    // mid-history to verify mid-slice fallback to `messages.search`!
                    if req.limit == 1 {
                        return ServerAction::ok(&tl_e::messages::Messages::Messages(
                            tl_t::messages::Messages {
                                messages: vec![tl_message(
                                    10,
                                    1_700_000_010,
                                    tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3009 }),
                                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                                    "chan post",
                                    None,
                                )],
                                topics: Vec::new(),
                                chats: vec![priv_ch],
                                users: vec![self_u],
                            },
                        ));
                    }
                    return ServerAction::err(400, "CHANNEL_PRIVATE");
                }
                if req.limit == 1 {
                    return ServerAction::ok(&tl_e::messages::Messages::Slice(
                        tl_t::messages::MessagesSlice {
                            inexact: false,
                            count: 5,
                            next_rate: None,
                            offset_id_offset: None,
                            search_flood: None,
                            messages: vec![tl_message(
                                1,
                                1_700_000_001,
                                tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                                Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                                "exact256k",
                                None,
                            )],
                            topics: Vec::new(),
                            chats: Vec::new(),
                            users: vec![self_u, peer_u],
                        },
                    ));
                }
                let p_user = tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 });
                let from_self = Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 }));
                // 5 messages returned newest-first:
                // 1: exact 256 KiB document on DC 2
                // 2: 300 KiB document on DC 2 -> FILE_MIGRATE_4 -> chunk 0 OK, chunk 1 FILE_REFERENCE_EXPIRED -> refresh -> chunk 1 + 2 OK
                // 3 & 4: same photo ID 77001 with DIFFERENT file_reference bytes -> LoadedFileCache hits!
                // 5: photo ID 77002 returning VERSION_INVALID -> gracefully marked Unavailable
                let msgs = vec![
                    tl_message(
                        5,
                        1_700_000_005,
                        p_user.clone(),
                        from_self.clone(),
                        "version_invalid_photo",
                        Some(tl_photo_media(77_002, 10, 2, b"v_old")),
                    ),
                    tl_message(
                        4,
                        1_700_000_004,
                        p_user.clone(),
                        from_self.clone(),
                        "dup_photo_new_ref",
                        Some(tl_photo_media(
                            77_001,
                            photo_bytes_srv.len() as i32,
                            2,
                            b"ref_v2",
                        )),
                    ),
                    tl_message(
                        3,
                        1_700_000_003,
                        p_user.clone(),
                        from_self.clone(),
                        "dup_photo_old_ref",
                        Some(tl_photo_media(
                            77_001,
                            photo_bytes_srv.len() as i32,
                            2,
                            b"ref_v1",
                        )),
                    ),
                    tl_message(
                        2,
                        1_700_000_002,
                        p_user.clone(),
                        from_self.clone(),
                        "mid_ref_300k",
                        Some(tl_doc_media(
                            55_002,
                            file_300k_srv.len() as i64,
                            2,
                            b"doc_ref_v1",
                            "midref300k.bin",
                        )),
                    ),
                    tl_message(
                        1,
                        1_700_000_001,
                        p_user,
                        from_self,
                        "exact_256k",
                        Some(tl_doc_media(
                            55_001,
                            exact_256k_srv.len() as i64,
                            2,
                            b"exact_ref",
                            "exact256k.bin",
                        )),
                    ),
                ];
                ServerAction::ok(&tl_e::messages::Messages::Messages(
                    tl_t::messages::Messages {
                        messages: msgs,
                        topics: Vec::new(),
                        chats: Vec::new(),
                        users: vec![self_u, peer_u],
                    },
                ))
            }
            c if c == tl_f::messages::Search::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                assert_eq!(call.split_range, Some((1, 500)));
                let req = tl_f::messages::Search::from_bytes(&call.inner_body).unwrap();
                assert_eq!(req.from_id, Some(InputPeer::Self_));
                // Include a channel photo whose file_reference expires so we also verify
                // `channels.getMessages` refresh is NOT wrapped in `InvokeWithMessagesRange`!
                ServerAction::ok(&tl_e::messages::Messages::Messages(
                    tl_t::messages::Messages {
                        messages: vec![tl_message(
                            10,
                            1_700_000_010,
                            tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3009 }),
                            Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                            "my channel post",
                            Some(tl_photo_media(
                                88_001,
                                photo_bytes_srv.len() as i32,
                                2,
                                b"ch_stale",
                            )),
                        )],
                        topics: Vec::new(),
                        chats: vec![priv_ch],
                        users: vec![self_u],
                    },
                ))
            }
            c if c == tl_f::messages::GetMessages::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                assert_eq!(
                    call.split_range,
                    Some((1, 500)),
                    "non-channel messages.getMessages refresh must carry split range"
                );
                ServerAction::ok(&tl_e::messages::Messages::Messages(
                    tl_t::messages::Messages {
                        messages: vec![tl_message(
                            2,
                            1_700_000_002,
                            tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                            Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                            "mid_ref_300k",
                            Some(tl_doc_media(
                                55_002,
                                file_300k_srv.len() as i64,
                                2,
                                b"doc_ref_v2",
                                "midref300k.bin",
                            )),
                        )],
                        topics: Vec::new(),
                        chats: Vec::new(),
                        users: vec![self_u, peer_u],
                    },
                ))
            }
            c if c == tl_f::channels::GetMessages::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                st.chan_ref_refresh_split_range = Some(call.split_range);
                ServerAction::ok(&tl_e::messages::Messages::Messages(
                    tl_t::messages::Messages {
                        messages: vec![tl_message(
                            10,
                            1_700_000_010,
                            tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3009 }),
                            Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                            "my channel post",
                            Some(tl_photo_media(
                                88_001,
                                photo_bytes_srv.len() as i32,
                                2,
                                b"ch_fresh",
                            )),
                        )],
                        topics: Vec::new(),
                        chats: vec![priv_ch],
                        users: vec![self_u],
                    },
                ))
            }
            c if c == tl_f::upload::GetFile::CONSTRUCTOR_ID => {
                assert_eq!(call.takeout_id, Some(takeout_id));
                let req = tl_f::upload::GetFile::from_bytes(&call.inner_body).unwrap();
                assert!(
                    !req.precise && !req.cdn_supported,
                    "export upload.getFile must send MTP_flags(0) matching export_api_wrap.cpp:763"
                );
                assert_eq!(req.limit, FILE_CHUNK_SIZE);

                match req.location {
                    InputFileLocation::InputTakeoutFileLocation => {
                        ServerAction::err(400, "TAKEOUT_FILE_EMPTY")
                    }
                    InputFileLocation::InputDocumentFileLocation(doc) if doc.id == 55_001 => {
                        st.exact_256k_offsets.push(req.offset);
                        let start = req.offset as usize;
                        assert!(
                            start < exact_256k_srv.len(),
                            "must NOT request past EOF on exact 128 KiB multiple (got offset {start})"
                        );
                        let end = (start + req.limit as usize).min(exact_256k_srv.len());
                        ServerAction::ok(&tl_e::upload::File::File(tl_t::upload::File {
                            r#type: tl_e::storage::FileType::FileUnknown,
                            mtime: 1_700_000_001,
                            bytes: exact_256k_srv[start..end].to_vec(),
                        }))
                    }
                    InputFileLocation::InputDocumentFileLocation(doc) if doc.id == 55_002 => {
                        if call.dc_id != 4 {
                            return ServerAction::err(303, "FILE_MIGRATE_4");
                        }
                        st.file_300k_offsets.push((
                            call.dc_id,
                            req.offset,
                            req.precise,
                            doc.file_reference.clone(),
                        ));
                        // Chunk 1 (`offset == 128 KiB`) expires `doc_ref_v1` to verify mid-file
                        // refresh resumes at `offset == 128 KiB` without re-fetching `offset == 0`!
                        if req.offset == i64::from(FILE_CHUNK_SIZE)
                            && doc.file_reference == b"doc_ref_v1"
                        {
                            st.mid_file_ref_expired_once = true;
                            return ServerAction::err(400, "FILE_REFERENCE_EXPIRED");
                        }
                        let start = req.offset as usize;
                        let end = (start + req.limit as usize).min(file_300k_srv.len());
                        ServerAction::ok(&tl_e::upload::File::File(tl_t::upload::File {
                            r#type: tl_e::storage::FileType::FileUnknown,
                            mtime: 1_700_000_002,
                            bytes: file_300k_srv[start..end].to_vec(),
                        }))
                    }
                    InputFileLocation::InputPhotoFileLocation(p) if p.id == 77_001 => {
                        st.photo_fetches += 1;
                        ServerAction::ok(&tl_e::upload::File::File(tl_t::upload::File {
                            r#type: tl_e::storage::FileType::FilePng,
                            mtime: 1_700_000_003,
                            bytes: photo_bytes_srv.clone(),
                        }))
                    }
                    InputFileLocation::InputPhotoFileLocation(p) if p.id == 77_002 => {
                        ServerAction::err(400, "VERSION_INVALID")
                    }
                    InputFileLocation::InputPhotoFileLocation(p) if p.id == 88_001 => {
                        if p.file_reference == b"ch_stale" {
                            return ServerAction::err(400, "FILE_REFERENCE_EXPIRED");
                        }
                        ServerAction::ok(&tl_e::upload::File::File(tl_t::upload::File {
                            r#type: tl_e::storage::FileType::FilePng,
                            mtime: 1_700_000_010,
                            bytes: photo_bytes_srv.clone(),
                        }))
                    }
                    other => ServerAction::err(400, format!("UNEXPECTED_LOC_{other:?}")),
                }
            }
            other => ServerAction::err(400, format!("UNHANDLED_0x{other:08x}")),
        }
    });

    let rpc = server.create_export_rpc(2);
    let source = TlExportSource::new(rpc);
    let settings = Settings {
        path: dir_with_slash(&dir),
        format: OutputFormat::HtmlAndJson,
        types: ExportTypes::OTHER_DATA
            | ExportTypes::PERSONAL_CHATS
            | ExportTypes::PRIVATE_CHANNELS,
        full_chats: ExportTypes::PERSONAL_CHATS
            | ExportTypes::BOT_CHATS
            | ExportTypes::PRIVATE_CHANNELS,
        media: MediaSettings {
            types: MediaTypes::PHOTO | MediaTypes::FILE,
            size_limit: 4 * 1024 * 1024,
        },
        ..Default::default()
    };

    let (mut controller, _handle) = ExportController::new(source, InputPeer::Empty);
    controller.set_skip_path_normalization(true);
    let final_state = controller
        .run(&settings, &Environment::default_english())
        .await;
    assert!(
        matches!(final_state, ExportState::Finished(_)),
        "expected Finished, got {final_state:?}"
    );

    let st = state.lock().unwrap();
    // 1. Dialog splits queried newest-to-oldest: `(501, 1000)` then `(1, 500)`.
    assert_eq!(st.dialog_splits_queried, vec![(501, 1000), (1, 500)]);

    // 2. Exact 256 KiB file requested ONLY offsets `[0, 131072]` (no over-EOF request!).
    assert_eq!(st.exact_256k_offsets, vec![0, i64::from(FILE_CHUNK_SIZE)]);

    // 3. Mid-file FILE_REFERENCE_EXPIRED resumed at offset 128 KiB on DC 4 WITHOUT re-fetching 0!
    assert!(st.mid_file_ref_expired_once);
    assert_eq!(
        st.file_300k_offsets,
        vec![
            (4, 0, false, b"doc_ref_v1".to_vec()),
            (4, i64::from(FILE_CHUNK_SIZE), false, b"doc_ref_v1".to_vec()),
            (4, i64::from(FILE_CHUNK_SIZE), false, b"doc_ref_v2".to_vec()),
            (
                4,
                i64::from(FILE_CHUNK_SIZE) * 2,
                false,
                b"doc_ref_v2".to_vec()
            ),
        ]
    );

    // 4. LoadedFileCache deduplicated photo 77001 across `ref_v1` and `ref_v2` (downloaded ONCE!).
    assert_eq!(st.photo_fetches, 1);

    // 5. Channel `channels.getMessages` refresh was wrapped in `InvokeWithTakeout` ONLY (no split range).
    assert_eq!(st.chan_ref_refresh_split_range, Some(None));

    // 6. `account.finishTakeoutSession(success = true)` called at end.
    assert_eq!(st.finish_success, Some(true));
    drop(st);

    // 7. Reassembled files on disk match byte-for-byte.
    assert_eq!(
        fs::read(dir.join("chats/chat_1/files/exact256k.bin")).expect("read exact256k.bin"),
        exact_256k
    );
    assert_eq!(
        fs::read(dir.join("chats/chat_1/files/midref300k.bin")).expect("read midref300k.bin"),
        file_300k
    );
    // `TAKEOUT_FILE_EMPTY` wrote a valid 0-byte `lists/other_data.json`.
    assert_eq!(
        fs::read(dir.join("lists/other_data.json")).expect("read other_data.json"),
        Vec::<u8>::new()
    );

    // 8. HTML + JSON output integrity.
    let json: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("result.json")).unwrap()).unwrap();
    let chats = json["chats"]["list"].as_array().unwrap();
    assert_eq!(chats.len(), 2);
    let user_msgs = chats[0]["messages"].as_array().unwrap();
    assert_eq!(user_msgs.len(), 5);
    assert_eq!(user_msgs[0]["file"], "chats/chat_1/files/exact256k.bin");
    assert_eq!(user_msgs[1]["file"], "chats/chat_1/files/midref300k.bin");
    assert_eq!(user_msgs[2]["photo"], user_msgs[3]["photo"]);
    assert_eq!(
        user_msgs[4]["photo"],
        "(File unavailable, please try again later)"
    );

    let chan_msgs = chats[1]["messages"].as_array().unwrap();
    assert_eq!(chan_msgs.len(), 1);
    assert_eq!(chan_msgs[0]["text"], "my channel post");
    assert!(
        chan_msgs[0]["photo"]
            .as_str()
            .unwrap()
            .starts_with("chats/chat_2/photos/")
    );

    let index_html = fs::read_to_string(dir.join("export_results.html")).expect("read html");
    assert!(index_html.contains("PeerOne") || index_html.contains("chats.html"));
    let chat1_html =
        fs::read_to_string(dir.join("chats/chat_1/messages.html")).expect("chat1 html");
    assert!(chat1_html.contains("exact_256k"));
    assert!(chat1_html.contains("mid_ref_300k"));

    let _ = fs::remove_dir_all(&dir);
}

// ============================================================================
// Test 3: `TAKEOUT_INIT_DELAY_X` and `TAKEOUT_INVALID` over MTProto 2.0
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtproto2_takeout_init_delay_and_takeout_invalid_recovery() {
    // 1. `TAKEOUT_INIT_DELAY_86400` on `account.initTakeoutSession`.
    let dir1 = temp_dir("init_delay");
    let srv_delay = ScriptedMtprotoServer::new(|call: &TakeoutWireCall| match call.inner_ctor {
        c if c == tl_f::users::GetUsers::CONSTRUCTOR_ID => {
            ServerAction::ok(&vec![tl_user(1001, "Self", true)])
        }
        c if c == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID => {
            ServerAction::err(420, "TAKEOUT_INIT_DELAY_86400")
        }
        other => ServerAction::err(400, format!("UNEXPECTED_0x{other:08x}")),
    });

    let settings1 = Settings {
        path: dir_with_slash(&dir1),
        format: OutputFormat::Json,
        types: ExportTypes::PERSONAL_INFO,
        ..Default::default()
    };
    let (mut ctrl1, _) = ExportController::new(
        TlExportSource::new(srv_delay.create_export_rpc(2)),
        InputPeer::Empty,
    );
    ctrl1.set_skip_path_normalization(true);
    let st1 = ctrl1.run(&settings1, &Environment::default_english()).await;
    match st1 {
        ExportState::ApiError(err) => {
            assert_eq!(err.code, 420);
            assert_eq!(err.kind, "TAKEOUT_INIT_DELAY_86400");
            assert_eq!(err.parse_takeout_init_delay(), Some(86_400));
            assert!(!err.is_takeout_invalid());
        }
        other => panic!("expected ApiError(TAKEOUT_INIT_DELAY_86400), got {other:?}"),
    }
    // Only `users.getUsers` + `account.initTakeoutSession` should have been sent.
    let ctors1: Vec<u32> = srv_delay
        .recorded_calls()
        .iter()
        .map(|c| c.inner_ctor)
        .collect();
    assert_eq!(
        ctors1,
        vec![
            tl_f::users::GetUsers::CONSTRUCTOR_ID,
            tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID,
        ]
    );
    let _ = fs::remove_dir_all(&dir1);

    // 2. `TAKEOUT_INVALID` mid-session (during `users.getFullUser`).
    let dir2 = temp_dir("takeout_invalid");
    let srv_invalid = ScriptedMtprotoServer::new(|call: &TakeoutWireCall| match call.inner_ctor {
        c if c == tl_f::users::GetUsers::CONSTRUCTOR_ID => {
            ServerAction::ok(&vec![tl_user(1001, "Self", true)])
        }
        c if c == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_e::account::Takeout::Takeout(tl_t::account::Takeout {
                id: 44_444,
            }))
        }
        c if c == tl_f::users::GetFullUser::CONSTRUCTOR_ID => {
            assert_eq!(call.takeout_id, Some(44_444));
            ServerAction::err(400, "TAKEOUT_INVALID")
        }
        other => ServerAction::err(400, format!("UNEXPECTED_0x{other:08x}")),
    });

    let settings2 = Settings {
        path: dir_with_slash(&dir2),
        format: OutputFormat::Json,
        types: ExportTypes::PERSONAL_INFO,
        ..Default::default()
    };
    let (mut ctrl2, _) = ExportController::new(
        TlExportSource::new(srv_invalid.create_export_rpc(2)),
        InputPeer::Empty,
    );
    ctrl2.set_skip_path_normalization(true);
    let st2 = ctrl2.run(&settings2, &Environment::default_english()).await;
    match st2 {
        ExportState::ApiError(err) => {
            assert_eq!(err.code, 400);
            assert!(err.is_takeout_invalid());
            assert_eq!(err.parse_takeout_init_delay(), None);
        }
        other => panic!("expected ApiError(TAKEOUT_INVALID), got {other:?}"),
    }
    let _ = fs::remove_dir_all(&dir2);
}

// ============================================================================
// Test 4: Empty `messages.getSplitRanges` Fallback, `useOnlyLastSplit`,
//         `photos.getUserPhotos` `max_id` Pagination, `contacts.getTopPeers`
//         Count Pagination, `single_peer_from` `offset_date`, and Cancelled Finish
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtproto2_empty_splits_pagination_date_bounds_and_cancel_finish() {
    let dir = temp_dir("splits_pagination");
    let takeout_id = 55_555_i64;

    #[derive(Default)]
    struct PageState {
        dialog_ranges: Vec<(i32, i32)>,
        userpic_max_ids: Vec<(i32, i64)>, // (offset, max_id)
        top_peer_offsets: Vec<i32>,
        history_first_offset_date: Option<(i32, i32)>, // (offset_id, offset_date)
        cancelled_finish_success: Option<bool>,
    }

    let pstate = Arc::new(Mutex::new(PageState::default()));
    let pstate_srv = Arc::clone(&pstate);

    let server = ScriptedMtprotoServer::new(move |call: &TakeoutWireCall| {
        let mut st = pstate_srv.lock().unwrap();
        let self_u = tl_user(1001, "SelfUser", true);
        let peer_u = tl_user(1002, "ContactTwo", false);
        let pub_ch = tl_channel(6001, "Public Broadcast", true);

        match call.inner_ctor {
            c if c == tl_f::users::GetUsers::CONSTRUCTOR_ID => ServerAction::ok(&vec![self_u]),
            c if c == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_e::account::Takeout::Takeout(tl_t::account::Takeout {
                    id: takeout_id,
                }))
            }
            c if c == tl_f::account::FinishTakeoutSession::CONSTRUCTOR_ID => {
                let req =
                    tl_f::account::FinishTakeoutSession::from_bytes(&call.inner_body).unwrap();
                st.cancelled_finish_success = Some(req.success);
                ServerAction::ok(&true)
            }
            c if c == tl_f::messages::GetSplitRanges::CONSTRUCTOR_ID => {
                // Return EMPTY split ranges -> client MUST synthesize `[1, i32::MAX]`!
                ServerAction::ok(&Vec::<tl_e::MessageRange>::new())
            }
            c if c == tl_f::photos::GetUserPhotos::CONSTRUCTOR_ID => {
                let req = tl_f::photos::GetUserPhotos::from_bytes(&call.inner_body).unwrap();
                if req.limit == 0 {
                    return ServerAction::ok(&tl_e::photos::Photos::Slice(
                        tl_t::photos::PhotosSlice {
                            count: 2,
                            photos: Vec::new(),
                            users: vec![self_u],
                        },
                    ));
                }
                st.userpic_max_ids.push((req.offset, req.max_id));
                if req.max_id == 0 {
                    ServerAction::ok(&tl_e::photos::Photos::Slice(tl_t::photos::PhotosSlice {
                        count: 2,
                        photos: vec![tl_e::Photo::Photo(tl_t::Photo {
                            has_stickers: false,
                            id: 800_002,
                            access_hash: 1,
                            file_reference: b"up2".to_vec(),
                            date: 1_700_000_020,
                            sizes: vec![tl_e::PhotoSize::PhotoSize(tl_t::PhotoSize {
                                r#type: "s".to_owned(),
                                w: 64,
                                h: 64,
                                size: 4,
                            })],
                            video_sizes: None,
                            dc_id: 2,
                        })],
                        users: vec![self_u],
                    }))
                } else {
                    assert_eq!(req.max_id, 800_002);
                    ServerAction::ok(&tl_e::photos::Photos::Photos(tl_t::photos::Photos {
                        photos: vec![tl_e::Photo::Photo(tl_t::Photo {
                            has_stickers: false,
                            id: 800_001,
                            access_hash: 1,
                            file_reference: b"up1".to_vec(),
                            date: 1_700_000_010,
                            sizes: vec![tl_e::PhotoSize::PhotoSize(tl_t::PhotoSize {
                                r#type: "s".to_owned(),
                                w: 64,
                                h: 64,
                                size: 4,
                            })],
                            video_sizes: None,
                            dc_id: 2,
                        })],
                        users: vec![self_u],
                    }))
                }
            }
            c if c == tl_f::contacts::GetSaved::CONSTRUCTOR_ID => {
                ServerAction::ok(&Vec::<tl_e::SavedContact>::new())
            }
            c if c == tl_f::contacts::GetTopPeers::CONSTRUCTOR_ID => {
                let req = tl_f::contacts::GetTopPeers::from_bytes(&call.inner_body).unwrap();
                st.top_peer_offsets.push(req.offset);
                let peer_entry = if req.offset == 0 {
                    tl_e::TopPeer::TopPeer(tl_t::TopPeer {
                        peer: tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                        rating: 10.0,
                    })
                } else {
                    tl_e::TopPeer::TopPeer(tl_t::TopPeer {
                        peer: tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 }),
                        rating: 5.0,
                    })
                };
                ServerAction::ok(&tl_e::contacts::TopPeers::TopPeers(
                    tl_t::contacts::TopPeers {
                        categories: vec![tl_e::TopPeerCategoryPeers::TopPeerCategoryPeers(
                            tl_t::TopPeerCategoryPeers {
                                category: tl_e::TopPeerCategory::Correspondents,
                                count: 2,
                                peers: vec![peer_entry],
                            },
                        )],
                        chats: Vec::new(),
                        users: vec![self_u, peer_u],
                    },
                ))
            }
            c if c == tl_f::channels::GetLeftChannels::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_e::messages::Chats::Chats(tl_t::messages::Chats {
                    chats: Vec::new(),
                }))
            }
            c if c == tl_f::messages::GetDialogs::CONSTRUCTOR_ID => {
                let range = call
                    .split_range
                    .expect("synthesized [1, i32::MAX] split range must be sent");
                st.dialog_ranges.push(range);
                ServerAction::ok(&tl_e::messages::Dialogs::Dialogs(tl_t::messages::Dialogs {
                    dialogs: vec![tl_dialog(
                        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 6001 }),
                        2,
                    )],
                    messages: vec![tl_message(
                        2,
                        1_700_000_500,
                        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 6001 }),
                        None,
                        "pub",
                        None,
                    )],
                    chats: vec![pub_ch],
                    users: vec![self_u],
                }))
            }
            c if c == tl_f::messages::GetHistory::CONSTRUCTOR_ID => {
                let req = tl_f::messages::GetHistory::from_bytes(&call.inner_body).unwrap();
                if req.limit == MESSAGES_SLICE_LIMIT && st.history_first_offset_date.is_none() {
                    st.history_first_offset_date = Some((req.offset_id, req.offset_date));
                }
                ServerAction::ok(&tl_e::messages::Messages::Messages(
                    tl_t::messages::Messages {
                        messages: vec![tl_message(
                            2,
                            1_700_000_500,
                            tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 6001 }),
                            None,
                            "filtered_msg",
                            None,
                        )],
                        topics: Vec::new(),
                        chats: vec![pub_ch],
                        users: vec![self_u],
                    },
                ))
            }
            c if c == tl_f::messages::Search::CONSTRUCTOR_ID => ServerAction::ok(
                &tl_e::messages::Messages::Messages(tl_t::messages::Messages {
                    messages: vec![tl_message(
                        2,
                        1_700_000_500,
                        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 6001 }),
                        Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                        "my_pub_msg",
                        None,
                    )],
                    topics: Vec::new(),
                    chats: vec![pub_ch],
                    users: vec![self_u],
                }),
            ),
            c if c == tl_f::upload::GetFile::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_e::upload::File::File(tl_t::upload::File {
                    r#type: tl_e::storage::FileType::FilePng,
                    mtime: 1_700_000_010,
                    bytes: vec![1, 2, 3, 4],
                }))
            }
            other => ServerAction::err(400, format!("UNHANDLED_0x{other:08x}")),
        }
    });

    // Run channel-only + userpics + contacts export with empty split ranges.
    let settings = Settings {
        path: dir_with_slash(&dir),
        format: OutputFormat::Json,
        types: ExportTypes::USERPICS | ExportTypes::CONTACTS | ExportTypes::PUBLIC_CHANNELS,
        full_chats: ExportTypes::PERSONAL_CHATS | ExportTypes::BOT_CHATS,
        single_peer_from: 1_700_000_400,
        ..Default::default()
    };
    let (mut ctrl, _) = ExportController::new(
        TlExportSource::new(server.create_export_rpc(2)),
        InputPeer::Empty,
    );
    ctrl.set_skip_path_normalization(true);
    let final_state = ctrl.run(&settings, &Environment::default_english()).await;
    assert!(
        matches!(final_state, ExportState::Finished(_)),
        "expected Finished, got {final_state:?}"
    );

    {
        let st = pstate.lock().unwrap();
        // Empty `getSplitRanges` synthesized `[1, i32::MAX]`.
        assert!(!st.dialog_ranges.is_empty());
        assert!(st.dialog_ranges.iter().all(|&r| r == (1, i32::MAX)));
        // `photos.getUserPhotos` paginated using `offset = 0, max_id = 800_002`.
        assert_eq!(st.userpic_max_ids, vec![(0, 0), (0, 800_002)]);
        // `contacts.getTopPeers` paginated by count (`0` then `1`).
        assert_eq!(st.top_peer_offsets, vec![0, 1]);
    }

    // Also verify `finish_takeout_cancelled` sends `account.finishTakeoutSession { success: false }`
    // and `single_peer_from` sends `offset_date = single_peer_from - 1, offset_id = 0` on history.
    let dir_cancel = temp_dir("cancel_finish");
    let mut src_cancel = TlExportSource::new(server.create_export_rpc(2));
    let cancel_settings = Settings {
        path: dir_with_slash(&dir_cancel),
        format: OutputFormat::Json,
        types: ExportTypes::PERSONAL_CHATS,
        single_peer_from: 1_700_000_400,
        ..Default::default()
    };
    use tg_export::controller::ExportSource;
    let _ = src_cancel
        .start_export(&cancel_settings, &tg_export::output::stats::Stats::new())
        .await
        .expect("start_export");
    src_cancel
        .finish_takeout_cancelled()
        .await
        .expect("finish_takeout_cancelled");
    assert_eq!(pstate.lock().unwrap().cancelled_finish_success, Some(false));

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&dir_cancel);
}

// ============================================================================
// Test 5: `useOnlyLastSplit` (Channel-Only Export Queries Only Newest Split)
//         & `single_peer_from` History `offset_date`
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtproto2_use_only_last_split_and_single_peer_date_offset() {
    let dir = temp_dir("last_split");
    let takeout_id = 66_666_i64;
    let dialog_ranges = Arc::new(Mutex::new(Vec::<(i32, i32)>::new()));
    let history_first = Arc::new(Mutex::new(None::<(i32, i32)>));
    let dr_srv = Arc::clone(&dialog_ranges);
    let hf_srv = Arc::clone(&history_first);

    let server = ScriptedMtprotoServer::new(move |call: &TakeoutWireCall| {
        let self_u = tl_user(1001, "Self", true);
        let pub_ch = tl_channel(6002, "Channel Two", true);
        match call.inner_ctor {
            c if c == tl_f::users::GetUsers::CONSTRUCTOR_ID => ServerAction::ok(&vec![self_u]),
            c if c == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_e::account::Takeout::Takeout(tl_t::account::Takeout {
                    id: takeout_id,
                }))
            }
            c if c == tl_f::account::FinishTakeoutSession::CONSTRUCTOR_ID => {
                ServerAction::ok(&true)
            }
            c if c == tl_f::messages::GetSplitRanges::CONSTRUCTOR_ID => ServerAction::ok(&vec![
                tl_e::MessageRange::MessageRange(tl_t::MessageRange {
                    min_id: 1,
                    max_id: 100,
                }),
                tl_e::MessageRange::MessageRange(tl_t::MessageRange {
                    min_id: 101,
                    max_id: 200,
                }),
            ]),
            c if c == tl_f::channels::GetLeftChannels::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_e::messages::Chats::Chats(tl_t::messages::Chats {
                    chats: Vec::new(),
                }))
            }
            c if c == tl_f::messages::GetDialogs::CONSTRUCTOR_ID => {
                dr_srv.lock().unwrap().push(call.split_range.unwrap());
                ServerAction::ok(&tl_e::messages::Dialogs::Dialogs(tl_t::messages::Dialogs {
                    dialogs: vec![tl_dialog(
                        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 6002 }),
                        150,
                    )],
                    messages: vec![tl_message(
                        150,
                        1_700_000_500,
                        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 6002 }),
                        None,
                        "latest",
                        None,
                    )],
                    chats: vec![pub_ch],
                    users: vec![self_u],
                }))
            }
            c if c == tl_f::messages::GetHistory::CONSTRUCTOR_ID => {
                let req = tl_f::messages::GetHistory::from_bytes(&call.inner_body).unwrap();
                if req.limit == MESSAGES_SLICE_LIMIT {
                    let mut g = hf_srv.lock().unwrap();
                    if g.is_none() {
                        *g = Some((req.offset_id, req.offset_date));
                    }
                }
                ServerAction::ok(&tl_e::messages::Messages::Messages(
                    tl_t::messages::Messages {
                        messages: vec![tl_message(
                            150,
                            1_700_000_500,
                            tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 6002 }),
                            None,
                            "latest",
                            None,
                        )],
                        topics: Vec::new(),
                        chats: vec![pub_ch],
                        users: vec![self_u],
                    },
                ))
            }
            other => ServerAction::err(400, format!("UNHANDLED_0x{other:08x}")),
        }
    });

    // Channel-only export: `useOnlyLastSplit` MUST query ONLY split `(101, 200)`, never `(1, 100)`!
    let settings = Settings {
        path: dir_with_slash(&dir),
        format: OutputFormat::Json,
        types: ExportTypes::PRIVATE_CHANNELS,
        full_chats: ExportTypes::PERSONAL_CHATS
            | ExportTypes::BOT_CHATS
            | ExportTypes::PRIVATE_CHANNELS,
        single_peer_from: 1_700_000_400,
        ..Default::default()
    };
    let (mut ctrl, _) = ExportController::new(
        TlExportSource::new(server.create_export_rpc(2)),
        InputPeer::Empty,
    );
    ctrl.set_skip_path_normalization(true);
    let st = ctrl.run(&settings, &Environment::default_english()).await;
    assert!(matches!(st, ExportState::Finished(_)), "got {st:?}");

    let ranges = dialog_ranges.lock().unwrap();
    assert!(!ranges.is_empty());
    assert!(
        ranges.iter().all(|&r| r == (101, 200)),
        "channel-only export must only query last split (101, 200), got {ranges:?}"
    );
    // First history slice with `single_peer_from = 1_700_000_400` sends `offset_id = 0, offset_date = 1_700_000_399`.
    assert_eq!(*history_first.lock().unwrap(), Some((0, 1_700_000_399)));

    let _ = fs::remove_dir_all(&dir);
}

// ============================================================================
// Test 6: Failed `FILE_REFERENCE_EXPIRED` Refresh & `LOCATION_NOT_AVAILABLE`
//         Gracefully Mark Single File Unavailable Instead of Aborting Export
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtproto2_file_ref_refresh_failure_does_not_abort_export() {
    let dir = temp_dir("ref_refresh_fail");
    let takeout_id = 77_777_i64;

    let server = ScriptedMtprotoServer::new(move |call: &TakeoutWireCall| {
        let self_u = tl_user(1001, "Self", true);
        let peer_u = tl_user(1002, "PeerTwo", false);
        match call.inner_ctor {
            c if c == tl_f::users::GetUsers::CONSTRUCTOR_ID => ServerAction::ok(&vec![self_u]),
            c if c == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_e::account::Takeout::Takeout(tl_t::account::Takeout {
                    id: takeout_id,
                }))
            }
            c if c == tl_f::account::FinishTakeoutSession::CONSTRUCTOR_ID => {
                ServerAction::ok(&true)
            }
            c if c == tl_f::messages::GetSplitRanges::CONSTRUCTOR_ID => {
                ServerAction::ok(&Vec::<tl_e::MessageRange>::new())
            }
            c if c == tl_f::messages::GetDialogs::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_e::messages::Dialogs::Dialogs(tl_t::messages::Dialogs {
                    dialogs: vec![tl_dialog(
                        tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                        2,
                    )],
                    messages: vec![tl_message(
                        2,
                        1_700_000_002,
                        tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                        Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                        "msg2",
                        None,
                    )],
                    chats: Vec::new(),
                    users: vec![self_u, peer_u],
                }))
            }
            c if c == tl_f::messages::GetHistory::CONSTRUCTOR_ID => {
                let p_user = tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 });
                let from_self = Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 }));
                ServerAction::ok(&tl_e::messages::Messages::Messages(
                    tl_t::messages::Messages {
                        messages: vec![
                            tl_message(
                                2,
                                1_700_000_002,
                                p_user.clone(),
                                from_self.clone(),
                                "loc_not_avail",
                                Some(tl_photo_media(99_002, 8, 2, b"ok")),
                            ),
                            tl_message(
                                1,
                                1_700_000_001,
                                p_user,
                                from_self,
                                "ref_refresh_fails",
                                Some(tl_photo_media(99_001, 8, 2, b"expired")),
                            ),
                        ],
                        topics: Vec::new(),
                        chats: Vec::new(),
                        users: vec![self_u, peer_u],
                    },
                ))
            }
            c if c == tl_f::upload::GetFile::CONSTRUCTOR_ID => {
                let req = tl_f::upload::GetFile::from_bytes(&call.inner_body).unwrap();
                match req.location {
                    InputFileLocation::InputPhotoFileLocation(p) if p.id == 99_001 => {
                        ServerAction::err(400, "FILE_REFERENCE_EXPIRED")
                    }
                    InputFileLocation::InputPhotoFileLocation(p) if p.id == 99_002 => {
                        ServerAction::err(400, "LOCATION_NOT_AVAILABLE")
                    }
                    _ => ServerAction::err(400, "LOCATION_INVALID"),
                }
            }
            c if c == tl_f::messages::GetMessages::CONSTRUCTOR_ID => {
                // Refresh itself fails (e.g. message deleted on server) -> must mark file
                // unavailable instead of aborting the export (`export_api_wrap.cpp:3729`).
                ServerAction::err(400, "MESSAGE_ID_INVALID")
            }
            other => ServerAction::err(400, format!("UNHANDLED_0x{other:08x}")),
        }
    });

    let settings = Settings {
        path: dir_with_slash(&dir),
        format: OutputFormat::Json,
        types: ExportTypes::PERSONAL_CHATS,
        media: MediaSettings {
            types: MediaTypes::PHOTO,
            size_limit: 4 * 1024 * 1024,
        },
        ..Default::default()
    };
    let (mut ctrl, _) = ExportController::new(
        TlExportSource::new(server.create_export_rpc(2)),
        InputPeer::Empty,
    );
    ctrl.set_skip_path_normalization(true);
    let st = ctrl.run(&settings, &Environment::default_english()).await;
    assert!(
        matches!(st, ExportState::Finished(_)),
        "expected Finished despite refresh failure & LOCATION_NOT_AVAILABLE, got {st:?}"
    );

    let json: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("result.json")).unwrap()).unwrap();
    let msgs = json["chats"]["list"][0]["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(
        msgs[0]["photo"],
        "(File unavailable, please try again later)"
    );
    assert_eq!(
        msgs[1]["photo"],
        "(File unavailable, please try again later)"
    );

    let _ = fs::remove_dir_all(&dir);
}
