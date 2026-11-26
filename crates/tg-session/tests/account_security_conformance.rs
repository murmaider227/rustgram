//! Byte-level tdesktop conformance tests for post-login account security, 2FA cloud password,
//! sessions/web authorizations, notification settings, account settings sync, presence timing,
//! and self-user updates against an in-process scripted fake Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses synthetic test fixtures only (fake `api_id = 1`, fake passwords, fake emails,
//!   ephemeral test-only RSA keypair).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{
    AccountSettingsManager, ApiCredentials, ApiError, AuthorizationsManager, CloudPasswordManager,
    CloudPasswordRandom, CloudPasswordState, DEFAULT_OFFLINE_IDLE_TIMEOUT_MS,
    DEFAULT_ONLINE_UPDATE_PERIOD_MS, DefaultNotifyScope, DisallowedGifts, FullNotifySettings,
    GlobalPrivacyState, IDLE_FINISH_CHECK_MS, InvocationError, MuteValue, NotifySettingsManager,
    NotifySound, NotifyTarget, ResetPasswordOutcome, UnarchiveOnNewMessage, UpdateEvent,
    UpdatesHandler, UsernameCheckResult, WebAuthorizationsManager,
    convert::{apply_tl_user_full, apply_tl_users_and_chats, peer_notify_settings_from_tl},
    skip_add_notify_exception,
};
use tg_core::settings::{CoreSettings, NotifyView};
use tg_crypto::dh::{
    GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, is_prime_and_good, mod_exp,
};
use tg_crypto::rsa::PublicKey;
use tg_crypto::srp::{self, PasswordKdfAlgoModPow, SecureSecretAlgo};
use tg_crypto::{
    AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, pbkdf2_hmac_sha512_64, sha1, sha256,
};
use tg_data::{
    Birthday, ChangeEvent, ChannelId, DocumentId, LoadedStatus, MsgId, PeerId, PeerUpdateFlags,
    Session, UserId,
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
use tg_session::{
    MtprotoRpc, SessionEvent, SessionRuntime, SessionRuntimeConfig, build_mtproto_client_config,
};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, RawObject,
    Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::mpsc;

// ============================================================================
// Test-only constants (fake credentials, ephemeral test RSA key)
// ============================================================================

const FAKE_API_ID: i32 = 1;
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const SELF_UID: i64 = 777_001;

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
// Independent Reference SRP-6a & PH2 Computation (from Telegram spec)
// ============================================================================

/// Independently computes Telegram's `PH2` password hash `x` from raw SHA-256 and PBKDF2-HMAC-SHA512
/// per `core/core_cloud_password.cpp:59-66`:
/// `H(salt2 || PBKDF2(H(salt2 || H(salt1 || password || salt1) || salt2), salt1, 100000, 64) || salt2)`.
fn spec_ph2_hash(salt1: &[u8], salt2: &[u8], password: &[u8]) -> [u8; 32] {
    let h1 = sha256(&[salt1, password, salt1]);
    let h2 = sha256(&[salt2, &h1, salt2]);
    let h3 = pbkdf2_hmac_sha512_64(&h2, salt1, 100_000);
    sha256(&[salt2, &h3, salt2])
}

/// Independently computes Telegram's 256-byte big-endian SRP verifier `v = g^x mod p`.
fn spec_verifier(g: u8, p: &[u8], x: &[u8; 32]) -> [u8; 256] {
    let raw = mod_exp(&[g], x, p).expect("g^x mod p");
    pad_be_256(&raw)
}

/// Computes a valid server SRP ephemeral public value `B = (k*v + g^b) mod p` and verifies
/// a client's `(A, M1)` proof using independent big-integer math via `tg_crypto::dh::mod_exp`
/// and byte-level modular addition/multiplication.
struct SpecSrpServer {
    salt1: Vec<u8>,
    salt2: Vec<u8>,
    g: i32,
    p: Vec<u8>,
    verifier_v: [u8; 256],
    server_b_secret: [u8; 256],
    srp_b: [u8; 256],
}

impl SpecSrpServer {
    fn from_verifier(
        salt1: Vec<u8>,
        salt2: Vec<u8>,
        g: i32,
        p: Vec<u8>,
        verifier_v: [u8; 256],
        server_b_secret: [u8; 256],
    ) -> Self {
        let p_256 = pad_be_256(&p);
        let g_256 = pad_be_256(&(g as u32).to_be_bytes());
        let k_bytes = sha256(&[&p_256, &g_256]);
        let g_b = pad_be_256(&mod_exp(&(g as u32).to_be_bytes(), &server_b_secret, &p).unwrap());
        let kv = be_mod_mul(&k_bytes, &verifier_v, &p);
        let srp_b = be_mod_add(&kv, &g_b, &p);
        Self {
            salt1,
            salt2,
            g,
            p,
            verifier_v,
            server_b_secret,
            srp_b,
        }
    }

    fn verify_client_check(&self, a_bytes: &[u8], m1_bytes: &[u8]) -> bool {
        if a_bytes.len() != 256 || m1_bytes.len() != 32 {
            return false;
        }
        let p_256 = pad_be_256(&self.p);
        let g_256 = pad_be_256(&(self.g as u32).to_be_bytes());
        let u_bytes = sha256(&[a_bytes, &self.srp_b]);
        // Server computes S = (A * v^u)^b mod p
        let v_u = pad_be_256(&mod_exp(&self.verifier_v, &u_bytes, &self.p).unwrap());
        let a_vu = be_mod_mul(a_bytes, &v_u, &self.p);
        let s_srv = pad_be_256(&mod_exp(&a_vu, &self.server_b_secret, &self.p).unwrap());
        let k_session = sha256(&[&s_srv]);

        let h_p = sha256(&[&p_256]);
        let h_g = sha256(&[&g_256]);
        let mut h_xor = [0u8; 32];
        for i in 0..32 {
            h_xor[i] = h_p[i] ^ h_g[i];
        }
        let expected_m1 = sha256(&[
            &h_xor,
            &sha256(&[&self.salt1]),
            &sha256(&[&self.salt2]),
            a_bytes,
            &self.srp_b,
            &k_session,
        ]);
        expected_m1.as_slice() == m1_bytes
    }
}

/// Simple big-endian unsigned `(a + b) mod p` on 256-byte operands (for test oracle only).
fn be_mod_add(a: &[u8], b: &[u8], p: &[u8]) -> [u8; 256] {
    let a = pad_be_256(a);
    let b = pad_be_256(b);
    let p = pad_be_256(p);
    let mut sum = [0u8; 257];
    let mut carry = 0u16;
    for i in (0..256).rev() {
        let s = u16::from(a[i]) + u16::from(b[i]) + carry;
        sum[i + 1] = (s & 0xFF) as u8;
        carry = s >> 8;
    }
    sum[0] = carry as u8;
    be_rem_257(&sum, &p)
}

/// Simple big-endian unsigned `(a * b) mod p` via binary double-and-add (for test oracle only).
fn be_mod_mul(a: &[u8], b: &[u8], p: &[u8]) -> [u8; 256] {
    let p_256 = pad_be_256(p);
    let mut acc = [0u8; 256];
    let mut base = pad_be_256(a);
    for &byte in b.iter().rev() {
        for bit in 0..8 {
            if (byte >> bit) & 1 == 1 {
                acc = be_mod_add(&acc, &base, &p_256);
            }
            base = be_mod_add(&base, &base, &p_256);
        }
    }
    acc
}

fn be_rem_257(val: &[u8; 257], p: &[u8; 256]) -> [u8; 256] {
    let mut cur = *val;
    loop {
        let ge = cur[0] > 0 || &cur[1..] >= p.as_slice();
        if !ge {
            break;
        }
        let mut borrow = 0i16;
        for i in (0..256).rev() {
            let diff = i16::from(cur[i + 1]) - i16::from(p[i]) - borrow;
            if diff < 0 {
                cur[i + 1] = (diff + 256) as u8;
                borrow = 1;
            } else {
                cur[i + 1] = diff as u8;
                borrow = 0;
            }
        }
        cur[0] = (i16::from(cur[0]) - borrow) as u8;
    }
    let mut out = [0u8; 256];
    out.copy_from_slice(&cur[1..]);
    out
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

    fn bool_true() -> Self {
        Self::Reply(BOOL_TRUE_ID.to_le_bytes().to_vec())
    }

    fn bool_false() -> Self {
        Self::Reply(BOOL_FALSE_ID.to_le_bytes().to_vec())
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

    fn rpcs_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.inner
            .rpcs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.ctor == ctor)
            .map(|r| r.body.clone())
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
                                    pq: vec![0x01, 0x87], // 17 * 23
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
                                let shared = pad_be_256(&mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME).unwrap());
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

                        let Some(entry) = server.inner.keys.lock().unwrap().get(&auth_key_id).cloned() else {
                            let _ = peer.tx.send(Err(TransportError::ServerError(
                                TransportErrorCode::AuthKeyNotFound,
                            )));
                            break;
                        };
                        let dec = parse_encrypted_packet(&raw, &entry.key, Side::Client)
                            .expect("decrypt MTProto 2.0 packet");
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
                            if body.len() < 4 { continue; }
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
                                    let ns_id = next_server_msg_id(&mut server_msg_counter, false);
                                    send_encrypted(&peer, &entry.key, entry.server_salt, session_id, ns_id, true, &mut server_seq_counter, &ns.to_bytes());
                                }
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

                            let inner_body = unwrap_init_connection(&body);
                            let inner_ctor = u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                            server.inner.rpcs.lock().unwrap().push(RpcRecord {
                                ctor: inner_ctor,
                                body: inner_body.clone(),
                            });

                            let handler = server.inner.handler.lock().unwrap().clone();
                            let action = handler(&server, IncomingRpc { ctor: inner_ctor, body: inner_body });
                            let result_payload = match action {
                                ServerAction::Reply(bytes) => bytes,
                                ServerAction::RpcError(code, msg) => {
                                    mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                        error_code: code,
                                        error_message: msg,
                                    }).to_bytes()
                                }
                            };
                            let rpc_res = Boxed(RpcResult {
                                req_msg_id: msg_id as i64,
                                result: RawObject(result_payload),
                            }).to_bytes();
                            let r_id = next_server_msg_id(&mut server_msg_counter, true);
                            send_encrypted(&peer, &entry.key, entry.server_salt, session_id, r_id, true, &mut server_seq_counter, &rpc_res);
                        }
                    }
                }
            }
            server.inner.live_pushers.lock().unwrap().remove(&conn_id);
        });
    }
}

fn unwrap_init_connection(body: &[u8]) -> Vec<u8> {
    if body.len() < 4 {
        return body.to_vec();
    }
    let ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());
    if ctor == tl_fns::InvokeWithLayer::<RawObject>::CONSTRUCTOR_ID {
        let mut cur = Cursor::new(&body[4..]);
        let _layer = i32::deserialize(&mut cur).unwrap();
        let after_layer = &body[body.len() - cur.remaining()..];
        let init_ctor = u32::from_le_bytes(after_layer[0..4].try_into().unwrap());
        if init_ctor == tl_fns::InitConnection::<RawObject>::CONSTRUCTOR_ID {
            let mut icur = Cursor::new(&after_layer[4..]);
            let flags = i32::deserialize(&mut icur).unwrap() as u32;
            let _api_id = i32::deserialize(&mut icur).unwrap();
            let _device_model = String::deserialize(&mut icur).unwrap();
            let _system_version = String::deserialize(&mut icur).unwrap();
            let _app_version = String::deserialize(&mut icur).unwrap();
            let _system_lang_code = String::deserialize(&mut icur).unwrap();
            let _lang_pack = String::deserialize(&mut icur).unwrap();
            let _lang_code = String::deserialize(&mut icur).unwrap();
            if (flags & 1) != 0 {
                let _proxy = tl_enums::InputClientProxy::deserialize(&mut icur).unwrap();
            }
            if (flags & 2) != 0 {
                let _params = tl_enums::JSONValue::deserialize(&mut icur).unwrap();
            }
            let query_bytes = &after_layer[after_layer.len() - icur.remaining()..];
            return unwrap_init_connection(query_bytes);
        }
        return unwrap_init_connection(after_layer);
    }
    body.to_vec()
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
        let server = self.clone();
        Box::pin(async move {
            let conn_id = server.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
            let (conn, peer) = FakeConnection::pair();
            server.spawn_connection(conn_id, protocol_dc_id, peer);
            Ok(Box::new(conn) as Box<dyn PacketConnection>)
        })
    }
}

fn build_connected_rpc(server: &ScriptedServer) -> (Client, MtprotoRpc) {
    let creds = ApiCredentials {
        api_id: FAKE_API_ID,
        api_hash: "00000000000000000000000000000000".to_owned(),
    };
    let mut cfg: ClientConfig =
        build_mtproto_client_config(&creds, 2, tg_core::Environment::Production, None);
    cfg.proxy = ProxyData::none();
    cfg.use_pfs = false;
    cfg.ping_interval = Duration::from_secs(600);

    let (client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());
    (client, rpc)
}

fn sample_tl_user(
    id: i64,
    first: &str,
    last: &str,
    username: Option<&str>,
    is_self: bool,
) -> tl_types::User {
    tl_types::User {
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
        stories_unavailable: true,
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
        access_hash: Some(id * 100 + 7),
        first_name: Some(first.to_owned()),
        last_name: if last.is_empty() {
            None
        } else {
            Some(last.to_owned())
        },
        username: username.map(str::to_owned),
        phone: Some("9990001234".to_owned()),
        photo: None,
        status: Some(tl_enums::UserStatus::Offline(tl_types::UserStatusOffline {
            was_online: 1_700_000_000,
        })),
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

fn sample_tl_channel(id: i64, title: &str, broadcast: bool, megagroup: bool) -> tl_types::Channel {
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
        forum: megagroup,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: true,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 1_000 + 9),
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
    }
}

fn default_empty_notify() -> tl_enums::PeerNotifySettings {
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

#[allow(clippy::too_many_arguments)]
fn sample_password_tl(
    has_password: bool,
    current_algo: Option<tl_enums::PasswordKdfAlgo>,
    srp_b: Option<Vec<u8>>,
    srp_id: Option<i64>,
    hint: Option<&str>,
    has_recovery: bool,
    has_secure_values: bool,
    unconfirmed_pattern: Option<&str>,
    pending_reset_date: Option<i32>,
) -> tl_types::account::Password {
    tl_types::account::Password {
        has_recovery,
        has_secure_values,
        has_password,
        current_algo,
        srp_b,
        srp_id,
        hint: hint.map(str::to_owned),
        email_unconfirmed_pattern: unconfirmed_pattern.map(str::to_owned),
        new_algo: tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
            tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                salt1: b"srv_new_salt1_prefix".to_vec(),
                salt2: b"srv_new_salt2_const".to_vec(),
                g: 3,
                p: GOOD_PRIME.to_vec(),
            },
        ),
        new_secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                salt: b"srv_sec_salt".to_vec(),
            },
        ),
        secure_random: vec![0xAB; 32],
        pending_reset_date,
        login_email_pattern: Some("l***@example.test".to_owned()),
    }
}

// ============================================================================
// TEST 1: Independent SRP-6a & PH2 Known-Answer Vectors + DH Checks
// ============================================================================

#[test]
fn srp_and_ph2_known_answer_vectors_and_dh_checks() {
    let salt1 = b"rustgram_account_salt1_v1".to_vec();
    let salt2 = b"rustgram_account_salt2_v1".to_vec();
    let password = b"synthetic-2fa-fixture-pass";
    let algo = PasswordKdfAlgoModPow {
        salt1: salt1.clone(),
        salt2: salt2.clone(),
        g: 3,
        p: GOOD_PRIME.to_vec(),
    };

    // 1. Independent PH2 hash & 256-byte verifier v = g^x mod p.
    let expected_x = spec_ph2_hash(&salt1, &salt2, password);
    let actual_x = srp::compute_hash(&algo, password);
    assert_eq!(actual_x, expected_x, "PH2 hash x mismatch vs spec");

    let expected_v = spec_verifier(3, &GOOD_PRIME, &expected_x);
    let actual_v = srp::compute_digest(&algo, password).expect("compute_digest");
    assert_eq!(
        actual_v, expected_v,
        "PH2 verifier v = g^x mod p mismatch vs spec"
    );
    assert_eq!(actual_v.len(), 256);

    // 2. Independent SRP-6a server verification of client (A, M1).
    let srv_b_secret = [0x5Au8; 256];
    let cli_a_secret = [0x3Cu8; 256];
    let srp_srv = SpecSrpServer::from_verifier(
        salt1.clone(),
        salt2.clone(),
        3,
        GOOD_PRIME.to_vec(),
        expected_v,
        srv_b_secret,
    );
    let check =
        srp::compute_check_with_random(&algo, &srp_srv.srp_b, 998_877, &actual_x, &cli_a_secret)
            .expect("SRP check computation");
    assert_eq!(check.srp_id, 998_877);
    assert!(
        srp_srv.verify_client_check(&check.a, &check.m1),
        "independent SRP server must accept client (A, M1)"
    );

    // Wrong password hash must fail server M1 verification.
    let wrong_x = srp::compute_hash(&algo, b"wrong-fixture-pass");
    let wrong_check =
        srp::compute_check_with_random(&algo, &srp_srv.srp_b, 998_877, &wrong_x, &cli_a_secret)
            .expect("SRP check for wrong password computes A/M1");
    assert!(
        !srp_srv.verify_client_check(&wrong_check.a, &wrong_check.m1),
        "independent SRP server must reject wrong password M1"
    );

    // 3. Client salt extension (`validate_new_algo` appends exactly 32 random bytes to salt1).
    let extended = srp::validate_new_algo(algo.clone());
    assert_eq!(extended.salt1.len(), salt1.len() + 32);
    assert_eq!(&extended.salt1[..salt1.len()], salt1.as_slice());
    assert_eq!(extended.salt2, salt2);

    // Secure secret salt extension accepts PBKDF2 (+32 bytes) and rejects legacy SHA512.
    let sec_ext =
        srp::validate_new_secure_secret_algo(SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
            salt: b"prefix".to_vec(),
        })
        .expect("PBKDF2 new secure algo");
    match sec_ext {
        SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt } => {
            assert_eq!(salt.len(), 6 + 32);
            assert_eq!(&salt[..6], b"prefix");
        }
        other => panic!("unexpected secure algo: {other:?}"),
    }
    assert!(
        srp::validate_new_secure_secret_algo(SecureSecretAlgo::Sha512 {
            salt: b"old".to_vec()
        })
        .is_err()
    );

    // 4. DH parameter validation (`mtproto_dh_utils.cpp`): reject invalid g, prime, and B.
    assert!(is_prime_and_good(&GOOD_PRIME, 3));
    assert!(is_prime_and_good(&GOOD_PRIME, 4));
    assert!(is_prime_and_good(&GOOD_PRIME, 5));
    assert!(is_prime_and_good(&GOOD_PRIME, 7));
    assert!(!is_prime_and_good(&GOOD_PRIME, 1));
    assert!(!is_prime_and_good(&GOOD_PRIME, 2)); // GOOD_PRIME % 8 != 7
    assert!(!is_prime_and_good(&GOOD_PRIME, 8));
    assert!(!is_prime_and_good(&[0xFF; 128], 3)); // 1024-bit rejected

    assert!(
        srp::compute_check_with_random(&algo, &[0u8; 256], 1, &actual_x, &cli_a_secret).is_err(),
        "B = 0 must be rejected"
    );
    assert!(
        srp::compute_check_with_random(&algo, &GOOD_PRIME, 1, &actual_x, &cli_a_secret).is_err(),
        "B >= p must be rejected"
    );

    // 5. `CloudPasswordState::from_account_password` (`core_cloud_password.cpp:303-356`):
    //    validates `new_algo` (+32 salt1), `new_secure_algo` (+32 salt), and `outdated_client`.
    let pwd_ok = sample_password_tl(false, None, None, None, None, false, false, None, None);
    let parsed_ok = CloudPasswordState::from_account_password(&pwd_ok);
    assert!(!parsed_ok.outdated_client);
    assert!(!parsed_ok.unknown_algorithm);
    assert_eq!(
        parsed_ok.new_algo.as_ref().unwrap().salt1.len(),
        b"srv_new_salt1_prefix".len() + 32
    );
    assert!(parsed_ok.new_secure_algo.is_some());

    let mut pwd_unknown_new = pwd_ok.clone();
    pwd_unknown_new.new_algo = tl_enums::PasswordKdfAlgo::Unknown;
    let parsed_unknown_new = CloudPasswordState::from_account_password(&pwd_unknown_new);
    assert!(parsed_unknown_new.outdated_client);
    assert!(parsed_unknown_new.new_algo.is_none());

    let mut pwd_unknown_sec = pwd_ok;
    pwd_unknown_sec.new_secure_algo = tl_enums::SecurePasswordKdfAlgo::Unknown;
    assert!(CloudPasswordState::from_account_password(&pwd_unknown_sec).outdated_client);
}

// ============================================================================
// TEST 2: Cloud Password Set / Email / Change (+Passport Re-encrypt) / Remove
// ============================================================================

#[tokio::test]
async fn cloud_password_set_change_passport_reencrypt_and_remove_over_mtproto() {
    struct ServerPwdState {
        has_password: bool,
        algo: Option<PasswordKdfAlgoModPow>,
        verifier: Option<[u8; 256]>,
        hint: String,
        email_unconfirmed: Option<String>,
        has_recovery: bool,
        srp_counter: i64,
        active_srp_id: Option<i64>,
        active_srp: Option<SpecSrpServer>,
        passport_plain: Option<[u8; 32]>,
        passport_settings: Option<tl_types::SecureSecretSettings>,
        fail_next_confirm_flood: bool,
        corrupt_passport_id: bool,
    }

    let initial_passport_secret = srp::generate_secret_bytes();
    let srv_state = Arc::new(Mutex::new(ServerPwdState {
        has_password: false,
        algo: None,
        verifier: None,
        hint: String::new(),
        email_unconfirmed: None,
        has_recovery: false,
        srp_counter: 1000,
        active_srp_id: None,
        active_srp: None,
        passport_plain: Some(initial_passport_secret),
        passport_settings: None,
        fail_next_confirm_flood: true,
        corrupt_passport_id: false,
    }));

    let st_handler = Arc::clone(&srv_state);
    let server = ScriptedServer::new(move |_srv, rpc| {
        let mut st = st_handler.lock().unwrap();
        match rpc.ctor {
            x if x == tl_fns::account::GetPassword::CONSTRUCTOR_ID => {
                let (cur_algo, srp_b, srp_id) = if st.has_password {
                    st.srp_counter += 1;
                    let id = st.srp_counter;
                    let algo = st.algo.clone().unwrap();
                    let verifier = st.verifier.unwrap();
                    let mut b_sec = [0x66u8; 256];
                    b_sec[0..8].copy_from_slice(&id.to_be_bytes());
                    let srp_srv = SpecSrpServer::from_verifier(
                        algo.salt1.clone(),
                        algo.salt2.clone(),
                        algo.g,
                        algo.p.clone(),
                        verifier,
                        b_sec,
                    );
                    let b_vec = srp_srv.srp_b.to_vec();
                    st.active_srp_id = Some(id);
                    st.active_srp = Some(srp_srv);
                    (
                        Some(tg_api::modpow_algo_to_tl(&algo)),
                        Some(b_vec),
                        Some(id),
                    )
                } else {
                    (None, None, None)
                };
                let pwd = sample_password_tl(
                    st.has_password,
                    cur_algo,
                    srp_b,
                    srp_id,
                    (!st.hint.is_empty()).then_some(st.hint.as_str()),
                    st.has_recovery,
                    st.passport_settings.is_some(),
                    st.email_unconfirmed.as_deref(),
                    None,
                );
                ServerAction::ok(&tl_enums::account::Password::Password(pwd))
            }
            x if x == tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID => {
                let req = tl_fns::account::GetPasswordSettings::from_bytes(&rpc.body).unwrap();
                match req.password {
                    tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(chk) => {
                        // Single-use srp_id consumption check (real Telegram behavior!).
                        if st.active_srp_id != Some(chk.srp_id) {
                            return ServerAction::err(400, "SRP_ID_INVALID");
                        }
                        let ok = st
                            .active_srp
                            .as_ref()
                            .is_some_and(|s| s.verify_client_check(&chk.a, &chk.m1));
                        st.active_srp_id = None;
                        if !ok {
                            return ServerAction::err(400, "PASSWORD_HASH_INVALID");
                        }
                        let mut sec = st.passport_settings.clone();
                        if st.corrupt_passport_id
                            && let Some(ref mut s) = sec
                        {
                            s.secure_secret_id ^= 1;
                        }
                        ServerAction::ok(&tl_enums::account::PasswordSettings::PasswordSettings(
                            tl_types::account::PasswordSettings {
                                email: st
                                    .has_recovery
                                    .then(|| "test-recovery@example.test".to_owned()),
                                secure_settings: sec
                                    .map(tl_enums::SecureSecretSettings::SecureSecretSettings),
                            },
                        ))
                    }
                    tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty => {
                        ServerAction::err(400, "PASSWORD_HASH_INVALID")
                    }
                }
            }
            x if x == tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID => {
                let req = tl_fns::account::UpdatePasswordSettings::from_bytes(&rpc.body).unwrap();
                if st.has_password {
                    let tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(chk) = req.password
                    else {
                        return ServerAction::err(400, "PASSWORD_HASH_INVALID");
                    };
                    if st.active_srp_id != Some(chk.srp_id) {
                        return ServerAction::err(400, "SRP_ID_INVALID");
                    }
                    let ok = st
                        .active_srp
                        .as_ref()
                        .is_some_and(|s| s.verify_client_check(&chk.a, &chk.m1));
                    st.active_srp_id = None;
                    if !ok {
                        return ServerAction::err(400, "PASSWORD_HASH_INVALID");
                    }
                } else {
                    assert!(matches!(
                        req.password,
                        tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty
                    ));
                }

                let tl_enums::account::PasswordInputSettings::PasswordInputSettings(ns) =
                    req.new_settings;
                if let (Some(algo), Some(hash)) = (ns.new_algo, ns.new_password_hash) {
                    if hash.is_empty() {
                        assert!(matches!(algo, tl_enums::PasswordKdfAlgo::Unknown));
                        st.has_password = false;
                        st.algo = None;
                        st.verifier = None;
                        st.hint.clear();
                    } else {
                        let tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(m) = algo else {
                            return ServerAction::err(400, "NEW_SETTINGS_INVALID");
                        };
                        assert_eq!(hash.len(), 256);
                        let mut v = [0u8; 256];
                        v.copy_from_slice(&hash);
                        st.has_password = true;
                        st.algo = Some(PasswordKdfAlgoModPow {
                            salt1: m.salt1,
                            salt2: m.salt2,
                            g: m.g,
                            p: m.p,
                        });
                        st.verifier = Some(v);
                        st.hint = ns.hint.unwrap_or_default();
                    }
                }
                if let Some(tl_enums::SecureSecretSettings::SecureSecretSettings(sec)) =
                    ns.new_secure_settings
                {
                    st.passport_settings = Some(sec);
                }
                if let Some(email) = ns.email {
                    st.email_unconfirmed = Some(format!("{}***@example.test", &email[..1]));
                    return ServerAction::err(400, "EMAIL_UNCONFIRMED_6");
                }
                ServerAction::bool_true()
            }
            x if x == tl_fns::account::ResendPasswordEmail::CONSTRUCTOR_ID => {
                ServerAction::bool_true()
            }
            x if x == tl_fns::account::ConfirmPasswordEmail::CONSTRUCTOR_ID => {
                let req = tl_fns::account::ConfirmPasswordEmail::from_bytes(&rpc.body).unwrap();
                if st.fail_next_confirm_flood {
                    st.fail_next_confirm_flood = false;
                    return ServerAction::err(420, "FLOOD_WAIT_15");
                }
                if req.code != "654321" {
                    return ServerAction::err(400, "CODE_INVALID");
                }
                st.email_unconfirmed = None;
                st.has_recovery = true;
                ServerAction::bool_true()
            }
            x if x == tl_fns::account::CancelPasswordEmail::CONSTRUCTOR_ID => {
                st.email_unconfirmed = None;
                ServerAction::bool_true()
            }
            other => panic!("unexpected RPC ctor 0x{other:08x}"),
        }
    });

    let (_client, rpc) = build_connected_rpc(&server);
    let mut mgr = CloudPasswordManager::new();

    // Step 1: Set initial 2FA password with hint and recovery email (expect EMAIL_UNCONFIRMED_6).
    let extra_salt1 = [0x11u8; 32];
    let set_res = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.set_password_with_random(
            &rpc,
            "",
            "first-test-2fa-pass",
            "first-hint",
            true,
            "recovery-fixture@example.test",
            CloudPasswordRandom {
                new_salt1_extra: Some(&extra_salt1),
                ..CloudPasswordRandom::default()
            },
        ),
    )
    .await
    .unwrap()
    .expect("initial set_password");
    assert_eq!(set_res.unconfirmed_code_length, 6);
    assert!(set_res.state.has_password);
    assert_eq!(set_res.state.hint, "first-hint");
    assert_eq!(set_res.state.unconfirmed_pattern, "r***@example.test");

    // Verify wire bytes of `account.updatePasswordSettings`: flags == 0x03 (new_algo+hash+hint & email),
    // client appended 32-byte salt1 extension, and new_password_hash == spec PH2 verifier.
    let upd_calls = server.rpcs_of(tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID);
    assert_eq!(upd_calls.len(), 1);
    let upd_req = tl_fns::account::UpdatePasswordSettings::from_bytes(&upd_calls[0]).unwrap();
    let tl_enums::account::PasswordInputSettings::PasswordInputSettings(ns) = upd_req.new_settings;
    let expected_salt1 = [b"srv_new_salt1_prefix".as_slice(), &extra_salt1].concat();
    let expected_v1 = spec_verifier(
        3,
        &GOOD_PRIME,
        &spec_ph2_hash(
            &expected_salt1,
            b"srv_new_salt2_const",
            b"first-test-2fa-pass",
        ),
    );
    assert_eq!(
        ns.new_password_hash.as_deref(),
        Some(expected_v1.as_slice())
    );
    assert_eq!(ns.hint.as_deref(), Some("first-hint"));
    assert_eq!(ns.email.as_deref(), Some("recovery-fixture@example.test"));

    // Step 2: Resend, FLOOD_WAIT_15 (surfaces immediately via handleFloodErrors), CODE_INVALID, then confirm.
    let _ = tokio::time::timeout(TEST_TIMEOUT, mgr.resend_password_email(&rpc))
        .await
        .unwrap()
        .expect("resend_password_email");
    let flood_err = tokio::time::timeout(TEST_TIMEOUT, mgr.confirm_password_email(&rpc, "654321"))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        flood_err,
        ApiError::Invocation(InvocationError::Rpc(ref r)) if r.name == "FLOOD_WAIT_15"
    ));
    let invalid_err =
        tokio::time::timeout(TEST_TIMEOUT, mgr.confirm_password_email(&rpc, "000000"))
            .await
            .unwrap()
            .unwrap_err();
    assert!(matches!(
        invalid_err,
        ApiError::Invocation(InvocationError::Rpc(ref r)) if r.name == "CODE_INVALID"
    ));
    let confirmed = tokio::time::timeout(TEST_TIMEOUT, mgr.confirm_password_email(&rpc, "654321"))
        .await
        .unwrap()
        .expect("confirm_password_email");
    assert!(confirmed.has_recovery);
    assert!(confirmed.unconfirmed_pattern.is_empty());

    // Step 3: Email-only update (`flags = 0x02`, NO password/hint fields) + cancelPasswordEmail.
    let a_email = [0x22u8; 256];
    let email_res = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.set_recovery_email(
            &rpc,
            "first-test-2fa-pass",
            "new-email@example.test",
            Some(&a_email),
        ),
    )
    .await
    .unwrap()
    .expect("set_recovery_email");
    assert_eq!(email_res.unconfirmed_code_length, 6);
    let upd_calls = server.rpcs_of(tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID);
    let email_only_req =
        tl_fns::account::UpdatePasswordSettings::from_bytes(&upd_calls[1]).unwrap();
    let tl_enums::account::PasswordInputSettings::PasswordInputSettings(ns_email) =
        email_only_req.new_settings;
    assert!(ns_email.new_algo.is_none());
    assert!(ns_email.new_password_hash.is_none());
    assert!(ns_email.hint.is_none());
    assert_eq!(ns_email.email.as_deref(), Some("new-email@example.test"));
    assert!(ns_email.new_secure_settings.is_none());
    // Also verify wire flag bit on PasswordInputSettings is literally 0x02 (f_email only).
    let raw_settings =
        tl_enums::account::PasswordInputSettings::PasswordInputSettings(ns_email).to_bytes();
    let wire_flags = u32::from_le_bytes(raw_settings[4..8].try_into().unwrap());
    assert_eq!(
        wire_flags,
        1 << 1,
        "email-only update must set flags == 0x02"
    );

    let cancelled = tokio::time::timeout(TEST_TIMEOUT, mgr.cancel_password_email(&rpc))
        .await
        .unwrap()
        .expect("cancel_password_email");
    assert!(cancelled.unconfirmed_pattern.is_empty());

    // Step 4: Seed a Telegram Passport secure secret encrypted under the current password,
    // verify corrupted secret_id aborts with SUGGEST_SECRET_RESET, then change password cleanly!
    {
        let mut st = srv_state.lock().unwrap();
        let plain = st.passport_plain.unwrap();
        let old_sec_algo = SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
            salt: b"old_passport_salt_32bytes_padded".to_vec(),
        };
        let old_hash = srp::compute_secure_secret_hash(&old_sec_algo, b"first-test-2fa-pass");
        let enc = srp::encrypt_secure_secret(&plain, &old_hash).unwrap();
        st.passport_settings = Some(tl_types::SecureSecretSettings {
            secure_algo: tg_api::secure_secret_algo_to_tl(&old_sec_algo),
            secure_secret: enc.to_vec(),
            secure_secret_id: srp::count_secure_secret_id(&plain) as i64,
        });
        st.corrupt_passport_id = true;
    }

    let a1 = [0x31u8; 256];
    let a2 = [0x32u8; 256];
    let corrupt_err = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.set_password_with_random(
            &rpc,
            "first-test-2fa-pass",
            "second-test-2fa-pass",
            "second-hint",
            false,
            "",
            CloudPasswordRandom {
                first_srp_a: Some(&a1),
                second_srp_a: Some(&a2),
                ..CloudPasswordRandom::default()
            },
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        matches!(corrupt_err, ApiError::ServerError(ref s) if s == "SUGGEST_SECRET_RESET"),
        "expected SUGGEST_SECRET_RESET, got {corrupt_err:?}"
    );
    // Ensure UpdatePasswordSettings was NOT called on Passport decrypt/id mismatch!
    assert_eq!(
        server
            .rpcs_of(tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID)
            .len(),
        2
    );

    // Now clear corruption and change password -> must fetch fresh srp_id after getPasswordSettings
    // and re-encrypt Passport secret under the new password!
    srv_state.lock().unwrap().corrupt_passport_id = false;
    let extra_salt2 = [0x22u8; 32];
    let extra_sec_salt = [0x33u8; 32];
    let changed = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.set_password_with_random(
            &rpc,
            "first-test-2fa-pass",
            "second-test-2fa-pass",
            "second-hint",
            false,
            "",
            CloudPasswordRandom {
                new_salt1_extra: Some(&extra_salt2),
                new_secure_salt_extra: Some(&extra_sec_salt),
                first_srp_a: Some(&a1),
                second_srp_a: Some(&a2),
            },
        ),
    )
    .await
    .unwrap()
    .expect("password change with Passport re-encryption");
    assert_eq!(changed.unconfirmed_code_length, 0);
    assert!(changed.state.has_password);
    assert_eq!(changed.state.hint, "second-hint");

    // Verify server stored re-encrypted Passport secret decryptable with the NEW password!
    {
        let st = srv_state.lock().unwrap();
        let sec = st.passport_settings.as_ref().unwrap();
        let new_sec_algo = tg_api::parse_secure_secret_algo(&sec.secure_algo).unwrap();
        match &new_sec_algo {
            SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt } => {
                assert_eq!(
                    salt,
                    &[b"srv_sec_salt".as_slice(), &extra_sec_salt].concat()
                );
            }
            other => panic!("expected PBKDF2 new secure algo, got {other:?}"),
        }
        let new_hash = srp::compute_secure_secret_hash(&new_sec_algo, b"second-test-2fa-pass");
        let decrypted = srp::decrypt_secure_secret(&sec.secure_secret, &new_hash)
            .expect("re-encrypted Passport secret must decrypt under new password");
        assert_eq!(decrypted, initial_passport_secret);
        assert_eq!(
            sec.secure_secret_id as u64,
            srp::count_secure_secret_id(&initial_passport_secret)
        );
    }

    // Verify `check_password` works against the newly changed password!
    let _ps = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.check_password_with_random(&rpc, "second-test-2fa-pass", Some(&a1)),
    )
    .await
    .unwrap()
    .expect("check_password with new password");

    // Step 5: Remove password (`new_password = ""`): sends PasswordKdfAlgo::Unknown + empty hash.
    let removed = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.set_password_with_random(
            &rpc,
            "second-test-2fa-pass",
            "",
            "",
            false,
            "",
            CloudPasswordRandom {
                first_srp_a: Some(&a2),
                ..CloudPasswordRandom::default()
            },
        ),
    )
    .await
    .unwrap()
    .expect("remove password");
    assert!(!removed.state.has_password);
    let upd_calls = server.rpcs_of(tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID);
    let remove_req =
        tl_fns::account::UpdatePasswordSettings::from_bytes(upd_calls.last().unwrap()).unwrap();
    let tl_enums::account::PasswordInputSettings::PasswordInputSettings(ns_rem) =
        remove_req.new_settings;
    assert!(matches!(
        ns_rem.new_algo,
        Some(tl_enums::PasswordKdfAlgo::Unknown)
    ));
    assert_eq!(ns_rem.new_password_hash.as_deref(), Some(&[][..]));
    assert_eq!(ns_rem.hint.as_deref(), Some(""));
    assert!(ns_rem.email.is_none());
}

// ============================================================================
// TEST 3: Password Recovery (`auth.*Recovery*`) & Reset Wait Period
// ============================================================================

#[tokio::test]
async fn cloud_password_recovery_and_reset_wait_period_over_mtproto() {
    let reset_step = Arc::new(AtomicU32::new(0));
    let check_step = Arc::new(AtomicU32::new(0));
    let pending_reset = Arc::new(Mutex::new(None::<i32>));
    let has_pwd = Arc::new(Mutex::new(true));

    let rs = Arc::clone(&reset_step);
    let cs = Arc::clone(&check_step);
    let pr = Arc::clone(&pending_reset);
    let hp = Arc::clone(&has_pwd);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::auth::RequestPasswordRecovery::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::auth::PasswordRecovery::PasswordRecovery(tl_types::auth::PasswordRecovery {
                email_pattern: "t***@example.test".to_owned(),
            }),
        ),
        x if x == tl_fns::auth::CheckRecoveryPassword::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::CheckRecoveryPassword::from_bytes(&rpc.body).unwrap();
            let n = cs.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                ServerAction::err(420, "FLOOD_WAIT_20")
            } else if req.code == "777888" {
                ServerAction::bool_true()
            } else {
                ServerAction::err(400, "CODE_INVALID")
            }
        }
        x if x == tl_fns::account::GetPassword::CONSTRUCTOR_ID => {
            let is_set = *hp.lock().unwrap();
            let pending = *pr.lock().unwrap();
            ServerAction::ok(&tl_enums::account::Password::Password(sample_password_tl(
                is_set, None, None, None, None, true, false, None, pending,
            )))
        }
        x if x == tl_fns::auth::RecoverPassword::CONSTRUCTOR_ID => {
            let req = tl_fns::auth::RecoverPassword::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.code, "777888");
            *hp.lock().unwrap() = req.new_settings.is_some();
            ServerAction::ok(&tl_enums::auth::Authorization::Authorization(
                tl_types::auth::Authorization {
                    setup_password_required: false,
                    otherwise_relogin_days: None,
                    tmp_sessions: None,
                    future_auth_token: None,
                    user: tl_enums::User::User(sample_tl_user(
                        SELF_UID,
                        "Test",
                        "Self",
                        Some("testself"),
                        true,
                    )),
                },
            ))
        }
        x if x == tl_fns::account::ResetPassword::CONSTRUCTOR_ID => {
            let step = rs.fetch_add(1, Ordering::Relaxed);
            match step {
                0 => {
                    *pr.lock().unwrap() = Some(1_700_604_800);
                    ServerAction::ok(
                        &tl_enums::account::ResetPasswordResult::ResetPasswordRequestedWait(
                            tl_types::account::ResetPasswordRequestedWait {
                                until_date: 1_700_604_800,
                            },
                        ),
                    )
                }
                1 => ServerAction::ok(
                    &tl_enums::account::ResetPasswordResult::ResetPasswordFailedWait(
                        tl_types::account::ResetPasswordFailedWait {
                            retry_date: 1_700_999_999,
                        },
                    ),
                ),
                _ => {
                    *pr.lock().unwrap() = None;
                    *hp.lock().unwrap() = false;
                    ServerAction::ok(&tl_enums::account::ResetPasswordResult::ResetPasswordOk)
                }
            }
        }
        x if x == tl_fns::account::DeclinePasswordReset::CONSTRUCTOR_ID => {
            *pr.lock().unwrap() = None;
            ServerAction::bool_true()
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_connected_rpc(&server);
    let mut mgr = CloudPasswordManager::new();

    // 1. Recovery email request & check (FLOOD_WAIT_20 -> CODE_INVALID -> ok).
    let pattern = tokio::time::timeout(TEST_TIMEOUT, mgr.request_password_recovery(&rpc))
        .await
        .unwrap()
        .expect("request_password_recovery");
    assert_eq!(pattern, "t***@example.test");

    let flood = tokio::time::timeout(TEST_TIMEOUT, mgr.check_recovery_password(&rpc, "777888"))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        flood,
        ApiError::Invocation(InvocationError::Rpc(ref r)) if r.name == "FLOOD_WAIT_20"
    ));
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, mgr.check_recovery_password(&rpc, "bad"))
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, mgr.check_recovery_password(&rpc, "777888"))
            .await
            .unwrap()
            .unwrap()
    );

    // 2. `auth.recoverPassword` without new password (`flags == 0`, `new_settings: None`).
    let cleared = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.recover_password(&rpc, "777888", "", "", None),
    )
    .await
    .unwrap()
    .expect("recover without new password");
    assert!(!cleared.has_password);

    // 3. `auth.recoverPassword` WITH new password + hint (`flags == 0x01`, PH2 verifier verified).
    let extra = [0x44u8; 32];
    let _recovered_new = tokio::time::timeout(
        TEST_TIMEOUT,
        mgr.recover_password(
            &rpc,
            "777888",
            "recovered-pass-123",
            "rec-hint",
            Some(&extra),
        ),
    )
    .await
    .unwrap()
    .expect("recover with new password");

    let rec_calls = server.rpcs_of(tl_fns::auth::RecoverPassword::CONSTRUCTOR_ID);
    assert_eq!(rec_calls.len(), 2);
    // First call wire flags == 0
    assert_eq!(
        u32::from_le_bytes(rec_calls[0][4..8].try_into().unwrap()),
        0
    );
    // Second call wire flags == 1 with valid PH2 hash
    assert_eq!(
        u32::from_le_bytes(rec_calls[1][4..8].try_into().unwrap()),
        1
    );
    let rec2 = tl_fns::auth::RecoverPassword::from_bytes(&rec_calls[1]).unwrap();
    let tl_enums::account::PasswordInputSettings::PasswordInputSettings(ns) =
        rec2.new_settings.unwrap();
    let expected_salt1 = [b"srv_new_salt1_prefix".as_slice(), &extra].concat();
    let expected_v = spec_verifier(
        3,
        &GOOD_PRIME,
        &spec_ph2_hash(
            &expected_salt1,
            b"srv_new_salt2_const",
            b"recovered-pass-123",
        ),
    );
    assert_eq!(ns.new_password_hash.as_deref(), Some(expected_v.as_slice()));
    assert_eq!(ns.hint.as_deref(), Some("rec-hint"));
    assert!(ns.email.is_none());

    // 4. `account.resetPassword` wait period states + `account.declinePasswordReset`.
    let w1 = tokio::time::timeout(TEST_TIMEOUT, mgr.reset_password(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        w1,
        ResetPasswordOutcome::RequestedWait {
            until_date: 1_700_604_800
        }
    );
    assert_eq!(mgr.state().unwrap().pending_reset_date, 1_700_604_800);

    let declined = tokio::time::timeout(TEST_TIMEOUT, mgr.decline_password_reset(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(declined.pending_reset_date, 0);

    let w2 = tokio::time::timeout(TEST_TIMEOUT, mgr.reset_password(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        w2,
        ResetPasswordOutcome::FailedWait {
            retry_date: 1_700_999_999
        }
    );

    let w3 = tokio::time::timeout(TEST_TIMEOUT, mgr.reset_password(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(w3, ResetPasswordOutcome::ResetOk { ref state } if !state.has_password));
}

// ============================================================================
// TEST 4: Active Sessions, TTL, Change Settings Flags, & `updateNewAuthorization`
// ============================================================================

#[tokio::test]
async fn sessions_authorizations_ttl_flags_and_new_auth_updates_over_mtproto() {
    let other_hash: i64 = 0x1122_3344_5566_7788;
    let pending_hash: i64 = 0x2233_4455_6677_8899;
    let unreviewed_hash: i64 = 0x3344_5566_7788_99AA;
    let gone_hash: i64 = 0x4455_6677_8899_AABB;
    let unreviewed_confirmed_on_server = Arc::new(Mutex::new(false));
    let ucos = Arc::clone(&unreviewed_confirmed_on_server);

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
        x if x == tl_fns::account::GetAuthorizations::CONSTRUCTOR_ID => {
            let unconf = !*ucos.lock().unwrap();
            let auths = vec![
                tl_enums::Authorization::Authorization(tl_types::Authorization {
                    current: true,
                    official_app: true,
                    password_pending: false,
                    encrypted_requests_disabled: false,
                    call_requests_disabled: true,
                    unconfirmed: false,
                    hash: 999_999_999, // server non-zero raw hash -> client must normalize to 0!
                    device_model: "Desktop".to_owned(),
                    platform: "Windows".to_owned(),
                    system_version: "11".to_owned(),
                    api_id: 2040,
                    app_name: "Telegram Desktop".to_owned(),
                    app_version: "5.0.0".to_owned(),
                    date_created: 1_700_000_000,
                    date_active: 1_700_001_000,
                    ip: "198.51.100.1".to_owned(),
                    country: "Testland".to_owned(),
                    region: "TestCity".to_owned(),
                }),
                tl_enums::Authorization::Authorization(tl_types::Authorization {
                    current: false,
                    official_app: true,
                    password_pending: false,
                    encrypted_requests_disabled: false,
                    call_requests_disabled: false,
                    unconfirmed: false,
                    hash: other_hash,
                    device_model: "Phone Fixture".to_owned(),
                    platform: "Android".to_owned(),
                    system_version: "14".to_owned(),
                    api_id: 6,
                    app_name: "Test Mobile".to_owned(),
                    app_version: "10.0".to_owned(),
                    date_created: 1_699_000_000,
                    date_active: 0, // zero date_active -> active_time falls back to date_created
                    ip: "198.51.100.2".to_owned(),
                    country: "Testland".to_owned(),
                    region: String::new(),
                }),
                tl_enums::Authorization::Authorization(tl_types::Authorization {
                    current: false,
                    official_app: false,
                    password_pending: true, // incomplete -> excluded from total()
                    encrypted_requests_disabled: false,
                    call_requests_disabled: false,
                    unconfirmed: unconf,
                    hash: pending_hash,
                    device_model: "Pending Device".to_owned(),
                    platform: "Linux".to_owned(),
                    system_version: "6.8".to_owned(),
                    api_id: 17349,
                    app_name: "Pending".to_owned(),
                    app_version: "1.0".to_owned(),
                    date_created: 1_700_000_500,
                    date_active: 1_700_000_600,
                    ip: "198.51.100.3".to_owned(),
                    country: "Testland".to_owned(),
                    region: String::new(),
                }),
            ];
            ServerAction::ok(&tl_enums::account::Authorizations::Authorizations(
                tl_types::account::Authorizations {
                    authorization_ttl_days: 180,
                    authorizations: auths,
                },
            ))
        }
        x if x == tl_fns::account::ResetAuthorization::CONSTRUCTOR_ID => {
            let req = tl_fns::account::ResetAuthorization::from_bytes(&rpc.body).unwrap();
            if req.hash == gone_hash {
                ServerAction::err(400, "HASH_INVALID")
            } else {
                ServerAction::bool_true()
            }
        }
        x if x == tl_fns::auth::ResetAuthorizations::CONSTRUCTOR_ID => ServerAction::bool_true(),
        x if x == tl_fns::account::SetAuthorizationTTL::CONSTRUCTOR_ID => ServerAction::bool_true(),
        x if x == tl_fns::account::ChangeAuthorizationSettings::CONSTRUCTOR_ID => {
            let req = tl_fns::account::ChangeAuthorizationSettings::from_bytes(&rpc.body).unwrap();
            if req.hash == gone_hash {
                ServerAction::err(400, "HASH_INVALID")
            } else {
                ServerAction::bool_true()
            }
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_connected_rpc(&server);
    let mut auths = AuthorizationsManager::new();

    // 1. `account.getAuthorizations`: current hash normalized to 0, total excludes password_pending.
    let list = tokio::time::timeout(TEST_TIMEOUT, auths.reload(&rpc, 1_700_002_000))
        .await
        .unwrap()
        .expect("reload authorizations")
        .to_vec();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].hash, 0, "current session hash must normalize to 0");
    assert!(auths.calls_disabled_here());
    assert_eq!(list[1].hash, other_hash);
    assert_eq!(
        list[1].active_time, 1_699_000_000,
        "date_active=0 must fall back to date_created"
    );
    assert!(list[2].incomplete);
    assert_eq!(
        auths.total(),
        2,
        "total() must exclude password_pending sessions"
    );
    assert_eq!(auths.ttl_days(), 180);

    // 2. `account.setAuthorizationTTL`.
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, auths.set_ttl_days(&rpc, 365))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(auths.ttl_days(), 365);
    let ttl_req = tl_fns::account::SetAuthorizationTTL::from_bytes(
        &server.rpcs_of(tl_fns::account::SetAuthorizationTTL::CONSTRUCTOR_ID)[0],
    )
    .unwrap();
    assert_eq!(ttl_req.authorization_ttl_days, 365);

    // 3. `account.changeAuthorizationSettings` exact flags:
    //    (a) toggle call_requests_disabled only -> flags == 0x02 (`1 << 1`)
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            auths.change_settings(&rpc, 0, false, None, Some(false))
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert!(!auths.calls_disabled_here());
    //    (b) toggle encrypted_requests_disabled only -> flags == 0x01 (`1 << 0`)
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            auths.change_settings(&rpc, other_hash, false, Some(true), None)
        )
        .await
        .unwrap()
        .unwrap()
    );
    let chg_calls = server.rpcs_of(tl_fns::account::ChangeAuthorizationSettings::CONSTRUCTOR_ID);
    assert_eq!(chg_calls.len(), 2);
    assert_eq!(
        u32::from_le_bytes(chg_calls[0][4..8].try_into().unwrap()),
        1 << 1
    );
    assert_eq!(
        u32::from_le_bytes(chg_calls[1][4..8].try_into().unwrap()),
        1 << 0
    );

    // 4. `account.resetAuthorization` rejects hash == 0 locally; terminates other_hash over wire.
    assert!(auths.terminate(&rpc, 0).await.is_err());
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, auths.terminate(&rpc, other_hash))
            .await
            .unwrap()
            .unwrap()
    );
    assert!(!auths.list().iter().any(|e| e.hash == other_hash));

    // 5. `auth.resetAuthorizations` terminates all other sessions while keeping current (hash == 0).
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, auths.terminate_all_other(&rpc))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(auths.list().len(), 1);
    assert_eq!(auths.list()[0].hash, 0);

    // 6. Server-pushed `updateNewAuthorization` through live `SessionRuntime` -> `SessionEvent::NewAuthorization`.
    let runtime = SessionRuntime::start_mtproto_on(
        UserId::from_tl(SELF_UID),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let mut ev_rx = runtime.subscribe_events();
    tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
        .await
        .unwrap()
        .unwrap();

    let push_auth = tl_types::UpdateNewAuthorization {
        unconfirmed: true,
        hash: unreviewed_hash,
        date: Some(1_700_002_000),
        device: Some("New Laptop Fixture".to_owned()),
        location: Some("Testville".to_owned()),
    };
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::NewAuthorization(push_auth.clone()),
            date: 1_700_002_000,
        }),
    ));

    let received_auth = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            match ev_rx.recv().await.unwrap() {
                SessionEvent::NewAuthorization(a) => return *a,
                _ => continue,
            }
        }
    })
    .await
    .expect("receive SessionEvent::NewAuthorization");
    assert_eq!(received_auth, push_auth);

    // Apply into AuthorizationsManager & verify review (confirmed=true -> flags == 1<<3),
    // HASH_INVALID resolution, and autoconfirm expiration.
    auths.apply_new_authorization(&received_auth, 1_700_002_000);
    assert_eq!(auths.unreviewed(1_700_002_000).len(), 1);

    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            auths.review_unreviewed(&rpc, unreviewed_hash, true, 1_700_002_010)
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert!(auths.unreviewed(1_700_002_010).is_empty());
    let confirm_wire = server
        .rpcs_of(tl_fns::account::ChangeAuthorizationSettings::CONSTRUCTOR_ID)
        .pop()
        .unwrap();
    assert_eq!(
        u32::from_le_bytes(confirm_wire[4..8].try_into().unwrap()),
        1 << 3,
        "confirm review must set ONLY flags.3 (confirmed)"
    );

    // Stale duplicate update with <= reviewed date must be ignored.
    auths.apply_new_authorization(&received_auth, 1_700_002_020);
    assert!(auths.unreviewed(1_700_002_020).is_empty());

    // HASH_INVALID on review removes entry as resolved.
    auths.apply_new_authorization(
        &tl_types::UpdateNewAuthorization {
            unconfirmed: true,
            hash: gone_hash,
            date: Some(1_700_002_100),
            device: Some("Gone Device".to_owned()),
            location: Some("Elsewhere".to_owned()),
        },
        1_700_002_100,
    );
    assert_eq!(auths.unreviewed(1_700_002_100).len(), 1);
    let gone_res = tokio::time::timeout(
        TEST_TIMEOUT,
        auths.review_unreviewed(&rpc, gone_hash, false, 1_700_002_105),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!gone_res);
    assert!(auths.unreviewed(1_700_002_105).is_empty());

    // Reconcile unreviewed against server `account.getAuthorizations` snapshot.
    auths.apply_new_authorization(
        &tl_types::UpdateNewAuthorization {
            unconfirmed: true,
            hash: pending_hash,
            date: Some(1_700_002_200),
            device: Some("Pending Device".to_owned()),
            location: Some("Testland".to_owned()),
        },
        1_700_002_200,
    );
    assert_eq!(auths.unreviewed(1_700_002_200).len(), 1);
    *unreviewed_confirmed_on_server.lock().unwrap() = true;
    let _ = tokio::time::timeout(TEST_TIMEOUT, auths.reload(&rpc, 1_700_002_210))
        .await
        .unwrap()
        .unwrap();
    assert!(
        auths.unreviewed(1_700_002_210).is_empty(),
        "reload must reconcile confirmed sessions out of unreviewed list"
    );

    runtime.shutdown_async().await;
}

// ============================================================================
// TEST 5: Web Authorizations (`account.getWebAuthorizations` / `resetWebAuthorization(s)`)
// ============================================================================

#[tokio::test]
async fn web_authorizations_get_and_reset_over_mtproto() {
    let bot_id: i64 = 888_001;
    let web_hash_1: i64 = 0x5566_7788_1122_3344;
    let web_hash_2: i64 = 0x6677_8899_2233_4455;

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::account::GetWebAuthorizations::CONSTRUCTOR_ID => {
            let mut bot = sample_tl_user(bot_id, "Widget", "Bot", Some("widget_test_bot"), false);
            bot.bot = true;
            ServerAction::ok(&tl_enums::account::WebAuthorizations::WebAuthorizations(
                tl_types::account::WebAuthorizations {
                    authorizations: vec![
                        tl_enums::WebAuthorization::WebAuthorization(tl_types::WebAuthorization {
                            hash: web_hash_1,
                            bot_id,
                            domain: "example.test".to_owned(),
                            browser: "Chromium".to_owned(),
                            platform: "Windows".to_owned(),
                            date_created: 1_700_000_100,
                            date_active: 1_700_000_900,
                            ip: "198.51.100.10".to_owned(),
                            region: "TestRegion".to_owned(),
                        }),
                        tl_enums::WebAuthorization::WebAuthorization(tl_types::WebAuthorization {
                            hash: web_hash_2,
                            bot_id,
                            domain: "docs.example.test".to_owned(),
                            browser: "Firefox".to_owned(),
                            platform: "Linux".to_owned(),
                            date_created: 1_700_000_200,
                            date_active: 0,
                            ip: "198.51.100.11".to_owned(),
                            region: "TestRegion".to_owned(),
                        }),
                    ],
                    users: vec![tl_enums::User::User(bot)],
                },
            ))
        }
        x if x == tl_fns::account::ResetWebAuthorization::CONSTRUCTOR_ID => {
            let req = tl_fns::account::ResetWebAuthorization::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.hash, web_hash_1);
            ServerAction::bool_true()
        }
        x if x == tl_fns::account::ResetWebAuthorizations::CONSTRUCTOR_ID => {
            ServerAction::bool_true()
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_connected_rpc(&server);
    let mut session = Session::new(UserId::from_tl(SELF_UID));
    let mut webs = WebAuthorizationsManager::new();

    let list = tokio::time::timeout(TEST_TIMEOUT, webs.reload(&rpc, &mut session))
        .await
        .unwrap()
        .expect("reload web authorizations");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].domain, "example.test");
    assert_eq!(list[0].active_time, 1_700_000_900);
    assert_eq!(list[1].active_time, 1_700_000_200);
    // Bot user from response must be applied into Session.
    assert!(session.user(UserId::from_tl(bot_id)).is_some());

    assert!(
        tokio::time::timeout(TEST_TIMEOUT, webs.terminate(&rpc, web_hash_1))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(webs.list().len(), 1);
    assert_eq!(webs.list()[0].hash, web_hash_2);

    assert!(
        tokio::time::timeout(TEST_TIMEOUT, webs.terminate_all(&rpc))
            .await
            .unwrap()
            .unwrap()
    );
    assert!(webs.list().is_empty());
}

// ============================================================================
// TEST 6: Notification Settings (Peers, Scopes, ForumTopic, Sounds, Stories,
//         Exceptions, and `updateNotifySettings` application)
// ============================================================================

#[tokio::test]
async fn notify_settings_scopes_topics_sounds_stories_exceptions_and_updates_over_mtproto() {
    let peer_uid: i64 = 555_001;
    let forum_cid: i64 = 999_001;
    let broadcast_cid: i64 = 999_002;
    let now_secs = 1_700_010_000;

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                pts: 20,
                qts: 0,
                date: now_secs,
                seq: 1,
                unread_count: 0,
            }))
        }
        x if x == tl_fns::account::GetNotifySettings::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetNotifySettings::from_bytes(&rpc.body).unwrap();
            match req.peer {
                tl_enums::InputNotifyPeer::InputNotifyUsers => {
                    ServerAction::ok(&tl_enums::PeerNotifySettings::PeerNotifySettings(
                        tl_types::PeerNotifySettings {
                            show_previews: Some(true),
                            silent: Some(false),
                            mute_until: Some(0),
                            // Verify iOS/Android sounds do NOT leak into desktop when other_sound is None!
                            ios_sound: Some(tl_enums::NotificationSound::Local(
                                tl_types::NotificationSoundLocal {
                                    title: "iOS".to_owned(),
                                    data: "100.caf".to_owned(),
                                },
                            )),
                            android_sound: Some(tl_enums::NotificationSound::Ringtone(
                                tl_types::NotificationSoundRingtone { id: 999 },
                            )),
                            other_sound: None,
                            stories_muted: Some(false),
                            stories_hide_sender: Some(true),
                            stories_ios_sound: None,
                            stories_android_sound: None,
                            stories_other_sound: Some(tl_enums::NotificationSound::Ringtone(
                                tl_types::NotificationSoundRingtone { id: 42_001 },
                            )),
                        },
                    ))
                }
                tl_enums::InputNotifyPeer::InputNotifyForumTopic(_) => {
                    ServerAction::ok(&tl_enums::PeerNotifySettings::PeerNotifySettings(
                        tl_types::PeerNotifySettings {
                            show_previews: Some(false),
                            silent: None,
                            mute_until: Some(i32::MAX),
                            ios_sound: None,
                            android_sound: None,
                            other_sound: Some(tl_enums::NotificationSound::None),
                            stories_muted: None,
                            stories_hide_sender: None,
                            stories_ios_sound: None,
                            stories_android_sound: None,
                            stories_other_sound: None,
                        },
                    ))
                }
                _ => ServerAction::ok(&default_empty_notify()),
            }
        }
        x if x == tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID => {
            ServerAction::bool_true()
        }
        x if x == tl_fns::account::GetNotifyExceptions::CONSTRUCTOR_ID => {
            let u1 = sample_tl_user(peer_uid, "Peer", "One", Some("peerone"), false);
            let self_u = sample_tl_user(SELF_UID, "Self", "User", Some("selfuser"), true);
            ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::NotifySettings(tl_types::UpdateNotifySettings {
                        peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
                        }),
                        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                            tl_types::PeerNotifySettings {
                                show_previews: Some(true),
                                silent: None,
                                mute_until: Some(now_secs + 3600),
                                ios_sound: None,
                                android_sound: None,
                                other_sound: Some(tl_enums::NotificationSound::Local(
                                    tl_types::NotificationSoundLocal {
                                        title: "Chime".to_owned(),
                                        data: "chime.ogg".to_owned(),
                                    },
                                )),
                                stories_muted: Some(true),
                                stories_hide_sender: Some(true),
                                stories_ios_sound: None,
                                stories_android_sound: None,
                                stories_other_sound: None,
                            },
                        ),
                    }),
                    // Self user exception must be filtered out by SkipAddException!
                    tl_enums::Update::NotifySettings(tl_types::UpdateNotifySettings {
                        peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: SELF_UID }),
                        }),
                        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                            tl_types::PeerNotifySettings {
                                show_previews: None,
                                silent: None,
                                mute_until: Some(i32::MAX),
                                ios_sound: None,
                                android_sound: None,
                                other_sound: Some(tl_enums::NotificationSound::Ringtone(
                                    tl_types::NotificationSoundRingtone { id: 777 },
                                )),
                                stories_muted: None,
                                stories_hide_sender: None,
                                stories_ios_sound: None,
                                stories_android_sound: None,
                                stories_other_sound: None,
                            },
                        ),
                    }),
                ],
                users: vec![tl_enums::User::User(u1), tl_enums::User::User(self_u)],
                chats: Vec::new(),
                date: now_secs,
                seq: 0,
            }))
        }
        x if x == tl_fns::messages::GetPeerDialogs::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                dialogs: vec![],
                messages: vec![],
                chats: vec![],
                users: vec![],
                state: tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 0,
                    qts: 0,
                    date: 0,
                    seq: 0,
                    unread_count: 0,
                }),
            }),
        ),
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_connected_rpc(&server);
    let mut session = Session::new(UserId::from_tl(SELF_UID));
    apply_tl_users_and_chats(
        &mut session,
        &[
            tl_enums::User::User(sample_tl_user(SELF_UID, "Self", "User", None, true)),
            tl_enums::User::User(sample_tl_user(peer_uid, "Peer", "One", None, false)),
        ],
        &[
            tl_enums::Chat::Channel(sample_tl_channel(forum_cid, "Forum", false, true)),
            tl_enums::Chat::Channel(sample_tl_channel(broadcast_cid, "News", true, false)),
        ],
    );
    let peer_pid = PeerId::from_user(UserId::from_tl(peer_uid));
    let forum_pid = PeerId::from_channel(ChannelId::from_tl(forum_cid));
    let broadcast_pid = PeerId::from_channel(ChannelId::from_tl(broadcast_cid));
    session.ensure_history(peer_pid);
    session.ensure_history(forum_pid);
    session.ensure_history(broadcast_pid);

    let mut notify_mgr = NotifySettingsManager::new();

    // 1. Regression check: `peer_notify_settings_from_tl` ignores `ios_sound`/`android_sound`.
    let users_scope = tokio::time::timeout(
        TEST_TIMEOUT,
        notify_mgr.get_notify_settings(
            &rpc,
            &mut session,
            NotifyTarget::Scope(DefaultNotifyScope::Users),
            now_secs,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        users_scope.sound, None,
        "desktop must NOT inherit ios_sound/android_sound"
    );
    assert_eq!(users_scope.stories_hide_sender, Some(true));
    assert_eq!(
        users_scope.stories_sound,
        Some(NotifySound::Ringtone(42_001))
    );

    let raw_ios_only =
        tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
            show_previews: Some(true),
            silent: None,
            mute_until: Some(0),
            ios_sound: Some(tl_enums::NotificationSound::Local(
                tl_types::NotificationSoundLocal {
                    title: "iOS".to_owned(),
                    data: "100.caf".to_owned(),
                },
            )),
            android_sound: None,
            other_sound: None,
            stories_muted: None,
            stories_hide_sender: None,
            stories_ios_sound: None,
            stories_android_sound: None,
            stories_other_sound: None,
        });
    assert_eq!(peer_notify_settings_from_tl(&raw_ios_only).sound, None);

    // 2. Forum topic notification settings (`inputNotifyForumTopic`) + inheritance.
    let topic_target = NotifyTarget::ForumTopic {
        peer: forum_pid,
        top_msg_id: MsgId(42),
    };
    let topic_ns = tokio::time::timeout(
        TEST_TIMEOUT,
        notify_mgr.get_notify_settings(&rpc, &mut session, topic_target, now_secs),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(topic_ns.mute_until, Some(i32::MAX));
    assert_eq!(topic_ns.sound, Some(NotifySound::None));
    assert!(notify_mgr.is_topic_muted(&session, forum_pid, MsgId(42), now_secs));

    // 3. `account.updateNotifySettings` across all 4 sounds, mute_until, show_previews, stories fields, and reset (`flags == 0`).
    let custom_settings = FullNotifySettings {
        show_previews: Some(false),
        silent: Some(true),
        mute_until: Some(MuteValue::Period(3600).until(now_secs)),
        sound: Some(NotifySound::Ringtone(9_876_543)),
        stories_muted: Some(true),
        stories_hide_sender: Some(true),
        stories_sound: Some(NotifySound::Local {
            title: "StoryBell".to_owned(),
            data: "story_bell.ogg".to_owned(),
        }),
    };
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            notify_mgr.update_notify_settings(
                &rpc,
                &mut session,
                NotifyTarget::Peer(peer_pid),
                &custom_settings,
                now_secs,
            )
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert!(notify_mgr.is_peer_muted(&session, peer_pid, now_secs));
    assert!(
        !notify_mgr.is_peer_muted(&session, peer_pid, now_secs + 3601),
        "timed mute must expire after period"
    );

    // Update Chats & Broadcasts scopes with Default and None sounds.
    let chats_default = FullNotifySettings {
        mute_until: Some(MuteValue::Forever.until(now_secs)),
        sound: Some(NotifySound::Default),
        show_previews: Some(true),
        ..FullNotifySettings::default()
    };
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            notify_mgr.update_notify_settings(
                &rpc,
                &mut session,
                NotifyTarget::Scope(DefaultNotifyScope::Chats),
                &chats_default,
                now_secs,
            )
        )
        .await
        .unwrap()
        .unwrap()
    );
    let broadcasts_default = FullNotifySettings {
        mute_until: Some(MuteValue::Unmute.until(now_secs)),
        sound: Some(NotifySound::None),
        silent: Some(true),
        ..FullNotifySettings::default()
    };
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            notify_mgr.update_notify_settings(
                &rpc,
                &mut session,
                NotifyTarget::Scope(DefaultNotifyScope::Broadcasts),
                &broadcasts_default,
                now_secs,
            )
        )
        .await
        .unwrap()
        .unwrap()
    );

    // Reset peer_pid back to default -> wire `InputPeerNotifySettings` must have `flags == 0`!
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            notify_mgr.reset_notify_settings(
                &rpc,
                &mut session,
                NotifyTarget::Peer(peer_pid),
                now_secs,
            )
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert!(
        !notify_mgr.is_peer_muted(&session, peer_pid, now_secs),
        "reset peer must inherit Users scope default (mute_until = 0 -> unmuted)"
    );

    let upd_calls = server.rpcs_of(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID);
    assert_eq!(upd_calls.len(), 4);
    let first_upd = tl_fns::account::UpdateNotifySettings::from_bytes(&upd_calls[0]).unwrap();
    let tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(first_s) = first_upd.settings;
    assert_eq!(first_s.show_previews, Some(false));
    assert_eq!(first_s.silent, Some(true));
    assert_eq!(first_s.mute_until, Some(now_secs + 3600));
    assert_eq!(
        first_s.sound,
        Some(tl_enums::NotificationSound::Ringtone(
            tl_types::NotificationSoundRingtone { id: 9_876_543 }
        ))
    );
    assert_eq!(first_s.stories_muted, Some(true));
    assert_eq!(first_s.stories_hide_sender, Some(true));
    assert_eq!(
        first_s.stories_sound,
        Some(tl_enums::NotificationSound::Local(
            tl_types::NotificationSoundLocal {
                title: "StoryBell".to_owned(),
                data: "story_bell.ogg".to_owned(),
            }
        ))
    );
    // Reset call wire flags == 0!
    let reset_upd = tl_fns::account::UpdateNotifySettings::from_bytes(&upd_calls[3]).unwrap();
    let reset_settings_bytes = reset_upd.settings.to_bytes();
    assert_eq!(
        u32::from_le_bytes(reset_settings_bytes[4..8].try_into().unwrap()),
        0,
        "resetToDefault must serialize InputPeerNotifySettings with flags == 0"
    );

    // 4. `account.getNotifyExceptions`: verifies wire flags + SkipAddException + Saved Messages sound rule.
    let applied = tokio::time::timeout(
        TEST_TIMEOUT,
        notify_mgr.get_notify_exceptions(
            &rpc,
            &mut session,
            Some(DefaultNotifyScope::Users),
            false,
            true,
            now_secs,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(applied, 2);
    let exc_call = &server.rpcs_of(tl_fns::account::GetNotifyExceptions::CONSTRUCTOR_ID)[0];
    let exc_flags = u32::from_le_bytes(exc_call[4..8].try_into().unwrap());
    assert_eq!(
        exc_flags,
        (1 << 0) | (1 << 2),
        "GetNotifyExceptions flags must match peer=bit0 + compare_stories=bit2"
    );
    let user_excs = notify_mgr.exceptions(DefaultNotifyScope::Users).unwrap();
    assert!(user_excs.contains(&peer_pid));
    assert!(
        !user_excs.contains(&session.self_peer_id()),
        "self peer must be skipped by SkipAddException"
    );
    assert!(skip_add_notify_exception(
        session.peer(session.self_peer_id()).unwrap()
    ));
    // Saved Messages explicitly ignores peer sound override and follows scope default (`data_notify_settings.cpp:582`).
    assert_eq!(
        notify_mgr.effective_peer_sound(&session, session.self_peer_id()),
        NotifySound::Default
    );
    assert_eq!(
        notify_mgr.effective_peer_sound(&session, peer_pid),
        NotifySound::Local {
            title: "Chime".to_owned(),
            data: "chime.ogg".to_owned(),
        }
    );

    // 5. Server-pushed `updateNotifySettings` via `SessionRuntime` / `UpdatesHandler`:
    //    scope default (`NotifyUsers`) + peer reset (`mute_until = None`) inherits scope default +
    //    `NotifyForumTopic` surfaces via `SessionEvent::NotifySettingsUpdated`.
    let runtime = SessionRuntime::start_mtproto_on(
        UserId::from_tl(SELF_UID),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .unwrap();
    let mut ev_rx = runtime.subscribe_events();
    let mut change_rx = runtime.subscribe_changes();
    tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
        .await
        .unwrap()
        .unwrap();
    {
        let mut s = runtime.session().write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(sample_tl_user(
                peer_uid, "Peer", "One", None, false,
            ))],
            &[],
        );
        let history = s.ensure_history(peer_pid);
        history.in_chat_list = true;
        history.set_folder(None);
        s.sync_dialog_lists(peer_pid);
    }

    // Push NotifyUsers scope default: muted forever!
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::NotifySettings(tl_types::UpdateNotifySettings {
                peer: tl_enums::NotifyPeer::NotifyUsers,
                notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                    tl_types::PeerNotifySettings {
                        mute_until: Some(i32::MAX),
                        show_previews: Some(true),
                        silent: None,
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
            date: now_secs,
        }),
    ));
    let ev1 = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let SessionEvent::NotifySettingsUpdated { peer, .. } = ev_rx.recv().await.unwrap() {
                return peer;
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(ev1, tl_enums::NotifyPeer::NotifyUsers));
    assert!(
        runtime
            .session()
            .read()
            .await
            .history(peer_pid)
            .unwrap()
            .muted,
        "peer inheriting Users default must become muted when NotifyUsers default changes"
    );

    // Push peer NotifySettings with only sound change (mute_until stays None -> still muted)
    // and verify PeerUpdateFlags::NOTIFICATIONS is emitted!
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::NotifySettings(tl_types::UpdateNotifySettings {
                peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
                }),
                notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                    tl_types::PeerNotifySettings {
                        mute_until: None,
                        show_previews: Some(false),
                        silent: None,
                        ios_sound: None,
                        android_sound: None,
                        other_sound: Some(tl_enums::NotificationSound::Ringtone(
                            tl_types::NotificationSoundRingtone { id: 555 },
                        )),
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_ios_sound: None,
                        stories_android_sound: None,
                        stories_other_sound: None,
                    },
                ),
            }),
            date: now_secs + 1,
        }),
    ));
    let saw_notify_flag = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let Ok(ChangeEvent::PeerUpdated { peer_id, flags }) = change_rx.recv().await
                && peer_id == peer_pid
                && flags.contains(PeerUpdateFlags::NOTIFICATIONS)
            {
                return true;
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_notify_flag);
    assert!(
        runtime
            .session()
            .read()
            .await
            .history(peer_pid)
            .unwrap()
            .muted,
        "peer with mute_until=None must still inherit muted NotifyUsers scope default"
    );

    // Push NotifyForumTopic update -> surfaced via SessionEvent::NotifySettingsUpdated.
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::NotifySettings(tl_types::UpdateNotifySettings {
                peer: tl_enums::NotifyPeer::NotifyForumTopic(tl_types::NotifyForumTopic {
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: forum_cid,
                    }),
                    top_msg_id: 42,
                }),
                notify_settings: default_empty_notify(),
            }),
            date: now_secs + 2,
        }),
    ));
    let ev_topic = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let SessionEvent::NotifySettingsUpdated { peer, .. } = ev_rx.recv().await.unwrap()
                && matches!(peer, tl_enums::NotifyPeer::NotifyForumTopic(_))
            {
                return peer;
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(ev_topic, tl_enums::NotifyPeer::NotifyForumTopic(t) if t.top_msg_id == 42));

    runtime.shutdown_async().await;
}

// ============================================================================
// TEST 7: Account Settings Sync (Content, GlobalPrivacy, TTL, Profile,
//         Username, Birthday, Personal Channel)
// ============================================================================

#[tokio::test]
async fn account_settings_sync_content_global_privacy_ttl_profile_username_birthday_channel_over_mtproto()
 {
    let can_change_content = Arc::new(Mutex::new(false));
    let gp_reject_premium_once = Arc::new(Mutex::new(true));
    let bday_flood_once = Arc::new(Mutex::new(true));
    let personal_ch_id: i64 = 444_555;

    let ccc = Arc::clone(&can_change_content);
    let grp = Arc::clone(&gp_reject_premium_once);
    let bfo = Arc::clone(&bday_flood_once);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::account::GetContentSettings::CONSTRUCTOR_ID => {
            let can_change = *ccc.lock().unwrap();
            ServerAction::ok(&tl_enums::account::ContentSettings::ContentSettings(
                tl_types::account::ContentSettings {
                    sensitive_enabled: false,
                    sensitive_can_change: can_change,
                },
            ))
        }
        x if x == tl_fns::account::SetContentSettings::CONSTRUCTOR_ID => ServerAction::bool_true(),
        x if x == tl_fns::account::GetGlobalPrivacySettings::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(
                tl_types::GlobalPrivacySettings {
                    archive_and_mute_new_noncontact_peers: true,
                    keep_archived_unmuted: false,
                    keep_archived_folders: true,
                    hide_read_marks: true,
                    new_noncontact_peers_require_premium: false,
                    display_gifts_button: true,
                    noncontact_peers_paid_stars: Some(25),
                    disallowed_gifts: Some(
                        tl_enums::DisallowedGiftsSettings::DisallowedGiftsSettings(
                            tl_types::DisallowedGiftsSettings {
                                disallow_unlimited_stargifts: true,
                                disallow_limited_stargifts: false,
                                disallow_unique_stargifts: true,
                                disallow_premium_gifts: false,
                                disallow_stargifts_from_channels: true,
                            },
                        ),
                    ),
                },
            ))
        }
        x if x == tl_fns::account::SetGlobalPrivacySettings::CONSTRUCTOR_ID => {
            let req = tl_fns::account::SetGlobalPrivacySettings::from_bytes(&rpc.body).unwrap();
            let mut reject = grp.lock().unwrap();
            if *reject {
                *reject = false;
                return ServerAction::err(403, "PREMIUM_ACCOUNT_REQUIRED");
            }
            ServerAction::ok(&req.settings)
        }
        x if x == tl_fns::account::GetAccountTTL::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::AccountDaysTTL::AccountDaysTTL(tl_types::AccountDaysTTL { days: 180 }),
        ),
        x if x == tl_fns::account::SetAccountTTL::CONSTRUCTOR_ID => ServerAction::bool_true(),
        x if x == tl_fns::account::UpdateProfile::CONSTRUCTOR_ID => {
            let req = tl_fns::account::UpdateProfile::from_bytes(&rpc.body).unwrap();
            if req.first_name.as_deref() == Some("Unchanged") {
                return ServerAction::err(400, "NAME_NOT_MODIFIED");
            }
            let first = req.first_name.unwrap_or_else(|| "Existing".to_owned());
            let last = req.last_name.unwrap_or_default();
            ServerAction::ok(&tl_enums::User::User(sample_tl_user(
                SELF_UID,
                &first,
                &last,
                Some("self_initial"),
                true,
            )))
        }
        x if x == tl_fns::account::CheckUsername::CONSTRUCTOR_ID => {
            let req = tl_fns::account::CheckUsername::from_bytes(&rpc.body).unwrap();
            match req.username.as_str() {
                "valid_free_name" => ServerAction::bool_true(),
                "taken_name" => ServerAction::bool_false(),
                "fragment_name" => ServerAction::err(400, "USERNAME_PURCHASE_AVAILABLE"),
                _ => ServerAction::err(400, "USERNAME_INVALID"),
            }
        }
        x if x == tl_fns::account::UpdateUsername::CONSTRUCTOR_ID => {
            let req = tl_fns::account::UpdateUsername::from_bytes(&rpc.body).unwrap();
            if req.username == "same_handle" {
                return ServerAction::err(400, "USERNAME_NOT_MODIFIED");
            }
            let un = (!req.username.is_empty()).then_some(req.username.as_str());
            ServerAction::ok(&tl_enums::User::User(sample_tl_user(
                SELF_UID, "Updated", "Name", un, true,
            )))
        }
        x if x == tl_fns::account::UpdateBirthday::CONSTRUCTOR_ID => {
            let mut flood = bfo.lock().unwrap();
            if *flood {
                *flood = false;
                return ServerAction::err(420, "FLOOD_WAIT_10");
            }
            ServerAction::bool_true()
        }
        x if x == tl_fns::account::UpdatePersonalChannel::CONSTRUCTOR_ID => {
            ServerAction::bool_true()
        }
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_connected_rpc(&server);
    let mut session = Session::new(UserId::from_tl(SELF_UID));
    apply_tl_users_and_chats(
        &mut session,
        &[tl_enums::User::User(sample_tl_user(
            SELF_UID,
            "Initial",
            "Self",
            Some("self_initial"),
            true,
        ))],
        &[tl_enums::Chat::Channel(sample_tl_channel(
            personal_ch_id,
            "Personal Blog",
            true,
            false,
        ))],
    );
    let mut acct = AccountSettingsManager::new();

    // 1. `account.getContentSettings` / `setContentSettings`: refuses when `sensitive_can_change == false`.
    let cs = tokio::time::timeout(TEST_TIMEOUT, acct.get_content_settings(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert!(!cs.sensitive_can_change);
    assert!(acct.set_content_settings(&rpc, true).await.is_err());
    assert!(
        server
            .rpcs_of(tl_fns::account::SetContentSettings::CONSTRUCTOR_ID)
            .is_empty(),
        "must not issue SetContentSettings RPC when sensitive_can_change is false"
    );

    *can_change_content.lock().unwrap() = true;
    let cs2 = tokio::time::timeout(TEST_TIMEOUT, acct.get_content_settings(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert!(cs2.sensitive_can_change);
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, acct.set_content_settings(&rpc, true))
            .await
            .unwrap()
            .unwrap()
    );
    let sc_call = &server.rpcs_of(tl_fns::account::SetContentSettings::CONSTRUCTOR_ID)[0];
    assert_eq!(u32::from_le_bytes(sc_call[4..8].try_into().unwrap()), 1);

    // 2. `account.getGlobalPrivacySettings` / `setGlobalPrivacySettings`:
    //    exact UnarchiveOnNewMessage mapping, always-set flags.5 & flags.6, PREMIUM_ACCOUNT_REQUIRED retry!
    let gp = tokio::time::timeout(TEST_TIMEOUT, acct.get_global_privacy_settings(&rpc))
        .await
        .unwrap()
        .unwrap();
    assert!(gp.archive_and_mute_new_noncontact_peers);
    assert_eq!(
        gp.unarchive_on_new_message,
        UnarchiveOnNewMessage::NotInFoldersUnmuted
    );
    assert!(gp.hide_read_marks);
    assert_eq!(gp.noncontact_peers_paid_stars, 25);
    assert!(gp.disallowed_gifts.unlimited);
    assert!(gp.disallowed_gifts.unique);
    assert!(gp.disallowed_gifts.from_channels);
    assert!(gp.disallowed_gifts.display_gifts_button);

    let desired_gp = GlobalPrivacyState {
        archive_and_mute_new_noncontact_peers: false,
        unarchive_on_new_message: UnarchiveOnNewMessage::None,
        hide_read_marks: true,
        new_noncontact_peers_require_premium: true,
        noncontact_peers_paid_stars: 50,
        disallowed_gifts: DisallowedGifts {
            premium: true,
            ..DisallowedGifts::default()
        },
    };
    let applied_gp = tokio::time::timeout(
        TEST_TIMEOUT,
        acct.set_global_privacy_settings(&rpc, desired_gp, true),
    )
    .await
    .unwrap()
    .unwrap();
    // Server rejected first attempt with PREMIUM_ACCOUNT_REQUIRED; client auto-retried cleanly!
    let gp_calls = server.rpcs_of(tl_fns::account::SetGlobalPrivacySettings::CONSTRUCTOR_ID);
    assert_eq!(
        gp_calls.len(),
        2,
        "must auto-retry on PREMIUM_ACCOUNT_REQUIRED"
    );
    let first_gp = tl_fns::account::SetGlobalPrivacySettings::from_bytes(&gp_calls[0]).unwrap();
    let tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(g1) = first_gp.settings;
    assert!(g1.keep_archived_unmuted && g1.keep_archived_folders);
    assert!(g1.new_noncontact_peers_require_premium);
    assert_eq!(g1.noncontact_peers_paid_stars, Some(50));
    assert!(g1.disallowed_gifts.is_some());

    let retry_gp = tl_fns::account::SetGlobalPrivacySettings::from_bytes(&gp_calls[1]).unwrap();
    let tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(g2) = retry_gp.settings;
    assert!(!g2.new_noncontact_peers_require_premium);
    // flags.5 (`noncontact_peers_paid_stars: Some(0)`) & flags.6 (`disallowed_gifts: Some(...)`) ALWAYS set!
    assert_eq!(g2.noncontact_peers_paid_stars, Some(0));
    assert!(g2.disallowed_gifts.is_some());
    assert!(g2.hide_read_marks);
    assert_eq!(
        applied_gp.unarchive_on_new_message,
        UnarchiveOnNewMessage::None
    );
    assert_eq!(applied_gp.noncontact_peers_paid_stars, 0);

    // 3. `account.getAccountTTL` / `setAccountTTL`.
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, acct.get_account_ttl(&rpc))
            .await
            .unwrap()
            .unwrap(),
        180
    );
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, acct.set_account_ttl(&rpc, 365))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(acct.account_ttl_days(), 365);

    // 4. `account.updateProfile`: name-only (`flags == 0x03`, promotes last when first empty,
    //    NAME_NOT_MODIFIED) and bio-only (`flags == 0x04`, updates local `about`).
    tokio::time::timeout(
        TEST_TIMEOUT,
        acct.update_profile_name(&rpc, &mut session, "", "Promoted"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        session.user(UserId::from_tl(SELF_UID)).unwrap().first_name,
        "Promoted"
    );
    tokio::time::timeout(
        TEST_TIMEOUT,
        acct.update_profile_name(&rpc, &mut session, "Unchanged", ""),
    )
    .await
    .unwrap()
    .expect("NAME_NOT_MODIFIED must succeed");
    tokio::time::timeout(
        TEST_TIMEOUT,
        acct.update_profile_bio(&rpc, &mut session, "Fixture bio line"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        session.peer(session.self_peer_id()).unwrap().common().about,
        "Fixture bio line"
    );

    let prof_calls = server.rpcs_of(tl_fns::account::UpdateProfile::CONSTRUCTOR_ID);
    assert_eq!(prof_calls.len(), 3);
    assert_eq!(
        u32::from_le_bytes(prof_calls[0][4..8].try_into().unwrap()),
        0x03,
        "name update must set flags == 0x03 (first_name | last_name)"
    );
    assert_eq!(
        u32::from_le_bytes(prof_calls[2][4..8].try_into().unwrap()),
        0x04,
        "bio update must set flags == 0x04 (about only)"
    );

    // 5. `account.checkUsername` & `account.updateUsername`.
    assert_eq!(
        acct.check_username(&rpc, "ab").await.unwrap(),
        UsernameCheckResult::Invalid
    );
    assert_eq!(
        acct.check_username(&rpc, "@valid_free_name").await.unwrap(),
        UsernameCheckResult::Available
    );
    assert_eq!(
        acct.check_username(&rpc, "taken_name").await.unwrap(),
        UsernameCheckResult::Occupied
    );
    assert_eq!(
        acct.check_username(&rpc, "fragment_name").await.unwrap(),
        UsernameCheckResult::PurchaseAvailable
    );
    acct.update_username(&rpc, &mut session, "@new_handle")
        .await
        .unwrap();
    assert_eq!(
        session
            .user(UserId::from_tl(SELF_UID))
            .unwrap()
            .usernames
            .editable_username(),
        "new_handle"
    );
    acct.update_username(&rpc, &mut session, "same_handle")
        .await
        .expect("USERNAME_NOT_MODIFIED must succeed");
    acct.update_username(&rpc, &mut session, "").await.unwrap();
    assert_eq!(
        session
            .user(UserId::from_tl(SELF_UID))
            .unwrap()
            .usernames
            .editable_username(),
        ""
    );

    // 6. `account.updateBirthday`: FLOOD_WAIT_10 via handleFloodErrors, year=0 omitted, clear flags==0.
    let bday_flood = acct
        .update_birthday(
            &rpc,
            &mut session,
            Some(Birthday {
                day: 15,
                month: 6,
                year: 0,
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        bday_flood,
        ApiError::Invocation(InvocationError::Rpc(ref r)) if r.name == "FLOOD_WAIT_10"
    ));
    assert!(
        acct.update_birthday(
            &rpc,
            &mut session,
            Some(Birthday {
                day: 15,
                month: 6,
                year: 0,
            }),
        )
        .await
        .unwrap()
    );
    assert!(
        acct.update_birthday(&rpc, &mut session, None)
            .await
            .unwrap()
    );
    assert!(
        session
            .user(UserId::from_tl(SELF_UID))
            .unwrap()
            .birthday
            .is_none()
    );
    let bday_calls = server.rpcs_of(tl_fns::account::UpdateBirthday::CONSTRUCTOR_ID);
    assert_eq!(bday_calls.len(), 3);
    let bday_set = tl_fns::account::UpdateBirthday::from_bytes(&bday_calls[1]).unwrap();
    let Some(tl_enums::Birthday::Birthday(bd)) = bday_set.birthday else {
        panic!("expected Some(Birthday)");
    };
    assert_eq!((bd.day, bd.month, bd.year), (15, 6, None));
    assert_eq!(
        u32::from_le_bytes(bday_calls[2][4..8].try_into().unwrap()),
        0,
        "clearing birthday must serialize flags == 0"
    );

    // 7. `account.updatePersonalChannel`: InputChannel with access_hash vs InputChannel::Empty.
    let cid = ChannelId::from_tl(personal_ch_id);
    assert!(
        acct.update_personal_channel(&rpc, &mut session, Some(cid))
            .await
            .unwrap()
    );
    assert_eq!(
        session
            .user(UserId::from_tl(SELF_UID))
            .unwrap()
            .personal_channel_id,
        Some(cid)
    );
    assert!(
        acct.update_personal_channel(&rpc, &mut session, None)
            .await
            .unwrap()
    );
    assert_eq!(
        session
            .user(UserId::from_tl(SELF_UID))
            .unwrap()
            .personal_channel_id,
        None
    );
    let ch_calls = server.rpcs_of(tl_fns::account::UpdatePersonalChannel::CONSTRUCTOR_ID);
    assert_eq!(ch_calls.len(), 2);
    let set_ch = tl_fns::account::UpdatePersonalChannel::from_bytes(&ch_calls[0]).unwrap();
    assert!(matches!(
        set_ch.channel,
        tl_enums::InputChannel::InputChannel(c)
            if c.channel_id == personal_ch_id && c.access_hash == personal_ch_id * 1_000 + 9
    ));
    let clr_ch = tl_fns::account::UpdatePersonalChannel::from_bytes(&ch_calls[1]).unwrap();
    assert!(matches!(clr_ch.channel, tl_enums::InputChannel::Empty));
}

// ============================================================================
// TEST 8: `account.updateStatus` Timing, `updateUser*` Self Updates, & Local Settings Mirror
// ============================================================================

#[tokio::test]
async fn presence_update_status_timing_self_updates_and_local_settings_mirror_over_mtproto() {
    let now_secs = 1_700_020_000;

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::updates::State::State(tl_types::updates::State {
                pts: 50,
                qts: 0,
                date: now_secs,
                seq: 1,
                unread_count: 0,
            }))
        }
        x if x == tl_fns::account::UpdateStatus::CONSTRUCTOR_ID => ServerAction::bool_true(),
        other => panic!("unexpected RPC ctor 0x{other:08x}"),
    });

    let (_client, rpc) = build_connected_rpc(&server);
    let mut session = Session::new(UserId::from_tl(SELF_UID));
    apply_tl_users_and_chats(
        &mut session,
        &[tl_enums::User::User(sample_tl_user(
            SELF_UID,
            "Self",
            "Old",
            Some("old_self"),
            true,
        ))],
        &[],
    );

    let mut handler = UpdatesHandler::new();
    handler.initialize(&rpc, &mut session, 0).await.unwrap();
    handler.set_online_config(
        DEFAULT_ONLINE_UPDATE_PERIOD_MS,
        DEFAULT_OFFLINE_IDLE_TIMEOUT_MS,
    );

    // 1. Focus gained (active, non-idle at t = 1000ms) -> sends `offline: false`.
    let r1 = handler
        .update_online_timed(&rpc, &mut session, true, 1_000, 1_000, now_secs, false)
        .await
        .unwrap();
    assert!(r1.sent);
    assert!(r1.online);
    assert_eq!(r1.next_update_in_ms, DEFAULT_OFFLINE_IDLE_TIMEOUT_MS);
    assert_eq!(r1.idle_finish_check_ms, None);
    assert!(handler.is_quit_prevent());

    // 2. Still active & non-idle 10s later (< 120s period) -> MUST NOT resend!
    let r2 = handler
        .update_online_timed(
            &rpc,
            &mut session,
            true,
            11_000,
            11_000,
            now_secs + 10,
            false,
        )
        .await
        .unwrap();
    assert!(
        !r2.sent,
        "must not spam updateStatus before onlineUpdatePeriod"
    );
    assert!(r2.online);
    assert_eq!(
        server
            .rpcs_of(tl_fns::account::UpdateStatus::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // 3. Another device sends `updateUserStatus(self, Offline)` -> emits SelfOtherOffline
    //    and `got_other_offline = true` immediately re-asserts `offline: false` even inside 120s!
    handler
        .handle_updates(
            &rpc,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::UserStatus(tl_types::UpdateUserStatus {
                    user_id: SELF_UID,
                    status: tl_enums::UserStatus::Offline(tl_types::UserStatusOffline {
                        was_online: now_secs + 12,
                    }),
                }),
                date: now_secs + 12,
            }),
            None,
            13_000,
        )
        .await
        .unwrap();
    assert_eq!(handler.other_online(), now_secs + 12);
    assert!(
        handler
            .drain_events()
            .contains(&UpdateEvent::SelfOtherOffline {
                was_online: Some(now_secs + 12)
            })
    );
    let r3 = handler
        .update_online_timed(
            &rpc,
            &mut session,
            true,
            13_000,
            13_000,
            now_secs + 12,
            true,
        )
        .await
        .unwrap();
    assert!(
        r3.sent,
        "must re-assert online immediately when another device goes offline"
    );
    assert!(r3.online);

    // 4. Periodic keepalive after `onlineUpdatePeriod` (120s after t=13_000 -> t=133_000).
    let r4 = handler
        .update_online_timed(
            &rpc,
            &mut session,
            true,
            133_000,
            133_000,
            now_secs + 132,
            false,
        )
        .await
        .unwrap();
    assert!(r4.sent, "must send keepalive at onlineUpdatePeriod");
    assert!(r4.online);

    // 5. Window stays active, user goes idle for >= 30s (`offlineIdleTimeout`) ->
    //    transitions `is_idle = true`, schedules 900ms idle finish check, sends `offline: true`!
    let r5 = handler
        .update_online_timed(
            &rpc,
            &mut session,
            true,
            133_000,
            163_000,
            now_secs + 162,
            false,
        )
        .await
        .unwrap();
    assert!(r5.sent);
    assert!(!r5.online);
    assert!(handler.is_idle());
    assert_eq!(r5.idle_finish_check_ms, Some(IDLE_FINISH_CHECK_MS));
    assert!(!handler.is_quit_prevent());

    // 6. Still idle 5s later -> MUST NOT spam `offline: true`!
    let r6 = handler
        .update_online_timed(
            &rpc,
            &mut session,
            true,
            133_000,
            168_000,
            now_secs + 167,
            false,
        )
        .await
        .unwrap();
    assert!(!r6.sent, "must not resend offline while already offline");

    // 7. `check_idle_finish` while still idle returns None; once user moves, returns Some(online=true)!
    assert!(
        handler
            .check_idle_finish(&rpc, &mut session, true, 133_000, 168_900, now_secs + 168)
            .await
            .unwrap()
            .is_none()
    );
    let r7 = handler
        .check_idle_finish(&rpc, &mut session, true, 169_500, 169_800, now_secs + 169)
        .await
        .unwrap()
        .expect("user return from idle must resume online");
    assert!(r7.sent && r7.online);
    assert!(!handler.is_idle());

    // Verify exact `offline` boolean sequence on wire: [false, false, false, true, false].
    let status_calls: Vec<bool> = server
        .rpcs_of(tl_fns::account::UpdateStatus::CONSTRUCTOR_ID)
        .iter()
        .map(|b| {
            tl_fns::account::UpdateStatus::from_bytes(b)
                .unwrap()
                .offline
        })
        .collect();
    assert_eq!(status_calls, vec![false, false, false, true, false]);

    // 8. Live `SessionRuntime` quit-prevent offline status + `updateUser*` self updates!
    let runtime = SessionRuntime::start_mtproto_on(
        UserId::from_tl(SELF_UID),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .unwrap();
    let mut ev_rx = runtime.subscribe_events();
    let mut change_rx = runtime.subscribe_changes();
    tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
        .await
        .unwrap()
        .unwrap();

    // Seed full-loaded self user.
    {
        let mut s = runtime.session().write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(sample_tl_user(
                SELF_UID,
                "Self",
                "Old",
                Some("old_self"),
                true,
            ))],
            &[],
        );
        apply_tl_user_full(
            &mut s,
            &tl_enums::UserFull::UserFull(tl_types::UserFull {
                blocked: false,
                phone_calls_available: false,
                phone_calls_private: false,
                can_pin_message: true,
                has_scheduled: false,
                video_calls_available: false,
                voice_messages_forbidden: false,
                translations_disabled: false,
                stories_pinned_available: false,
                blocked_my_stories_from: false,
                wallpaper_overridden: false,
                contact_require_premium: false,
                read_dates_private: false,
                sponsored_enabled: false,
                can_view_revenue: false,
                bot_can_manage_emoji_status: false,
                display_gifts_button: false,
                noforwards_my_enabled: false,
                noforwards_peer_enabled: false,
                unofficial_security_risk: false,
                id: SELF_UID,
                about: Some("Old bio".to_owned()),
                settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
                    report_spam: false,
                    add_contact: false,
                    block_contact: false,
                    share_contact: false,
                    need_contacts_exception: false,
                    report_geo: false,
                    autoarchived: false,
                    invite_members: false,
                    request_chat_broadcast: false,
                    business_bot_paused: false,
                    business_bot_can_reply: false,
                    geo_distance: None,
                    request_chat_title: None,
                    request_chat_date: None,
                    business_bot_id: None,
                    business_bot_manage_url: None,
                    charge_paid_message_stars: None,
                    registration_month: None,
                    phone_country: None,
                    name_change_date: None,
                    photo_change_date: None,
                }),
                personal_photo: None,
                profile_photo: None,
                fallback_photo: None,
                notify_settings: default_empty_notify(),
                bot_info: None,
                pinned_msg_id: None,
                common_chats_count: 0,
                folder_id: None,
                ttl_period: None,
                theme: None,
                private_forward_name: None,
                bot_group_admin_rights: None,
                bot_broadcast_admin_rights: None,
                wallpaper: None,
                stories: None,
                business_work_hours: None,
                business_location: None,
                business_greeting_message: None,
                business_away_message: None,
                business_intro: None,
                birthday: None,
                personal_channel_id: None,
                personal_channel_message: None,
                stargifts_count: None,
                starref_program: None,
                bot_verification: None,
                send_paid_messages_stars: None,
                disallowed_gifts: None,
                stars_rating: None,
                stars_my_pending_rating: None,
                stars_my_pending_rating_date: None,
                main_tab: None,
                saved_music: None,
                note: None,
                bot_manager_id: None,
                share_theme: None,
                wallet_card: None,
            }),
        );
        assert_eq!(
            s.user(UserId::from_tl(SELF_UID))
                .unwrap()
                .common
                .loaded_status,
            LoadedStatus::Full
        );
    }

    // Mark runtime online via `update_presence(true, 0)`.
    let rt_pres = runtime.update_presence(true, 0).await.unwrap();
    assert!(rt_pres.sent && rt_pres.online);

    // Push `updateUserName`, `updateUserPhone`, `updateUserEmojiStatus`, `updateUser` in one batch!
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![
                tl_enums::Update::UserName(tl_types::UpdateUserName {
                    user_id: SELF_UID,
                    first_name: "New\nFirst".to_owned(),
                    last_name: "Last".to_owned(),
                    usernames: vec![
                        tl_enums::Username::Username(tl_types::Username {
                            editable: false,
                            active: true,
                            deletable: false,
                            expired: false,
                            username: "collectible_one".to_owned(),
                        }),
                        tl_enums::Username::Username(tl_types::Username {
                            editable: true,
                            active: false,
                            deletable: false,
                            expired: false,
                            username: "inactive_old".to_owned(),
                        }),
                        tl_enums::Username::Username(tl_types::Username {
                            editable: true,
                            active: true,
                            deletable: false,
                            expired: false,
                            username: "active_editable".to_owned(),
                        }),
                    ],
                }),
                tl_enums::Update::UserPhone(tl_types::UpdateUserPhone {
                    user_id: SELF_UID,
                    phone: "9991112233".to_owned(),
                }),
                tl_enums::Update::UserEmojiStatus(tl_types::UpdateUserEmojiStatus {
                    user_id: SELF_UID,
                    emoji_status: tl_enums::EmojiStatus::EmojiStatus(tl_types::EmojiStatus {
                        document_id: 5_432_100,
                        until: Some(now_secs + 3600),
                    }),
                }),
                tl_enums::Update::User(tl_types::UpdateUser { user_id: SELF_UID }),
            ],
            users: Vec::new(),
            chats: Vec::new(),
            date: now_secs + 200,
            seq: 2,
        }),
    ));

    // Await `SessionEvent::UserFullRefreshNeeded`.
    let refresh_uid = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let SessionEvent::UserFullRefreshNeeded { user_id } = ev_rx.recv().await.unwrap() {
                return user_id;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(refresh_uid, UserId::from_tl(SELF_UID));

    // Verify all 3 peer update flags (NAME|USERNAME|USERNAMES, PHONE_NUMBER, EMOJI_STATUS) fired.
    let mut seen_flags = PeerUpdateFlags::empty();
    while let Ok(ev) = change_rx.try_recv() {
        if let ChangeEvent::PeerUpdated { peer_id, flags } = ev
            && peer_id == PeerId::from_user(UserId::from_tl(SELF_UID))
        {
            seen_flags |= flags;
        }
    }
    assert!(
        seen_flags.contains(
            PeerUpdateFlags::NAME | PeerUpdateFlags::USERNAME | PeerUpdateFlags::USERNAMES
        )
    );
    assert!(seen_flags.contains(PeerUpdateFlags::PHONE_NUMBER));
    assert!(seen_flags.contains(PeerUpdateFlags::EMOJI_STATUS));

    {
        let s = runtime.session().read().await;
        let u = s.user(UserId::from_tl(SELF_UID)).unwrap();
        assert_eq!(u.first_name, "New First");
        assert_eq!(u.last_name, "Last");
        assert_eq!(u.usernames.editable_username(), "active_editable");
        assert_eq!(u.phone, "9991112233");
        assert_eq!(
            u.common.emoji_status.document_id,
            DocumentId::from_tl(5_432_100)
        );
        assert_eq!(u.common.emoji_status.until, now_secs + 3600);
    }

    // Shutdown while online -> must send quit-prevent `account.updateStatus(offline: true)`!
    let count_before_shutdown = server
        .rpcs_of(tl_fns::account::UpdateStatus::CONSTRUCTOR_ID)
        .len();
    runtime.shutdown_async().await;
    let all_status = server.rpcs_of(tl_fns::account::UpdateStatus::CONSTRUCTOR_ID);
    assert_eq!(all_status.len(), count_before_shutdown + 1);
    let last_status =
        tl_fns::account::UpdateStatus::from_bytes(all_status.last().unwrap()).unwrap();
    assert!(
        last_status.offline,
        "shutdown while online must send quit-prevent account.updateStatus(offline: true)"
    );

    // 9. Verify `tg-core` local settings mirrors round-trip cleanly with tdesktop QDataStream format.
    let local = CoreSettings {
        sound_notify: false,
        desktop_notify: true,
        flash_bounce_notify: false,
        notify_view: NotifyView::ShowName,
        notify_about_pinned: false,
        notify_from_all: false,
        custom_device_model: "Rustgram Conformance Fixture".to_owned(),
        ..CoreSettings::default()
    };
    let blob = local.serialize();
    let mut restored = CoreSettings::default();
    restored
        .add_from_serialized(&blob, tg_core::version::APP_VERSION as i32)
        .unwrap();
    assert!(!restored.sound_notify);
    assert!(restored.desktop_notify);
    assert!(!restored.flash_bounce_notify);
    assert_eq!(restored.notify_view, NotifyView::ShowName);
    assert!(!restored.notify_about_pinned);
    assert!(!restored.notify_from_all);
    assert_eq!(restored.custom_device_model, "Rustgram Conformance Fixture");
}
