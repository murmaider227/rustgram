//! Byte-level tdesktop conformance suite for Payments, Telegram Stars, Star Gifts, and Telegram
//! Passport against an in-process scripted fake Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Security & money-safety notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Never initiates real payments or moves Stars/TON; all payment/withdrawal RPCs are asserted
//!   strictly on wire encoding against the in-process fake MTProto 2.0 server.
//! - Uses synthetic test fixtures only (fake `api_id = 1`, fake passwords, fictional names,
//!   ephemeral test-only RSA keypair).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{ApiCredentials, InvocationError, RpcClient};
use tg_crypto::aes::{aes_cbc_decrypt, aes_cbc_encrypt};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::hash::{sha1, sha256, sha512};
use tg_crypto::rsa::PublicKey;
use tg_crypto::srp::{
    PasswordKdfAlgoModPow, SecureSecretAlgo, check_secret_bytes, compute_check,
    compute_secure_secret_hash, count_secure_secret_id, decrypt_secure_secret,
    encrypt_secure_secret,
};
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, pbkdf2_hmac_sha512_64};
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

// ============================================================================
// Test-only constants (fake credentials, ephemeral test RSA key)
// ============================================================================

const FAKE_API_ID: i32 = 1;
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Test-only 2048-bit RSA modulus `n` (ephemeral fixture for in-process DH & OAEP tests only).
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

/// Independent RSAES-OAEP-SHA1 decryption oracle using `TEST_RSA_D_HEX` for verifying
/// `account.acceptAuthorization` bot credentials encryption on the fake server.
fn spec_decrypt_rsa_oaep_sha1(ciphertext: &[u8]) -> Vec<u8> {
    let n = hex_decode(TEST_RSA_N_HEX);
    let d = hex_decode(TEST_RSA_D_HEX);
    let em = pad_be_256(&mod_exp(ciphertext, &d, &n).expect("RSA private modexp"));
    assert_eq!(em[0], 0x00, "OAEP leading zero byte");
    let h_len = 20usize;
    let masked_seed = &em[1..1 + h_len];
    let masked_db = &em[1 + h_len..];

    let mgf1 = |seed: &[u8], out_len: usize| -> Vec<u8> {
        let mut t = Vec::with_capacity(out_len + h_len);
        let mut counter = 0u32;
        while t.len() < out_len {
            let c = counter.to_be_bytes();
            t.extend_from_slice(&sha1(&[seed, &c]));
            counter += 1;
        }
        t.truncate(out_len);
        t
    };

    let seed_mask = mgf1(masked_db, h_len);
    let mut seed = vec![0u8; h_len];
    for i in 0..h_len {
        seed[i] = masked_seed[i] ^ seed_mask[i];
    }
    let db_mask = mgf1(&seed, masked_db.len());
    let mut db = vec![0u8; masked_db.len()];
    for i in 0..masked_db.len() {
        db[i] = masked_db[i] ^ db_mask[i];
    }
    let l_hash = sha1(&[&[]]);
    assert_eq!(&db[..h_len], &l_hash, "OAEP label hash mismatch");
    let sep_pos = db[h_len..]
        .iter()
        .position(|&b| b != 0)
        .expect("OAEP separator");
    assert_eq!(db[h_len + sep_pos], 0x01, "OAEP 0x01 delimiter");
    db[h_len + sep_pos + 1..].to_vec()
}

// ============================================================================
// Independent Reference SRP-6a Oracle
// ============================================================================

fn spec_ph2_hash(salt1: &[u8], salt2: &[u8], password: &[u8]) -> [u8; 32] {
    let h1 = sha256(&[salt1, password, salt1]);
    let h2 = sha256(&[salt2, &h1, salt2]);
    let h3 = pbkdf2_hmac_sha512_64(&h2, salt1, 100_000);
    sha256(&[salt2, &h3, salt2])
}

fn spec_verifier(g: u8, p: &[u8], x: &[u8; 32]) -> [u8; 256] {
    pad_be_256(&mod_exp(&[g], x, p).expect("g^x mod p"))
}

struct SpecSrpServer {
    salt1: Vec<u8>,
    salt2: Vec<u8>,
    g: i32,
    p: Vec<u8>,
    verifier_v: [u8; 256],
    server_b_secret: [u8; 256],
    srp_b: [u8; 256],
    srp_id: i64,
}

impl SpecSrpServer {
    fn new(password: &[u8], srp_id: i64) -> Self {
        let salt1 = b"wp55-salt-one-1234".to_vec();
        let salt2 = b"wp55-salt-two-5678".to_vec();
        let g = 3i32;
        let p = GOOD_PRIME.to_vec();
        let x = spec_ph2_hash(&salt1, &salt2, password);
        let verifier_v = spec_verifier(g as u8, &p, &x);
        let mut server_b_secret = [0x33u8; 256];
        server_b_secret[0] = 0x11;
        server_b_secret[255] = 0x77;

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
            srp_id,
        }
    }

    fn kdf_algo(&self) -> PasswordKdfAlgoModPow {
        PasswordKdfAlgoModPow {
            salt1: self.salt1.clone(),
            salt2: self.salt2.clone(),
            g: self.g,
            p: self.p.clone(),
        }
    }

    fn to_password_tl(&self, has_secure_values: bool) -> tl_enums::account::Password {
        tl_enums::account::Password::Password(tl_types::account::Password {
            has_recovery: true,
            has_secure_values,
            has_password: true,
            current_algo: Some(
                tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                    tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                        salt1: self.salt1.clone(),
                        salt2: self.salt2.clone(),
                        g: self.g,
                        p: self.p.clone(),
                    },
                ),
            ),
            srp_b: Some(self.srp_b.to_vec()),
            srp_id: Some(self.srp_id),
            hint: Some("synthetic hint".to_owned()),
            email_unconfirmed_pattern: None,
            new_algo: tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                    salt1: self.salt1.clone(),
                    salt2: self.salt2.clone(),
                    g: self.g,
                    p: self.p.clone(),
                },
            ),
            new_secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                    salt: b"wp55-sec-salt".to_vec(),
                },
            ),
            secure_random: vec![0x42; 32],
            pending_reset_date: None,
            login_email_pattern: None,
        })
    }

    fn verify_tl_check(&self, check: &tl_enums::InputCheckPasswordSRP) -> bool {
        let tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(c) = check else {
            return false;
        };
        if c.srp_id != self.srp_id || c.a.len() != 256 || c.m1.len() != 32 {
            return false;
        }
        let p_256 = pad_be_256(&self.p);
        let g_256 = pad_be_256(&(self.g as u32).to_be_bytes());
        let u_bytes = sha256(&[&c.a, &self.srp_b]);
        let v_u = pad_be_256(&mod_exp(&self.verifier_v, &u_bytes, &self.p).unwrap());
        let a_vu = be_mod_mul(&c.a, &v_u, &self.p);
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
            &c.a,
            &self.srp_b,
            &k_session,
        ]);
        expected_m1.as_slice() == c.m1.as_slice()
    }
}

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
        if !(cur[0] > 0 || &cur[1..] >= p.as_slice()) {
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
// Scripted Fake MTProto 2.0 Server
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

    fn spawn_connection(&self, conn_id: u64, _protocol_dc_id: i16, mut peer: FakeServerPeer) {
        let server = self.clone();

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

            while let Some(raw) = peer.rx.recv().await {
                if raw.len() < 8 {
                    break;
                }
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
                        server_nonce[8..16]
                            .copy_from_slice(&0xA55A_C33C_1234_5678u64.to_le_bytes());
                        let res_pq = mtp_enums::ResPQ::ResPQ(mtp_types::ResPQ {
                            nonce: req.nonce,
                            server_nonce,
                            pq: vec![0x01, 0x87], // 17 * 23
                            server_public_key_fingerprints: vec![
                                server.inner.rsa_pub.fingerprint() as i64,
                            ],
                        });
                        let out_id = next_server_msg_id(&mut server_msg_counter, true);
                        let _ = peer
                            .tx
                            .send(Ok(encode_unencrypted_packet(out_id, &res_pq.to_bytes())));
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
                        let out_id = next_server_msg_id(&mut server_msg_counter, true);
                        let _ = peer
                            .tx
                            .send(Ok(encode_unencrypted_packet(out_id, &resp.to_bytes())));
                    } else if ctor == mtp_functions::SetClientDHParams::CONSTRUCTOR_ID {
                        let req = mtp_functions::SetClientDHParams::from_bytes(body).unwrap();
                        let pending = dh_pending.take().unwrap();
                        let dec =
                            aes_ige_decrypt(&req.encrypted_data, &pending.aes_key, &pending.aes_iv);
                        let mut dec_cur = Cursor::new(&dec[20..]);
                        let mtp_enums::ClientDHInnerData::ClientDHInnerData(cdh) =
                            mtp_enums::ClientDHInnerData::deserialize(&mut dec_cur).unwrap();
                        let shared =
                            pad_be_256(&mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME).unwrap());
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
                        let out_id = next_server_msg_id(&mut server_msg_counter, true);
                        let _ = peer
                            .tx
                            .send(Ok(encode_unencrypted_packet(out_id, &ok.to_bytes())));
                    }
                    continue;
                }

                let Some(entry) = server.inner.keys.lock().unwrap().get(&auth_key_id).cloned()
                else {
                    let _ = peer.tx.send(Err(TransportError::ServerError(
                        TransportErrorCode::AuthKeyNotFound,
                    )));
                    break;
                };
                let dec = parse_encrypted_packet(&raw, &entry.key, Side::Client)
                    .expect("decrypt MTProto 2.0 packet");
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
                        let ping = mtp_functions::PingDelayDisconnect::from_bytes(&body).unwrap();
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

                    let inner_body = unwrap_init_connection(&body);
                    let inner_ctor = u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                    server.inner.rpcs.lock().unwrap().push(RpcRecord {
                        ctor: inner_ctor,
                        body: inner_body.clone(),
                    });

                    let handler = server.inner.handler.lock().unwrap().clone();
                    let action = handler(
                        &server,
                        IncomingRpc {
                            ctor: inner_ctor,
                            body: inner_body,
                        },
                    );
                    let result_payload = match action {
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
                        result: RawObject(result_payload),
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

fn make_deterministic_passport_secret(mut base: [u8; 32]) -> [u8; 32] {
    let sum: u64 = base.iter().map(|&b| u64::from(b)).sum();
    let modulo = sum % 255;
    let add = 255 + 239 - modulo;
    base[0] = ((u64::from(base[0]) + add) % 255) as u8;
    assert!(check_secret_bytes(&base));
    base
}

fn sample_invoice(currency: &str, amount: i64, flexible: bool) -> tl_enums::Invoice {
    tl_enums::Invoice::Invoice(tl_types::Invoice {
        test: true,
        name_requested: flexible,
        phone_requested: false,
        email_requested: flexible,
        shipping_address_requested: flexible,
        flexible,
        phone_to_provider: false,
        email_to_provider: false,
        recurring: false,
        currency: currency.to_owned(),
        prices: vec![tl_enums::LabeledPrice::LabeledPrice(
            tl_types::LabeledPrice {
                label: "Sample Item".to_owned(),
                amount,
            },
        )],
        max_tip_amount: Some(500),
        suggested_tip_amounts: Some(vec![100, 250]),
        terms_url: None,
        subscription_period: None,
    })
}

fn sample_unique_gift(
    id: i64,
    gift_id: i64,
    slug: &str,
    title: &str,
    num: i32,
) -> tl_enums::StarGift {
    tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
        require_premium: false,
        resale_ton_only: false,
        theme_available: true,
        burned: false,
        crafted: false,
        id,
        gift_id,
        title: title.to_owned(),
        slug: slug.to_owned(),
        num,
        owner_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: 777_001,
        })),
        owner_name: Some("Synthetic Owner".to_owned()),
        owner_address: None,
        attributes: vec![
            tl_enums::StarGiftAttribute::Model(tl_types::StarGiftAttributeModel {
                crafted: false,
                name: "Emerald".to_owned(),
                document: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 9001 }),
                rarity: tl_enums::StarGiftAttributeRarity::Rare,
            }),
            tl_enums::StarGiftAttribute::Pattern(tl_types::StarGiftAttributePattern {
                name: "Crown".to_owned(),
                document: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 9002 }),
                rarity: tl_enums::StarGiftAttributeRarity::StarGiftAttributeRarity(
                    tl_types::StarGiftAttributeRarity { permille: 50 },
                ),
            }),
            tl_enums::StarGiftAttribute::Backdrop(tl_types::StarGiftAttributeBackdrop {
                name: "Midnight".to_owned(),
                backdrop_id: 7,
                center_color: 0x102030,
                edge_color: 0x050A15,
                pattern_color: 0x405060,
                text_color: 0xFFFFFF,
                rarity: tl_enums::StarGiftAttributeRarity::Epic,
            }),
        ],
        availability_issued: 10,
        availability_total: 100,
        gift_address: None,
        resell_amount: None,
        released_by: None,
        value_amount: Some(5000),
        value_currency: Some("USD".to_owned()),
        value_usd_amount: Some(5000),
        theme_peer: None,
        peer_color: None,
        host_id: None,
        offer_min_stars: Some(200),
        craft_chance_permille: Some(250),
    })
}

fn sample_saved_star_gift(
    id: i64,
    gift_id: i64,
    slug: &str,
    msg_id: i32,
) -> tl_enums::SavedStarGift {
    tl_enums::SavedStarGift::SavedStarGift(tl_types::SavedStarGift {
        name_hidden: false,
        unsaved: false,
        refunded: false,
        can_upgrade: false,
        pinned_to_top: false,
        upgrade_separate: false,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: 777_002,
        })),
        date: 1_750_000_000,
        gift: sample_unique_gift(id, gift_id, slug, "Crystal Gem", 7),
        message: Some(tl_enums::TextWithEntities::TextWithEntities(
            tl_types::TextWithEntities {
                text: "Enjoy!".to_owned(),
                entities: Vec::new(),
            },
        )),
        msg_id: Some(msg_id),
        saved_id: None,
        convert_stars: None,
        upgrade_stars: None,
        can_export_at: Some(1_740_000_000),
        transfer_stars: Some(25),
        can_transfer_at: Some(1_740_000_000),
        can_resell_at: Some(1_740_000_000),
        collection_id: Some(vec![1]),
        prepaid_upgrade_hash: None,
        drop_original_details_stars: None,
        gift_num: Some(7),
        can_craft_at: Some(1_740_000_000),
    })
}

fn count_hash_u64(values: &[u64]) -> i64 {
    let mut hash = 0u64;
    for &v in values {
        hash ^= hash >> 21;
        hash ^= hash << 35;
        hash ^= hash >> 4;
        hash = hash.wrapping_add(v);
    }
    hash as i64
}

// ============================================================================
// 1. Payments Form, ValidateRequestedInfo, SendPaymentForm (New, Saved+SRP,
//    3DS VerificationNeeded), SendStarsForm & Stars Subscriptions Conformance
// ============================================================================

#[tokio::test]
async fn payments_checkout_stars_and_subscriptions_conformance() {
    let srp = Arc::new(SpecSrpServer::new(b"test-2fa-pass", 55_001));
    let expected_tmp = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let send_count = Arc::new(AtomicU32::new(0));

    let srp_h = Arc::clone(&srp);
    let tmp_h = expected_tmp.clone();
    let send_count_h = Arc::clone(&send_count);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::GetPaymentForm::from_bytes(&rpc.body).unwrap();
            match req.invoice {
                tl_enums::InputInvoice::Slug(ref s) if s.slug == "fiat_checkout" => {
                    assert!(
                        req.theme_params.is_some(),
                        "payments_form.cpp:473 sets f_theme_params"
                    );
                    ServerAction::ok(&tl_enums::payments::PaymentForm::PaymentForm(
                        tl_types::payments::PaymentForm {
                            can_save_credentials: true,
                            password_missing: false,
                            form_id: 90_001,
                            bot_id: 800_001,
                            title: "Synthetic Book".to_owned(),
                            description: "Test invoice".to_owned(),
                            photo: None,
                            invoice: sample_invoice("USD", 1500, true),
                            provider_id: 800_002,
                            url: "https://payments.example.invalid/form".to_owned(),
                            native_provider: Some("stripe".to_owned()),
                            native_params: Some(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                                data: r#"{"publishable_key":"pk_test_123","need_zip":true}"#
                                    .to_owned(),
                            })),
                            additional_methods: None,
                            saved_info: None,
                            saved_credentials: Some(vec![tl_enums::PaymentSavedCredentials::Card(
                                tl_types::PaymentSavedCredentialsCard {
                                    id: "card_saved_1".to_owned(),
                                    title: "visa *4242".to_owned(),
                                },
                            )]),
                            users: Vec::new(),
                        },
                    ))
                }
                tl_enums::InputInvoice::Slug(ref s) if s.slug == "stars_item" => ServerAction::ok(
                    &tl_enums::payments::PaymentForm::Stars(tl_types::payments::PaymentFormStars {
                        form_id: 90_002,
                        bot_id: 800_001,
                        title: "Stars Item".to_owned(),
                        description: "Pay with Stars".to_owned(),
                        photo: None,
                        invoice: sample_invoice("XTR", 100, false),
                        users: Vec::new(),
                    }),
                ),
                _ => ServerAction::err(400, "INVOICE_INVALID"),
            }
        }
        c if c == tl_fns::payments::ValidateRequestedInfo::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::ValidateRequestedInfo::from_bytes(&rpc.body).unwrap();
            assert!(req.save, "save flag must match requested info");
            let tl_enums::PaymentRequestedInfo::PaymentRequestedInfo(info) = req.info;
            assert_eq!(info.name.as_deref(), Some("Alex River"));
            assert_eq!(info.email.as_deref(), Some("alex@example.invalid"));
            assert!(info.shipping_address.is_some());
            ServerAction::ok(
                &tl_enums::payments::ValidatedRequestedInfo::ValidatedRequestedInfo(
                    tl_types::payments::ValidatedRequestedInfo {
                        id: Some("req_info_77".to_owned()),
                        shipping_options: Some(vec![tl_enums::ShippingOption::ShippingOption(
                            tl_types::ShippingOption {
                                id: "express".to_owned(),
                                title: "Express Shipping".to_owned(),
                                prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                                    tl_types::LabeledPrice {
                                        label: "Express".to_owned(),
                                        amount: 500,
                                    },
                                )],
                            },
                        )]),
                    },
                ),
            )
        }
        c if c == tl_fns::account::GetPassword::CONSTRUCTOR_ID => {
            ServerAction::ok(&srp_h.to_password_tl(false))
        }
        c if c == tl_fns::account::GetTmpPassword::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetTmpPassword::from_bytes(&rpc.body).unwrap();
            assert_eq!(
                req.period, 900,
                "payments_form.cpp:46 kPasswordPeriod = 15m"
            );
            if !srp_h.verify_tl_check(&req.password) {
                return ServerAction::err(400, "PASSWORD_HASH_INVALID");
            }
            ServerAction::ok(&tl_enums::account::TmpPassword::TmpPassword(
                tl_types::account::TmpPassword {
                    tmp_password: tmp_h.clone(),
                    valid_until: now_unix_secs() + 900,
                },
            ))
        }
        c if c == tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::SendPaymentForm::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.form_id, 90_001);
            assert_eq!(req.requested_info_id.as_deref(), Some("req_info_77"));
            assert_eq!(req.shipping_option_id.as_deref(), Some("express"));
            assert_eq!(req.tip_amount, Some(250));
            let n = send_count_h.fetch_add(1, Ordering::SeqCst);
            match req.credentials {
                tl_enums::InputPaymentCredentials::Saved(s) => {
                    assert_eq!(s.id, "card_saved_1");
                    assert_eq!(s.tmp_password, tmp_h);
                    if n == 0 {
                        ServerAction::ok(
                            &tl_enums::payments::PaymentResult::PaymentVerificationNeeded(
                                tl_types::payments::PaymentVerificationNeeded {
                                    url: "https://3ds.example.invalid/verify?tx=1".to_owned(),
                                },
                            ),
                        )
                    } else {
                        ServerAction::ok(&tl_enums::payments::PaymentResult::PaymentResult(
                            tl_types::payments::PaymentResult {
                                updates: tl_enums::Updates::TooLong,
                            },
                        ))
                    }
                }
                _ => ServerAction::err(400, "PAYMENT_CREDENTIALS_INVALID"),
            }
        }
        c if c == tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::SendStarsForm::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.form_id, 90_002);
            ServerAction::ok(&tl_enums::payments::PaymentResult::PaymentResult(
                tl_types::payments::PaymentResult {
                    updates: tl_enums::Updates::TooLong,
                },
            ))
        }
        c if c == tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::ChangeStarsSubscription::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.peer, tl_enums::InputPeer::Self_);
            assert_eq!(req.subscription_id, "sub_42");
            assert!(
                req.canceled.is_some(),
                "api_credits.cpp:397 always sets f_canceled"
            );
            ServerAction::bool_true()
        }
        c if c == tl_fns::payments::GetStarsGiftOptions::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::GetStarsGiftOptions::from_bytes(&rpc.body).unwrap();
            assert!(
                matches!(req.user_id, Some(tl_enums::InputUser::InputUser(ref u)) if u.user_id == 777_002)
            );
            let opts = vec![tl_enums::StarsGiftOption::StarsGiftOption(
                tl_types::StarsGiftOption {
                    extended: false,
                    stars: 500,
                    store_product: Some("org.telegram.stars.gift.500".to_owned()),
                    currency: "USD".to_owned(),
                    amount: 999,
                },
            )];
            ServerAction::ok(&opts)
        }
        _ => ServerAction::err(400, format!("UNEXPECTED_CTOR_{:#x}", rpc.ctor)),
    });

    let (_client, rpc) = build_connected_rpc(&server);

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Load fiat payment form with theme_params
        let invoice_fiat = tl_enums::InputInvoice::Slug(tl_types::InputInvoiceSlug {
            slug: "fiat_checkout".to_owned(),
        });
        let form = rpc
            .invoke(&tl_fns::payments::GetPaymentForm {
                invoice: invoice_fiat.clone(),
                theme_params: Some(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: r##"{"bg_color":"#17212b"}"##.to_owned(),
                })),
            })
            .await
            .expect("getPaymentForm fiat");
        assert!(matches!(
            form,
            tl_enums::payments::PaymentForm::PaymentForm(ref f) if f.form_id == 90_001
        ));

        // 2. Validate requested info
        let validated = rpc
            .invoke(&tl_fns::payments::ValidateRequestedInfo {
                save: true,
                invoice: invoice_fiat.clone(),
                info: tl_enums::PaymentRequestedInfo::PaymentRequestedInfo(
                    tl_types::PaymentRequestedInfo {
                        name: Some("Alex River".to_owned()),
                        phone: None,
                        email: Some("alex@example.invalid".to_owned()),
                        shipping_address: Some(tl_enums::PostAddress::PostAddress(
                            tl_types::PostAddress {
                                street_line1: "100 Main St".to_owned(),
                                street_line2: String::new(),
                                city: "Metropolis".to_owned(),
                                state: "CA".to_owned(),
                                country_iso2: "US".to_owned(),
                                post_code: "90001".to_owned(),
                            },
                        )),
                    },
                ),
            })
            .await
            .expect("validateRequestedInfo");
        let tl_enums::payments::ValidatedRequestedInfo::ValidatedRequestedInfo(v) = validated;
        assert_eq!(v.id.as_deref(), Some("req_info_77"));
        assert_eq!(v.shipping_options.as_ref().map(Vec::len), Some(1));

        // 3. SRP 2FA -> account.getTmpPassword (verified by SpecSrpServer)
        let tl_enums::account::Password::Password(pwd) =
            rpc.invoke(&tl_fns::account::GetPassword {}).await.unwrap();
        let check = compute_check(
            &srp.kdf_algo(),
            pwd.srp_b.as_deref().unwrap(),
            pwd.srp_id.unwrap(),
            b"test-2fa-pass",
        )
        .expect("SRP compute_check");
        let tl_enums::account::TmpPassword::TmpPassword(tmp) = rpc
            .invoke(&tl_fns::account::GetTmpPassword {
                password: tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(
                    tl_types::InputCheckPasswordSRP {
                        srp_id: check.srp_id,
                        a: check.a.to_vec(),
                        m1: check.m1.to_vec(),
                    },
                ),
                period: 900,
            })
            .await
            .expect("getTmpPassword");
        assert_eq!(tmp.tmp_password, expected_tmp);

        // 4. SendPaymentForm -> VerificationNeeded (3DS), then Finished
        let send_req = tl_fns::payments::SendPaymentForm {
            form_id: 90_001,
            invoice: invoice_fiat,
            requested_info_id: Some("req_info_77".to_owned()),
            shipping_option_id: Some("express".to_owned()),
            credentials: tl_enums::InputPaymentCredentials::Saved(
                tl_types::InputPaymentCredentialsSaved {
                    id: "card_saved_1".to_owned(),
                    tmp_password: tmp.tmp_password,
                },
            ),
            tip_amount: Some(250),
        };
        let res1 = rpc.invoke(&send_req).await.expect("sendPaymentForm 3ds");
        assert!(matches!(
            res1,
            tl_enums::payments::PaymentResult::PaymentVerificationNeeded(ref v)
                if v.url == "https://3ds.example.invalid/verify?tx=1"
        ));
        let res2 = rpc.invoke(&send_req).await.expect("sendPaymentForm paid");
        assert!(matches!(
            res2,
            tl_enums::payments::PaymentResult::PaymentResult(_)
        ));

        // 5. Stars form -> SendStarsForm
        let stars_invoice = tl_enums::InputInvoice::Slug(tl_types::InputInvoiceSlug {
            slug: "stars_item".to_owned(),
        });
        let stars_form = rpc
            .invoke(&tl_fns::payments::GetPaymentForm {
                invoice: stars_invoice.clone(),
                theme_params: None,
            })
            .await
            .expect("getPaymentForm stars");
        assert!(matches!(
            stars_form,
            tl_enums::payments::PaymentForm::Stars(ref f) if f.form_id == 90_002
        ));
        let stars_paid = rpc
            .invoke(&tl_fns::payments::SendStarsForm {
                form_id: 90_002,
                invoice: stars_invoice,
            })
            .await
            .expect("sendStarsForm");
        assert!(matches!(
            stars_paid,
            tl_enums::payments::PaymentResult::PaymentResult(_)
        ));

        // 6. ChangeStarsSubscription (cancel + renew)
        assert!(
            rpc.invoke(&tl_fns::payments::ChangeStarsSubscription {
                peer: tl_enums::InputPeer::Self_,
                subscription_id: "sub_42".to_owned(),
                canceled: Some(true),
            })
            .await
            .expect("cancel subscription")
        );
        assert!(
            rpc.invoke(&tl_fns::payments::ChangeStarsSubscription {
                peer: tl_enums::InputPeer::Self_,
                subscription_id: "sub_42".to_owned(),
                canceled: Some(false),
            })
            .await
            .expect("renew subscription")
        );

        // 7. GetStarsGiftOptions with recipient InputUser
        let gift_opts = rpc
            .invoke(&tl_fns::payments::GetStarsGiftOptions {
                user_id: Some(tl_enums::InputUser::InputUser(tl_types::InputUser {
                    user_id: 777_002,
                    access_hash: 12345,
                })),
            })
            .await
            .expect("getStarsGiftOptions");
        assert_eq!(gift_opts.len(), 1);
    })
    .await
    .expect("timeout");
}

// ============================================================================
// 2. Star Gifts Full Protocol Conformance (All 7 WP-55 RPCs + Upgrade,
//    Transfer, Resale, Auctions, Crafting & Hash Caching)
// ============================================================================

#[tokio::test]
async fn star_gifts_wp55_and_lifecycle_conformance() {
    let srp = Arc::new(SpecSrpServer::new(b"gift-withdraw-pass", 55_002));
    let tmp_secret = vec![0x10, 0x20, 0x30, 0x40, 0x50];
    let collections_hash = count_hash_u64(&[1111, 2222]);
    let auctions_hash = count_hash_u64(&[3u64, 1_750_000_100u64]);

    let srp_h = Arc::clone(&srp);
    let tmp_h = tmp_secret.clone();

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::payments::CheckCanSendGift::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::CheckCanSendGift::from_bytes(&rpc.body).unwrap();
            if req.gift_id == 1001 {
                ServerAction::ok(&tl_enums::payments::CheckCanSendGiftResult::Ok)
            } else {
                ServerAction::ok(&tl_enums::payments::CheckCanSendGiftResult::Fail(
                    tl_types::payments::CheckCanSendGiftResultFail {
                        reason: tl_enums::TextWithEntities::TextWithEntities(
                            tl_types::TextWithEntities {
                                text: "Gift is locked until release window.".to_owned(),
                                entities: Vec::new(),
                            },
                        ),
                    },
                ))
            }
        }
        c if c == tl_fns::account::GetPassword::CONSTRUCTOR_ID => {
            ServerAction::ok(&srp_h.to_password_tl(false))
        }
        c if c == tl_fns::account::GetTmpPassword::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetTmpPassword::from_bytes(&rpc.body).unwrap();
            assert_eq!(
                req.period, 600,
                "transfer_gift_box.cpp:80 kTmpPasswordPeriod = 10m"
            );
            if !srp_h.verify_tl_check(&req.password) {
                return ServerAction::err(400, "PASSWORD_HASH_INVALID");
            }
            ServerAction::ok(&tl_enums::account::TmpPassword::TmpPassword(
                tl_types::account::TmpPassword {
                    tmp_password: tmp_h.clone(),
                    valid_until: now_unix_secs() + 600,
                },
            ))
        }
        c if c == tl_fns::payments::WithdrawStarGift::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::WithdrawStarGift::from_bytes(&rpc.body).unwrap();
            if req.tmp_password != tmp_h {
                return ServerAction::err(400, "TMP_PASSWORD_INVALID");
            }
            assert!(matches!(
                req.stargift,
                tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser {
                    msg_id: 401
                })
            ));
            if req.keep_on_profile {
                ServerAction::bool_true()
            } else {
                ServerAction::bool_false()
            }
        }
        c if c == tl_fns::payments::GetStarGiftWithdrawalUrl::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::GetStarGiftWithdrawalUrl::from_bytes(&rpc.body).unwrap();
            if req.tmp_password != tmp_h {
                return ServerAction::err(400, "TMP_PASSWORD_INVALID");
            }
            assert!(matches!(
                req.stargift,
                tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser {
                    msg_id: 401
                })
            ));
            ServerAction::ok(
                &tl_enums::payments::StarGiftWithdrawalUrl::StarGiftWithdrawalUrl(
                    tl_types::payments::StarGiftWithdrawalUrl {
                        url: "https://fragment.com/gift/CrystalGem-7?token=synthetic".to_owned(),
                    },
                ),
            )
        }
        c if c == tl_fns::payments::DeleteStarGift::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::DeleteStarGift::from_bytes(&rpc.body).unwrap();
            assert!(matches!(
                req.stargift,
                tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser {
                    msg_id: 402
                })
            ));
            ServerAction::bool_true()
        }
        c if c == tl_fns::payments::GetSavedStarGift::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::GetSavedStarGift::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.stargift.len(), 1);
            ServerAction::ok(&tl_enums::payments::SavedStarGifts::SavedStarGifts(
                tl_types::payments::SavedStarGifts {
                    count: 1,
                    chat_notifications_enabled: Some(true),
                    gifts: vec![sample_saved_star_gift(5001, 1001, "CrystalGem-7", 401)],
                    next_offset: None,
                    chats: Vec::new(),
                    users: Vec::new(),
                },
            ))
        }
        c if c == tl_fns::payments::ToggleChatStarGiftNotifications::CONSTRUCTOR_ID => {
            let req =
                tl_fns::payments::ToggleChatStarGiftNotifications::from_bytes(&rpc.body).unwrap();
            assert!(matches!(
                req.peer,
                tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                    channel_id: 300_001,
                    ..
                })
            ));
            // Verify wire flag bit 0 (`flags.0?true`) matches `req.enabled`.
            let flags = u32::from_le_bytes(rpc.body[4..8].try_into().unwrap());
            assert_eq!((flags & 1) != 0, req.enabled);
            ServerAction::bool_true()
        }
        c if c == tl_fns::payments::GetStarGiftCollections::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::GetStarGiftCollections::from_bytes(&rpc.body).unwrap();
            if req.hash == collections_hash {
                ServerAction::ok(&tl_enums::payments::StarGiftCollections::NotModified)
            } else {
                ServerAction::ok(
                    &tl_enums::payments::StarGiftCollections::StarGiftCollections(
                        tl_types::payments::StarGiftCollections {
                            collections: vec![
                                tl_enums::StarGiftCollection::StarGiftCollection(
                                    tl_types::StarGiftCollection {
                                        collection_id: 1,
                                        title: "Gems".to_owned(),
                                        icon: None,
                                        gifts_count: 2,
                                        hash: 1111,
                                    },
                                ),
                                tl_enums::StarGiftCollection::StarGiftCollection(
                                    tl_types::StarGiftCollection {
                                        collection_id: 2,
                                        title: "Rare".to_owned(),
                                        icon: None,
                                        gifts_count: 1,
                                        hash: 2222,
                                    },
                                ),
                            ],
                        },
                    ),
                )
            }
        }
        c if c == tl_fns::payments::UpgradeStarGift::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::UpgradeStarGift::from_bytes(&rpc.body).unwrap();
            assert!(req.keep_original_details);
            ServerAction::ok(&tl_enums::Updates::TooLong)
        }
        c if c == tl_fns::payments::TransferStarGift::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::TransferStarGift::from_bytes(&rpc.body).unwrap();
            assert!(matches!(req.to_id, tl_enums::InputPeer::User(_)));
            ServerAction::ok(&tl_enums::Updates::TooLong)
        }
        c if c == tl_fns::payments::GetResaleStarGifts::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::GetResaleStarGifts::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.gift_id, 1001);
            assert!(req.sort_by_price);
            assert_eq!(req.attributes_hash, Some(0));
            ServerAction::ok(&tl_enums::payments::ResaleStarGifts::ResaleStarGifts(
                tl_types::payments::ResaleStarGifts {
                    count: 1,
                    gifts: vec![sample_unique_gift(
                        5001,
                        1001,
                        "CrystalGem-7",
                        "Crystal Gem",
                        7,
                    )],
                    next_offset: None,
                    attributes: Some(Vec::new()),
                    attributes_hash: Some(77_777),
                    chats: Vec::new(),
                    counters: None,
                    users: Vec::new(),
                },
            ))
        }
        c if c == tl_fns::payments::GetStarGiftActiveAuctions::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::GetStarGiftActiveAuctions::from_bytes(&rpc.body).unwrap();
            if req.hash == auctions_hash {
                ServerAction::ok(&tl_enums::payments::StarGiftActiveAuctions::NotModified)
            } else {
                ServerAction::ok(
                    &tl_enums::payments::StarGiftActiveAuctions::StarGiftActiveAuctions(
                        tl_types::payments::StarGiftActiveAuctions {
                            auctions: vec![
                                tl_enums::StarGiftActiveAuctionState::StarGiftActiveAuctionState(
                                    tl_types::StarGiftActiveAuctionState {
                                        gift: sample_unique_gift(
                                            5002,
                                            1002,
                                            "AuctionGem-1",
                                            "Auction Gem",
                                            1,
                                        ),
                                        state: tl_enums::StarGiftAuctionState::StarGiftAuctionState(
                                            tl_types::StarGiftAuctionState {
                                                version: 3,
                                                start_date: 1_750_000_000,
                                                end_date: 1_750_010_000,
                                                min_bid_amount: 500,
                                                bid_levels: Vec::new(),
                                                top_bidders: Vec::new(),
                                                next_round_at: 1_750_003_600,
                                                last_gift_num: 1,
                                                gifts_left: 5,
                                                current_round: 1,
                                                total_rounds: 3,
                                                rounds: Vec::new(),
                                            },
                                        ),
                                        user_state:
                                            tl_enums::StarGiftAuctionUserState::StarGiftAuctionUserState(
                                                tl_types::StarGiftAuctionUserState {
                                                    returned: false,
                                                    bid_amount: Some(600),
                                                    bid_date: Some(1_750_000_100),
                                                    min_bid_amount: Some(650),
                                                    bid_peer: None,
                                                    acquired_count: 0,
                                                },
                                            ),
                                    },
                                ),
                            ],
                            users: Vec::new(),
                            chats: Vec::new(),
                        },
                    ),
                )
            }
        }
        c if c == tl_fns::payments::CraftStarGift::CONSTRUCTOR_ID => {
            let req = tl_fns::payments::CraftStarGift::from_bytes(&rpc.body).unwrap();
            if req.stargift.len() == 1 {
                ServerAction::err(400, "STARGIFT_CRAFT_TOO_EARLY_120")
            } else {
                ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![tl_enums::Update::StarGiftCraftFail],
                    users: Vec::new(),
                    chats: Vec::new(),
                    date: now_unix_secs(),
                    seq: 1,
                }))
            }
        }
        _ => ServerAction::err(400, format!("UNEXPECTED_CTOR_{:#x}", rpc.ctor)),
    });

    let (_client, rpc) = build_connected_rpc(&server);

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. payments.checkCanSendGift (Ok and Fail)
        let ok = rpc
            .invoke(&tl_fns::payments::CheckCanSendGift { gift_id: 1001 })
            .await
            .expect("checkCanSendGift ok");
        assert!(matches!(ok, tl_enums::payments::CheckCanSendGiftResult::Ok));
        let fail = rpc
            .invoke(&tl_fns::payments::CheckCanSendGift { gift_id: 9999 })
            .await
            .expect("checkCanSendGift fail");
        assert!(matches!(
            fail,
            tl_enums::payments::CheckCanSendGiftResult::Fail(ref f)
                if matches!(&f.reason, tl_enums::TextWithEntities::TextWithEntities(t)
                    if t.text.contains("locked"))
        ));

        // 2. SRP -> account.getTmpPassword(600) -> withdrawStarGift & getStarGiftWithdrawalUrl
        let tl_enums::account::Password::Password(pwd) =
            rpc.invoke(&tl_fns::account::GetPassword {}).await.unwrap();
        let check = compute_check(
            &srp.kdf_algo(),
            pwd.srp_b.as_deref().unwrap(),
            pwd.srp_id.unwrap(),
            b"gift-withdraw-pass",
        )
        .unwrap();
        let tl_enums::account::TmpPassword::TmpPassword(tmp) = rpc
            .invoke(&tl_fns::account::GetTmpPassword {
                password: tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(
                    tl_types::InputCheckPasswordSRP {
                        srp_id: check.srp_id,
                        a: check.a.to_vec(),
                        m1: check.m1.to_vec(),
                    },
                ),
                period: 600,
            })
            .await
            .unwrap();

        let sg_401 =
            tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser { msg_id: 401 });
        // Refused tmp_password surfaces TMP_PASSWORD_INVALID
        let bad_withdraw = rpc
            .invoke(&tl_fns::payments::WithdrawStarGift {
                keep_on_profile: true,
                stargift: sg_401.clone(),
                tmp_password: vec![0x00],
            })
            .await;
        assert!(matches!(
            bad_withdraw,
            Err(InvocationError::Rpc(ref e)) if e.name == "TMP_PASSWORD_INVALID"
        ));
        // Valid tmp_password + keep_on_profile = true
        assert!(
            rpc.invoke(&tl_fns::payments::WithdrawStarGift {
                keep_on_profile: true,
                stargift: sg_401.clone(),
                tmp_password: tmp.tmp_password.clone(),
            })
            .await
            .expect("withdrawStarGift")
        );
        // GetStarGiftWithdrawalUrl
        let tl_enums::payments::StarGiftWithdrawalUrl::StarGiftWithdrawalUrl(url_res) = rpc
            .invoke(&tl_fns::payments::GetStarGiftWithdrawalUrl {
                stargift: sg_401.clone(),
                tmp_password: tmp.tmp_password,
            })
            .await
            .expect("getStarGiftWithdrawalUrl");
        assert_eq!(
            url_res.url,
            "https://fragment.com/gift/CrystalGem-7?token=synthetic"
        );

        // 3. DeleteStarGift
        assert!(
            rpc.invoke(&tl_fns::payments::DeleteStarGift {
                stargift: tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser {
                    msg_id: 402,
                }),
            })
            .await
            .expect("deleteStarGift")
        );

        // 4. GetSavedStarGift (single lookup)
        let tl_enums::payments::SavedStarGifts::SavedStarGifts(saved) = rpc
            .invoke(&tl_fns::payments::GetSavedStarGift {
                stargift: vec![sg_401.clone()],
            })
            .await
            .expect("getSavedStarGift");
        assert_eq!(saved.count, 1);
        assert_eq!(saved.chat_notifications_enabled, Some(true));
        assert_eq!(saved.gifts.len(), 1);

        // 5. ToggleChatStarGiftNotifications (enabled = true & false)
        let ch_peer = tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
            channel_id: 300_001,
            access_hash: 88_888,
        });
        assert!(
            rpc.invoke(&tl_fns::payments::ToggleChatStarGiftNotifications {
                enabled: true,
                peer: ch_peer.clone(),
            })
            .await
            .expect("toggle notifications on")
        );
        assert!(
            rpc.invoke(&tl_fns::payments::ToggleChatStarGiftNotifications {
                enabled: false,
                peer: ch_peer,
            })
            .await
            .expect("toggle notifications off")
        );

        // 6. Collections CountHash caching -> NotModified
        let cols1 = rpc
            .invoke(&tl_fns::payments::GetStarGiftCollections {
                peer: tl_enums::InputPeer::Self_,
                hash: 0,
            })
            .await
            .expect("getStarGiftCollections");
        assert!(matches!(
            cols1,
            tl_enums::payments::StarGiftCollections::StarGiftCollections(ref c)
                if c.collections.len() == 2
        ));
        let cols2 = rpc
            .invoke(&tl_fns::payments::GetStarGiftCollections {
                peer: tl_enums::InputPeer::Self_,
                hash: collections_hash,
            })
            .await
            .expect("getStarGiftCollections not modified");
        assert!(matches!(
            cols2,
            tl_enums::payments::StarGiftCollections::NotModified
        ));

        // 7. Upgrade, Transfer, Resale, Active Auctions (hash -> NotModified), Craft
        let _ = rpc
            .invoke(&tl_fns::payments::UpgradeStarGift {
                keep_original_details: true,
                stargift: sg_401.clone(),
            })
            .await
            .expect("upgradeStarGift");
        let _ = rpc
            .invoke(&tl_fns::payments::TransferStarGift {
                stargift: sg_401.clone(),
                to_id: tl_enums::InputPeer::User(tl_types::InputPeerUser {
                    user_id: 777_002,
                    access_hash: 12345,
                }),
            })
            .await
            .expect("transferStarGift");
        let resale = rpc
            .invoke(&tl_fns::payments::GetResaleStarGifts {
                sort_by_price: true,
                sort_by_num: false,
                for_craft: false,
                stars_only: false,
                attributes_hash: Some(0),
                gift_id: 1001,
                attributes: None,
                offset: String::new(),
                limit: 50,
            })
            .await
            .expect("getResaleStarGifts");
        let tl_enums::payments::ResaleStarGifts::ResaleStarGifts(r) = resale;
        assert_eq!(r.attributes_hash, Some(77_777));

        let auc1 = rpc
            .invoke(&tl_fns::payments::GetStarGiftActiveAuctions { hash: 0 })
            .await
            .expect("getStarGiftActiveAuctions");
        assert!(matches!(
            auc1,
            tl_enums::payments::StarGiftActiveAuctions::StarGiftActiveAuctions(_)
        ));
        let auc2 = rpc
            .invoke(&tl_fns::payments::GetStarGiftActiveAuctions {
                hash: auctions_hash,
            })
            .await
            .expect("getStarGiftActiveAuctions NotModified");
        assert!(matches!(
            auc2,
            tl_enums::payments::StarGiftActiveAuctions::NotModified
        ));

        let craft_early = rpc
            .invoke(&tl_fns::payments::CraftStarGift {
                stargift: vec![sg_401.clone()],
            })
            .await;
        assert!(matches!(
            craft_early,
            Err(InvocationError::Rpc(ref e)) if e.name == "STARGIFT_CRAFT_TOO_EARLY_120"
        ));
        let craft_fail = rpc
            .invoke(&tl_fns::payments::CraftStarGift {
                stargift: vec![sg_401.clone(), sg_401],
            })
            .await
            .expect("craftStarGift fail update");
        assert!(matches!(
            craft_fail,
            tl_enums::Updates::Updates(ref u)
                if matches!(u.updates.first(), Some(tl_enums::Update::StarGiftCraftFail))
        ));
    })
    .await
    .expect("timeout");
}

// ============================================================================
// 3. Telegram Passport End-to-End Conformance: help.getPassportConfig
//    (PassportConfig + NotModified), SRP-2FA Unlock, AES-256-CBC
//    account.saveSecureValue & RSA-OAEP account.acceptAuthorization
// ============================================================================

#[tokio::test]
async fn passport_config_srp_save_and_accept_authorization_conformance() {
    let password = b"passport-cloud-pass-233";
    let srp = Arc::new(SpecSrpServer::new(password, 55_003));

    // Pre-generate synthetic master secret encrypted under PBKDF2-HMAC-SHA512.
    let master_secret = make_deterministic_passport_secret([0x21u8; 32]);
    let master_secret_id = count_secure_secret_id(&master_secret);
    let sec_algo = SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
        salt: b"passport-sec-salt-01".to_vec(),
    };
    let sec_hash = compute_secure_secret_hash(&sec_algo, password);
    let enc_master = encrypt_secure_secret(&master_secret, &sec_hash).expect("encrypt master");

    let cfg_hash = 0x5A5A_1234i32;
    let saved_value_hash = vec![0x99u8; 32];
    let verified_accept = Arc::new(Mutex::new(Option::<serde_json::Value>::None));

    let srp_h = Arc::clone(&srp);
    let saved_hash_h = saved_value_hash.clone();
    let verified_accept_h = Arc::clone(&verified_accept);
    let enc_master_vec = enc_master.to_vec();

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::help::GetPassportConfig::CONSTRUCTOR_ID => {
            let req = tl_fns::help::GetPassportConfig::from_bytes(&rpc.body).unwrap();
            if req.hash == cfg_hash {
                ServerAction::ok(&tl_enums::help::PassportConfig::NotModified)
            } else {
                ServerAction::ok(&tl_enums::help::PassportConfig::PassportConfig(
                    tl_types::help::PassportConfig {
                        hash: cfg_hash,
                        countries_langs: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                            data: r#"{"US":"en","DE":"de","FR":"fr"}"#.to_owned(),
                        }),
                    },
                ))
            }
        }
        c if c == tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetAuthorizationForm::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.bot_id, 888_001);
            assert_eq!(req.scope, r#"{"data":["personal_details"],"v":1}"#);
            ServerAction::ok(&tl_enums::account::AuthorizationForm::AuthorizationForm(
                tl_types::account::AuthorizationForm {
                    required_types: vec![tl_enums::SecureRequiredType::SecureRequiredType(
                        tl_types::SecureRequiredType {
                            native_names: true,
                            selfie_required: false,
                            translation_required: false,
                            r#type: tl_enums::SecureValueType::PersonalDetails,
                        },
                    )],
                    values: Vec::new(),
                    errors: Vec::new(),
                    users: Vec::new(),
                    privacy_policy_url: Some("https://bot.example.invalid/privacy".to_owned()),
                },
            ))
        }
        c if c == tl_fns::account::GetPassword::CONSTRUCTOR_ID => {
            ServerAction::ok(&srp_h.to_password_tl(true))
        }
        c if c == tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID => {
            let req = tl_fns::account::GetPasswordSettings::from_bytes(&rpc.body).unwrap();
            if !srp_h.verify_tl_check(&req.password) {
                return ServerAction::err(400, "PASSWORD_HASH_INVALID");
            }
            ServerAction::ok(&tl_enums::account::PasswordSettings::PasswordSettings(
                tl_types::account::PasswordSettings {
                    email: Some("user@example.invalid".to_owned()),
                    secure_settings: Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
                        tl_types::SecureSecretSettings {
                            secure_algo:
                                tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                                    tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                                        salt: b"passport-sec-salt-01".to_vec(),
                                    },
                                ),
                            secure_secret: enc_master_vec.clone(),
                            secure_secret_id: master_secret_id as i64,
                        },
                    )),
                },
            ))
        }
        c if c == tl_fns::account::SaveSecureValue::CONSTRUCTOR_ID => {
            let req = tl_fns::account::SaveSecureValue::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.secure_secret_id, master_secret_id as i64);
            let tl_enums::InputSecureValue::InputSecureValue(val) = req.value;
            assert_eq!(val.r#type, tl_enums::SecureValueType::PersonalDetails);
            let Some(tl_enums::SecureData::SecureData(sd)) = val.data else {
                return ServerAction::err(400, "SECURE_VALUE_EMPTY");
            };
            assert_eq!(sd.data_hash.len(), 32);
            assert_eq!(sd.secret.len(), 32);
            assert_eq!(sd.data.len() % 16, 0);
            ServerAction::ok(&tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
                r#type: tl_enums::SecureValueType::PersonalDetails,
                data: Some(tl_enums::SecureData::SecureData(sd)),
                front_side: None,
                reverse_side: None,
                selfie: None,
                translation: None,
                files: None,
                plain_data: None,
                hash: saved_hash_h.clone(),
            }))
        }
        c if c == tl_fns::account::AcceptAuthorization::CONSTRUCTOR_ID => {
            let req = tl_fns::account::AcceptAuthorization::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.bot_id, 888_001);
            assert_eq!(req.value_hashes.len(), 1);
            let tl_enums::SecureCredentialsEncrypted::SecureCredentialsEncrypted(creds) =
                req.credentials;
            // Bot server side: decrypt OAEP secret with bot private key, then AES-256-CBC decrypt
            // credentials JSON and verify SHA-256 hash & nonce!
            let cred_secret = spec_decrypt_rsa_oaep_sha1(&creds.secret);
            assert_eq!(cred_secret.len(), 32);
            assert!(check_secret_bytes(&cred_secret));
            let key_iv = sha512(&[&cred_secret, &creds.hash]);
            let key: [u8; 32] = key_iv[0..32].try_into().unwrap();
            let iv: [u8; 16] = key_iv[32..48].try_into().unwrap();
            let padded = aes_cbc_decrypt(&creds.data, &key, &iv).expect("decrypt creds");
            assert_eq!(sha256(&[&padded]).as_slice(), creds.hash.as_slice());
            let pad_len = usize::from(padded[0]);
            assert!((32..=255).contains(&pad_len));
            let json: serde_json::Value =
                serde_json::from_slice(&padded[pad_len..]).expect("valid creds JSON");
            *verified_accept_h.lock().unwrap() = Some(json);
            ServerAction::bool_true()
        }
        _ => ServerAction::err(400, format!("UNEXPECTED_CTOR_{:#x}", rpc.ctor)),
    });

    let (_client, rpc) = build_connected_rpc(&server);

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. help.getPassportConfig: initial load -> cached hash -> NotModified
        let cfg1 = rpc
            .invoke(&tl_fns::help::GetPassportConfig { hash: 0 })
            .await
            .expect("getPassportConfig initial");
        let received_hash = match cfg1 {
            tl_enums::help::PassportConfig::PassportConfig(c) => {
                let tl_enums::DataJSON::DataJSON(j) = c.countries_langs;
                let parsed: serde_json::Value = serde_json::from_str(&j.data).unwrap();
                assert_eq!(parsed["US"], "en");
                assert_eq!(parsed["DE"], "de");
                c.hash
            }
            tl_enums::help::PassportConfig::NotModified => panic!("expected full config"),
        };
        assert_eq!(received_hash, cfg_hash);

        let cfg2 = rpc
            .invoke(&tl_fns::help::GetPassportConfig {
                hash: received_hash,
            })
            .await
            .expect("getPassportConfig cached");
        assert!(matches!(cfg2, tl_enums::help::PassportConfig::NotModified));

        // 2. account.getAuthorizationForm + account.getPassword + SRP account.getPasswordSettings
        let _form = rpc
            .invoke(&tl_fns::account::GetAuthorizationForm {
                bot_id: 888_001,
                scope: r#"{"data":["personal_details"],"v":1}"#.to_owned(),
                public_key: "TEST_PUB_KEY".to_owned(),
            })
            .await
            .expect("getAuthorizationForm");

        let tl_enums::account::Password::Password(pwd) =
            rpc.invoke(&tl_fns::account::GetPassword {}).await.unwrap();
        let check = compute_check(
            &srp.kdf_algo(),
            pwd.srp_b.as_deref().unwrap(),
            pwd.srp_id.unwrap(),
            password,
        )
        .unwrap();
        let tl_enums::account::PasswordSettings::PasswordSettings(ps) = rpc
            .invoke(&tl_fns::account::GetPasswordSettings {
                password: tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(
                    tl_types::InputCheckPasswordSRP {
                        srp_id: check.srp_id,
                        a: check.a.to_vec(),
                        m1: check.m1.to_vec(),
                    },
                ),
            })
            .await
            .expect("getPasswordSettings");

        let tl_enums::SecureSecretSettings::SecureSecretSettings(ss) =
            ps.secure_settings.expect("secure_settings");
        let unlocked_master =
            decrypt_secure_secret(&ss.secure_secret, &sec_hash).expect("decrypt master secret");
        assert_eq!(
            count_secure_secret_id(&unlocked_master),
            ss.secure_secret_id as u64
        );

        // 3. Encrypt personal_details value (AES-256-CBC + master-encrypted secret) & saveSecureValue
        let value_secret = make_deterministic_passport_secret([0x44u8; 32]);
        let plaintext =
            br#"{"first_name":"Alex","last_name":"River","residence_country_code":"US"}"#;
        let pad_len = 48 - (plaintext.len() % 16);
        let mut padded = Vec::with_capacity(pad_len + plaintext.len());
        padded.push(pad_len as u8);
        padded.extend(std::iter::repeat_n(0xABu8, pad_len - 1));
        padded.extend_from_slice(plaintext);
        let data_hash = sha256(&[&padded]);
        let kv = sha512(&[&value_secret, &data_hash]);
        let enc_val = aes_cbc_encrypt(
            &padded,
            &kv[0..32].try_into().unwrap(),
            &kv[32..48].try_into().unwrap(),
        )
        .unwrap();
        let sv_kv = sha512(&[&unlocked_master, &data_hash]);
        let enc_val_secret = aes_cbc_encrypt(
            &value_secret,
            &sv_kv[0..32].try_into().unwrap(),
            &sv_kv[32..48].try_into().unwrap(),
        )
        .unwrap();

        let tl_enums::SecureValue::SecureValue(saved) = rpc
            .invoke(&tl_fns::account::SaveSecureValue {
                value: tl_enums::InputSecureValue::InputSecureValue(tl_types::InputSecureValue {
                    r#type: tl_enums::SecureValueType::PersonalDetails,
                    data: Some(tl_enums::SecureData::SecureData(tl_types::SecureData {
                        data: enc_val,
                        data_hash: data_hash.to_vec(),
                        secret: enc_val_secret,
                    })),
                    front_side: None,
                    reverse_side: None,
                    selfie: None,
                    translation: None,
                    files: None,
                    plain_data: None,
                }),
                secure_secret_id: master_secret_id as i64,
            })
            .await
            .expect("saveSecureValue");
        assert_eq!(saved.hash, saved_value_hash);

        // 4. Encrypt credentials JSON with fresh secret + RSA-OAEP(test_rsa_public_key) & acceptAuthorization
        let cred_secret = make_deterministic_passport_secret([0x66u8; 32]);
        let cred_json = serde_json::json!({
            "secure_data": {
                "personal_details": {
                    "data": {
                        "data_hash": "stub_hash",
                        "secret": "stub_secret",
                    }
                }
            },
            "nonce": "wp55-nonce-999"
        })
        .to_string();
        let cp = 48 - (cred_json.len() % 16);
        let mut c_padded = Vec::with_capacity(cp + cred_json.len());
        c_padded.push(cp as u8);
        c_padded.extend(std::iter::repeat_n(0xCDu8, cp - 1));
        c_padded.extend_from_slice(cred_json.as_bytes());
        let c_hash = sha256(&[&c_padded]);
        let c_kv = sha512(&[&cred_secret, &c_hash]);
        let c_enc = aes_cbc_encrypt(
            &c_padded,
            &c_kv[0..32].try_into().unwrap(),
            &c_kv[32..48].try_into().unwrap(),
        )
        .unwrap();
        let c_oaep_secret = test_rsa_public_key()
            .encrypt_oaep(&cred_secret)
            .expect("RSA-OAEP encrypt");

        let accepted = rpc
            .invoke(&tl_fns::account::AcceptAuthorization {
                bot_id: 888_001,
                scope: r#"{"data":["personal_details"],"v":1}"#.to_owned(),
                public_key: "TEST_PUB_KEY".to_owned(),
                value_hashes: vec![tl_enums::SecureValueHash::SecureValueHash(
                    tl_types::SecureValueHash {
                        r#type: tl_enums::SecureValueType::PersonalDetails,
                        hash: saved.hash,
                    },
                )],
                credentials: tl_enums::SecureCredentialsEncrypted::SecureCredentialsEncrypted(
                    tl_types::SecureCredentialsEncrypted {
                        data: c_enc,
                        hash: c_hash.to_vec(),
                        secret: c_oaep_secret,
                    },
                ),
            })
            .await
            .expect("acceptAuthorization");
        assert!(accepted);

        // Verify the fake server decrypted the OAEP credentials secret & JSON end-to-end!
        let decoded_creds = verified_accept
            .lock()
            .unwrap()
            .clone()
            .expect("server decrypted credentials JSON");
        assert_eq!(decoded_creds["nonce"], "wp55-nonce-999");
        assert!(decoded_creds["secure_data"]["personal_details"]["data"].is_object());
        assert_eq!(
            server
                .rpcs_of(tl_fns::help::GetPassportConfig::CONSTRUCTOR_ID)
                .len(),
            2
        );
    })
    .await
    .expect("timeout");
}
