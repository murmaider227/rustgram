//! Byte-level tdesktop conformance tests for MTProto file transfers (`tg-mtproto`,
//! `tg-storage`, `tg-api`, `tg-session`, `tg-media`) against an in-process scripted fake
//! Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233):
//! - `Telegram/SourceFiles/storage/download_manager_mtproto.cpp`
//! - `Telegram/SourceFiles/storage/file_download.cpp` / `file_download_mtproto.cpp`
//! - `Telegram/SourceFiles/storage/streamed_file_downloader.cpp`
//! - `Telegram/SourceFiles/storage/file_upload.cpp`
//! - `Telegram/SourceFiles/data/data_file_origin.cpp`
//! - `Telegram/SourceFiles/apiwrap.cpp` (`refreshFileReference`)
//! - `Telegram/SourceFiles/mtproto/dedicated_file_loader.cpp`
//! - `Telegram/SourceFiles/mtproto/mtp_instance.cpp` (`onErrorDefault`)
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - No real credentials, API keys, phone numbers, or secrets; uses only fake test values
//!   (`api_id = 1`, `api_hash = "0000...0000"`) and an ephemeral test-only RSA keypair.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{
    ApiCredentials, FileOrigin as ApiFileOrigin, InvocationError as ApiInvocationError, RpcClient,
    retry_once_on_file_reference_error,
};
use tg_core::Environment;
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256, sha256_bytes};
use tg_data::{Channel, ChannelId, PeerId, PeerUpdateFlags, Session, User, UserId};
use tg_media::error::MediaError;
use tg_media::transfer::download::{
    CancelToken, DcBalanceState, DownloadRequest, DownloadScheduler, MAX_SESSIONS_COUNT,
    PartAssembler, REMOVE_SESSION_AFTER_TIMEOUTS, RETRY_ADD_SESSION_SUCCESSES,
    RETRY_ADD_SESSION_TIMEOUT_MS, START_SESSIONS_COUNT, START_WAITED_IN_SESSION, download_file,
};
use tg_media::transfer::upload::{
    BIG_FILE_THRESHOLD, FAST_REQUEST_THRESHOLD_MS, MAX_FILE_SIZE_DEFAULT, MAX_FILE_SIZE_PREMIUM,
    MAX_UPLOAD_PARTS_DEFAULT, MAX_UPLOAD_PARTS_PREMIUM, SLOW_REQUEST_THRESHOLD_MS,
    UPLOAD_PART_SIZE_32K, UPLOAD_PART_SIZE_64K, UPLOAD_PART_SIZE_128K, UPLOAD_PART_SIZE_256K,
    UPLOAD_PART_SIZE_512K, UploadConfig, UploadPlan, UploadSessionBalancer, UploadedInputFile,
    choose_upload_part_size, compute_md5_hex, parse_missing_file_part, upload_bytes,
    upload_single_part,
};
use tg_media::transfer::{
    BoxFuture, CDN_HASH_CHUNK_SIZE, CdnFileHash, CdnRedirect, DOWNLOAD_PART_SIZE,
    FileOrigin as MediaFileOrigin, GetCdnFileRequest, GetCdnFileResponse, GetFileRequest,
    GetFileResponse, GetWebFileRequest, GetWebFileResponse, MediaInvoker, SaveBigFilePartRequest,
    SaveFilePartRequest, TransferLocation, apply_cdn_aes_ctr, cdn_ctr_iv,
    validate_get_cdn_file_request, validate_get_file_request,
};
use tg_mtproto::dc_options::{
    DcId, DcOptionFlags, DcType, download_dc_id, updater_dc_id, upload_dc_id,
};
use tg_mtproto::file_loader::{
    DEDICATED_CHUNK_SIZE, DEDICATED_CONCURRENT_REQUESTS, DedicatedFileDownloader,
};
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, InvokeOptions, PacketConnection,
    TransportFactory,
};
use tg_session::{MtprotoRpc, build_mtproto_client_config};
use tg_storage::{
    ChunkFuture, ChunkProvider, StreamedDownloadError, StreamedDownloaderConfig,
    StreamedFileDownloader,
};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, LAYER, RawObject, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::Notify;

// ============================================================================
// Test-only fixtures (ephemeral RSA keypair, fake API creds, helpers)
// ============================================================================

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

/// Test-only 2048-bit RSA private exponent `d` corresponding to `TEST_RSA_N_HEX`, `e = 65537`.
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
    let n = hex_decode(TEST_RSA_N_HEX);
    let e = vec![0x01, 0x00, 0x01];
    PublicKey::from_components(&n, &e).expect("valid test RSA public key")
}

/// Encodes an RSA public key `(n, e=65537)` into a PKCS#1 PEM string for `help.getCdnConfig`.
fn test_rsa_pkcs1_pem() -> String {
    fn der_int(raw: &[u8]) -> Vec<u8> {
        let trimmed = raw
            .iter()
            .position(|&b| b != 0)
            .map_or(&raw[raw.len() - 1..], |i| &raw[i..]);
        let need_pad = trimmed[0] & 0x80 != 0;
        let len = trimmed.len() + usize::from(need_pad);
        let mut out = vec![0x02];
        if len < 0x80 {
            out.push(len as u8);
        } else if len <= 0xFF {
            out.push(0x81);
            out.push(len as u8);
        } else {
            out.push(0x82);
            out.push((len >> 8) as u8);
            out.push((len & 0xFF) as u8);
        }
        if need_pad {
            out.push(0x00);
        }
        out.extend_from_slice(trimmed);
        out
    }

    let n_der = der_int(&hex_decode(TEST_RSA_N_HEX));
    let e_der = der_int(&[0x01, 0x00, 0x01]);
    let seq_len = n_der.len() + e_der.len();
    let mut der = vec![0x30];
    if seq_len <= 0xFF {
        der.push(0x81);
        der.push(seq_len as u8);
    } else {
        der.push(0x82);
        der.push((seq_len >> 8) as u8);
        der.push((seq_len & 0xFF) as u8);
    }
    der.extend_from_slice(&n_der);
    der.extend_from_slice(&e_der);

    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for chunk in der.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        b64.push(B64[((triple >> 18) & 0x3F) as usize] as char);
        b64.push(B64[((triple >> 12) & 0x3F) as usize] as char);
        b64.push(if chunk.len() > 1 {
            B64[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        b64.push(if chunk.len() > 2 {
            B64[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    let mut pem = String::from("-----BEGIN RSA PUBLIC KEY-----\n");
    for line in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END RSA PUBLIC KEY-----\n");
    pem
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

fn deterministic_bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| {
            let v = (i.wrapping_mul(31).wrapping_add(usize::from(seed) * 17)) ^ (i >> 8);
            (v & 0xFF) as u8
        })
        .collect()
}

// ============================================================================
// Byte-Level Scripted Fake MTProto 2.0 Server (supports async/gated RPCs,
// multi-DC download/upload connections, MediaCluster, and CDN DCs)
// ============================================================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DhRecord {
    dc_id: DcId,
    protocol_dc_id: i16,
    inner_dc: i32,
    is_temp: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct InitConnectionRecord {
    dc_id: DcId,
    device_model: String,
    system_version: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct RpcRecord {
    conn_id: u64,
    dc_id: DcId,
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct IncomingRpc {
    conn_id: u64,
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

type AsyncRpcHandler = Arc<
    dyn Fn(ScriptedServer, IncomingRpc) -> Pin<Box<dyn Future<Output = ServerAction> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ServerKeyEntry {
    key: AuthKey,
    server_salt: u64,
    is_temp: bool,
    bound_perm_key_id: Option<u64>,
}

struct ScriptedServerInner {
    rsa_pub: PublicKey,
    rsa_n: Vec<u8>,
    rsa_d: Vec<u8>,
    next_conn_id: AtomicU64,
    dh_counter: AtomicU64,
    keys: Mutex<HashMap<u64, ServerKeyEntry>>,
    dh_records: Mutex<Vec<DhRecord>>,
    inits: Mutex<Vec<InitConnectionRecord>>,
    rpcs: Mutex<Vec<RpcRecord>>,
    binds: Mutex<Vec<(DcId, u64, i64)>>,
    handler: AsyncRpcHandler,
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
        let handler = Arc::new(handler);
        Self::new_async(move |srv, rpc| {
            let h = Arc::clone(&handler);
            Box::pin(async move { h(&srv, rpc) })
        })
    }

    fn new_async<F>(handler: F) -> Self
    where
        F: Fn(ScriptedServer, IncomingRpc) -> Pin<Box<dyn Future<Output = ServerAction> + Send>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            inner: Arc::new(ScriptedServerInner {
                rsa_pub: test_rsa_public_key(),
                rsa_n: hex_decode(TEST_RSA_N_HEX),
                rsa_d: hex_decode(TEST_RSA_D_HEX),
                next_conn_id: AtomicU64::new(1),
                dh_counter: AtomicU64::new(1),
                keys: Mutex::new(HashMap::new()),
                dh_records: Mutex::new(Vec::new()),
                inits: Mutex::new(Vec::new()),
                rpcs: Mutex::new(Vec::new()),
                binds: Mutex::new(Vec::new()),
                handler: Arc::new(handler),
            }),
        }
    }

    fn register_rsa_key_on(&self, client: &Client) {
        client
            .dc_options()
            .add_public_key(self.inner.rsa_pub.clone());
    }

    fn dh_records(&self) -> Vec<DhRecord> {
        self.inner.dh_records.lock().unwrap().clone()
    }

    fn inits(&self) -> Vec<InitConnectionRecord> {
        self.inner.inits.lock().unwrap().clone()
    }

    fn rpcs(&self) -> Vec<RpcRecord> {
        self.inner.rpcs.lock().unwrap().clone()
    }

    fn binds(&self) -> Vec<(DcId, u64, i64)> {
        self.inner.binds.lock().unwrap().clone()
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
                is_temp: bool,
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
                        let (new_nonce, is_temp, inner_dc) = match pq_inner {
                            mtp_enums::PQInnerData::Dc(d) => (d.new_nonce, false, d.dc),
                            mtp_enums::PQInnerData::TempDc(d) => {
                                assert!(d.expires_in > 0);
                                (d.new_nonce, true, d.dc)
                            }
                            other => panic!("unexpected PQInnerData: {other:?}"),
                        };
                        server.inner.dh_records.lock().unwrap().push(DhRecord {
                            dc_id,
                            protocol_dc_id,
                            inner_dc,
                            is_temp,
                        });
                        assert_eq!(
                            inner_dc.unsigned_abs(),
                            u32::from(protocol_dc_id.unsigned_abs())
                        );

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
                            is_temp,
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
                                is_temp: pending.is_temp,
                                bound_perm_key_id: None,
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
                let entry = {
                    let keys = server.inner.keys.lock().unwrap();
                    keys.get(&auth_key_id).cloned()
                };
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
                    if outer_ctor == tl_fns::auth::BindTempAuthKey::CONSTRUCTOR_ID {
                        let bind = tl_fns::auth::BindTempAuthKey::from_bytes(&body).unwrap();
                        {
                            let mut keys = server.inner.keys.lock().unwrap();
                            if let Some(temp_entry) = keys.get_mut(&auth_key_id) {
                                temp_entry.bound_perm_key_id = Some(bind.perm_auth_key_id as u64);
                            }
                        }
                        server.inner.binds.lock().unwrap().push((
                            dc_id,
                            auth_key_id,
                            bind.perm_auth_key_id,
                        ));
                        let rpc_res = Boxed(RpcResult {
                            req_msg_id: msg_id as i64,
                            result: RawObject(BOOL_TRUE_ID.to_le_bytes().to_vec()),
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
                        continue;
                    }

                    // Unwrap `invokeWithLayer(initConnection(...))` if present.
                    let inner_body = if body.len() >= 4
                        && u32::from_le_bytes(body[0..4].try_into().unwrap())
                            == tl_fns::InvokeWithLayer::<RawObject>::CONSTRUCTOR_ID
                    {
                        let mut cur = Cursor::new(&body[4..]);
                        let layer = i32::deserialize(&mut cur).expect("invokeWithLayer.layer");
                        assert_eq!(layer, LAYER);
                        let rest = &body[body.len() - cur.remaining()..];
                        let ic_ctor = u32::from_le_bytes(rest[0..4].try_into().unwrap());
                        assert_eq!(ic_ctor, tl_fns::InitConnection::<RawObject>::CONSTRUCTOR_ID);
                        let mut icur = Cursor::new(&rest[4..]);
                        let flags =
                            i32::deserialize(&mut icur).expect("initConnection.flags") as u32;
                        let _api_id = i32::deserialize(&mut icur).expect("initConnection.api_id");
                        let device_model =
                            String::deserialize(&mut icur).expect("initConnection.device_model");
                        let system_version =
                            String::deserialize(&mut icur).expect("initConnection.system_version");
                        let _app_version =
                            String::deserialize(&mut icur).expect("initConnection.app_version");
                        let _sys_lang = String::deserialize(&mut icur)
                            .expect("initConnection.system_lang_code");
                        let _lang_pack =
                            String::deserialize(&mut icur).expect("initConnection.lang_pack");
                        let _lang_code =
                            String::deserialize(&mut icur).expect("initConnection.lang_code");
                        if (flags & (1 << 0)) != 0 {
                            let _ = tl_enums::InputClientProxy::deserialize(&mut icur)
                                .expect("initConnection.proxy");
                        }
                        if (flags & (1 << 1)) != 0 {
                            let _ = tl_enums::JSONValue::deserialize(&mut icur)
                                .expect("initConnection.params");
                        }
                        server
                            .inner
                            .inits
                            .lock()
                            .unwrap()
                            .push(InitConnectionRecord {
                                dc_id,
                                device_model,
                                system_version,
                            });
                        rest[rest.len() - icur.remaining()..].to_vec()
                    } else {
                        body
                    };

                    if inner_body.len() < 4 {
                        continue;
                    }
                    let inner_ctor = u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                    server.inner.rpcs.lock().unwrap().push(RpcRecord {
                        conn_id,
                        dc_id,
                        ctor: inner_ctor,
                        body: inner_body.clone(),
                    });

                    // Spawn async RPC handler so concurrent requests on one or multiple
                    // connections execute truly in parallel without head-of-line blocking!
                    let srv_clone = server.clone();
                    let tx_clone = tx.clone();
                    let key_clone = entry.key.clone();
                    let salt = entry.server_salt;
                    let msg_ctr = Arc::clone(&server_msg_counter);
                    let seq_ctr = Arc::clone(&server_seq_counter);
                    tokio::spawn(async move {
                        let action = (srv_clone.inner.handler)(
                            srv_clone.clone(),
                            IncomingRpc {
                                conn_id,
                                dc_id,
                                ctor: inner_ctor,
                                body: inner_body,
                            },
                        )
                        .await;
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
                        let r_id = next_server_msg_id(&msg_ctr, true);
                        send_encrypted(
                            &tx_clone,
                            &key_clone,
                            salt,
                            session_id,
                            r_id,
                            true,
                            &seq_ctr,
                            &rpc_res.to_bytes(),
                        );
                    });
                }
            }
        });
    }
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

fn test_client_config(main_dc: DcId, use_pfs: bool) -> ClientConfig {
    let mut cfg = build_mtproto_client_config(
        &ApiCredentials {
            api_id: FAKE_API_ID,
            api_hash: FAKE_API_HASH.to_owned(),
        },
        main_dc,
        Environment::Production,
        None,
    );
    cfg.proxy = ProxyData::none();
    cfg.use_pfs = use_pfs;
    cfg.ping_interval = Duration::from_secs(3600);
    cfg
}

fn tl_authorization(user_id: i64, first_name: &str) -> tl_enums::auth::Authorization {
    tl_enums::auth::Authorization::Authorization(tl_types::auth::Authorization {
        setup_password_required: false,
        otherwise_relogin_days: None,
        tmp_sessions: None,
        future_auth_token: None,
        user: tl_enums::User::User(tl_types::User {
            self_: true,
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
            id: user_id,
            access_hash: Some(987_654_321),
            first_name: Some(first_name.to_owned()),
            last_name: None,
            username: Some("testuser".to_owned()),
            phone: None,
            photo: None,
            status: None,
            bot_info_version: None,
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: Some("en".to_owned()),
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_community_id: None,
        }),
    })
}

fn make_tl_document(id: i64, access_hash: i64, file_reference: Vec<u8>) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash,
        file_reference,
        date: 1_700_000_000,
        mime_type: "application/octet-stream".to_owned(),
        size: 1024,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![],
    })
}

fn make_tl_photo(id: i64, access_hash: i64, file_reference: Vec<u8>) -> tl_enums::Photo {
    tl_enums::Photo::Photo(tl_types::Photo {
        has_stickers: false,
        id,
        access_hash,
        file_reference,
        date: 1_700_000_000,
        sizes: vec![],
        video_sizes: None,
        dc_id: 2,
    })
}

fn compute_cdn_hashes(plaintext: &[u8], from_offset: u64) -> Vec<tl_enums::FileHash> {
    let chunk = CDN_HASH_CHUNK_SIZE as usize;
    let mut off = (from_offset as usize / chunk) * chunk;
    let mut out = Vec::new();
    while off < plaintext.len() {
        let end = (off + chunk).min(plaintext.len());
        let slice = &plaintext[off..end];
        out.push(tl_enums::FileHash::FileHash(tl_types::FileHash {
            offset: off as i64,
            limit: slice.len() as i32,
            hash: sha256_bytes(slice).to_vec(),
        }));
        off = end;
    }
    out
}

// ============================================================================
// Bridge adapter: MediaInvoker over tg_session::MtprotoRpc + shifted file DCs
// ============================================================================

fn map_api_err_to_media(err: ApiInvocationError) -> MediaError {
    match err {
        ApiInvocationError::Rpc(r) => MediaError::Rpc {
            code: r.code,
            message: r.name,
        },
        other => MediaError::Rpc {
            code: 500,
            message: other.to_string(),
        },
    }
}

fn map_tl_hashes(hashes: Vec<tl_enums::FileHash>) -> Vec<CdnFileHash> {
    hashes
        .into_iter()
        .map(|h| {
            let tl_enums::FileHash::FileHash(f) = h;
            CdnFileHash {
                offset: f.offset as u64,
                limit: f.limit as u32,
                hash: f.hash,
            }
        })
        .collect()
}

fn transfer_loc_to_tl(
    loc: &TransferLocation,
) -> tg_media::error::Result<tl_enums::InputFileLocation> {
    match loc {
        TransferLocation::Document {
            id,
            access_hash,
            file_reference,
            thumb_size,
        } => Ok(tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: *id as i64,
                access_hash: *access_hash as i64,
                file_reference: file_reference.clone(),
                thumb_size: thumb_size.clone(),
            },
        )),
        TransferLocation::Photo {
            id,
            access_hash,
            file_reference,
            thumb_size,
        } => Ok(tl_enums::InputFileLocation::InputPhotoFileLocation(
            tl_types::InputPhotoFileLocation {
                id: *id as i64,
                access_hash: *access_hash as i64,
                file_reference: file_reference.clone(),
                thumb_size: thumb_size.clone(),
            },
        )),
        other => Err(MediaError::InvalidTransferParam(format!(
            "unsupported location in conformance bridge: {other:?}"
        ))),
    }
}

/// Real MTProto 2.0 `MediaInvoker` backed by `MtprotoRpc`, routing downloads through
/// `download_dc_id(dc_id, session_index)` and uploads through `upload_dc_id(session_index)`
/// exactly like tdesktop's `DownloadMtprotoTask` and `Storage::Uploader`.
struct MtprotoMediaInvoker {
    rpc: MtprotoRpc,
    session: Session,
}

impl MtprotoMediaInvoker {
    fn new(rpc: MtprotoRpc, session: Session) -> Self {
        Self { rpc, session }
    }
}

impl MediaInvoker for MtprotoMediaInvoker {
    fn get_file(
        &self,
        dc_id: i32,
        session_index: usize,
        req: GetFileRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<GetFileResponse>> {
        Box::pin(async move {
            validate_get_file_request(req.offset, req.limit, req.precise)?;
            let shifted = download_dc_id(dc_id, session_index as i32);
            let tl_req = tl_fns::upload::GetFile {
                precise: req.precise,
                cdn_supported: req.cdn_supported,
                location: transfer_loc_to_tl(&req.location)?,
                offset: req.offset as i64,
                limit: req.limit as i32,
            };
            let resp = self
                .rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_api_err_to_media)?;
            match resp {
                tl_enums::upload::File::File(f) => Ok(GetFileResponse::File { bytes: f.bytes }),
                tl_enums::upload::File::CdnRedirect(r) => {
                    Ok(GetFileResponse::CdnRedirect(CdnRedirect {
                        dc_id: r.dc_id,
                        file_token: r.file_token,
                        encryption_key: r.encryption_key,
                        encryption_iv: r.encryption_iv,
                        file_hashes: map_tl_hashes(r.file_hashes),
                    }))
                }
            }
        })
    }

    fn get_web_file(
        &self,
        dc_id: i32,
        session_index: usize,
        req: GetWebFileRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<GetWebFileResponse>> {
        Box::pin(async move {
            let TransferLocation::WebFile { url, access_hash } = req.location else {
                return Err(MediaError::InvalidTransferParam("expected WebFile".into()));
            };
            let shifted = download_dc_id(dc_id, session_index as i32);
            let tl_req = tl_fns::upload::GetWebFile {
                location: tl_enums::InputWebFileLocation::InputWebFileLocation(
                    tl_types::InputWebFileLocation {
                        url: String::from_utf8(url).unwrap(),
                        access_hash: access_hash as i64,
                    },
                ),
                offset: req.offset as i32,
                limit: req.limit as i32,
            };
            let tl_enums::upload::WebFile::WebFile(wf) = self
                .rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_api_err_to_media)?;
            Ok(GetWebFileResponse {
                size: wf.size as u64,
                mime_type: wf.mime_type,
                bytes: wf.bytes,
            })
        })
    }

    fn get_cdn_file(
        &self,
        cdn_dc_id: i32,
        session_index: usize,
        req: GetCdnFileRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<GetCdnFileResponse>> {
        Box::pin(async move {
            validate_get_cdn_file_request(req.offset, req.limit)?;
            let shifted = download_dc_id(cdn_dc_id, session_index as i32);
            let tl_req = tl_fns::upload::GetCdnFile {
                file_token: req.file_token,
                offset: req.offset as i64,
                limit: req.limit as i32,
            };
            let resp = self
                .rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_api_err_to_media)?;
            match resp {
                tl_enums::upload::CdnFile::CdnFile(f) => Ok(GetCdnFileResponse::Bytes(f.bytes)),
                tl_enums::upload::CdnFile::ReuploadNeeded(r) => {
                    Ok(GetCdnFileResponse::ReuploadNeeded {
                        request_token: r.request_token,
                    })
                }
            }
        })
    }

    fn get_cdn_file_hashes(
        &self,
        dc_id: i32,
        session_index: usize,
        file_token: Vec<u8>,
        offset: u64,
    ) -> BoxFuture<'_, tg_media::error::Result<Vec<CdnFileHash>>> {
        Box::pin(async move {
            let shifted = download_dc_id(dc_id, session_index as i32);
            let tl_req = tl_fns::upload::GetCdnFileHashes {
                file_token,
                offset: offset as i64,
            };
            let hashes = self
                .rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_api_err_to_media)?;
            Ok(map_tl_hashes(hashes))
        })
    }

    fn reupload_cdn_file(
        &self,
        dc_id: i32,
        session_index: usize,
        file_token: Vec<u8>,
        request_token: Vec<u8>,
    ) -> BoxFuture<'_, tg_media::error::Result<Vec<CdnFileHash>>> {
        Box::pin(async move {
            let shifted = download_dc_id(dc_id, session_index as i32);
            let tl_req = tl_fns::upload::ReuploadCdnFile {
                file_token,
                request_token,
            };
            let hashes = self
                .rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_api_err_to_media)?;
            Ok(map_tl_hashes(hashes))
        })
    }

    fn refresh_file_reference(
        &self,
        origin: MediaFileOrigin,
        location: TransferLocation,
    ) -> BoxFuture<'_, tg_media::error::Result<Vec<u8>>> {
        Box::pin(async move {
            let api_origin = if let (Some(peer), Some(msg)) = (origin.peer_id, origin.msg_id) {
                ApiFileOrigin::Message {
                    peer_id: PeerId(peer),
                    msg_id: msg,
                }
            } else if let Some(set_id) = origin.sticker_set_id {
                ApiFileOrigin::StickerSet {
                    set_id,
                    access_hash: 1,
                }
            } else {
                return Err(MediaError::Rpc {
                    code: 400,
                    message: "FILE_REFERENCE_EXPIRED".into(),
                });
            };
            let updated = tg_api::refresh_file_reference(&self.rpc, &self.session, &api_origin)
                .await
                .map_err(|e| MediaError::Rpc {
                    code: 400,
                    message: e.to_string(),
                })?;
            let tl_loc = transfer_loc_to_tl(&location)?;
            updated
                .lookup_tl_location(&tl_loc)
                .ok_or_else(|| MediaError::Rpc {
                    code: 400,
                    message: "FILE_REFERENCE_EXPIRED".into(),
                })
        })
    }

    fn save_file_part(
        &self,
        session_index: usize,
        req: SaveFilePartRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<bool>> {
        Box::pin(async move {
            let shifted = upload_dc_id(session_index as i32);
            let tl_req = tl_fns::upload::SaveFilePart {
                file_id: req.file_id,
                file_part: req.file_part as i32,
                bytes: req.bytes,
            };
            self.rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_api_err_to_media)
        })
    }

    fn save_big_file_part(
        &self,
        session_index: usize,
        req: SaveBigFilePartRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<bool>> {
        Box::pin(async move {
            let shifted = upload_dc_id(session_index as i32);
            let tl_req = tl_fns::upload::SaveBigFilePart {
                file_id: req.file_id,
                file_part: req.file_part as i32,
                file_total_parts: req.file_total_parts as i32,
                bytes: req.bytes,
            };
            self.rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_api_err_to_media)
        })
    }
}

// ============================================================================
// 1. Download Conformance: alignment, 1 MiB boundary, flags, true parallel
//    scheduling, per-DC session scaling, EOF, DedicatedFileDownloader,
//    and StreamedFileDownloader over MTProto 2.0
// ============================================================================

#[tokio::test]
async fn conformance_1_download_alignment_flags_parallel_sessions_and_eof() {
    // 1A. Exhaustive upload.getFile & upload.getCdnFile alignment & 1 MiB boundary rules.
    for shift in 12..=20 {
        let limit = 1u32 << shift; // 4 KiB ..= 1 MiB
        assert!(validate_get_file_request(0, limit, false).is_ok());
        assert!(validate_get_file_request(u64::from(limit), limit, false).is_ok());
        assert!(validate_get_file_request(u64::from(limit) * 3, limit, false).is_ok());
        assert!(validate_get_cdn_file_request(0, limit).is_ok());
        assert!(validate_get_cdn_file_request(u64::from(limit), limit).is_ok());
    }
    // Non-power-of-two limits rejected in non-precise & CDN mode.
    for bad_limit in [
        0,
        1024,
        3072,
        12 * 1024,
        96 * 1024,
        768 * 1024,
        2 * 1024 * 1024,
    ] {
        assert!(validate_get_file_request(0, bad_limit, false).is_err());
        assert!(validate_get_cdn_file_request(0, bad_limit).is_err());
    }
    // Offset not divisible by limit rejected for non-precise AND for upload.getCdnFile!
    assert!(validate_get_file_request(4096, 128 * 1024, false).is_err());
    assert!(validate_get_cdn_file_request(4096, 128 * 1024).is_err());
    // Precise mode allows 4 KiB-aligned offsets, but strictly forbids crossing any 1 MiB boundary.
    assert!(validate_get_file_request(4096, 128 * 1024, true).is_ok());
    assert!(validate_get_file_request(1024 * 1024 - 4096, 4096, true).is_ok());
    assert!(validate_get_file_request(1024 * 1024 - 4096, 8192, true).is_err());
    assert!(
        validate_get_file_request(2 * 1024 * 1024 - 128 * 1024 + 4096, 128 * 1024, true).is_err()
    );
    assert!(validate_get_file_request(100, 4096, true).is_err());

    // 1B. Per-DC session count scaling (download_manager_mtproto.cpp:22-361).
    let mut bal = DcBalanceState::default();
    assert_eq!(bal.sessions.len(), START_SESSIONS_COUNT);
    assert_eq!(bal.sessions[0].max_waited_amount, START_WAITED_IN_SESSION);
    // Timeout at 1 session must NOT accumulate timeout debt (`download_manager_mtproto.cpp:325-326`).
    for t in 0..5 {
        assert_eq!(bal.record_timeout(0, 1000 + t * 100), None);
    }
    assert_eq!(bal.timeouts, 0);
    // Scale from 1 -> MAX_SESSIONS_COUNT (8) via fast successes across all active sessions.
    let mut now = 10_000u64;
    while bal.sessions.len() < MAX_SESSIONS_COUNT {
        let count = bal.sessions.len();
        let mut added = false;
        for _ in 0..RETRY_ADD_SESSION_SUCCESSES {
            for s in 0..count {
                let max_w = bal.sessions[s].max_waited_amount;
                if bal.record_request_succeeded(
                    s,
                    max_w,
                    now - 50,
                    now,
                    DOWNLOAD_PART_SIZE as usize,
                ) {
                    added = true;
                }
                now += 100;
            }
        }
        assert!(added, "expected session pool to grow past {count}");
    }
    assert_eq!(bal.sessions.len(), MAX_SESSIONS_COUNT);
    // Shed highest session after REMOVE_SESSION_AFTER_TIMEOUTS slow/timed-out requests.
    let mut shed = None;
    for _ in 0..REMOVE_SESSION_AFTER_TIMEOUTS {
        now += 500;
        shed = bal.record_timeout(7, now);
    }
    assert_eq!(shed, Some(7));
    assert_eq!(bal.sessions.len(), MAX_SESSIONS_COUNT - 1);
    // Respect RETRY_ADD_SESSION_TIMEOUT_MS backoff before re-adding session #7.
    let remove_time = bal.last_session_remove_ms;
    for _ in 0..(2 * RETRY_ADD_SESSION_SUCCESSES) {
        for s in 0..bal.sessions.len() {
            let max_w = bal.sessions[s].max_waited_amount;
            let added = bal.record_request_succeeded(
                s,
                max_w,
                remove_time + 10,
                remove_time + RETRY_ADD_SESSION_TIMEOUT_MS,
                DOWNLOAD_PART_SIZE as usize,
            );
            assert!(
                !added,
                "must respect backoff before re-adding removed session"
            );
        }
    }
    let mut sched = DownloadScheduler::new();
    sched.enqueue(2, 1, 0);
    sched.enqueue(2, 2, 10);
    assert_eq!(
        sched.poll_next(2, DOWNLOAD_PART_SIZE as usize, |_| true),
        Some((2, 0))
    );

    // 1C. End-to-end MTProto 2.0 download: true parallel scheduling across shifted download
    // connections, precise/cdn_supported flags, and EOF detection (short tail + exact multiple).
    let file_parallel = deterministic_bytes(4 * 128 * 1024 + 777, 1); // 512 KiB + 777 B (5 chunks)
    let file_exact_eof = deterministic_bytes(2 * 128 * 1024, 2); // exact 256 KiB, unknown full_size

    let gate_128k_seen = Arc::new(AtomicUsize::new(0));
    let gate_256k_arrived = Arc::new(Notify::new());
    let seen_conns = Arc::new(Mutex::new(HashSet::<u64>::new()));
    let observed_flags = Arc::new(Mutex::new(Vec::<(i64, i64, bool, bool)>::new()));

    let fp = file_parallel.clone();
    let fe = file_exact_eof.clone();
    let g_seen = Arc::clone(&gate_128k_seen);
    let g_notify = Arc::clone(&gate_256k_arrived);
    let conns_rec = Arc::clone(&seen_conns);
    let flags_rec = Arc::clone(&observed_flags);

    let server = ScriptedServer::new_async(move |_srv, rpc| {
        let fp = fp.clone();
        let fe = fe.clone();
        let g_seen = Arc::clone(&g_seen);
        let g_notify = Arc::clone(&g_notify);
        let conns_rec = Arc::clone(&conns_rec);
        let flags_rec = Arc::clone(&flags_rec);
        Box::pin(async move {
            assert_eq!(rpc.ctor, tl_fns::upload::GetFile::CONSTRUCTOR_ID);
            let req = tl_fns::upload::GetFile::from_bytes(&rpc.body).unwrap();
            let doc_id = match &req.location {
                tl_enums::InputFileLocation::InputDocumentFileLocation(d) => d.id,
                other => panic!("unexpected location: {other:?}"),
            };
            flags_rec
                .lock()
                .unwrap()
                .push((doc_id, req.offset, req.precise, req.cdn_supported));
            if doc_id == 100 {
                conns_rec.lock().unwrap().insert(rpc.conn_id);
                // Gate offset 128 KiB until offset 256 KiB arrives concurrently!
                // If download_file ran waves sequentially, this would deadlock and time out!
                if req.offset == 128 * 1024 {
                    g_seen.fetch_add(1, Ordering::SeqCst);
                    g_notify.notified().await;
                } else if req.offset == 256 * 1024 {
                    while g_seen.load(Ordering::SeqCst) == 0 {
                        tokio::task::yield_now().await;
                    }
                    g_notify.notify_waiters();
                }
                let start = (req.offset as usize).min(fp.len());
                let end = (start + req.limit as usize).min(fp.len());
                ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FilePartial,
                    mtime: 1_700_000_000,
                    bytes: fp[start..end].to_vec(),
                }))
            } else if doc_id == 101 {
                let start = (req.offset as usize).min(fe.len());
                let end = (start + req.limit as usize).min(fe.len());
                ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FilePartial,
                    mtime: 1_700_000_000,
                    bytes: fe[start..end].to_vec(),
                }))
            } else {
                ServerAction::err(400, "FILE_ID_INVALID")
            }
        })
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());
    let invoker = MtprotoMediaInvoker::new(rpc.clone(), Session::new(UserId(1)));
    let cancel = CancelToken::new();

    // Parallel download across 4 shifted download sessions.
    let req1 = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 100,
            access_hash: 100,
            file_reference: vec![1, 2],
            thumb_size: String::new(),
        },
        file_parallel.len() as u64,
    );
    let got1 = tokio::time::timeout(
        TEST_TIMEOUT,
        download_file(&invoker, req1, None, &cancel, |_| {}),
    )
    .await
    .expect("parallel download must not deadlock")
    .expect("parallel download ok");
    assert_eq!(got1, file_parallel);
    assert!(
        seen_conns.lock().unwrap().len() >= 2,
        "parallel wave must dispatch across multiple shifted download connections"
    );

    // Unknown-size EOF when file length is an exact multiple of part_size, with precise=true & cdn_supported=false.
    let mut req2 = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 101,
            access_hash: 101,
            file_reference: vec![3, 4],
            thumb_size: String::new(),
        },
        0, // unknown size -> EOF detected by empty 0-byte chunk at offset 256 KiB
    );
    req2.precise = true;
    req2.cdn_supported = false;
    let got2 = tokio::time::timeout(
        TEST_TIMEOUT,
        download_file(&invoker, req2, None, &cancel, |_| {}),
    )
    .await
    .expect("eof download timeout")
    .expect("eof download ok");
    assert_eq!(got2, file_exact_eof);

    let flags = observed_flags.lock().unwrap().clone();
    assert!(
        flags
            .iter()
            .any(|&(id, _, precise, cdn)| id == 100 && !precise && cdn),
        "default upload.getFile must send precise=false, cdn_supported=true"
    );
    assert!(
        flags
            .iter()
            .any(|&(id, off, precise, cdn)| id == 101 && off == 256 * 1024 && precise && !cdn),
        "exact-multiple unknown-size EOF must send empty-chunk probe at 256 KiB with precise=true, cdn_supported=false"
    );

    // 1D. DedicatedFileDownloader (`tg-mtproto/src/file_loader.rs`) over `updater_dc_id(2)`.
    let mut dedicated =
        DedicatedFileDownloader::new(2, file_exact_eof.len() as u64, 0, DEDICATED_CHUNK_SIZE)
            .unwrap();
    assert_eq!(dedicated.shifted_dc_id(), updater_dc_id(2));
    assert_eq!(DEDICATED_CONCURRENT_REQUESTS, 4);
    let c0 = dedicated.next_chunk_request().unwrap();
    let c1 = dedicated.next_chunk_request().unwrap();
    assert_eq!(c0, (0, DEDICATED_CHUNK_SIZE));
    assert_eq!(c1, (DEDICATED_CHUNK_SIZE as u64, DEDICATED_CHUNK_SIZE));
    assert_eq!(dedicated.next_chunk_request(), None);
    // Oversized chunk and short non-final chunk must be rejected.
    assert!(
        dedicated
            .on_chunk_received(0, vec![0u8; DEDICATED_CHUNK_SIZE + 1])
            .is_err()
    );
    assert!(
        dedicated
            .on_chunk_received(0, vec![0u8; DEDICATED_CHUNK_SIZE - 1])
            .is_err()
    );
    // Out-of-order receipt: c1 arrives first, then duplicate c1 rejected, then c0 drains both.
    assert!(
        dedicated
            .on_chunk_received(c1.0, file_exact_eof[DEDICATED_CHUNK_SIZE..].to_vec())
            .unwrap()
            .is_empty()
    );
    assert!(
        dedicated
            .on_chunk_received(c1.0, file_exact_eof[DEDICATED_CHUNK_SIZE..].to_vec())
            .is_err()
    );
    let drained = dedicated
        .on_chunk_received(c0.0, file_exact_eof[..DEDICATED_CHUNK_SIZE].to_vec())
        .unwrap();
    assert_eq!(drained.len(), 2);
    assert!(dedicated.is_complete());

    // 1E. StreamedFileDownloader (`tg-storage`) backed by live MTProto `upload.getFile`.
    struct RealMtprotoChunkProvider {
        rpc: MtprotoRpc,
    }
    impl ChunkProvider for RealMtprotoChunkProvider {
        fn fetch_chunk(&self, offset: u64, length: usize) -> ChunkFuture<'_> {
            let rpc = self.rpc.clone();
            Box::pin(async move {
                let req = tl_fns::upload::GetFile {
                    precise: false,
                    cdn_supported: false,
                    location: tl_enums::InputFileLocation::InputDocumentFileLocation(
                        tl_types::InputDocumentFileLocation {
                            id: 101,
                            access_hash: 101,
                            file_reference: vec![3, 4],
                            thumb_size: String::new(),
                        },
                    ),
                    offset: offset as i64,
                    limit: DOWNLOAD_PART_SIZE as i32,
                };
                let tl_enums::upload::File::File(f) = rpc
                    .invoke_in_dc(download_dc_id(2, 0), &req)
                    .await
                    .map_err(|e| StreamedDownloadError::ProviderFailed {
                        offset,
                        message: e.to_string(),
                    })?
                else {
                    return Err(StreamedDownloadError::ProviderFailed {
                        offset,
                        message: "unexpected redirect".into(),
                    });
                };
                Ok(f.bytes[..length.min(f.bytes.len())].to_vec())
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("streamed.bin");
    let mut scfg = StreamedDownloaderConfig::new(101, file_exact_eof.len() as u64, &target);
    scfg.expected_sha256 = Some(sha256_bytes(&file_exact_eof));
    let sdl =
        StreamedFileDownloader::open(scfg, Arc::new(RealMtprotoChunkProvider { rpc })).unwrap();
    let tail_slice = sdl.read_range(128 * 1024 + 100, 500).await.unwrap();
    assert_eq!(
        tail_slice,
        file_exact_eof[128 * 1024 + 100..128 * 1024 + 600]
    );
    let final_path = sdl.download_all().await.unwrap();
    assert_eq!(std::fs::read(&final_path).unwrap(), file_exact_eof);

    client.shutdown();
}

// ============================================================================
// 2. CDN Conformance: help.getCdnConfig, CDN RSA isolation & unbound temp keys,
//    fileCdnRedirect -> getCdnFile + AES-256-CTR with offset-folded IV,
//    cdnFileReuploadNeeded -> reuploadCdnFile, getCdnFileHashes SHA-256,
//    tampered-piece rejection, mid-wave token invalidation fallback
// ============================================================================

#[tokio::test]
async fn conformance_2_cdn_redirect_ctr_reupload_hashes_tamper_and_fallback() {
    let key: [u8; 32] = [0x4B; 32];
    let iv: [u8; 16] = [
        0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0xDE, 0xAD, 0xBE,
        0xEF,
    ];
    // Verify offset folding matches `DownloadMtprotoTask::cdnPartLoaded` (`download_manager_mtproto.cpp:694-698`).
    assert_eq!(
        cdn_ctr_iv(&iv, 0).unwrap(),
        [
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0x00, 0x00,
            0x00, 0x00
        ]
    );
    assert_eq!(
        cdn_ctr_iv(&iv, 256 * 1024).unwrap(),
        [
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0x00, 0x00,
            0x40, 0x00
        ]
    );

    let payload_ok = deterministic_bytes(3 * 128 * 1024 + 555, 7); // 4 chunks (384 KiB + 555 B)
    let payload_exact = deterministic_bytes(2 * 128 * 1024, 9); // exact 256 KiB (EOF test)
    let payload_tamper = deterministic_bytes(2 * 128 * 1024, 11);
    let payload_fallback = deterministic_bytes(3 * 128 * 1024, 13);

    let reupload_needed_once = Arc::new(AtomicUsize::new(1));
    let fallback_token_invalid_once = Arc::new(AtomicUsize::new(1));
    let reupload_calls = Arc::new(AtomicUsize::new(0));
    let hash_calls = Arc::new(AtomicUsize::new(0));
    let cdn_auth_exports = Arc::new(AtomicUsize::new(0));

    let p_ok = payload_ok.clone();
    let p_exact = payload_exact.clone();
    let p_tamper = payload_tamper.clone();
    let p_fb = payload_fallback.clone();
    let reup_once = Arc::clone(&reupload_needed_once);
    let fb_once = Arc::clone(&fallback_token_invalid_once);
    let reup_ctr = Arc::clone(&reupload_calls);
    let hash_ctr = Arc::clone(&hash_calls);
    let cdn_exp = Arc::clone(&cdn_auth_exports);

    let cdn_pem = test_rsa_pkcs1_pem();

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::help::GetCdnConfig::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 2, "help.getCdnConfig must run on main DC");
            ServerAction::ok(&tl_enums::CdnConfig::CdnConfig(tl_types::CdnConfig {
                public_keys: vec![tl_enums::CdnPublicKey::CdnPublicKey(
                    tl_types::CdnPublicKey {
                        dc_id: 203,
                        public_key: cdn_pem.clone(),
                    },
                )],
            }))
        }
        x if x == tl_fns::auth::ExportAuthorization::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::ExportAuthorization::from_bytes(&rpc.body).unwrap();
            if req.dc_id == 203 {
                cdn_exp.fetch_add(1, Ordering::SeqCst);
            }
            ServerAction::err(400, "DC_ID_INVALID")
        }
        x if x == tl_fns::upload::GetFile::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 2, "upload.getFile must run on home DC 2");
            let req = tl_fns::upload::GetFile::from_bytes(&rpc.body).unwrap();
            let doc_id = match req.location {
                tl_enums::InputFileLocation::InputDocumentFileLocation(d) => d.id,
                other => panic!("unexpected location: {other:?}"),
            };
            match doc_id {
                200 => {
                    // Return only chunk 0 hash initially -> forces upload.getCdnFileHashes for later chunks!
                    let mut initial = compute_cdn_hashes(&p_ok, 0);
                    initial.truncate(1);
                    ServerAction::ok(&tl_enums::upload::File::CdnRedirect(
                        tl_types::upload::FileCdnRedirect {
                            dc_id: 203,
                            file_token: b"cdn-tok-200".to_vec(),
                            encryption_key: key.to_vec(),
                            encryption_iv: iv.to_vec(),
                            file_hashes: initial,
                        },
                    ))
                }
                201 => ServerAction::ok(&tl_enums::upload::File::CdnRedirect(
                    tl_types::upload::FileCdnRedirect {
                        dc_id: 203,
                        file_token: b"cdn-tok-201".to_vec(),
                        encryption_key: key.to_vec(),
                        encryption_iv: iv.to_vec(),
                        file_hashes: compute_cdn_hashes(&p_exact, 0),
                    },
                )),
                202 => ServerAction::ok(&tl_enums::upload::File::CdnRedirect(
                    tl_types::upload::FileCdnRedirect {
                        dc_id: 203,
                        file_token: b"cdn-tok-202".to_vec(),
                        encryption_key: key.to_vec(),
                        encryption_iv: iv.to_vec(),
                        file_hashes: compute_cdn_hashes(&p_tamper, 0),
                    },
                )),
                203 => {
                    if fb_once.load(Ordering::SeqCst) > 0 {
                        // First redirect omits hashes for offsets >= 128K so offset 128K lands in unchecked_cdn
                        // while offset 256K fails with FILE_TOKEN_INVALID in the same wave!
                        let mut initial = compute_cdn_hashes(&p_fb, 0);
                        initial.truncate(1);
                        ServerAction::ok(&tl_enums::upload::File::CdnRedirect(
                            tl_types::upload::FileCdnRedirect {
                                dc_id: 203,
                                file_token: b"cdn-tok-203".to_vec(),
                                encryption_key: key.to_vec(),
                                encryption_iv: iv.to_vec(),
                                file_hashes: initial,
                            },
                        ))
                    } else {
                        let start = (req.offset as usize).min(p_fb.len());
                        let end = (start + req.limit as usize).min(p_fb.len());
                        ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                            r#type: tl_enums::storage::FileType::FilePartial,
                            mtime: 1_700_000_000,
                            bytes: p_fb[start..end].to_vec(),
                        }))
                    }
                }
                _ => ServerAction::err(400, "FILE_ID_INVALID"),
            }
        }
        x if x == tl_fns::upload::GetCdnFile::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 203, "upload.getCdnFile MUST go to CDN DC 203");
            let req = tl_fns::upload::GetCdnFile::from_bytes(&rpc.body).unwrap();
            let offset = req.offset as u64;
            let limit = req.limit as usize;
            if req.file_token == b"cdn-tok-bad-401" {
                return ServerAction::err(400, "FILE_ID_INVALID");
            }
            if req.file_token == b"cdn-tok-200" && reup_once.swap(0, Ordering::SeqCst) == 1 {
                return ServerAction::ok(&tl_enums::upload::CdnFile::ReuploadNeeded(
                    tl_types::upload::CdnFileReuploadNeeded {
                        request_token: b"req-reupload-200".to_vec(),
                    },
                ));
            }
            if req.file_token == b"cdn-tok-203" && offset == 256 * 1024 {
                fb_once.store(0, Ordering::SeqCst);
                return ServerAction::err(400, "FILE_TOKEN_INVALID");
            }
            let src = if req.file_token == b"cdn-tok-200" {
                &p_ok
            } else if req.file_token == b"cdn-tok-201" {
                &p_exact
            } else if req.file_token == b"cdn-tok-202" {
                &p_tamper
            } else if req.file_token == b"cdn-tok-203" {
                &p_fb
            } else {
                return ServerAction::err(400, "FILE_TOKEN_INVALID");
            };
            let start = (offset as usize).min(src.len());
            let end = (start + limit).min(src.len());
            let mut chunk = src[start..end].to_vec();
            if !chunk.is_empty() {
                apply_cdn_aes_ctr(&mut chunk, &key, &iv, offset).unwrap();
                if req.file_token == b"cdn-tok-202" && offset == 128 * 1024 {
                    chunk[42] ^= 0x01; // 1-bit tamper in second 128 KiB piece!
                }
            }
            ServerAction::ok(&tl_enums::upload::CdnFile::CdnFile(
                tl_types::upload::CdnFile { bytes: chunk },
            ))
        }
        x if x == tl_fns::upload::ReuploadCdnFile::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 2, "upload.reuploadCdnFile MUST go to home DC 2");
            let req = tl_fns::upload::ReuploadCdnFile::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.file_token, b"cdn-tok-200");
            assert_eq!(req.request_token, b"req-reupload-200");
            reup_ctr.fetch_add(1, Ordering::SeqCst);
            // Return only chunk 0 hash so chunk 1..3 still exercise upload.getCdnFileHashes!
            let mut h = compute_cdn_hashes(&p_ok, 0);
            h.truncate(1);
            ServerAction::ok(&h)
        }
        x if x == tl_fns::upload::GetCdnFileHashes::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 2, "upload.getCdnFileHashes MUST go to home DC 2");
            let req = tl_fns::upload::GetCdnFileHashes::from_bytes(&rpc.body).unwrap();
            hash_ctr.fetch_add(1, Ordering::SeqCst);
            let src = if req.file_token == b"cdn-tok-200" {
                &p_ok
            } else {
                &p_fb
            };
            ServerAction::ok(&compute_cdn_hashes(src, req.offset as u64))
        }
        other => panic!("unexpected RPC ctor 0x{other:08x} on dc {}", rpc.dc_id),
    });

    // Use `use_pfs = false` deliberately to verify CDN DC 203 STILL uses an unbound temporary key!
    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);

    // Register CDN DC 203 option.
    client
        .dc_options()
        .add_endpoint(tg_mtproto::dc_options::DcEndpoint {
            id: 203,
            flags: DcOptionFlags(DcOptionFlags::CDN),
            ip: "127.0.0.203".to_owned(),
            port: 443,
            secret: Vec::new(),
        });
    assert_eq!(client.dc_options().dc_type(203), DcType::Cdn);
    // Before help.getCdnConfig, CDN DC 203 MUST NOT fall back to regular Telegram RSA keys!
    assert!(
        client
            .dc_options()
            .get_dc_rsa_key(203, &[test_rsa_public_key().fingerprint()])
            .is_none(),
        "CDN DC must not fall back to regular Telegram RSA keys before help.getCdnConfig"
    );

    // Fetch CDN RSA key via help.getCdnConfig.
    let cdn_count = tokio::time::timeout(TEST_TIMEOUT, client.request_cdn_config())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cdn_count, 1);
    assert!(client.dc_options().has_cdn_keys_for_dc(203));

    let rpc = MtprotoRpc::new(client.clone());
    let invoker = MtprotoMediaInvoker::new(rpc, Session::new(UserId(1)));
    let cancel = CancelToken::new();

    // 2A. Full CDN download: fileCdnRedirect -> cdnFileReuploadNeeded -> reuploadCdnFile ->
    //     getCdnFile (AES-256-CTR across 4 chunks) -> getCdnFileHashes -> SHA-256 verified!
    let req_ok = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 200,
            access_hash: 200,
            file_reference: vec![1],
            thumb_size: String::new(),
        },
        payload_ok.len() as u64,
    );
    let got_ok = tokio::time::timeout(
        TEST_TIMEOUT,
        download_file(&invoker, req_ok, None, &cancel, |_| {}),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(got_ok, payload_ok);
    assert_eq!(reupload_calls.load(Ordering::SeqCst), 1);
    assert!(hash_calls.load(Ordering::SeqCst) >= 1);

    // Verify CDN DC 203 used an unbound temporary key (is_temp=true, no bindTempAuthKey, not in persistent keys)
    // and initConnection sent device_model="n/a", system_version="n/a"!
    let cdn_dh: Vec<_> = server
        .dh_records()
        .into_iter()
        .filter(|d| d.dc_id == 203)
        .collect();
    assert!(!cdn_dh.is_empty());
    assert!(
        cdn_dh.iter().all(|d| d.is_temp),
        "CDN DC must ONLY negotiate temporary auth keys"
    );
    assert!(
        server.binds().iter().all(|&(dc, _, _)| dc != 203),
        "CDN DC must never send auth.bindTempAuthKey"
    );
    assert!(
        client
            .export_persistent_keys()
            .iter()
            .all(|&(dc, _)| dc != 203),
        "CDN DC key must never be stored/exported as a persistent key"
    );
    let cdn_init = server
        .inits()
        .into_iter()
        .find(|i| i.dc_id == 203)
        .expect("CDN initConnection recorded");
    assert_eq!(cdn_init.device_model, "n/a");
    assert_eq!(cdn_init.system_version, "n/a");

    // 2B. CDN exact-multiple unknown-size EOF (empty 0-byte chunk at offset 256 KiB must NOT fail CdnHashMissing).
    let req_exact = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 201,
            access_hash: 201,
            file_reference: vec![2],
            thumb_size: String::new(),
        },
        0,
    );
    let got_exact = tokio::time::timeout(
        TEST_TIMEOUT,
        download_file(&invoker, req_exact, None, &cancel, |_| {}),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(got_exact, payload_exact);

    // 2C. Tampered CDN 128 KiB piece MUST be rejected with CdnHashMismatch at exact offset!
    let req_tamper = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 202,
            access_hash: 202,
            file_reference: vec![3],
            thumb_size: String::new(),
        },
        payload_tamper.len() as u64,
    );
    let tamper_err = tokio::time::timeout(
        TEST_TIMEOUT,
        download_file(&invoker, req_tamper, None, &cancel, |_| {}),
    )
    .await
    .unwrap()
    .expect_err("tampered CDN piece must be rejected");
    assert!(
        matches!(tamper_err, MediaError::CdnHashMismatch { offset: 131_072 }),
        "expected CdnHashMismatch at 128 KiB, got {tamper_err:?}"
    );

    // 2D. Mid-wave FILE_TOKEN_INVALID while an earlier chunk in the wave is in `unchecked_cdn`
    //     must cleanly fall back to home DC 2 (never spurious Cancelled, never corrupted holes!).
    let req_fb = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 203,
            access_hash: 203,
            file_reference: vec![4],
            thumb_size: String::new(),
        },
        payload_fallback.len() as u64,
    );
    let got_fb = tokio::time::timeout(
        TEST_TIMEOUT,
        download_file(&invoker, req_fb, None, &cancel, |_| {}),
    )
    .await
    .unwrap()
    .expect("mid-wave FILE_TOKEN_INVALID must fall back cleanly to home DC");
    assert_eq!(got_fb, payload_fallback);

    // 2E. CDN DC returning 400 FILE_ID_INVALID must NEVER trigger auth.exportAuthorization to CDN DC!
    let bad_cdn_res = client
        .invoke_in_dc(
            download_dc_id(203, 0),
            &tl_fns::upload::GetCdnFile {
                file_token: b"cdn-tok-bad-401".to_vec(),
                offset: 0,
                limit: DOWNLOAD_PART_SIZE as i32,
            },
        )
        .await;
    assert!(bad_cdn_res.is_err());
    assert_eq!(
        cdn_auth_exports.load(Ordering::SeqCst),
        0,
        "must NEVER call auth.exportAuthorization for a CDN DC"
    );

    client.shutdown();
}

// ============================================================================
// 3. File Reference Expiry Conformance (`data_file_origin.cpp` & `apiwrap.cpp`):
//    FILE_REFERENCE_EXPIRED / FILE_REFERENCE_INVALID refetch origin and retry once
// ============================================================================

#[tokio::test]
async fn conformance_3_file_reference_expiry_origins_and_retry_once() {
    let get_file_attempts: Arc<Mutex<HashMap<i64, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let attempts_srv = Arc::clone(&get_file_attempts);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::upload::GetFile::CONSTRUCTOR_ID => {
            let req = tl_fns::upload::GetFile::from_bytes(&rpc.body).unwrap();
            let (id, fref) = match req.location {
                tl_enums::InputFileLocation::InputDocumentFileLocation(d) => {
                    (d.id, d.file_reference)
                }
                tl_enums::InputFileLocation::InputPhotoFileLocation(p) => (p.id, p.file_reference),
                other => panic!("unexpected location: {other:?}"),
            };
            *attempts_srv.lock().unwrap().entry(id).or_insert(0) += 1;
            if id == 9001 {
                // Always returns FILE_REFERENCE_EXPIRED even after refresh -> tests retry-ONCE cap!
                return ServerAction::err(400, "FILE_REFERENCE_EXPIRED");
            }
            if fref == b"stale-ref" {
                if id == 3002 {
                    return ServerAction::err(400, "FILE_REFERENCE_INVALID");
                }
                return ServerAction::err(400, "FILE_REFERENCE_EXPIRED");
            }
            assert_eq!(fref, format!("fresh-{id}").into_bytes());
            ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FilePartial,
                mtime: 1_700_000_000,
                bytes: format!("ok-{id}").into_bytes(),
            }))
        }
        // Origin 1: Private/group message -> messages.getMessages
        x if x == tl_fns::messages::GetMessages::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetMessages::from_bytes(&rpc.body).unwrap();
            let tl_enums::InputMessage::ID(mid) = &req.id[0] else {
                panic!("expected InputMessageID");
            };
            let (doc_id, new_ref) = match mid.id {
                50 => (3001i64, b"fresh-3001".to_vec()),
                90 => (9001i64, b"fresh-9001".to_vec()), // changed ref, still expires on retry
                91 => (9002i64, b"stale-ref".to_vec()),  // unchanged ref -> must NOT retry
                other => panic!("unexpected msg id {other}"),
            };
            let msg = tl_enums::Message::Message(tl_types::Message {
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
                id: mid.id,
                from_id: None,
                from_boosts_applied: None,
                from_rank: None,
                peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 42 }),
                saved_peer_id: None,
                fwd_from: None,
                via_bot_id: None,
                via_business_bot_id: None,
                reply_to: None,
                date: 1_700_000_000,
                message: String::new(),
                media: Some(tl_enums::MessageMedia::Document(
                    tl_types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: false,
                        document: Some(make_tl_document(doc_id, 1, new_ref)),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                )),
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
                guestchat_via_from: None,
                rich_message: None,
            });
            ServerAction::ok(&tl_enums::messages::Messages::Messages(
                tl_types::messages::Messages {
                    messages: vec![msg],
                    topics: vec![],
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        // Origin 2: Channel message -> channels.getMessages
        x if x == tl_fns::channels::GetMessages::CONSTRUCTOR_ID => {
            let msg = tl_enums::Message::Message(tl_types::Message {
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
                id: 77,
                from_id: None,
                from_boosts_applied: None,
                from_rank: None,
                peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 }),
                saved_peer_id: None,
                fwd_from: None,
                via_bot_id: None,
                via_business_bot_id: None,
                reply_to: None,
                date: 1_700_000_000,
                message: String::new(),
                media: Some(tl_enums::MessageMedia::Document(
                    tl_types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: false,
                        document: Some(make_tl_document(3002, 2, b"fresh-3002".to_vec())),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                )),
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
                guestchat_via_from: None,
                rich_message: None,
            });
            ServerAction::ok(&tl_enums::messages::Messages::ChannelMessages(
                tl_types::messages::ChannelMessages {
                    inexact: false,
                    pts: 10,
                    count: 1,
                    offset_id_offset: None,
                    messages: vec![msg],
                    topics: vec![],
                    chats: vec![],
                    users: vec![],
                },
            ))
        }
        // Origin 3: User photo -> photos.getUserPhotos(offset=-1, max_id=photo_id, limit=1)
        x if x == tl_fns::photos::GetUserPhotos::CONSTRUCTOR_ID => {
            let req = tl_fns::photos::GetUserPhotos::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.offset, -1);
            assert_eq!(req.max_id, 3003);
            assert_eq!(req.limit, 1);
            ServerAction::ok(&tl_enums::photos::Photos::Photos(
                tl_types::photos::Photos {
                    photos: vec![make_tl_photo(3003, 3, b"fresh-3003".to_vec())],
                    users: vec![],
                },
            ))
        }
        // Origin 4: Sticker set -> messages.getStickerSet
        x if x == tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
                set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                    archived: false,
                    official: false,
                    masks: false,
                    emojis: false,
                    text_color: false,
                    channel_emoji_status: false,
                    creator: false,
                    installed_date: None,
                    id: 888,
                    access_hash: 999,
                    title: "Pack".to_owned(),
                    short_name: "pack".to_owned(),
                    thumbs: None,
                    thumb_dc_id: None,
                    thumb_version: None,
                    thumb_document_id: None,
                    count: 1,
                    hash: 0,
                }),
                packs: vec![],
                keywords: vec![],
                documents: vec![make_tl_document(3004, 4, b"fresh-3004".to_vec())],
            }),
        ),
        // Origin 5: Wallpaper -> account.getWallPaper (both by ID and by slug)
        x if x == tl_fns::account::GetWallPaper::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetWallPaper::from_bytes(&rpc.body).unwrap();
            let doc_id = match req.wallpaper {
                tl_enums::InputWallPaper::InputWallPaper(_) => 3005,
                tl_enums::InputWallPaper::Slug(_) => 3006,
                other => panic!("unexpected InputWallPaper: {other:?}"),
            };
            ServerAction::ok(&tl_enums::WallPaper::WallPaper(tl_types::WallPaper {
                id: doc_id,
                creator: false,
                default: false,
                pattern: false,
                dark: false,
                access_hash: 55,
                slug: "wp-slug".to_owned(),
                document: make_tl_document(doc_id, 5, format!("fresh-{doc_id}").into_bytes()),
                settings: None,
            }))
        }
        // Origin 6: Saved GIFs -> messages.getSavedGifs
        x if x == tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::SavedGifs::SavedGifs(tl_types::messages::SavedGifs {
                hash: 0,
                gifs: vec![make_tl_document(3007, 7, b"fresh-3007".to_vec())],
            }),
        ),
        // Origin 7: Saved ringtones -> account.getSavedRingtones
        x if x == tl_fns::account::GetSavedRingtones::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::account::SavedRingtones::SavedRingtones(tl_types::account::SavedRingtones {
                hash: 0,
                ringtones: vec![make_tl_document(3008, 8, b"fresh-3008".to_vec())],
            }),
        ),
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());

    let mut session = Session::new(UserId(1));
    let mut alice = User::new(UserId(42), false);
    alice.access_hash = 4242;
    alice.first_name = "Alice".to_owned();
    session.upsert_user(alice, PeerUpdateFlags::empty());
    let mut chan = Channel::new(ChannelId(500));
    chan.access_hash = 5005;
    session.upsert_channel(chan, PeerUpdateFlags::empty());

    // Helper to issue upload.getFile wrapped in `retry_once_on_file_reference_error`.
    async fn fetch_with_refresh(
        rpc: &MtprotoRpc,
        session: &Session,
        origin: &ApiFileOrigin,
        loc: &mut tl_enums::InputFileLocation,
    ) -> Result<Vec<u8>, tg_api::ApiError> {
        retry_once_on_file_reference_error(rpc, session, origin, loc, |cur_loc| {
            let req = tl_fns::upload::GetFile {
                precise: false,
                cdn_supported: false,
                location: cur_loc.clone(),
                offset: 0,
                limit: 4096,
            };
            async move {
                let tl_enums::upload::File::File(f) = rpc.invoke(&req).await? else {
                    panic!("expected direct file");
                };
                Ok(f.bytes)
            }
        })
        .await
    }

    let cases: Vec<(ApiFileOrigin, tl_enums::InputFileLocation, &[u8])> = vec![
        (
            ApiFileOrigin::Message {
                peer_id: PeerId::from_user(UserId(42)),
                msg_id: 50,
            },
            tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: 3001,
                    access_hash: 1,
                    file_reference: b"stale-ref".to_vec(),
                    thumb_size: String::new(),
                },
            ),
            b"ok-3001",
        ),
        (
            ApiFileOrigin::Message {
                peer_id: PeerId::from_channel(ChannelId(500)),
                msg_id: 77,
            },
            tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: 3002,
                    access_hash: 2,
                    file_reference: b"stale-ref".to_vec(),
                    thumb_size: String::new(),
                },
            ),
            b"ok-3002",
        ),
        (
            ApiFileOrigin::UserPhoto {
                user_id: UserId(42),
                photo_id: 3003,
            },
            tl_enums::InputFileLocation::InputPhotoFileLocation(tl_types::InputPhotoFileLocation {
                id: 3003,
                access_hash: 3,
                file_reference: b"stale-ref".to_vec(),
                thumb_size: "x".to_owned(),
            }),
            b"ok-3003",
        ),
        (
            ApiFileOrigin::StickerSet {
                set_id: 888,
                access_hash: 999,
            },
            tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: 3004,
                    access_hash: 4,
                    file_reference: b"stale-ref".to_vec(),
                    thumb_size: String::new(),
                },
            ),
            b"ok-3004",
        ),
        (
            ApiFileOrigin::Wallpaper {
                paper_id: 3005,
                access_hash: 55,
                owner_id: None,
                slug: String::new(),
            },
            tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: 3005,
                    access_hash: 5,
                    file_reference: b"stale-ref".to_vec(),
                    thumb_size: String::new(),
                },
            ),
            b"ok-3005",
        ),
        (
            ApiFileOrigin::Wallpaper {
                paper_id: 3006,
                access_hash: 55,
                owner_id: Some(UserId(42)), // non-self owner + slug -> inputWallPaperSlug!
                slug: "wp-slug".to_owned(),
            },
            tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: 3006,
                    access_hash: 5,
                    file_reference: b"stale-ref".to_vec(),
                    thumb_size: String::new(),
                },
            ),
            b"ok-3006",
        ),
        (
            ApiFileOrigin::SavedGifs,
            tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: 3007,
                    access_hash: 7,
                    file_reference: b"stale-ref".to_vec(),
                    thumb_size: String::new(),
                },
            ),
            b"ok-3007",
        ),
        (
            ApiFileOrigin::Ringtones,
            tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: 3008,
                    access_hash: 8,
                    file_reference: b"stale-ref".to_vec(),
                    thumb_size: String::new(),
                },
            ),
            b"ok-3008",
        ),
    ];

    for (origin, mut loc, expected) in cases {
        let bytes = tokio::time::timeout(
            TEST_TIMEOUT,
            fetch_with_refresh(&rpc, &session, &origin, &mut loc),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(bytes, expected);
    }

    // Retry-ONCE cap: if refreshed reference STILL expires, fail after exactly 2 total getFile calls (1 initial + 1 retry).
    let mut loc_still_bad = tl_enums::InputFileLocation::InputDocumentFileLocation(
        tl_types::InputDocumentFileLocation {
            id: 9001,
            access_hash: 1,
            file_reference: b"stale-ref".to_vec(),
            thumb_size: String::new(),
        },
    );
    let err1 = fetch_with_refresh(
        &rpc,
        &session,
        &ApiFileOrigin::Message {
            peer_id: PeerId::from_user(UserId(42)),
            msg_id: 90,
        },
        &mut loc_still_bad,
    )
    .await
    .expect_err("must fail after 1 retry when ref stays expired");
    assert!(err1.to_string().contains("FILE_REFERENCE_EXPIRED"));
    assert_eq!(
        get_file_attempts.lock().unwrap().get(&9001).copied(),
        Some(2)
    );

    // Unchanged reference: if origin returns the SAME stale ref, do NOT retry upload.getFile at all!
    let mut loc_unchanged = tl_enums::InputFileLocation::InputDocumentFileLocation(
        tl_types::InputDocumentFileLocation {
            id: 9002,
            access_hash: 1,
            file_reference: b"stale-ref".to_vec(),
            thumb_size: String::new(),
        },
    );
    let err2 = fetch_with_refresh(
        &rpc,
        &session,
        &ApiFileOrigin::Message {
            peer_id: PeerId::from_user(UserId(42)),
            msg_id: 91,
        },
        &mut loc_unchanged,
    )
    .await
    .expect_err("must fail without retry when ref did not change");
    assert!(err2.to_string().contains("FILE_REFERENCE_EXPIRED"));
    assert_eq!(
        get_file_attempts.lock().unwrap().get(&9002).copied(),
        Some(1)
    );

    client.shutdown();
}

// ============================================================================
// 4. Migration Conformance: FILE_MIGRATE_X uses per-DC download connection
//    with auth.exportAuthorization / auth.importAuthorization FIRST, never
//    switches main_dc_id, and MediaCluster PFS first-key uses positive proto_dc
// ============================================================================

#[tokio::test]
async fn conformance_4_file_migrate_exports_auth_first_and_never_switches_main_dc() {
    let authed_dcs: Arc<Mutex<HashSet<DcId>>> = Arc::new(Mutex::new(HashSet::from([2])));
    let authed_srv = Arc::clone(&authed_dcs);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::auth::ExportAuthorization::CONSTRUCTOR_ID => {
            assert_eq!(
                rpc.dc_id, 2,
                "auth.exportAuthorization must go to main DC 2"
            );
            let req = tl_fns::auth::ExportAuthorization::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(
                &tl_enums::auth::ExportedAuthorization::ExportedAuthorization(
                    tl_types::auth::ExportedAuthorization {
                        id: 55_000 + i64::from(req.dc_id),
                        bytes: vec![0xCA, 0xFE, req.dc_id as u8],
                    },
                ),
            )
        }
        x if x == tl_fns::auth::ImportAuthorization::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::ImportAuthorization::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.id, 55_000 + i64::from(rpc.dc_id));
            assert_eq!(req.bytes, vec![0xCA, 0xFE, rpc.dc_id as u8]);
            authed_srv.lock().unwrap().insert(rpc.dc_id);
            ServerAction::ok(&tl_authorization(7, "Migrator"))
        }
        x if x == tl_fns::upload::GetFile::CONSTRUCTOR_ID => {
            if rpc.dc_id == 2 {
                return ServerAction::err(303, "FILE_MIGRATE_4");
            }
            assert_eq!(rpc.dc_id, 4);
            assert!(
                authed_srv.lock().unwrap().contains(&4),
                "auth.importAuthorization MUST complete on DC 4 BEFORE migrated upload.getFile is sent!"
            );
            ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FilePng,
                mtime: 1_700_000_000,
                bytes: b"migrated-from-dc4".to_vec(),
            }))
        }
        other => panic!("unexpected RPC ctor 0x{other:08x} on dc {}", rpc.dc_id),
    });

    // Enable PFS AND register a media_only endpoint on DC 4 so we simultaneously test
    // MediaCluster first-key creation (`session_private.cpp:2642-2644`)!
    let cfg = test_client_config(2, true);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    client
        .dc_options()
        .add_endpoint(tg_mtproto::dc_options::DcEndpoint {
            id: 4,
            flags: DcOptionFlags(DcOptionFlags::MEDIA_ONLY),
            ip: "127.0.0.4".to_owned(),
            port: 443,
            secret: Vec::new(),
        });
    assert_eq!(
        client.dc_options().dc_type(download_dc_id(4, 0)),
        DcType::MediaCluster
    );

    let req = tl_fns::upload::GetFile {
        precise: false,
        cdn_supported: false,
        location: tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: 4001,
                access_hash: 4001,
                file_reference: vec![1],
                thumb_size: String::new(),
            },
        ),
        offset: 0,
        limit: 4096,
    };

    let tl_enums::upload::File::File(got) = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&req))
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected direct file");
    };
    assert_eq!(got.bytes, b"migrated-from-dc4");
    assert_eq!(
        client.main_dc_id(),
        2,
        "FILE_MIGRATE_4 must NEVER switch main_dc_id"
    );

    // Verify exact wire order:
    //   1) upload.getFile on DC 2 (-> 303 FILE_MIGRATE_4)
    //   2) auth.exportAuthorization(4) on main DC 2
    //   3) auth.importAuthorization on DC 4 download connection
    //   4) upload.getFile on DC 4 download connection (ZERO wasted 401 on DC 4!)
    let rpcs = server.rpcs();
    let idx_gf2 = rpcs
        .iter()
        .position(|r| r.dc_id == 2 && r.ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID)
        .unwrap();
    let idx_exp = rpcs
        .iter()
        .position(|r| r.dc_id == 2 && r.ctor == tl_fns::auth::ExportAuthorization::CONSTRUCTOR_ID)
        .unwrap();
    let idx_imp = rpcs
        .iter()
        .position(|r| r.dc_id == 4 && r.ctor == tl_fns::auth::ImportAuthorization::CONSTRUCTOR_ID)
        .unwrap();
    let idx_gf4 = rpcs
        .iter()
        .position(|r| r.dc_id == 4 && r.ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID)
        .unwrap();
    assert!(idx_gf2 < idx_exp);
    assert!(idx_exp < idx_imp);
    assert!(idx_imp < idx_gf4);

    // Verify MediaCluster first-key creation on DC 4 used positive Regular inner_dc = 4!
    let dc4_dhs: Vec<_> = server
        .dh_records()
        .into_iter()
        .filter(|d| d.dc_id == 4)
        .collect();
    assert!(!dc4_dhs.is_empty());
    assert_eq!(
        dc4_dhs[0].inner_dc, 4,
        "first persistent/regular key on MediaCluster DC 4 must use Regular inner_dc = +4"
    );

    client.shutdown();
}

// ============================================================================
// 5. Upload Conformance: saveFilePart (<= 10 MiB) vs saveBigFilePart (> 10 MiB),
//    adaptive part sizing, part count limits, random file_id, MD5 small-only,
//    failed part retry, FILE_PART_X_MISSING, parallel upload sessions
// ============================================================================

#[tokio::test]
async fn conformance_5_upload_small_vs_big_sizing_retries_missing_part_and_parallel() {
    // 5A. Threshold, adaptive part sizing, part count limits (standard & premium), and random file_id.
    assert_eq!(BIG_FILE_THRESHOLD, 10 * 1024 * 1024);
    assert_eq!(
        choose_upload_part_size(512 * 1024, false).unwrap(),
        (UPLOAD_PART_SIZE_32K, 16)
    );
    assert_eq!(
        choose_upload_part_size(10 * 1024 * 1024, false).unwrap(),
        (UPLOAD_PART_SIZE_64K, 160)
    );
    assert_eq!(
        choose_upload_part_size(100 * 1024 * 1024, false).unwrap(),
        (UPLOAD_PART_SIZE_128K, 800)
    );
    assert_eq!(
        choose_upload_part_size(600 * 1024 * 1024, false).unwrap(),
        (UPLOAD_PART_SIZE_256K, 2400)
    );
    assert_eq!(
        choose_upload_part_size(1500 * 1024 * 1024, false).unwrap(),
        (UPLOAD_PART_SIZE_512K, 3000)
    );
    assert_eq!(
        choose_upload_part_size(MAX_FILE_SIZE_DEFAULT, false).unwrap(),
        (UPLOAD_PART_SIZE_512K, MAX_UPLOAD_PARTS_DEFAULT)
    );
    assert!(choose_upload_part_size(MAX_FILE_SIZE_DEFAULT + 1, false).is_err());
    assert_eq!(
        choose_upload_part_size(MAX_FILE_SIZE_PREMIUM, true).unwrap(),
        (UPLOAD_PART_SIZE_512K, MAX_UPLOAD_PARTS_PREMIUM)
    );
    assert!(choose_upload_part_size(MAX_FILE_SIZE_PREMIUM + 1, true).is_err());

    // Exact 10 MiB boundary -> small (saveFilePart); 10 MiB + 1 -> big (saveBigFilePart).
    let plan_10m = UploadPlan::new("ten.bin", BIG_FILE_THRESHOLD, UploadConfig::default()).unwrap();
    let plan_10m_plus1 = UploadPlan::new(
        "ten_plus.bin",
        BIG_FILE_THRESHOLD + 1,
        UploadConfig::default(),
    )
    .unwrap();
    assert!(!plan_10m.is_big);
    assert!(plan_10m_plus1.is_big);
    assert_ne!(plan_10m.file_id, 0);
    assert_ne!(plan_10m_plus1.file_id, 0);
    assert_ne!(plan_10m.file_id, plan_10m_plus1.file_id);

    // UploadSessionBalancer dynamic scale-up and scale-down (`file_upload.cpp:690-1098`).
    let mut ubal = UploadSessionBalancer::new(8);
    let empty_used = BTreeSet::new();
    let s0 = ubal.choose_session(512 * 1024, &empty_used, 1000).unwrap();
    let q0 = ubal.on_part_sent(s0, 512 * 1024);
    assert_eq!(
        ubal.on_part_finished(
            s0,
            512 * 1024,
            q0,
            1001,
            1001 + FAST_REQUEST_THRESHOLD_MS - 1
        ),
        None
    );
    assert!(ubal.can_add_session());
    ubal.on_part_sent(s0, 512 * 1024);
    let s1 = ubal.choose_session(512 * 1024, &empty_used, 2000).unwrap();
    assert_eq!(s1, 1);
    assert_eq!(ubal.session_count(), 2);
    assert_eq!(
        ubal.on_part_finished(s1, 512 * 1024, 0, 2000, 2000 + SLOW_REQUEST_THRESHOLD_MS),
        Some(1)
    );
    assert_eq!(ubal.session_count(), 1);

    // 5B. End-to-end MTProto 2.0 uploads over shifted upload connections (`upload_dc_id(0..N)`):
    //     - Small upload: saveFilePart, parallel sessions (gated concurrency check), part 1
    //       rejected once with boolFalse (retried!), exact MD5 checksum.
    //     - Big upload: saveBigFilePart with file_total_parts, NO MD5 checksum, plus
    //       FILE_PART_2_MISSING on finalize -> upload_single_part(2) -> finalize succeeds!
    #[derive(Default)]
    struct UploadStore {
        small_parts: HashMap<(i64, i32), Vec<u8>>,
        big_parts: HashMap<(i64, i32), (i32, Vec<u8>)>,
        conns_used: HashSet<u64>,
        reject_small_part1_once: bool,
        missing_big_part2_once: bool,
    }
    let store = Arc::new(Mutex::new(UploadStore {
        reject_small_part1_once: true,
        missing_big_part2_once: true,
        ..UploadStore::default()
    }));
    let part0_seen = Arc::new(AtomicUsize::new(0));
    let part1_arrived = Arc::new(Notify::new());

    let st_srv = Arc::clone(&store);
    let p0_seen = Arc::clone(&part0_seen);
    let p1_notify = Arc::clone(&part1_arrived);

    let server = ScriptedServer::new_async(move |_srv, rpc| {
        let st_srv = Arc::clone(&st_srv);
        let p0_seen = Arc::clone(&p0_seen);
        let p1_notify = Arc::clone(&p1_notify);
        Box::pin(async move {
            match rpc.ctor {
                x if x == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID => {
                    let req = tl_fns::upload::SaveFilePart::from_bytes(&rpc.body).unwrap();
                    st_srv.lock().unwrap().conns_used.insert(rpc.conn_id);
                    // Prove true parallel upload across shifted upload sessions!
                    if req.file_part == 0 {
                        p0_seen.fetch_add(1, Ordering::SeqCst);
                        p1_notify.notified().await;
                    } else if req.file_part == 1 {
                        while p0_seen.load(Ordering::SeqCst) == 0 {
                            tokio::task::yield_now().await;
                        }
                        p1_notify.notify_waiters();
                        let mut guard = st_srv.lock().unwrap();
                        if guard.reject_small_part1_once {
                            guard.reject_small_part1_once = false;
                            // Return boolFalse#bc799737 once to verify part retry!
                            return ServerAction::Reply(0xbc79_9737u32.to_le_bytes().to_vec());
                        }
                    }
                    st_srv
                        .lock()
                        .unwrap()
                        .small_parts
                        .insert((req.file_id, req.file_part), req.bytes);
                    ServerAction::Reply(BOOL_TRUE_ID.to_le_bytes().to_vec())
                }
                x if x == tl_fns::upload::SaveBigFilePart::CONSTRUCTOR_ID => {
                    let req = tl_fns::upload::SaveBigFilePart::from_bytes(&rpc.body).unwrap();
                    assert!(req.file_total_parts > 0);
                    st_srv.lock().unwrap().big_parts.insert(
                        (req.file_id, req.file_part),
                        (req.file_total_parts, req.bytes),
                    );
                    ServerAction::Reply(BOOL_TRUE_ID.to_le_bytes().to_vec())
                }
                x if x == tl_fns::messages::UploadMedia::CONSTRUCTOR_ID => {
                    let mut guard = st_srv.lock().unwrap();
                    if guard.missing_big_part2_once {
                        guard.missing_big_part2_once = false;
                        return ServerAction::err(400, "FILE_PART_2_MISSING");
                    }
                    ServerAction::ok(&tl_enums::MessageMedia::Empty)
                }
                other => panic!("unexpected upload RPC ctor 0x{other:08x}"),
            }
        })
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());
    let invoker = MtprotoMediaInvoker::new(rpc.clone(), Session::new(UserId(1)));
    let cancel = CancelToken::new();

    // Small file upload (4 parts of 32 KiB = 128 KiB, parallel over upload_dc_id(0..4)).
    let small_payload = deterministic_bytes(110 * 1024, 21);
    let small_res = tokio::time::timeout(
        TEST_TIMEOUT,
        upload_bytes(
            &invoker,
            "small.bin",
            &small_payload,
            UploadConfig {
                max_sessions: 4,
                ..UploadConfig::default()
            },
            &cancel,
            |_| {},
        ),
    )
    .await
    .expect("parallel small upload must not deadlock")
    .expect("small upload ok");

    let small_id = match small_res {
        UploadedInputFile::Small {
            id,
            parts,
            name,
            md5_checksum,
        } => {
            assert_ne!(id, 0);
            assert_eq!(parts, 4);
            assert_eq!(name, "small.bin");
            assert_eq!(md5_checksum, compute_md5_hex(&small_payload));
            id
        }
        UploadedInputFile::Big { .. } => panic!("expected Small InputFile"),
    };
    {
        let st = store.lock().unwrap();
        assert!(
            st.conns_used.len() >= 2,
            "parallel upload must use multiple shifted upload sessions"
        );
        let mut assembled = Vec::new();
        for p in 0..4i32 {
            assembled.extend_from_slice(st.small_parts.get(&(small_id, p)).expect("part saved"));
        }
        assert_eq!(assembled, small_payload);
    }

    // Big file upload (> custom 64 KiB threshold to keep test fast while exercising saveBigFilePart + file_total_parts).
    let big_payload = deterministic_bytes(150 * 1024, 33);
    let big_cfg = UploadConfig {
        big_file_threshold: 64 * 1024,
        max_sessions: 3,
        ..UploadConfig::default()
    };
    let big_res = tokio::time::timeout(
        TEST_TIMEOUT,
        upload_bytes(&invoker, "big.bin", &big_payload, big_cfg, &cancel, |_| {}),
    )
    .await
    .unwrap()
    .unwrap();
    let (big_id, big_parts) = match big_res {
        UploadedInputFile::Big { id, parts, name } => {
            assert_ne!(id, 0);
            assert_ne!(id, small_id);
            assert_eq!(parts, 5);
            assert_eq!(name, "big.bin");
            (id, parts)
        }
        UploadedInputFile::Small { .. } => panic!("expected Big InputFile without MD5"),
    };
    {
        let st = store.lock().unwrap();
        let mut assembled = Vec::new();
        for p in 0..big_parts as i32 {
            let (total, bytes) = st.big_parts.get(&(big_id, p)).expect("big part saved");
            assert_eq!(*total, big_parts as i32);
            assembled.extend_from_slice(bytes);
        }
        assert_eq!(assembled, big_payload);
    }

    // FILE_PART_X_MISSING on finalize (`messages.uploadMedia`): parse part index, re-upload via
    // `upload_single_part`, and retry finalize!
    let upload_media_req = tl_fns::messages::UploadMedia {
        business_connection_id: None,
        peer: tl_enums::InputPeer::Self_,
        media: tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
            nosound_video: false,
            force_file: true,
            spoiler: false,
            file: tl_enums::InputFile::Big(tl_types::InputFileBig {
                id: big_id,
                parts: big_parts as i32,
                name: "big.bin".to_owned(),
            }),
            thumb: None,
            mime_type: "application/octet-stream".to_owned(),
            attributes: vec![],
            stickers: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        }),
    };
    let first_err = rpc
        .invoke(&upload_media_req)
        .await
        .expect_err("first finalize must report FILE_PART_2_MISSING");
    let missing_idx = match &first_err {
        ApiInvocationError::Rpc(r) => {
            assert_eq!(r.missing_file_part(), Some(2));
            parse_missing_file_part(&r.name).expect("parsed missing part")
        }
        other => panic!("unexpected error: {other:?}"),
    };
    assert_eq!(missing_idx, 2);
    let plan =
        UploadPlan::with_file_id(big_id, "big.bin", big_payload.len() as u64, big_cfg).unwrap();
    upload_single_part(&invoker, &plan, &big_payload, missing_idx, 0, &cancel)
        .await
        .expect("re-upload missing part 2");
    rpc.invoke(&upload_media_req)
        .await
        .expect("finalize succeeds after re-uploading missing part 2");

    client.shutdown();
}

// ============================================================================
// 6. Error Handling Conformance: FLOOD_WAIT on file DCs, FILE_ID_INVALID,
//    LIMIT_INVALID, OFFSET_INVALID, and 500 exponential backoff retry
// ============================================================================

#[tokio::test(start_paused = true)]
async fn conformance_6_file_dc_errors_flood_wait_invalid_params_and_500_backoff() {
    let flood_once = Arc::new(AtomicUsize::new(1));
    let prem_flood_once = Arc::new(AtomicUsize::new(1));
    let file_id_invalid_dc4_count = Arc::new(AtomicUsize::new(0));
    let err_500_timestamps: Arc<Mutex<Vec<tokio::time::Instant>>> =
        Arc::new(Mutex::new(Vec::new()));
    let export_count = Arc::new(AtomicUsize::new(0));

    let fl = Arc::clone(&flood_once);
    let pfl = Arc::clone(&prem_flood_once);
    let fid4 = Arc::clone(&file_id_invalid_dc4_count);
    let ts500 = Arc::clone(&err_500_timestamps);
    let exp_ctr = Arc::clone(&export_count);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::auth::ExportAuthorization::CONSTRUCTOR_ID => {
            exp_ctr.fetch_add(1, Ordering::SeqCst);
            let req = tl_fns::auth::ExportAuthorization::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(
                &tl_enums::auth::ExportedAuthorization::ExportedAuthorization(
                    tl_types::auth::ExportedAuthorization {
                        id: 88_000 + i64::from(req.dc_id),
                        bytes: vec![0xAA, req.dc_id as u8],
                    },
                ),
            )
        }
        x if x == tl_fns::auth::ImportAuthorization::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_authorization(1, "ErrTest"))
        }
        x if x == tl_fns::upload::GetFile::CONSTRUCTOR_ID => {
            let req = tl_fns::upload::GetFile::from_bytes(&rpc.body).unwrap();
            let doc_id = match req.location {
                tl_enums::InputFileLocation::InputDocumentFileLocation(d) => d.id,
                _ => 0,
            };
            match doc_id {
                6001 => {
                    if fl.swap(0, Ordering::SeqCst) == 1 {
                        return ServerAction::err(420, "FLOOD_WAIT_2");
                    }
                    ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                        r#type: tl_enums::storage::FileType::FilePartial,
                        mtime: 1_700_000_000,
                        bytes: b"after-flood".to_vec(),
                    }))
                }
                6002 => {
                    if pfl.swap(0, Ordering::SeqCst) == 1 {
                        return ServerAction::err(420, "FLOOD_PREMIUM_WAIT_1");
                    }
                    ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                        r#type: tl_enums::storage::FileType::FilePartial,
                        mtime: 1_700_000_000,
                        bytes: b"after-prem-flood".to_vec(),
                    }))
                }
                6003 => {
                    // Guest DC 4: first 400 FILE_ID_INVALID triggers auth export+import and retries once;
                    // second 400 FILE_ID_INVALID returns the error cleanly without looping.
                    fid4.fetch_add(1, Ordering::SeqCst);
                    ServerAction::err(400, "FILE_ID_INVALID")
                }
                6004 => ServerAction::err(400, "LIMIT_INVALID"),
                6005 => ServerAction::err(400, "OFFSET_INVALID"),
                6006 => {
                    let mut times = ts500.lock().unwrap();
                    times.push(tokio::time::Instant::now());
                    if times.len() <= 3 {
                        // Fail 3 times -> backoffs 1s, 2s, 4s (total >= 7s virtual time).
                        return ServerAction::err(500, "INTERNAL_SERVER_ERROR");
                    }
                    ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                        r#type: tl_enums::storage::FileType::FilePartial,
                        mtime: 1_700_000_000,
                        bytes: b"after-500-backoff".to_vec(),
                    }))
                }
                _ => ServerAction::err(400, "FILE_ID_INVALID"),
            }
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let mut non_prem_rx = client.subscribe_non_premium_delays();

    let make_get_file = |id: i64| tl_fns::upload::GetFile {
        precise: false,
        cdn_supported: false,
        location: tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id,
                access_hash: id,
                file_reference: vec![1],
                thumb_size: String::new(),
            },
        ),
        offset: 0,
        limit: 4096,
    };

    // 6A. FLOOD_WAIT_2 on download DC 4 sleeps 2s+10ms (virtual time) and retries automatically.
    let t0 = tokio::time::Instant::now();
    let tl_enums::upload::File::File(f1) = client
        .invoke_in_dc(download_dc_id(4, 0), &make_get_file(6001))
        .await
        .unwrap()
    else {
        panic!("expected file");
    };
    assert_eq!(f1.bytes, b"after-flood");
    assert!(
        t0.elapsed() >= Duration::from_secs(2),
        "FLOOD_WAIT_2 must wait at least 2s before retry, elapsed={:?}",
        t0.elapsed()
    );

    // 6B. FLOOD_PREMIUM_WAIT_1 on download DC 4 sleeps 1s+10ms, notifies non-premium delay, retries.
    let tl_enums::upload::File::File(f2) = client
        .invoke_in_dc(download_dc_id(4, 0), &make_get_file(6002))
        .await
        .unwrap()
    else {
        panic!("expected file");
    };
    assert_eq!(f2.bytes, b"after-prem-flood");
    assert_eq!(non_prem_rx.try_recv().ok(), Some(1));

    // 6C. 400 FILE_ID_INVALID on guest DC 4 triggers 1 export/import retry, then fails cleanly.
    let err_fid_guest = client
        .invoke_in_dc(download_dc_id(4, 0), &make_get_file(6003))
        .await
        .expect_err("persistent FILE_ID_INVALID on guest DC must fail after 1 retry");
    assert!(err_fid_guest.to_string().contains("FILE_ID_INVALID"));
    assert_eq!(file_id_invalid_dc4_count.load(Ordering::SeqCst), 2);
    assert_eq!(export_count.load(Ordering::SeqCst), 1);

    // 400 FILE_ID_INVALID on main DC 2 must fail immediately with 0 extra auth exports.
    let err_fid_main = client
        .invoke(&make_get_file(6003))
        .await
        .expect_err("FILE_ID_INVALID on main DC must fail immediately");
    assert!(err_fid_main.to_string().contains("FILE_ID_INVALID"));
    assert_eq!(export_count.load(Ordering::SeqCst), 1);

    // 6D. 400 LIMIT_INVALID and 400 OFFSET_INVALID fail fast without retry.
    let err_lim = client
        .invoke_in_dc(download_dc_id(4, 0), &make_get_file(6004))
        .await
        .expect_err("LIMIT_INVALID must fail fast");
    assert!(err_lim.to_string().contains("LIMIT_INVALID"));
    let err_off = client
        .invoke_in_dc(download_dc_id(4, 0), &make_get_file(6005))
        .await
        .expect_err("OFFSET_INVALID must fail fast");
    assert!(err_off.to_string().contains("OFFSET_INVALID"));

    // 6E. 500 INTERNAL_SERVER_ERROR retries with exponential backoff (1s -> 2s -> 4s).
    let tl_enums::upload::File::File(f500) = client
        .invoke_in_dc(download_dc_id(4, 0), &make_get_file(6006))
        .await
        .unwrap()
    else {
        panic!("expected file after 500 backoff");
    };
    assert_eq!(f500.bytes, b"after-500-backoff");
    let times = err_500_timestamps.lock().unwrap().clone();
    assert_eq!(times.len(), 4, "3 failures + 1 final success = 4 attempts");
    let d1 = times[1].duration_since(times[0]);
    let d2 = times[2].duration_since(times[1]);
    let d3 = times[3].duration_since(times[2]);
    assert!(
        d1 >= Duration::from_millis(1_000),
        "1st backoff >= 1s, got {d1:?}"
    );
    assert!(
        d2 >= Duration::from_millis(2_000),
        "2nd backoff >= 2s, got {d2:?}"
    );
    assert!(
        d3 >= Duration::from_millis(4_000),
        "3rd backoff >= 4s, got {d3:?}"
    );
    assert!(
        d2 > d1 && d3 > d2,
        "500 backoff must double: {d1:?} < {d2:?} < {d3:?}"
    );

    // 6F. handle_flood_errors=true returns FLOOD_WAIT directly without sleeping.
    flood_once.store(1, Ordering::SeqCst);
    let err_flood_direct = client
        .invoke_with_options(
            &make_get_file(6001),
            InvokeOptions {
                dc_id: download_dc_id(4, 0),
                handle_flood_errors: true,
                ..InvokeOptions::default()
            },
        )
        .await
        .expect_err("handle_flood_errors must return FLOOD_WAIT directly");
    assert!(err_flood_direct.to_string().contains("FLOOD_WAIT_2"));

    // Also verify PartAssembler handles duplicate / overlapping writes safely (regression test).
    let mut asm = PartAssembler::with_capacity(20);
    asm.write_part(10, b"0123456789").unwrap();
    asm.write_part(0, b"abcdefghij").unwrap();
    asm.write_part(0, b"abcdefghij").unwrap(); // duplicate write must not make skipped_bytes negative
    assert_eq!(asm.downloaded_bytes(), 20);
    assert_eq!(asm.finalize().unwrap(), b"abcdefghij0123456789");

    client.shutdown();
}
