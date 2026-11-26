//! Byte-level tdesktop conformance tests for login and session RPC flows (`tg-mtproto`,
//! `tg-api`, `tg-session`) against an in-process scripted fake Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses obviously fake credentials (`api_id = 1`, `api_hash = "0000...0000"`), reserved
//!   test phone numbers (`+999 66 XXXXX`), and an ephemeral test-only RSA keypair.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tg_api::{
    ApiCredentials, ApiError, LoginErrorKind, LoginStateMachine, LoginStep, RpcClient,
    UpdatesHandler,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{ChannelId, FullMsgId, MsgId, PeerId, Session, UserId};
use tg_mtproto::config_loader::ConfigLoaderOptions;
use tg_mtproto::dc_options::{
    DcId, DcOptionFlags, DcType, ShiftedDcId, download_dc_id, protocol_dc_id,
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
use tg_session::{
    LogoutReason, MtprotoRpc, SessionEvent, SessionRuntime, SessionRuntimeConfig,
    build_mtproto_client_config,
};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, LAYER, RawObject, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::mpsc;

// ============================================================================
// Test-only constants (fake credentials, ephemeral test RSA key, SRP vector)
// ============================================================================

const FAKE_API_ID: i32 = 1;
const FAKE_API_HASH: &str = "00000000000000000000000000000000";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Independently computed deterministic SRP-6a vector following `core/core_cloud_password.cpp`:
/// - `salt1 = b"tgfork_srp_salt1_conformance"`
/// - `salt2 = b"tgfork_srp_salt2_conformance"`
/// - `password = "test-2fa-password-layer233"`
/// - `g = 3`, `p = GOOD_PRIME`, server `b = [0x5A; 256]`, client `a = [0x3C; 256]`.
const SRP_SALT1: &[u8] = b"tgfork_srp_salt1_conformance";
const SRP_SALT2: &[u8] = b"tgfork_srp_salt2_conformance";
const SRP_PASSWORD: &str = "test-2fa-password-layer233";
const SRP_CLIENT_A_SECRET: [u8; 256] = [0x3C; 256];

const SRP_B_HEX: &str = "\
03f8d79fd4dbcf5b1686672624d2bf24356509e38a254abe6221af59868b6a3e\
1ed7faf6da80d6eeae52e5dce891a77af2d4c24795c3a17751d07d01fbaba7dd\
5b211ffcd6a8ef4412b529ff23b6b1de806751a05585899af71be6fd609ff7ea\
c3c135745269964bd8810c227ad90b14efbf1f1a21734aec655de86656e9c968\
73745ee78f763720a71bbec2720f37db709f77f5640780f77eaf26072f914a9a\
3c32932125361d9c03f423d09f2c8d01771d7dc44e7e17d71917391a37ee6160\
f37f79cc7697a4b4b899825606e2c2f38e536eb73091f15c343df8e4240ac995\
46ff7e2fb4abb076f45db95e3ed2e72589b470fcd7b875591f1e0b6bbae999e9";

const SRP_EXPECTED_A_HEX: &str = "\
500471a1e3a41cd8be6055a0b43a8e8eaba6003a6dd4717f8534237b13262442\
5bebf316a49664b3254ee9d98cd9fb4b83edd6391ca2e541a9e25261d4fbc2a4\
fe77182898a65e0830cdd209c6b0d5c49ef81f25368a6a527d42e8a87ff3ee44\
bcc5f2859ac5dc8e7e982ba70fb85547b0a63ada7ee2459ce1b464892b3c6614\
e31367f82fe8c36721170692a7279f42f726fe9ecb094dee8d40de272d5d8b9f\
24876ee72db629ea43c0bade48545e4b7003ec7a8607fa1d6c164a996ec692d6\
f747ef270b9e9c8a747c896e97925f41e4ebbd3889eccf9a2f3982347620af3b\
5b55657fc075f91bcf27ef5365b34125152d7dc1205f8229b4cd9841a81c89f0";

const SRP_EXPECTED_M1_HEX: &str =
    "e7d005d0f71a34adc1ac3fef4fb8a3e2f93f95858c83c5696adabe1006fc1c65";

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
struct ConnectionRecord {
    ip: String,
    protocol_dc_id: i16,
}

#[derive(Debug, Clone)]
struct InitConnectionRecord {
    dc_id: DcId,
    layer: i32,
    api_id: i32,
    device_model: String,
    system_version: String,
    app_version: String,
    system_lang_code: String,
    lang_pack: String,
    lang_code: String,
    proxy: Option<tl_types::InputClientProxy>,
    params: Option<tl_enums::JSONValue>,
}

#[derive(Debug, Clone)]
struct RpcRecord {
    conn_id: u64,
    dc_id: DcId,
    auth_key_id: u64,
    session_id: u64,
    wrapped_with_init: bool,
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct IncomingRpc {
    dc_id: DcId,
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
enum ServerAction {
    /// Reply with `rpc_result(req_msg_id, payload)`.
    Reply(Vec<u8>),
    /// Reply with `rpc_result(req_msg_id, rpc_error(code, message))`.
    RpcError(i32, String),
    /// Reply with `rpc_result` and then drop the transport connection to force a reconnect.
    ReplyAndDropConnection(Vec<u8>),
    /// Forget the active key on the server and reply with transport error `-404` (`AuthKeyNotFound`).
    ForgetKeyAndSend404,
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
    is_temp: bool,
    bound_perm_key_id: Option<u64>,
    rejected: bool,
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
    connections: Mutex<Vec<ConnectionRecord>>,
    inits: Mutex<Vec<InitConnectionRecord>>,
    rpcs: Mutex<Vec<RpcRecord>>,
    binds: Mutex<Vec<(u64, i64)>>,
    live_pushers: Mutex<HashMap<u64, LiveConnPush>>,
    handler: Mutex<RpcHandler>,
}

/// In-process byte-level MTProto 2.0 fake Telegram server implementing [`TransportFactory`].
///
/// Speaks real unencrypted DH (`req_pq_multi` / `req_DH_params` / `set_client_DH_params`),
/// real AES-IGE MTProto 2.0 encrypted packets (`Side::Client` / `Side::Server`),
/// `msg_container`, `gzip_packed`, `ping`/`pong`, `new_session_created`,
/// `auth.bindTempAuthKey`, `invokeWithLayer(initConnection(...))`, `rpc_result`/`rpc_error`,
/// server-pushed `Updates`, transport disconnects, and `-404` key-mismatch errors.
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
                connections: Mutex::new(Vec::new()),
                inits: Mutex::new(Vec::new()),
                rpcs: Mutex::new(Vec::new()),
                binds: Mutex::new(Vec::new()),
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

    fn register_cdn_rsa_key_on(&self, client: &Client, cdn_dc_id: DcId) {
        client
            .dc_options()
            .set_cdn_keys(&[(cdn_dc_id, self.inner.rsa_pub.clone())]);
    }

    fn connections(&self) -> Vec<ConnectionRecord> {
        self.inner.connections.lock().unwrap().clone()
    }

    fn inits(&self) -> Vec<InitConnectionRecord> {
        self.inner.inits.lock().unwrap().clone()
    }

    fn rpcs(&self) -> Vec<RpcRecord> {
        self.inner.rpcs.lock().unwrap().clone()
    }

    fn binds(&self) -> Vec<(u64, i64)> {
        self.inner.binds.lock().unwrap().clone()
    }

    fn forget_key(&self, key_id: u64) {
        if let Some(entry) = self.inner.keys.lock().unwrap().get_mut(&key_id) {
            entry.rejected = true;
        }
    }

    fn push_update_to_dc(&self, dc_id: DcId, updates: tl_enums::Updates) -> bool {
        let guard = self.inner.live_pushers.lock().unwrap();
        // Pick the newest live connection for `dc_id`.
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
        assert_eq!(data_with_hash.len(), 224);
        let mut data_with_padding = data_with_hash[0..192].to_vec();
        data_with_padding.reverse();
        let expected_hash = sha256(&[&temp_key, &data_with_padding]);
        assert_eq!(
            &data_with_hash[192..224],
            &expected_hash,
            "RSA_PAD SHA-256 integrity mismatch"
        );
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
                is_temp: bool,
            }

            let mut dh_pending: Option<DhPending> = None;
            let mut active_key: Option<(AuthKey, u64, u64)> = None; // (key, salt, session_id)
            let mut server_msg_counter: u64 = 1;
            let mut server_seq_counter: u32 = 0;
            let mut sent_new_session = false;

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
                            // Unencrypted MTProto DH handshake packet sent by client:
                            // auth_key_id(8) || msg_id(8, divisible by 4) || msg_len(4) || body.
                            assert!(raw.len() >= 20, "unencrypted DH packet too short");
                            let client_msg_id =
                                u64::from_le_bytes(raw[8..16].try_into().unwrap());
                            assert_eq!(
                                client_msg_id % 4,
                                0,
                                "client unencrypted msg_id must be divisible by 4"
                            );
                            let msg_len =
                                u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
                            assert_eq!(raw.len(), 20 + msg_len);
                            let body = &raw[20..20 + msg_len];
                            let ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());
                            if ctor == mtp_functions::ReqPqMulti::CONSTRUCTOR_ID {
                                let req = mtp_functions::ReqPqMulti::from_bytes(body)
                                    .expect("parse req_pq_multi");
                                let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                                let mut server_nonce = [0u8; 16];
                                server_nonce[0..8].copy_from_slice(&seq.to_le_bytes());
                                server_nonce[8..16].copy_from_slice(&0xA55A_C33C_1234_5678u64.to_le_bytes());
                                let res_pq = mtp_enums::ResPQ::ResPQ(mtp_types::ResPQ {
                                    nonce: req.nonce,
                                    server_nonce,
                                    // pq = 17 * 23 = 391 (0x0187)
                                    pq: vec![0x01, 0x87],
                                    server_public_key_fingerprints: vec![
                                        server.inner.rsa_pub.fingerprint() as i64,
                                    ],
                                });
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let pkt = encode_unencrypted_packet(out_id, &res_pq.to_bytes());
                                let _ = peer.tx.send(Ok(pkt));
                            } else if ctor == mtp_functions::ReqDHParams::CONSTRUCTOR_ID {
                                let req = mtp_functions::ReqDHParams::from_bytes(body)
                                    .expect("parse req_DH_params");
                                assert_eq!(req.p, vec![17]);
                                assert_eq!(req.q, vec![23]);
                                assert_eq!(
                                    req.public_key_fingerprint as u64,
                                    server.inner.rsa_pub.fingerprint()
                                );
                                let plain = server.decrypt_rsa_pad(&req.encrypted_data);
                                let mut plain_cur = Cursor::new(&plain);
                                let pq_inner = mtp_enums::PQInnerData::deserialize(&mut plain_cur)
                                    .expect("parse PQInnerData");
                                let (new_nonce, is_temp, inner_dc) = match pq_inner {
                                    mtp_enums::PQInnerData::Dc(d) => {
                                        assert_eq!(d.nonce, req.nonce);
                                        assert_eq!(d.server_nonce, req.server_nonce);
                                        (d.new_nonce, false, d.dc)
                                    }
                                    mtp_enums::PQInnerData::TempDc(d) => {
                                        assert_eq!(d.nonce, req.nonce);
                                        assert_eq!(d.server_nonce, req.server_nonce);
                                        assert!(d.expires_in > 0);
                                        (d.new_nonce, true, d.dc)
                                    }
                                    other => panic!("unexpected PQInnerData variant: {other:?}"),
                                };
                                assert_eq!(inner_dc, i32::from(protocol_dc_id));

                                let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                                let mut a_srv = [0x42u8; 256];
                                a_srv[0..8].copy_from_slice(&seq.to_be_bytes());
                                a_srv[248..256].copy_from_slice(&(seq ^ 0x55AA_55AA).to_be_bytes());
                                let g_a = pad_be_256(
                                    &mod_exp(&[3], &a_srv, &GOOD_PRIME).expect("g^a mod p"),
                                );

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
                                    is_temp,
                                });
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let pkt = encode_unencrypted_packet(out_id, &resp.to_bytes());
                                let _ = peer.tx.send(Ok(pkt));
                            } else if ctor == mtp_functions::SetClientDHParams::CONSTRUCTOR_ID {
                                let req = mtp_functions::SetClientDHParams::from_bytes(body)
                                    .expect("parse set_client_DH_params");
                                let pending = dh_pending.take().expect("DH state before set_client");
                                assert_eq!(req.nonce, pending.nonce);
                                assert_eq!(req.server_nonce, pending.server_nonce);
                                let dec = aes_ige_decrypt(
                                    &req.encrypted_data,
                                    &pending.aes_key,
                                    &pending.aes_iv,
                                );
                                let mut dec_cur = Cursor::new(&dec[20..]);
                                let client_dh =
                                    mtp_enums::ClientDHInnerData::deserialize(&mut dec_cur)
                                        .expect("parse client_DH_inner_data");
                                let mtp_enums::ClientDHInnerData::ClientDHInnerData(cdh) = client_dh;
                                assert_eq!(cdh.nonce, pending.nonce);
                                assert_eq!(cdh.server_nonce, pending.server_nonce);

                                let shared = pad_be_256(
                                    &mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME)
                                        .expect("g_b^a mod p"),
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
                                        is_temp: pending.is_temp,
                                        bound_perm_key_id: None,
                                        rejected: false,
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
                                let pkt = encode_unencrypted_packet(out_id, &ok.to_bytes());
                                let _ = peer.tx.send(Ok(pkt));
                            } else {
                                panic!("unexpected unencrypted constructor: 0x{ctor:08x}");
                            }
                            continue;
                        }

                        // Encrypted MTProto 2.0 packet.
                        let key_entry = {
                            server.inner.keys.lock().unwrap().get(&auth_key_id).cloned()
                        };
                        let Some(entry) = key_entry.filter(|e| !e.rejected) else {
                            let _ = peer.tx.send(Err(TransportError::ServerError(
                                TransportErrorCode::AuthKeyNotFound,
                            )));
                            break;
                        };
                        let dec = parse_encrypted_packet(&raw, &entry.key, Side::Client)
                            .expect("decrypt client MTProto 2.0 packet");
                        let session_id = dec.session_id;
                        active_key = Some((entry.key.clone(), entry.server_salt, session_id));

                        // Unpack msg_container or single message.
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

                        let mut drop_conn = false;
                        for (msg_id, raw_msg) in inner_msgs {
                            let mut budget = 1024 * 1024;
                            let body = if raw_msg.len() >= 4
                                && u32::from_le_bytes(raw_msg[0..4].try_into().unwrap())
                                    == GzipPacked::CONSTRUCTOR_ID
                            {
                                unwrap_gzip_layers(&raw_msg, &mut budget, 0)
                                    .expect("unpack gzip")
                            } else {
                                raw_msg
                            };
                            if body.len() < 4 {
                                continue;
                            }
                            let outer_ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());

                            // Service messages handled automatically.
                            if outer_ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                                let ping = mtp_functions::Ping::from_bytes(&body).unwrap();
                                if !sent_new_session {
                                    sent_new_session = true;
                                    let ns = mtp_enums::NewSession::Created(
                                        mtp_types::NewSessionCreated {
                                            first_msg_id: msg_id as i64,
                                            unique_id: conn_id as i64,
                                            server_salt: entry.server_salt as i64,
                                        },
                                    );
                                    let ns_id = next_server_msg_id(&mut server_msg_counter, false);
                                    send_encrypted(
                                        &peer,
                                        &entry.key,
                                        entry.server_salt,
                                        session_id,
                                        ns_id,
                                        true,
                                        &mut server_seq_counter,
                                        &ns.to_bytes(),
                                    );
                                }
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
                                if outer_ctor == mtp_functions::GetFutureSalts::CONSTRUCTOR_ID {
                                    let now = now_unix_secs();
                                    let salts = mtp_enums::FutureSalts::FutureSalts(
                                        mtp_types::FutureSalts {
                                            req_msg_id: msg_id as i64,
                                            now,
                                            salts: vec![mtp_types::FutureSalt {
                                                valid_since: now - 60,
                                                valid_until: now + 3600,
                                                salt: entry.server_salt as i64,
                                            }],
                                        },
                                    );
                                    let s_id = next_server_msg_id(&mut server_msg_counter, true);
                                    send_encrypted(
                                        &peer,
                                        &entry.key,
                                        entry.server_salt,
                                        session_id,
                                        s_id,
                                        false,
                                        &mut server_seq_counter,
                                        &salts.to_bytes(),
                                    );
                                }
                                continue;
                            }
                            if outer_ctor == tl_fns::auth::BindTempAuthKey::CONSTRUCTOR_ID {
                                let bind = tl_fns::auth::BindTempAuthKey::from_bytes(&body)
                                    .expect("parse auth.bindTempAuthKey");
                                {
                                    let mut keys = server.inner.keys.lock().unwrap();
                                    assert!(
                                        keys.contains_key(&(bind.perm_auth_key_id as u64)),
                                        "bindTempAuthKey references unknown perm_auth_key_id"
                                    );
                                    if let Some(temp_entry) = keys.get_mut(&auth_key_id) {
                                        assert!(temp_entry.is_temp);
                                        temp_entry.bound_perm_key_id =
                                            Some(bind.perm_auth_key_id as u64);
                                    }
                                }
                                server
                                    .inner
                                    .binds
                                    .lock()
                                    .unwrap()
                                    .push((auth_key_id, bind.perm_auth_key_id));
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

                            // Unwrap invokeWithLayer(initConnection(...)) if present and record all fields.
                            let (wrapped_with_init, inner_body) =
                                unwrap_and_record_init(&server, dc_id, &body);
                            let inner_ctor =
                                u32::from_le_bytes(inner_body[0..4].try_into().unwrap());

                            server.inner.rpcs.lock().unwrap().push(RpcRecord {
                                conn_id,
                                dc_id,
                                auth_key_id,
                                session_id,
                                wrapped_with_init,
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
                                ServerAction::ReplyAndDropConnection(payload) => {
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
                                    let _ = peer.tx.send(Err(TransportError::Disconnected));
                                    drop_conn = true;
                                    break;
                                }
                                ServerAction::ForgetKeyAndSend404 => {
                                    server.forget_key(auth_key_id);
                                    let _ = peer.tx.send(Err(TransportError::ServerError(
                                        TransportErrorCode::AuthKeyNotFound,
                                    )));
                                    drop_conn = true;
                                    break;
                                }
                            }
                        }
                        if drop_conn {
                            break;
                        }
                    }
                }
            }

            server.inner.live_pushers.lock().unwrap().remove(&conn_id);
        });
    }
}

fn unwrap_and_record_init(server: &ScriptedServer, dc_id: DcId, raw: &[u8]) -> (bool, Vec<u8>) {
    if raw.len() < 4 {
        return (false, raw.to_vec());
    }
    let ctor = u32::from_le_bytes(raw[0..4].try_into().unwrap());
    if ctor != tl_fns::InvokeWithLayer::<RawObject>::CONSTRUCTOR_ID {
        return (false, raw.to_vec());
    }
    let mut cur = Cursor::new(&raw[4..]);
    let layer = i32::deserialize(&mut cur).expect("invokeWithLayer.layer");
    let rest = &raw[raw.len() - cur.remaining()..];
    let inner_ctor = u32::from_le_bytes(rest[0..4].try_into().unwrap());
    assert_eq!(
        inner_ctor,
        tl_fns::InitConnection::<RawObject>::CONSTRUCTOR_ID,
        "invokeWithLayer must wrap initConnection"
    );
    let mut icur = Cursor::new(&rest[4..]);
    let flags = i32::deserialize(&mut icur).expect("initConnection.flags") as u32;
    let api_id = i32::deserialize(&mut icur).expect("initConnection.api_id");
    let device_model = String::deserialize(&mut icur).expect("initConnection.device_model");
    let system_version = String::deserialize(&mut icur).expect("initConnection.system_version");
    let app_version = String::deserialize(&mut icur).expect("initConnection.app_version");
    let system_lang_code = String::deserialize(&mut icur).expect("initConnection.system_lang_code");
    let lang_pack = String::deserialize(&mut icur).expect("initConnection.lang_pack");
    let lang_code = String::deserialize(&mut icur).expect("initConnection.lang_code");
    let proxy = if (flags & (1 << 0)) != 0 {
        let tl_enums::InputClientProxy::InputClientProxy(p) =
            tl_enums::InputClientProxy::deserialize(&mut icur).expect("initConnection.proxy");
        Some(p)
    } else {
        None
    };
    let params = if (flags & (1 << 1)) != 0 {
        Some(tl_enums::JSONValue::deserialize(&mut icur).expect("initConnection.params"))
    } else {
        None
    };
    let query_bytes = rest[rest.len() - icur.remaining()..].to_vec();

    server
        .inner
        .inits
        .lock()
        .unwrap()
        .push(InitConnectionRecord {
            dc_id,
            layer,
            api_id,
            device_model,
            system_version,
            app_version,
            system_lang_code,
            lang_pack,
            lang_code,
            proxy,
            params,
        });
    (true, query_bytes)
}

impl TransportFactory for ScriptedServer {
    fn connect<'a>(
        &'a self,
        ip: &'a str,
        _port: u16,
        protocol_dc_id: i16,
        _secret: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn PacketConnection>, TransportError>> + Send + 'a>>
    {
        let ip = ip.to_owned();
        Box::pin(async move {
            let conn_id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
            self.inner
                .connections
                .lock()
                .unwrap()
                .push(ConnectionRecord { ip, protocol_dc_id });
            let (conn, peer) = FakeConnection::pair();
            self.spawn_connection(conn_id, protocol_dc_id, peer);
            Ok(Box::new(conn) as Box<dyn PacketConnection>)
        })
    }
}

// ============================================================================
// TL fixture builders (real Layer 233 shapes)
// ============================================================================

fn fake_credentials() -> ApiCredentials {
    ApiCredentials {
        api_id: FAKE_API_ID,
        api_hash: FAKE_API_HASH.to_owned(),
    }
}

fn test_client_config(main_dc: DcId, use_pfs: bool) -> ClientConfig {
    let mut cfg = build_mtproto_client_config(
        &fake_credentials(),
        main_dc,
        tg_core::Environment::Production,
        None,
    );
    cfg.proxy = ProxyData::none();
    cfg.use_pfs = use_pfs;
    cfg.ping_interval = Duration::from_secs(600);
    cfg
}

fn tl_user(id: i64, first_name: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
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
        access_hash: Some(id * 100 + 1),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(format!("user_{id}")),
        phone: Some("9996621234".to_owned()),
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
    }
}

fn tl_channel(id: i64, title: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: false,
        left: false,
        broadcast: true,
        verified: false,
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
        stories_unavailable: true,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        forum_sidebar: false,
        in_news: false,
        id,
        access_hash: Some(id * 77),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(10),
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

fn tl_message(peer: tl_enums::Peer, id: i32, date: i32, text: &str) -> tl_enums::Message {
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
        from_id: Some(peer.clone()),
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

fn tl_authorization(user_id: i64, first_name: &str) -> tl_enums::auth::Authorization {
    tl_enums::auth::Authorization::Authorization(tl_types::auth::Authorization {
        setup_password_required: false,
        otherwise_relogin_days: None,
        tmp_sessions: None,
        future_auth_token: None,
        user: tl_enums::User::User(tl_user(user_id, first_name, true)),
    })
}

fn empty_dialog_filters() -> tl_enums::messages::DialogFilters {
    tl_enums::messages::DialogFilters::DialogFilters(tl_types::messages::DialogFilters {
        tags_enabled: false,
        filters: vec![],
    })
}

fn tl_config_with_options(this_dc: DcId, dc_options: Vec<tl_enums::DcOption>) -> tl_enums::Config {
    let now = now_unix_secs();
    tl_enums::Config::Config(tl_types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: false,
        revoke_pm_inbox: true,
        blocked_mode: false,
        force_try_ipv6: false,
        date: now,
        expires: now + 3600,
        test_mode: false,
        this_dc,
        dc_options,
        dc_txt_domain_name: "apv3.stel.com".to_owned(),
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
        stickers_recent_limit: 50,
        channels_read_media_period: 604_800,
        tmp_sessions: Some(0),
        call_receive_timeout_ms: 20_000,
        call_ring_timeout_ms: 90_000,
        call_connect_timeout_ms: 30_000,
        call_packet_timeout_ms: 10_000,
        me_url_prefix: "https://t.me/".to_owned(),
        autoupdate_url_prefix: None,
        gif_search_username: Some("gif".to_owned()),
        venue_search_username: None,
        img_search_username: None,
        static_maps_provider: None,
        caption_length_max: 1024,
        message_length_max: 4096,
        webfile_dc_id: 4,
        suggested_lang_code: Some("en".to_owned()),
        lang_pack_version: Some(1),
        base_lang_pack_version: Some(1),
        reactions_default: None,
        autologin_token: None,
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

// ============================================================================
// 1. First request wrapping: invokeWithLayer(233, initConnection(...))
// ============================================================================

#[tokio::test]
async fn conformance_1_init_connection_wrapping_reconnect_and_new_key() {
    let call_num = Arc::new(AtomicU64::new(0));
    let call_num_srv = Arc::clone(&call_num);

    let server = ScriptedServer::new(move |_srv, rpc| {
        let n = call_num_srv.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(rpc.ctor, tl_fns::help::GetNearestDc::CONSTRUCTOR_ID);
        let reply = tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
            country: format!("C{n}"),
            this_dc: 2,
            nearest_dc: 2,
        })
        .to_bytes();
        match n {
            // Call 1: succeeds on conn 1.
            1 => ServerAction::Reply(reply),
            // Call 2: succeeds on conn 1, then server drops transport to force reconnect.
            2 => ServerAction::ReplyAndDropConnection(reply),
            // Call 3: succeeds on conn 2 (reconnected, same auth key).
            3 => ServerAction::Reply(reply),
            // Call 4: succeeds on conn 2 (same connection, NOT wrapped).
            4 => ServerAction::Reply(reply),
            // Call 5: server forgets active key and sends -404 -> forces new DH auth key.
            5 => ServerAction::ForgetKeyAndSend404,
            // Call 6 (retry of call 5 on brand-new key): succeeds!
            6 => ServerAction::Reply(reply),
            // Call 7: second request on new key -> NOT wrapped.
            _ => ServerAction::Reply(reply),
        }
    });

    let proxy = ProxyData::mtproto("198.51.100.77", 8443, "abababababababababababababababab");
    let mut cfg = build_mtproto_client_config(
        &fake_credentials(),
        2,
        tg_core::Environment::Production,
        None,
    );
    cfg.proxy = proxy;
    cfg.device_model = "ConformanceBox".to_owned();
    cfg.system_version = "Linux 6.12".to_owned();
    cfg.app_version = "5.14.0".to_owned();
    cfg.system_lang_code = "en-US".to_owned();
    cfg.lang_code = "en".to_owned();
    // 25h + 500s -> wraps modulo 24h to +1h + 500s = 4100s -> rounds to 4500s (or 3600s).
    // 4100 / 900 = 4.555 -> round() = 5 -> 5 * 900 = 4500.0 (matching prepareInitParams).
    cfg.tz_offset_secs = Some(25 * 3600 + 500);
    cfg.use_pfs = true;
    cfg.ping_interval = Duration::from_secs(600);

    let (client, _updates_rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);

    // Request 1: wrapped with invokeWithLayer(233, initConnection(...)).
    let r1 = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        r1,
        tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
            country: "C1".to_owned(),
            this_dc: 2,
            nearest_dc: 2,
        })
    );

    // Request 2 (same connection): NOT wrapped; server drops connection right after reply.
    let _r2 = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
        .await
        .unwrap()
        .unwrap();

    // Give the runner a moment to observe the disconnect before sending request 3.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Request 3 (reconnected, same auth key): MUST re-send invokeWithLayer(initConnection).
    let _r3 = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
        .await
        .unwrap()
        .unwrap();

    // Request 4 (same reconnected connection): MUST NOT wrap again.
    let _r4 = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
        .await
        .unwrap()
        .unwrap();

    // Request 5: server sends -404 -> destroys temp key, negotiates NEW key via DH, retries!
    let _r5 = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
        .await
        .unwrap()
        .unwrap();

    // Request 6 (same connection on new key): MUST NOT wrap again.
    let _r6 = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
        .await
        .unwrap()
        .unwrap();

    // Verify PFS auth.bindTempAuthKey ran for both the initial temp key and replacement temp key.
    assert!(
        server.binds().len() >= 2,
        "expected PFS bindTempAuthKey for initial and replacement temp keys, got {:?}",
        server.binds()
    );

    // Verify exact wrap/unwrap pattern across all 7 wire RPCs.
    let rpcs = server.rpcs();
    assert_eq!(rpcs.len(), 7, "expected 7 wire RPCs, got {rpcs:?}");
    assert!(rpcs[0].wrapped_with_init, "RPC 1 (initial) must be wrapped");
    assert!(
        !rpcs[1].wrapped_with_init,
        "RPC 2 (same conn) must NOT be wrapped"
    );
    assert_eq!(rpcs[0].conn_id, rpcs[1].conn_id);

    assert_ne!(
        rpcs[2].conn_id, rpcs[1].conn_id,
        "RPC 3 must be on a new connection"
    );
    assert_eq!(
        rpcs[2].auth_key_id, rpcs[0].auth_key_id,
        "reconnect reuses auth key"
    );
    assert!(
        rpcs[2].wrapped_with_init,
        "RPC 3 (after reconnect) must re-send initConnection"
    );
    assert!(
        !rpcs[3].wrapped_with_init,
        "RPC 4 (same reconnected conn) must NOT be wrapped"
    );

    // RPC 5 triggered -404; RPC 6 is the retry on a brand-new auth key.
    assert_ne!(
        rpcs[5].auth_key_id, rpcs[2].auth_key_id,
        "after -404 a new auth key must be used"
    );
    assert_ne!(
        rpcs[5].session_id, rpcs[2].session_id,
        "new auth key must rotate session_id"
    );
    assert!(
        rpcs[5].wrapped_with_init,
        "RPC 6 (after new auth key) must re-send initConnection"
    );
    assert!(
        !rpcs[6].wrapped_with_init,
        "RPC 7 (same new-key conn) must NOT be wrapped"
    );

    // Inspect exact tdesktop initConnection fields on every recorded initConnection.
    let inits = server.inits();
    assert_eq!(inits.len(), 3, "expected exactly 3 initConnection wrappers");
    for init in &inits {
        assert_eq!(init.layer, LAYER);
        assert_eq!(init.layer, 233);
        assert_eq!(init.api_id, FAKE_API_ID);
        assert_eq!(init.device_model, "ConformanceBox");
        assert_eq!(init.system_version, "Linux 6.12");
        assert_eq!(init.app_version, "5.14.0");
        assert_eq!(init.system_lang_code, "en-US");
        assert_eq!(init.lang_pack, "tdesktop");
        assert_eq!(init.lang_code, "en");
        assert_eq!(
            init.proxy,
            Some(tl_types::InputClientProxy {
                address: "198.51.100.77".to_owned(),
                port: 8443,
            })
        );
        assert_eq!(
            init.params,
            Some(tl_enums::JSONValue::JsonObject(tl_types::JsonObject {
                value: vec![tl_enums::JSONObjectValue::JsonObjectValue(
                    tl_types::JsonObjectValue {
                        key: "tz_offset".to_owned(),
                        value: tl_enums::JSONValue::JsonNumber(tl_types::JsonNumber {
                            value: 4500.0,
                        }),
                    }
                )],
            }))
        );
    }

    client.shutdown();
}

// ============================================================================
// 2. help.getConfig / DC options parsing & selection (main, media, CDN, flags)
// ============================================================================

#[tokio::test]
async fn conformance_2_help_get_config_and_dc_options_selection() {
    let dc_opts = vec![
        // DC 2 regular IPv4 static
        tl_enums::DcOption::DcOption(tl_types::DcOption {
            ipv6: false,
            media_only: false,
            tcpo_only: false,
            cdn: false,
            r#static: true,
            this_port_only: false,
            id: 2,
            ip_address: "149.154.167.50".to_owned(),
            port: 443,
            secret: None,
        }),
        // DC 2 regular IPv6 static
        tl_enums::DcOption::DcOption(tl_types::DcOption {
            ipv6: true,
            media_only: false,
            tcpo_only: false,
            cdn: false,
            r#static: true,
            this_port_only: false,
            id: 2,
            ip_address: "2001:67c:4e8:f002::a".to_owned(),
            port: 443,
            secret: None,
        }),
        // DC 2 media_only IPv4
        tl_enums::DcOption::DcOption(tl_types::DcOption {
            ipv6: false,
            media_only: true,
            tcpo_only: false,
            cdn: false,
            r#static: false,
            this_port_only: false,
            id: 2,
            ip_address: "149.154.167.222".to_owned(),
            port: 443,
            secret: None,
        }),
        // DC 4 tcpo_only IPv4 with secret
        tl_enums::DcOption::DcOption(tl_types::DcOption {
            ipv6: false,
            media_only: false,
            tcpo_only: true,
            cdn: false,
            r#static: false,
            this_port_only: true,
            id: 4,
            ip_address: "149.154.167.92".to_owned(),
            port: 8888,
            secret: Some(vec![0x11; 16]),
        }),
        // DC 5 IPv6-only regular
        tl_enums::DcOption::DcOption(tl_types::DcOption {
            ipv6: true,
            media_only: false,
            tcpo_only: false,
            cdn: false,
            r#static: true,
            this_port_only: false,
            id: 5,
            ip_address: "2001:b28:f23f:f005::a".to_owned(),
            port: 443,
            secret: None,
        }),
        // CDN DC 203
        tl_enums::DcOption::DcOption(tl_types::DcOption {
            ipv6: false,
            media_only: false,
            tcpo_only: false,
            cdn: true,
            r#static: false,
            this_port_only: false,
            id: 203,
            ip_address: "91.108.23.100".to_owned(),
            port: 443,
            secret: None,
        }),
    ];
    let config_resp = tl_config_with_options(2, dc_opts);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::help::GetConfig::CONSTRUCTOR_ID => ServerAction::ok(&config_resp),
        x if x == tl_fns::help::GetNearestDc::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                country: "NL".to_owned(),
                this_dc: rpc.dc_id,
                nearest_dc: rpc.dc_id,
            }))
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    server.register_cdn_rsa_key_on(&client, 203);

    // Fetch help.getConfig and apply it.
    let loaded = tokio::time::timeout(
        TEST_TIMEOUT,
        client.load_config_once(ConfigLoaderOptions {
            enumerate_dc_timeout: Duration::from_secs(30),
            special_request_timeout: Duration::from_secs(30),
            enable_doh_fallback: false,
            max_enum_rounds: 1,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loaded.this_dc, 2);
    assert_eq!(client.config_fields().web_file_dc_id, 4);

    // Verify stored flags and tdesktop DcOptions::lookup semantics (`mtproto/mtproto_dc_options.cpp`).
    let opts = client.dc_options();
    assert_eq!(opts.dc_type(2), DcType::Regular);
    assert_eq!(opts.dc_type(download_dc_id(2, 0)), DcType::MediaCluster);
    assert_eq!(opts.dc_type(203), DcType::Cdn);

    // Regular DC 2: IPv4 static only in direct mode (media_only excluded).
    let reg2 = opts.lookup(2, DcType::Regular, false);
    assert!(
        reg2.ipv4_tcp
            .iter()
            .any(|e| e.ip == "149.154.167.50" && e.flags.contains(DcOptionFlags::STATIC))
    );
    assert!(
        reg2.ipv4_tcp
            .iter()
            .all(|e| !e.flags.contains(DcOptionFlags::MEDIA_ONLY))
    );
    assert!(
        reg2.ipv6_tcp
            .iter()
            .any(|e| e.ip == "2001:67c:4e8:f002::a" && e.flags.contains(DcOptionFlags::IPV6))
    );

    // Media cluster DC 2: MUST select media_only endpoint.
    let media2 = opts.lookup(2, DcType::MediaCluster, false);
    assert!(
        media2
            .ipv4_tcp
            .iter()
            .all(|e| e.flags.contains(DcOptionFlags::MEDIA_ONLY))
    );
    assert_eq!(media2.ipv4_tcp[0].ip, "149.154.167.222");

    // DC 4 tcpo_only: present in TCP variants with secret, excluded from HTTP variants!
    let reg4 = opts.lookup(4, DcType::Regular, false);
    assert!(reg4.ipv4_tcp.iter().any(|e| {
        e.ip == "149.154.167.92"
            && e.port == 8888
            && e.flags.contains(DcOptionFlags::TCPO_ONLY)
            && e.secret == vec![0x11; 16]
    }));
    assert!(
        reg4.ipv4_http.iter().all(|e| e.ip != "149.154.167.92"),
        "tcpo_only endpoint must not appear in HTTP variants"
    );

    // CDN DC 203: selects CDN endpoint.
    let cdn203 = opts.lookup(203, DcType::Cdn, false);
    assert_eq!(cdn203.ipv4_tcp.len(), 1);
    assert_eq!(cdn203.ipv4_tcp[0].ip, "91.108.23.100");
    assert!(cdn203.ipv4_tcp[0].flags.contains(DcOptionFlags::CDN));

    // Verify connect_dc refuses IPv6-only DC 5 when use_ipv6 == false (bug fix check!).
    let mut no_v6_cfg = test_client_config(5, false);
    no_v6_cfg.use_ipv6 = false;
    let err = server
        .connect_dc(5, DcType::Regular, opts, &no_v6_cfg, &ProxyData::none())
        .await;
    assert!(
        err.is_err(),
        "connect_dc must NOT fall back to IPv6 when use_ipv6 is false"
    );

    // With use_ipv6 == true & prefer_ipv6 == true, DC 5 connects to IPv6 endpoint!
    let mut v6_cfg = test_client_config(5, false);
    v6_cfg.use_ipv6 = true;
    v6_cfg.prefer_ipv6 = true;
    let ok_v6 = server
        .connect_dc(5, DcType::Regular, opts, &v6_cfg, &ProxyData::none())
        .await;
    assert!(ok_v6.is_ok(), "connect_dc must use IPv6 when enabled");

    // Actually invoke on media-cluster DC 2 and CDN DC 203 to check dialed IPs & CDN initConnection.
    let _ = tokio::time::timeout(
        TEST_TIMEOUT,
        client.invoke_in_dc(download_dc_id(2, 0), &tl_fns::help::GetNearestDc),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = tokio::time::timeout(
        TEST_TIMEOUT,
        client.invoke_in_dc(203, &tl_fns::help::GetNearestDc),
    )
    .await
    .unwrap()
    .unwrap();

    let conns = server.connections();
    assert!(conns.iter().any(|c| c.ip == "149.154.167.222"
        && c.protocol_dc_id == protocol_dc_id(download_dc_id(2, 0), DcType::MediaCluster, false)));
    assert!(
        conns
            .iter()
            .any(|c| c.ip == "91.108.23.100" && c.protocol_dc_id == 203)
    );
    assert!(
        conns
            .iter()
            .any(|c| c.ip == "2001:b28:f23f:f005::a" && c.protocol_dc_id == 5)
    );

    // CDN initConnection must send "n/a" for device_model and system_version like tdesktop.
    let cdn_init = server
        .inits()
        .into_iter()
        .find(|i| i.dc_id == 203)
        .expect("CDN initConnection recorded");
    assert_eq!(cdn_init.device_model, "n/a");
    assert_eq!(cdn_init.system_version, "n/a");

    client.shutdown();
}

// ============================================================================
// 3. Migrations (PHONE / NETWORK / USER / FILE) & Guest DC Auth Export/Import
// ============================================================================

#[tokio::test]
async fn conformance_3_migrations_and_guest_dc_auth_export_import() {
    let authed_dcs: Arc<Mutex<Vec<DcId>>> = Arc::new(Mutex::new(vec![2]));
    let authed_dcs_srv = Arc::clone(&authed_dcs);
    let step = Arc::new(AtomicU64::new(0));
    let step_srv = Arc::clone(&step);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::help::GetNearestDc::CONSTRUCTOR_ID => {
            let s = step_srv.fetch_add(1, Ordering::SeqCst);
            match (s, rpc.dc_id) {
                (0, 2) => ServerAction::err(303, "PHONE_MIGRATE_4"),
                (1, 4) => ServerAction::err(303, "NETWORK_MIGRATE_5"),
                (2, 5) => ServerAction::err(303, "USER_MIGRATE_2"),
                (3, 2) => ServerAction::ok(&tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: "OK".to_owned(),
                    this_dc: 2,
                    nearest_dc: 2,
                })),
                other => panic!("unexpected getNearestDc hop: {other:?}"),
            }
        }
        x if x == tl_fns::auth::ExportAuthorization::CONSTRUCTOR_ID => {
            assert_eq!(
                rpc.dc_id, 2,
                "auth.exportAuthorization must go to main DC 2"
            );
            let req = tl_fns::auth::ExportAuthorization::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(
                &tl_enums::auth::ExportedAuthorization::ExportedAuthorization(
                    tl_types::auth::ExportedAuthorization {
                        id: 777_000 + i64::from(req.dc_id),
                        bytes: vec![0xDE, 0xAD, req.dc_id as u8],
                    },
                ),
            )
        }
        x if x == tl_fns::auth::ImportAuthorization::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::ImportAuthorization::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.id, 777_000 + i64::from(rpc.dc_id));
            assert_eq!(req.bytes, vec![0xDE, 0xAD, rpc.dc_id as u8]);
            authed_dcs_srv.lock().unwrap().push(rpc.dc_id);
            ServerAction::ok(&tl_authorization(42, "Alice"))
        }
        x if x == tl_fns::upload::GetFile::CONSTRUCTOR_ID => {
            // File lives on DC 4 when requested on DC 2; DC 3 file is on DC 3.
            if rpc.dc_id == 2 {
                return ServerAction::err(303, "FILE_MIGRATE_4");
            }
            let is_authed = authed_dcs_srv.lock().unwrap().contains(&rpc.dc_id);
            if !is_authed {
                return ServerAction::err(401, "AUTH_KEY_UNREGISTERED");
            }
            ServerAction::ok(&tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FilePng,
                mtime: 1_700_000_000,
                bytes: vec![0x89, 0x50, 0x4E, 0x47, rpc.dc_id as u8],
            }))
        }
        other => panic!("unexpected RPC ctor 0x{other:08x} on dc {}", rpc.dc_id),
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);

    // Part A: PHONE_MIGRATE_4 -> NETWORK_MIGRATE_5 -> USER_MIGRATE_2 switches main_dc_id and repeats!
    let nearest = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        nearest,
        tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
            country: "OK".to_owned(),
            this_dc: 2,
            nearest_dc: 2,
        })
    );
    assert_eq!(client.main_dc_id(), 2);

    // Part B: FILE_MIGRATE_4 on main DC 2 MUST NOT switch main_dc_id; must open a per-DC
    // connection to DC 4, copy auth via exportAuthorization(4) + importAuthorization, and finish!
    let get_file_req = tl_fns::upload::GetFile {
        precise: true,
        cdn_supported: false,
        location: tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: 1001,
                access_hash: 2002,
                file_reference: vec![1, 2, 3],
                thumb_size: String::new(),
            },
        ),
        offset: 0,
        limit: 4096,
    };
    let file4 = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&get_file_req))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        client.main_dc_id(),
        2,
        "FILE_MIGRATE_4 must NEVER change main_dc_id"
    );
    match file4 {
        tl_enums::upload::File::File(f) => assert_eq!(f.bytes, vec![0x89, 0x50, 0x4E, 0x47, 4]),
        other => panic!("unexpected file result: {other:?}"),
    }

    // Part C: Proactive auth copy before issuing a file request to guest DC 3.
    let dc3_download: ShiftedDcId = download_dc_id(3, 0);
    tokio::time::timeout(TEST_TIMEOUT, client.ensure_dc_authorized(dc3_download))
        .await
        .unwrap()
        .unwrap();
    let file3 = tokio::time::timeout(
        TEST_TIMEOUT,
        client.invoke_in_dc(dc3_download, &get_file_req),
    )
    .await
    .unwrap()
    .unwrap();
    match file3 {
        tl_enums::upload::File::File(f) => assert_eq!(f.bytes, vec![0x89, 0x50, 0x4E, 0x47, 3]),
        other => panic!("unexpected dc3 file result: {other:?}"),
    }

    // Verify exact wire order for DC 3: exportAuthorization(3) -> importAuthorization(3) -> getFile(3).
    let rpcs = server.rpcs();
    let dc3_export_idx = rpcs
        .iter()
        .position(|r| {
            r.ctor == tl_fns::auth::ExportAuthorization::CONSTRUCTOR_ID
                && tl_fns::auth::ExportAuthorization::from_bytes(&r.body)
                    .unwrap()
                    .dc_id
                    == 3
        })
        .expect("exportAuthorization(3) sent");
    let dc3_import_idx = rpcs
        .iter()
        .position(|r| r.dc_id == 3 && r.ctor == tl_fns::auth::ImportAuthorization::CONSTRUCTOR_ID)
        .expect("importAuthorization on DC 3 sent");
    let dc3_file_idx = rpcs
        .iter()
        .position(|r| r.dc_id == 3 && r.ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID)
        .expect("upload.getFile on DC 3 sent");
    assert!(dc3_export_idx < dc3_import_idx);
    assert!(dc3_import_idx < dc3_file_idx);

    client.shutdown();
}

// ============================================================================
// 4. Code-login sequence: sendCode, signIn, SRP 2FA (byte-exact M1),
//    signUp / authorizationSignUpRequired, INVALID/EXPIRED/FLOOD_WAIT
// ============================================================================

#[tokio::test]
async fn conformance_4_code_login_srp_signup_and_errors() {
    let send_code_calls = Arc::new(AtomicU64::new(0));
    let send_code_calls_srv = Arc::clone(&send_code_calls);
    let srp_verified = Arc::new(Mutex::new(false));
    let srp_verified_srv = Arc::clone(&srp_verified);

    let expected_a = hex_decode(SRP_EXPECTED_A_HEX);
    let expected_m1 = hex_decode(SRP_EXPECTED_M1_HEX);
    let srp_b = hex_decode(SRP_B_HEX);

    let server = ScriptedServer::new(move |_srv, rpc| {
        match rpc.ctor {
        x if x == tl_fns::auth::SendCode::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::SendCode::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.api_id, FAKE_API_ID);
            assert_eq!(req.api_hash, FAKE_API_HASH);
            // Verify tdesktop CodeSettings flags (all false / None, flags == 0).
            let tl_enums::CodeSettings::CodeSettings(cs) = req.settings;
            assert!(!cs.allow_flashcall);
            assert!(!cs.current_number);
            assert!(!cs.allow_app_hash);
            assert!(!cs.allow_missed_call);
            assert!(!cs.allow_firebase);
            assert!(!cs.unknown_number);
            assert!(cs.logout_tokens.is_none());
            assert!(cs.token.is_none());

            let call = send_code_calls_srv.fetch_add(1, Ordering::SeqCst);
            if req.phone_number.contains("9999") {
                return ServerAction::err(420, "FLOOD_WAIT_30");
            }
            if call == 0 && rpc.dc_id == 2 {
                return ServerAction::err(303, "PHONE_MIGRATE_4");
            }
            ServerAction::ok(&tl_enums::auth::SentCode::SentCode(
                tl_types::auth::SentCode {
                    r#type: tl_enums::auth::SentCodeType::App(tl_types::auth::SentCodeTypeApp {
                        length: 5,
                    }),
                    phone_code_hash: format!("hash_{call}"),
                    next_type: Some(tl_enums::auth::CodeType::Sms),
                    timeout: Some(60),
                },
            ))
        }
        x if x == tl_fns::auth::SignIn::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::SignIn::from_bytes(&rpc.body).unwrap();
            match req.phone_code.as_deref() {
                Some("00000") => ServerAction::err(400, "PHONE_CODE_INVALID"),
                Some("11111") => ServerAction::err(400, "PHONE_CODE_EXPIRED"),
                Some("22222") => ServerAction::err(401, "SESSION_PASSWORD_NEEDED"),
                Some("33333") => ServerAction::ok(
                    &tl_enums::auth::Authorization::SignUpRequired(
                        tl_types::auth::AuthorizationSignUpRequired {
                            terms_of_service: Some(
                                tl_enums::help::TermsOfService::TermsOfService(
                                    tl_types::help::TermsOfService {
                                        popup: true,
                                        id: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                                            data: r#"{"id":"tos-1"}"#.to_owned(),
                                        }),
                                        text: "Accept Telegram Terms".to_owned(),
                                        entities: vec![],
                                        min_age_confirm: None,
                                    },
                                ),
                            ),
                        },
                    ),
                ),
                other => panic!("unexpected signIn code: {other:?}"),
            }
        }
        x if x == tl_fns::account::GetPassword::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::account::Password::Password(
                tl_types::account::Password {
                    has_recovery: false,
                    has_secure_values: false,
                    has_password: true,
                    current_algo: Some(
                        tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                            tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                                salt1: SRP_SALT1.to_vec(),
                                salt2: SRP_SALT2.to_vec(),
                                g: 3,
                                p: GOOD_PRIME.to_vec(),
                            },
                        ),
                    ),
                    srp_b: Some(srp_b.clone()),
                    srp_id: Some(987_654_321),
                    hint: Some("conformance-hint".to_owned()),
                    email_unconfirmed_pattern: None,
                    new_algo: tl_enums::PasswordKdfAlgo::Unknown,
                    new_secure_algo: tl_enums::SecurePasswordKdfAlgo::Unknown,
                    secure_random: vec![0x77; 32],
                    pending_reset_date: None,
                    login_email_pattern: None,
                },
            ))
        }
        x if x == tl_fns::auth::CheckPassword::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::CheckPassword::from_bytes(&rpc.body).unwrap();
            let tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(srp) = req.password else {
                panic!("expected InputCheckPasswordSRP");
            };
            assert_eq!(srp.srp_id, 987_654_321);
            assert_eq!(srp.a, expected_a, "SRP A must match tdesktop byte-for-byte");
            assert_eq!(srp.m1, expected_m1, "SRP M1 proof must match tdesktop byte-for-byte");
            *srp_verified_srv.lock().unwrap() = true;
            ServerAction::ok(&tl_authorization(1001, "Alice"))
        }
        x if x == tl_fns::auth::SignUp::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::SignUp::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.phone_number, "9996623333");
            assert_eq!(req.first_name, "Ada");
            assert_eq!(req.last_name, "Lovelace");
            ServerAction::ok(&tl_authorization(2002, "Ada"))
        }
        x if x == tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID => {
            ServerAction::ok(&empty_dialog_filters())
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    }
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());

    // 4a. Code login -> PHONE_MIGRATE_4 -> PHONE_CODE_INVALID -> PHONE_CODE_EXPIRED ->
    //     SESSION_PASSWORD_NEEDED -> account.getPassword -> deterministic SRP checkPassword.
    let mut sm = LoginStateMachine::new(fake_credentials(), "US");
    sm.enter_phone_input();

    let step = sm.submit_phone(&rpc, "+999 662 1234").await.unwrap();
    assert_eq!(
        client.main_dc_id(),
        4,
        "PHONE_MIGRATE_4 must switch main DC to 4"
    );
    assert!(matches!(step, LoginStep::CodeInput(_)));

    // PHONE_CODE_INVALID stays on CodeInput.
    let err = sm.submit_code(&rpc, "00000").await.unwrap_err();
    assert_eq!(err, ApiError::Login(LoginErrorKind::PhoneCodeInvalid));
    assert!(matches!(sm.step(), LoginStep::CodeInput(_)));

    // PHONE_CODE_EXPIRED resets to PhoneInput (matching intro_code.cpp::goBack).
    let err = sm.submit_code(&rpc, "11111").await.unwrap_err();
    assert_eq!(err, ApiError::Login(LoginErrorKind::PhoneCodeExpired));
    assert!(matches!(sm.step(), LoginStep::PhoneInput));

    // Re-send code -> signIn -> SESSION_PASSWORD_NEEDED -> account.getPassword -> checkPassword.
    sm.submit_phone(&rpc, "+999 662 1234").await.unwrap();
    let step = sm.submit_code(&rpc, "22222").await.unwrap();
    assert!(matches!(step, LoginStep::PasswordCheck { .. }));

    let step = sm
        .submit_password_with_random(&rpc, SRP_PASSWORD, SRP_CLIENT_A_SECRET, Instant::now())
        .await
        .unwrap();
    assert!(
        *srp_verified.lock().unwrap(),
        "server verified exact SRP M1"
    );
    match step {
        LoginStep::Authorized { user, .. } => assert_eq!(user.id, 1001),
        other => panic!("expected Authorized after SRP, got {other:?}"),
    }

    // 4b. auth.authorizationSignUpRequired -> SignUp -> auth.signUp.
    let mut sm_signup = LoginStateMachine::new(fake_credentials(), "US");
    sm_signup.enter_phone_input();
    sm_signup.submit_phone(&rpc, "+999 662 3333").await.unwrap();
    let step = sm_signup.submit_code(&rpc, "33333").await.unwrap();
    assert!(matches!(step, LoginStep::SignUp { .. }));
    // Must reject signUp until Terms of Service accepted.
    assert_eq!(
        sm_signup.submit_sign_up(&rpc, "Ada", "Lovelace").await,
        Err(ApiError::Login(LoginErrorKind::TermsNotAccepted))
    );
    sm_signup.accept_terms().unwrap();
    let step = sm_signup
        .submit_sign_up(&rpc, "Ada", "Lovelace")
        .await
        .unwrap();
    match step {
        LoginStep::Authorized { user, .. } => assert_eq!(user.id, 2002),
        other => panic!("expected Authorized after signUp, got {other:?}"),
    }

    // 4c. FLOOD_WAIT_30 on login must return immediately (handle_flood_errors) with tdesktop UI message.
    let mut sm_flood = LoginStateMachine::new(fake_credentials(), "US");
    sm_flood.enter_phone_input();
    let t0 = tokio::time::Instant::now();
    let flood_err = sm_flood
        .submit_phone(&rpc, "+999 669 9999")
        .await
        .unwrap_err();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "login FLOOD_WAIT must surface immediately instead of sleeping in tg-mtproto (elapsed={elapsed:?})"
    );
    assert_eq!(
        flood_err,
        ApiError::Login(LoginErrorKind::FloodWait { seconds: 30 })
    );
    assert_eq!(
        LoginErrorKind::FloodWait { seconds: 30 }.to_string(),
        "Too many tries. Please try again later."
    );

    client.shutdown();
}

// ============================================================================
// 5. QR login: exportLoginToken -> expiry refresh -> updateLoginToken ->
//    loginTokenMigrateTo (DC switch + importLoginToken) -> loginTokenSuccess
// ============================================================================

#[tokio::test]
async fn conformance_5_qr_login_migrate_and_expiry_refresh() {
    let export_calls = Arc::new(AtomicU64::new(0));
    let export_calls_srv = Arc::clone(&export_calls);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::auth::ExportLoginToken::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::ExportLoginToken::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.api_id, FAKE_API_ID);
            assert_eq!(req.api_hash, FAKE_API_HASH);
            assert_eq!(req.except_ids, vec![99]);
            let n = export_calls_srv.fetch_add(1, Ordering::SeqCst);
            match n {
                // Initial token (expires at 1030).
                0 => ServerAction::ok(&tl_enums::auth::LoginToken::LoginToken(
                    tl_types::auth::LoginToken {
                        expires: 1_030,
                        token: b"qr_token_v1".to_vec(),
                    },
                )),
                // Expiry refresh token (expires at 1060).
                1 => ServerAction::ok(&tl_enums::auth::LoginToken::LoginToken(
                    tl_types::auth::LoginToken {
                        expires: 1_060,
                        token: b"qr_token_v2".to_vec(),
                    },
                )),
                // After server pushed updateLoginToken: redirect to DC 4!
                _ => ServerAction::ok(&tl_enums::auth::LoginToken::MigrateTo(
                    tl_types::auth::LoginTokenMigrateTo {
                        dc_id: 4,
                        token: b"migrate_token_dc4".to_vec(),
                    },
                )),
            }
        }
        x if x == tl_fns::auth::ImportLoginToken::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 4, "importLoginToken must target migrated DC 4");
            let req = tl_fns::auth::ImportLoginToken::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.token, b"migrate_token_dc4");
            ServerAction::ok(&tl_enums::auth::LoginToken::Success(
                tl_types::auth::LoginTokenSuccess {
                    authorization: tl_authorization(5555, "QrUser"),
                },
            ))
        }
        x if x == tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID => {
            ServerAction::ok(&empty_dialog_filters())
        }
        other => panic!("unexpected QR RPC ctor 0x{other:08x}"),
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());
    let mut updates_rx = rpc.subscribe_updates().expect("updates stream");

    let mut sm = LoginStateMachine::new(fake_credentials(), "US");

    // Step 1: start QR login -> receives token v1 with 30s refresh interval.
    let step = sm.start_qr_login(&rpc, 1_000, vec![99]).await.unwrap();
    match step {
        LoginStep::QrLogin(qr) => {
            assert_eq!(qr.token, b"qr_token_v1");
            assert_eq!(qr.expires, 1_030);
            assert_eq!(qr.refresh_after_secs, 30);
            assert!(qr.qr_url.starts_with("tg://login?token="));
        }
        other => panic!("expected QrLogin, got {other:?}"),
    }

    // Step 2: token expiry refresh at now=1030 -> receives token v2.
    let step = sm.refresh_qr_token(&rpc, 1_030).await.unwrap();
    match step {
        LoginStep::QrLogin(qr) => {
            assert_eq!(qr.token, b"qr_token_v2");
            assert_eq!(qr.expires, 1_060);
            assert_eq!(qr.refresh_after_secs, 30);
        }
        other => panic!("expected refreshed QrLogin, got {other:?}"),
    }

    // Step 3: server pushes real UpdateShort(Update::LoginToken) over the encrypted MTProto pipe.
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::LoginToken,
            date: 1_035,
        })
    ));
    let pushed = tokio::time::timeout(TEST_TIMEOUT, updates_rx.recv())
        .await
        .unwrap()
        .unwrap();

    // on_qr_updates -> exportLoginToken -> LoginToken::MigrateTo(dc=4) ->
    // set_main_dc_id(4) + importLoginToken(dc=4) -> LoginToken::Success -> Authorized!
    let handled = sm.on_qr_updates(&rpc, &pushed, 1_035).await.unwrap();
    assert!(handled);
    assert_eq!(
        client.main_dc_id(),
        4,
        "QR migration must switch main DC to 4"
    );
    match sm.step() {
        LoginStep::Authorized { user, .. } => assert_eq!(user.id, 5555),
        other => panic!("expected Authorized after QR migrate, got {other:?}"),
    }

    client.shutdown();
}

// ============================================================================
// 6. After login: updates.getState -> updates.getDifference (slice + tooLong),
//    pts/qts/seq gap handling, and updates.getChannelDifference
// ============================================================================

#[tokio::test]
async fn conformance_6_post_login_updates_state_diff_gaps_and_channel_diff() {
    let diff_queue: Arc<Mutex<VecDeque<tl_enums::updates::Difference>>> =
        Arc::new(Mutex::new(VecDeque::from([
            // Diff 1 (startup slice): pts 100 -> 102 with msg #1.
            tl_enums::updates::Difference::Slice(tl_types::updates::DifferenceSlice {
                new_messages: vec![tl_message(
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 }),
                    1,
                    1_700_000_010,
                    "slice msg 1",
                )],
                new_encrypted_messages: vec![],
                other_updates: vec![],
                chats: vec![],
                users: vec![tl_enums::User::User(tl_user(200, "Bob", false))],
                intermediate_state: tl_state(102, 5, 1_700_000_010, 11),
            }),
            // Diff 2 (startup final): pts 102 -> 103 with msg #2.
            tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
                new_messages: vec![tl_message(
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 }),
                    2,
                    1_700_000_020,
                    "final msg 2",
                )],
                new_encrypted_messages: vec![],
                other_updates: vec![],
                chats: vec![tl_enums::Chat::Channel(tl_channel(500, "News"))],
                users: vec![],
                state: tl_state(103, 5, 1_700_000_020, 12),
            }),
            // Diff 3 (after pts gap): differenceTooLong resetting pts to 200!
            tl_enums::updates::Difference::TooLong(tl_types::updates::DifferenceTooLong {
                pts: 200,
            }),
            // Diff 4 (after seq gap): catches up seq to 20.
            tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                date: 1_700_000_100,
                seq: 20,
            }),
            // Diff 5 (after qts gap): catches up qts to 10.
            tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
                new_messages: vec![],
                new_encrypted_messages: vec![],
                other_updates: vec![],
                chats: vec![],
                users: vec![],
                state: tl_state(200, 10, 1_700_000_110, 20),
            }),
        ])));
    let diff_queue_srv = Arc::clone(&diff_queue);

    let chan_diff_queue: Arc<Mutex<VecDeque<tl_enums::updates::ChannelDifference>>> =
        Arc::new(Mutex::new(VecDeque::from([
            // Non-final channel diff (pts 50 -> 55, final=false).
            tl_enums::updates::ChannelDifference::ChannelDifference(
                tl_types::updates::ChannelDifference {
                    r#final: false,
                    pts: 55,
                    timeout: Some(30),
                    new_messages: vec![tl_message(
                        tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 }),
                        501,
                        1_700_000_200,
                        "channel post 1",
                    )],
                    other_updates: vec![],
                    chats: vec![tl_enums::Chat::Channel(tl_channel(500, "News"))],
                    users: vec![],
                },
            ),
            // Final channel diff (pts 55 -> 60, final=true).
            tl_enums::updates::ChannelDifference::ChannelDifference(
                tl_types::updates::ChannelDifference {
                    r#final: true,
                    pts: 60,
                    timeout: Some(30),
                    new_messages: vec![tl_message(
                        tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 }),
                        502,
                        1_700_000_210,
                        "channel post 2",
                    )],
                    other_updates: vec![],
                    chats: vec![],
                    users: vec![],
                },
            ),
        ])));
    let chan_diff_queue_srv = Arc::clone(&chan_diff_queue);

    let recorded_get_diffs: Arc<Mutex<Vec<tl_fns::updates::GetDifference>>> =
        Arc::new(Mutex::new(Vec::new()));
    let recorded_get_diffs_srv = Arc::clone(&recorded_get_diffs);
    let recorded_chan_diffs: Arc<Mutex<Vec<tl_fns::updates::GetChannelDifference>>> =
        Arc::new(Mutex::new(Vec::new()));
    let recorded_chan_diffs_srv = Arc::clone(&recorded_chan_diffs);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_state(100, 5, 1_700_000_000, 10))
        }
        x if x == tl_fns::updates::GetDifference::CONSTRUCTOR_ID => {
            let req = tl_fns::updates::GetDifference::from_bytes(&rpc.body).unwrap();
            recorded_get_diffs_srv.lock().unwrap().push(req);
            let next = diff_queue_srv
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra updates.getDifference");
            ServerAction::ok(&next)
        }
        x if x == tl_fns::updates::GetChannelDifference::CONSTRUCTOR_ID => {
            let req = tl_fns::updates::GetChannelDifference::from_bytes(&rpc.body).unwrap();
            recorded_chan_diffs_srv.lock().unwrap().push(req);
            let next = chan_diff_queue_srv
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra updates.getChannelDifference");
            ServerAction::ok(&next)
        }
        other => panic!("unexpected updates RPC ctor 0x{other:08x}"),
    });

    let cfg = test_client_config(2, false);
    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());

    let mut session = Session::new(UserId(100));
    let mut handler = UpdatesHandler::new();

    // 1. updates.getState -> updates.getDifference (slice -> final) over real MTProto.
    handler.initialize(&rpc, &mut session, 0).await.unwrap();
    assert_eq!(handler.pts(), 100);
    handler.on_reconnect(&rpc, &mut session, 10).await.unwrap();
    assert_eq!(handler.pts(), 103);
    let bob_peer = PeerId::from_user(UserId(200));
    assert!(
        session
            .message(FullMsgId::new(bob_peer, MsgId(1)))
            .is_some()
    );
    assert!(
        session
            .message(FullMsgId::new(bob_peer, MsgId(2)))
            .is_some()
    );

    // Verify second getDifference used intermediate_state (pts=102, qts=5).
    {
        let diffs = recorded_get_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0].pts, 100);
        assert_eq!(diffs[0].qts, 5);
        assert_eq!(diffs[1].pts, 102);
        assert_eq!(diffs[1].qts, 5);
    }

    // 2. PTS gap (pts=106, pts_count=1 while local pts=103) -> buffered -> timeout triggers
    //    updates.getDifference -> differenceTooLong(pts=200) resets local pts!
    let pts_gap_update = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: tl_message(
                tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 }),
                9,
                1_700_000_050,
                "gap msg",
            ),
            pts: 106,
            pts_count: 1,
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_050,
        seq: 0,
    });
    handler
        .handle_updates(&rpc, &mut session, pts_gap_update, None, 1_000)
        .await
        .unwrap();
    assert_eq!(
        handler.pts(),
        103,
        "gapped update must be buffered, not applied immediately"
    );
    let deadline = handler.next_deadline().expect("pts gap timer scheduled");
    handler.on_tick(&rpc, &mut session, deadline).await.unwrap();
    assert_eq!(
        handler.pts(),
        200,
        "differenceTooLong must reset pts to 200"
    );
    let evs = handler.drain_events();
    assert!(evs.contains(&tg_api::UpdateEvent::DifferenceTooLong { pts: 200 }));

    // 3. SEQ gap (seq_start=15 > current seq=12 + 1) -> buffered -> timeout triggers getDifference!
    let seq_gap_update = tl_enums::Updates::Combined(tl_types::UpdatesCombined {
        updates: vec![],
        users: vec![],
        chats: vec![],
        date: 1_700_000_090,
        seq_start: 15,
        seq: 16,
    });
    handler
        .handle_updates(&rpc, &mut session, seq_gap_update, None, 5_000)
        .await
        .unwrap();
    let seq_deadline = handler.next_deadline().expect("seq gap timer scheduled");
    handler
        .on_tick(&rpc, &mut session, seq_deadline)
        .await
        .unwrap();
    assert_eq!(recorded_get_diffs.lock().unwrap().len(), 4);

    // 4. QTS gap (qts=8 > current qts=5 + 1) -> buffered -> timeout triggers getDifference!
    let qts_gap_update = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewEncryptedMessage(
            tl_types::UpdateNewEncryptedMessage {
                message: tl_enums::EncryptedMessage::Service(tl_types::EncryptedMessageService {
                    random_id: 1,
                    chat_id: 2,
                    date: 1_700_000_105,
                    bytes: vec![1, 2, 3],
                }),
                qts: 8,
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_105,
        seq: 0,
    });
    handler
        .handle_updates(&rpc, &mut session, qts_gap_update, None, 10_000)
        .await
        .unwrap();
    let qts_deadline = handler.next_deadline().expect("qts gap timer scheduled");
    handler
        .on_tick(&rpc, &mut session, qts_deadline)
        .await
        .unwrap();
    assert_eq!(recorded_get_diffs.lock().unwrap().len(), 5);

    // 5. Channel updates: init channel pts=50, then push channel PTS gap (pts=53, pts_count=1)
    //    -> timeout fires getChannelDifference(force=true) -> non-final (pts=55) -> final (pts=60)!
    let chan_id = ChannelId(500);
    let chan_peer = PeerId::from_channel(chan_id);
    assert!(session.peer(chan_peer).is_some());
    handler.init_channel_pts(chan_id, 50);

    let chan_gap_update = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewChannelMessage(
            tl_types::UpdateNewChannelMessage {
                message: tl_message(
                    tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 }),
                    503,
                    1_700_000_150,
                    "gapped channel post",
                ),
                pts: 53,
                pts_count: 1,
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_150,
        seq: 0,
    });
    handler
        .handle_updates(&rpc, &mut session, chan_gap_update, None, 20_000)
        .await
        .unwrap();
    let chan_deadline = handler
        .next_deadline()
        .expect("channel pts gap timer scheduled");
    handler
        .on_tick(&rpc, &mut session, chan_deadline)
        .await
        .unwrap();

    assert_eq!(handler.channel_pts(chan_id), Some(60));
    assert!(
        session
            .message(FullMsgId::new(chan_peer, MsgId(501)))
            .is_some()
    );
    assert!(
        session
            .message(FullMsgId::new(chan_peer, MsgId(502)))
            .is_some()
    );
    {
        let cdiffs = recorded_chan_diffs.lock().unwrap();
        assert_eq!(cdiffs.len(), 2);
        assert!(
            cdiffs[0].force,
            "PTS gap getChannelDifference must set force=true"
        );
        assert_eq!(cdiffs[0].pts, 50);
        assert_eq!(cdiffs[1].pts, 55);
    }

    client.shutdown();
}

// ============================================================================
// 7. Error routing: AUTH_KEY_UNREGISTERED / SESSION_REVOKED -> logout,
//    AUTH_KEY_DUPLICATED -> new key, 420 FLOOD_WAIT & FLOOD_PREMIUM_WAIT,
//    500 exponential retries, and -404 key mismatch (both PFS and non-PFS)
// ============================================================================

#[tokio::test]
async fn conformance_7_error_routing_logout_duplicated_flood_500_and_404() {
    // --- 7a. AUTH_KEY_UNREGISTERED & SESSION_REVOKED on main DC -> ForceLogout ---
    {
        let call = Arc::new(AtomicU64::new(0));
        let call_srv = Arc::clone(&call);
        let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
            x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_state(1, 0, 1_700_000_000, 1))
            }
            x if x == tl_fns::help::GetNearestDc::CONSTRUCTOR_ID => {
                let n = call_srv.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ServerAction::err(401, "AUTH_KEY_UNREGISTERED")
                } else {
                    ServerAction::err(401, "SESSION_REVOKED")
                }
            }
            other => panic!("unexpected 7a ctor 0x{other:08x}"),
        });

        let cfg = test_client_config(2, false);
        let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
        server.register_rsa_key_on(&client);
        let rpc = MtprotoRpc::new(client.clone());

        let rt = SessionRuntime::start_mtproto_on(
            UserId(42),
            rpc.clone(),
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
        .unwrap();
        let mut events_rx = rt.subscribe_events();
        rt.wait_ready().await.unwrap();
        // Drain Ready event.
        assert_eq!(events_rx.recv().await.unwrap(), SessionEvent::Ready);

        // Trigger AUTH_KEY_UNREGISTERED on main DC -> ForceLogout.
        let err1 = client
            .invoke(&tl_fns::help::GetNearestDc)
            .await
            .unwrap_err();
        assert!(err1.to_string().contains("AUTH_KEY_UNREGISTERED"));
        let ev1 = tokio::time::timeout(TEST_TIMEOUT, events_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ev1,
            SessionEvent::ForceLogout {
                reason: LogoutReason::Unauthorized {
                    error: "AUTH_KEY_UNREGISTERED".to_owned(),
                },
            }
        );

        // Trigger SESSION_REVOKED on main DC -> ForceLogout.
        let err2 = client
            .invoke(&tl_fns::help::GetNearestDc)
            .await
            .unwrap_err();
        assert!(err2.to_string().contains("SESSION_REVOKED"));
        let ev2 = tokio::time::timeout(TEST_TIMEOUT, events_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ev2,
            SessionEvent::ForceLogout {
                reason: LogoutReason::Unauthorized {
                    error: "SESSION_REVOKED".to_owned(),
                },
            }
        );

        rt.shutdown_async().await;
        client.shutdown();
    }

    // --- 7b. AUTH_KEY_DUPLICATED -> destroys keys, runs DH for new key, retries ---
    {
        let dup_count = Arc::new(AtomicU64::new(0));
        let dup_count_srv = Arc::clone(&dup_count);
        let server = ScriptedServer::new(move |_srv, _rpc| {
            let n = dup_count_srv.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ServerAction::err(406, "AUTH_KEY_DUPLICATED")
            } else {
                ServerAction::ok(&tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: "DUP_OK".to_owned(),
                    this_dc: 2,
                    nearest_dc: 2,
                }))
            }
        });

        let cfg = test_client_config(2, false);
        let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
        server.register_rsa_key_on(&client);

        let res = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            res,
            tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                country: "DUP_OK".to_owned(),
                this_dc: 2,
                nearest_dc: 2,
            })
        );
        let rpcs = server.rpcs();
        assert_eq!(rpcs.len(), 2);
        assert_ne!(
            rpcs[0].auth_key_id, rpcs[1].auth_key_id,
            "AUTH_KEY_DUPLICATED must negotiate a brand-new auth key"
        );
        assert!(
            rpcs[1].wrapped_with_init,
            "retry on new key after AUTH_KEY_DUPLICATED must re-send initConnection"
        );
        client.shutdown();
    }

    // --- 7c. 420 FLOOD_WAIT_1, 420 FLOOD_PREMIUM_WAIT_1, and 500 exponential retries ---
    {
        let mode = Arc::new(AtomicU64::new(0));
        let mode_srv = Arc::clone(&mode);
        let server = ScriptedServer::new(move |_srv, _rpc| {
            let n = mode_srv.fetch_add(1, Ordering::SeqCst);
            match n {
                0 => ServerAction::err(420, "FLOOD_WAIT_1"),
                1 => ServerAction::ok(&tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: "FW".to_owned(),
                    this_dc: 2,
                    nearest_dc: 2,
                })),
                2 => ServerAction::err(420, "FLOOD_PREMIUM_WAIT_1"),
                3 => ServerAction::ok(&tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: "FPW".to_owned(),
                    this_dc: 2,
                    nearest_dc: 2,
                })),
                // Two 500 errors -> waits 1.01s + 2.01s = 3.02s -> succeeds on attempt 3.
                4..=5 => ServerAction::err(500, "INTERNAL"),
                _ => ServerAction::ok(&tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: "RETRY500".to_owned(),
                    this_dc: 2,
                    nearest_dc: 2,
                })),
            }
        });

        let cfg = test_client_config(2, false);
        let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
        server.register_rsa_key_on(&client);
        let mut premium_rx = client.subscribe_non_premium_delays();

        // FLOOD_WAIT_1: sleeps ~1.01s and retries.
        let t0 = tokio::time::Instant::now();
        let _ = client
            .invoke_with_options(&tl_fns::help::GetNearestDc, InvokeOptions::default())
            .await
            .unwrap();
        let d_fw = t0.elapsed();
        assert!(
            d_fw >= Duration::from_millis(950) && d_fw < Duration::from_secs(4),
            "FLOOD_WAIT_1 should sleep ~1.01s, got {d_fw:?}"
        );

        // FLOOD_PREMIUM_WAIT_1: notifies non_premium_delays(1), sleeps ~1.01s, and retries.
        let t1 = tokio::time::Instant::now();
        let _ = client.invoke(&tl_fns::help::GetNearestDc).await.unwrap();
        let d_fpw = t1.elapsed();
        assert!(
            d_fpw >= Duration::from_millis(950) && d_fpw < Duration::from_secs(4),
            "FLOOD_PREMIUM_WAIT_1 should sleep ~1.01s, got {d_fpw:?}"
        );
        assert_eq!(premium_rx.try_recv().unwrap(), 1);

        // 500 INTERNAL x2: sleeps 1.01s + 2.01s = 3.02s and succeeds on attempt 3.
        let t2 = tokio::time::Instant::now();
        let r500 = client.invoke(&tl_fns::help::GetNearestDc).await.unwrap();
        let d_500 = t2.elapsed();
        assert!(
            d_500 >= Duration::from_millis(2_900) && d_500 < Duration::from_secs(6),
            "2x 500 retries should sleep 1+2s (~3.02s), got {d_500:?}"
        );
        assert_eq!(
            r500,
            tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                country: "RETRY500".to_owned(),
                this_dc: 2,
                nearest_dc: 2,
            })
        );
        client.shutdown();
    }

    // --- 7d. -404 key mismatch recovery under non-PFS (persistent key forgotten) ---
    {
        let attempt = Arc::new(AtomicU64::new(0));
        let attempt_srv = Arc::clone(&attempt);
        let server = ScriptedServer::new(move |_srv, _rpc| {
            let n = attempt_srv.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ServerAction::ForgetKeyAndSend404
            } else {
                ServerAction::ok(&tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: "RECOVERED_404".to_owned(),
                    this_dc: 2,
                    nearest_dc: 2,
                }))
            }
        });

        let cfg = test_client_config(2, false);
        let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
        server.register_rsa_key_on(&client);

        let res = tokio::time::timeout(TEST_TIMEOUT, client.invoke(&tl_fns::help::GetNearestDc))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            res,
            tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                country: "RECOVERED_404".to_owned(),
                this_dc: 2,
                nearest_dc: 2,
            })
        );
        let rpcs = server.rpcs();
        assert_eq!(rpcs.len(), 2);
        assert_ne!(
            rpcs[0].auth_key_id, rpcs[1].auth_key_id,
            "-404 on non-PFS persistent key must negotiate a fresh key via DH"
        );
        assert_ne!(rpcs[0].session_id, rpcs[1].session_id);
        assert!(rpcs[1].wrapped_with_init);

        client.shutdown();
    }
}
