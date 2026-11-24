//! Offline deterministic MTProto 2.0 protocol conformance tests for `tg-mtproto`.
//!
//! Cross-checks `tg-mtproto` end-to-end against the MTProto 2.0 specification and
//! Telegram Desktop (`Telegram/SourceFiles/mtproto/`):
//! 1. Auth key creation (DH 3-roundtrip exchange: `req_pq_multi` -> `req_DH_params` ->
//!    `set_client_DH_params`, `RSA_PAD`, `tmp_aes_key`/`iv`, `dh_gen_ok`/`retry`/`fail`,
//!    `server_DH_params_fail`, `auth_key_id`, `initial_salt`, and server-side rejections).
//! 2. MTProto 2.0 encrypted packet wire framing (`build_encrypted_packet` / `parse_encrypted_packet`)
//!    in both directions (`x = 0` and `x = 8`) with wrong-`x` and wrong-padding rejection.
//! 3. Message IDs, clock offset correction, `seq_no` (content-related vs service messages),
//!    server `msg_id` time window / parity / duplicate rejection, and
//!    `bad_msg_notification` / `bad_server_salt` recovery for codes 16, 17, 32, 33, 48.

use num_bigint::BigUint;
use sha1::{Digest as _, Sha1};
use sha2::Sha256;

use tg_crypto::aes::{aes_ige_decrypt, aes_ige_encrypt};
use tg_crypto::auth_key::{AuthKey, Side, encrypt_data_v2_with_padding};
use tg_crypto::dh::GOOD_PRIME;
use tg_crypto::rsa::PublicKey;
use tg_mtproto::auth_key::{AuthKeyType, Dcenter, create_single_auth_key};
use tg_mtproto::config::ClientConfig;
use tg_mtproto::dc_options::{DcOptions, DcType, Environment};
use tg_mtproto::message::{
    CUT_CONTAINER_ON_BYTES, DecryptedEnvelope, body_needs_ack, build_encrypted_packet,
    decode_unencrypted_packet, encode_unencrypted_packet, need_ack_for_constructor, pack_container,
    parse_encrypted_packet, unpack_container,
};
use tg_mtproto::received_ids::{IDS_BUFFER_SIZE, ReceivedIdsManager, RegisterIdResult};
use tg_mtproto::salts::SaltManager;
use tg_mtproto::session::{QueuedRequest, SessionAction, SessionEvent, SessionState};
use tg_mtproto::time::{MtprotoClock, is_bad_server_time, is_valid_server_msg_id, local_unix_secs};
use tg_mtproto::transport_adapter::{FakeConnection, FakeServerPeer};
use tg_tl::mtproto::{
    CoreMessage, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    Boxed, Cursor, Deserializable, Identifiable, RawObject, Serializable, enums as tl_enums,
    functions as tl_functions,
};

// ============================================================================
// Independent straight-from-spec helpers for offline DH server & packet checks
// ============================================================================

fn spec_sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn spec_sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn to_fixed_be<const N: usize>(v: &BigUint) -> [u8; N] {
    let raw = v.to_bytes_be();
    assert!(raw.len() <= N, "BigUint exceeded {N} bytes");
    let mut out = [0u8; N];
    out[N - raw.len()..].copy_from_slice(&raw);
    out
}

/// Deterministic 2048-bit RSA test keypair constructed from two fixed 1024-bit primes
/// with `e = 65537` so our offline spec server can decrypt `RSA_PAD` ciphertexts produced by
/// `tg-mtproto`.
#[derive(Clone)]
struct TestRsaKeypair {
    pub_key: PublicKey,
    n: BigUint,
    d: BigUint,
}

impl TestRsaKeypair {
    fn deterministic() -> Self {
        use num_traits::{One, Zero};
        fn is_prime_mr_det(n: &BigUint) -> bool {
            const SMALL: &[u32] = &[
                3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79,
                83, 89, 97,
            ];
            for &sp in SMALL {
                if (n % sp).is_zero() {
                    return false;
                }
            }
            let n_minus_1 = n - BigUint::one();
            let s = n_minus_1.trailing_zeros().unwrap_or(0);
            let d = &n_minus_1 >> (s as usize);
            for &a in &[2u32, 3, 5, 7, 11, 13, 17] {
                let mut x = BigUint::from(a).modpow(&d, n);
                if x == BigUint::one() || x == n_minus_1 {
                    continue;
                }
                let mut composite = true;
                for _ in 1..s {
                    x = x.modpow(&BigUint::from(2u32), n);
                    if x == n_minus_1 {
                        composite = false;
                        break;
                    }
                }
                if composite {
                    return false;
                }
            }
            true
        }
        fn next_prime(mut cand: BigUint, e: &BigUint) -> BigUint {
            if (&cand & BigUint::one()).is_zero() {
                cand += BigUint::one();
            }
            loop {
                if (&(&cand - BigUint::one()) % e) != BigUint::zero() && is_prime_mr_det(&cand) {
                    return cand;
                }
                cand += BigUint::from(2u32);
            }
        }

        let e = BigUint::from(65537u32);
        let p_seed = (BigUint::one() << 1023usize)
            | (BigUint::one() << 1022usize)
            | BigUint::from(0x0001_0001u64);
        let q_seed = (BigUint::one() << 1023usize)
            | (BigUint::one() << 1022usize)
            | (BigUint::one() << 512usize)
            | BigUint::from(1u64);
        let p = next_prime(p_seed, &e);
        let q = next_prime(q_seed, &e);
        let n = &p * &q;
        assert_eq!(n.bits(), 2048);
        let phi = (&p - 1u32) * (&q - 1u32);
        let d = modinv(&e, &phi).expect("e invertible mod phi(n)");
        assert_eq!((&e * &d) % &phi, BigUint::from(1u32));

        let pub_key = PublicKey::from_components(&n.to_bytes_be(), &e.to_bytes_be())
            .expect("valid test RSA public key");
        Self { pub_key, n, d }
    }

    /// Independently decrypts an MTProto 2.0 `RSA_PAD` 256-byte block per
    /// `https://core.telegram.org/mtproto/auth_key#presenting-proof-of-work-server-authentication`.
    fn decrypt_rsa_pad(&self, ciphertext: &[u8]) -> Vec<u8> {
        assert_eq!(
            ciphertext.len(),
            256,
            "RSA_PAD ciphertext must be 256 bytes"
        );
        let c = BigUint::from_bytes_be(ciphertext);
        assert!(c < self.n, "RSA ciphertext >= n");
        let m = c.modpow(&self.d, &self.n);
        let key_aes_encrypted = to_fixed_be::<256>(&m);

        let temp_key_xor = &key_aes_encrypted[0..32];
        let aes_encrypted = &key_aes_encrypted[32..256];
        let sha_aes_enc = spec_sha256(&[aes_encrypted]);
        let mut temp_key = [0u8; 32];
        for i in 0..32 {
            temp_key[i] = temp_key_xor[i] ^ sha_aes_enc[i];
        }

        let zero_iv = [0u8; 32];
        let data_with_hash = aes_ige_decrypt(aes_encrypted, &temp_key, &zero_iv);
        assert_eq!(data_with_hash.len(), 224);

        let data_pad_reversed = &data_with_hash[0..192];
        let temp_data_hash = &data_with_hash[192..224];
        let mut data_with_padding = data_pad_reversed.to_vec();
        data_with_padding.reverse();
        let expected_hash = spec_sha256(&[&temp_key, &data_with_padding]);
        assert_eq!(
            temp_data_hash, &expected_hash,
            "RSA_PAD SHA-256 verification failed"
        );
        data_with_padding
    }
}

fn modinv(a: &BigUint, m: &BigUint) -> Option<BigUint> {
    use num_bigint::BigInt;
    use num_traits::{One, Signed, Zero};

    let mut old_r = BigInt::from(a.clone());
    let mut r = BigInt::from(m.clone());
    let mut old_s = BigInt::one();
    let mut s = BigInt::zero();
    while !r.is_zero() {
        let q = &old_r / &r;
        let next_r = &old_r - &q * &r;
        old_r = r;
        r = next_r;
        let next_s = &old_s - &q * &s;
        old_s = s;
        s = next_s;
    }
    if old_r != BigInt::one() {
        return None;
    }
    let m_signed = BigInt::from(m.clone());
    let res = ((old_s % &m_signed) + &m_signed) % &m_signed;
    assert!(!res.is_negative());
    res.to_biguint()
}

fn spec_tmp_aes_key_iv(new_nonce: &[u8; 32], server_nonce: &[u8; 16]) -> ([u8; 32], [u8; 32]) {
    let h1 = spec_sha1(&[new_nonce, server_nonce]);
    let h2 = spec_sha1(&[server_nonce, new_nonce]);
    let h3 = spec_sha1(&[new_nonce, new_nonce]);
    let mut key = [0u8; 32];
    key[0..20].copy_from_slice(&h1);
    key[20..32].copy_from_slice(&h2[0..12]);
    let mut iv = [0u8; 32];
    iv[0..8].copy_from_slice(&h2[12..20]);
    iv[8..28].copy_from_slice(&h3);
    iv[28..32].copy_from_slice(&new_nonce[0..4]);
    (key, iv)
}

fn spec_new_nonce_hash(new_nonce: &[u8; 32], num: u8, aux_hash: &[u8; 8]) -> [u8; 16] {
    let h = spec_sha1(&[new_nonce, &[num], aux_hash]);
    let mut out = [0u8; 16];
    out.copy_from_slice(&h[4..20]);
    out
}

fn server_unencrypted_msg_id(unix_secs: i32, seq: u32) -> u64 {
    ((unix_secs as u64) << 32) | ((seq as u64) << 2) | 1
}

fn server_msg_id(unix_secs: i32, seq: u32, is_reply: bool) -> u64 {
    let low = if is_reply { 1u64 } else { 3u64 };
    ((unix_secs as u64) << 32) | ((seq as u64) << 2) | low
}

fn make_dc_options_with_rsa(key: &PublicKey) -> DcOptions {
    let opts = DcOptions::new(Environment::Production);
    opts.add_public_key(key.clone());
    opts
}

#[derive(Clone, Copy)]
enum DhServerBehavior {
    Normal,
    RetryOnceThenOk,
    DhGenFail,
    ServerDhParamsFail,
    TamperServerDhSha1,
    BadDhPrime,
    BadGaOne,
    WrongNewNonceHash1,
}

struct ServerDhResult {
    auth_key: [u8; 256],
    auth_key_id: u64,
    initial_salt: u64,
    observed_retry_ids: Vec<i64>,
    observed_dc: i32,
    observed_expires_in: Option<i32>,
}

/// Independent straight-from-spec MTProto 2.0 DH server task.
async fn run_spec_dh_server(
    mut peer: FakeServerPeer,
    rsa: TestRsaKeypair,
    server_time: i32,
    expected_dc: i32,
    behavior: DhServerBehavior,
) -> Option<ServerDhResult> {
    // 1. Receive req_pq_multi (unencrypted packet: auth_key_id == 0, msg_id % 4 == 0).
    let pkt1 = peer.rx.recv().await?;
    assert!(pkt1.len() >= 24);
    assert_eq!(&pkt1[0..8], &[0u8; 8], "req_pq_multi auth_key_id must be 0");
    let client_msg_id1 = u64::from_le_bytes(pkt1[8..16].try_into().unwrap());
    assert_eq!(
        client_msg_id1 % 4,
        0,
        "client msg_id must be divisible by 4"
    );
    let len1 = u32::from_le_bytes(pkt1[16..20].try_into().unwrap()) as usize;
    assert_eq!(20 + len1, pkt1.len());
    let req_pq = mtp_functions::ReqPqMulti::from_bytes(&pkt1[20..]).expect("req_pq_multi");

    // Known PQ from MTProto docs: 0x17ED48941A08F981 = 0x494C553B * 0x53911073.
    let pq_bytes = 0x17ED_4894_1A08_F981u64.to_be_bytes().to_vec();
    let expected_p = 0x494C_553Bu32.to_be_bytes().to_vec();
    let expected_q = 0x5391_1073u32.to_be_bytes().to_vec();
    let server_nonce = [
        0xA5, 0xCF, 0x4D, 0x31, 0xF8, 0x7A, 0x43, 0x8F, 0xDF, 0x05, 0x37, 0xD3, 0x84, 0x26, 0x99,
        0x07,
    ];

    let res_pq = mtp_enums::ResPQ::ResPQ(mtp_types::ResPQ {
        nonce: req_pq.nonce,
        server_nonce,
        pq: pq_bytes.clone(),
        server_public_key_fingerprints: vec![rsa.pub_key.fingerprint_i64()],
    })
    .to_bytes();
    peer.tx
        .send(Ok(encode_unencrypted_packet(
            server_unencrypted_msg_id(server_time, 1),
            &res_pq,
        )))
        .ok()?;

    // 2. Receive req_DH_params and independently decrypt RSA_PAD inner data.
    let pkt2 = peer.rx.recv().await?;
    assert_eq!(&pkt2[0..8], &[0u8; 8]);
    let client_msg_id2 = u64::from_le_bytes(pkt2[8..16].try_into().unwrap());
    assert_eq!(client_msg_id2 % 4, 0);
    assert!(client_msg_id2 > client_msg_id1);
    let req_dh = mtp_functions::ReqDHParams::from_bytes(&pkt2[20..]).expect("req_DH_params");
    assert_eq!(req_dh.nonce, req_pq.nonce);
    assert_eq!(req_dh.server_nonce, server_nonce);
    assert_eq!(req_dh.p, expected_p);
    assert_eq!(req_dh.q, expected_q);
    assert_eq!(req_dh.public_key_fingerprint, rsa.pub_key.fingerprint_i64());

    let inner_padded = rsa.decrypt_rsa_pad(&req_dh.encrypted_data);
    let mut cur = Cursor::new(&inner_padded);
    let pq_inner = mtp_enums::PQInnerData::deserialize(&mut cur).expect("PQInnerData");
    let (new_nonce, observed_dc, observed_expires_in) = match pq_inner {
        mtp_enums::PQInnerData::Dc(d) => {
            assert_eq!(d.pq, pq_bytes);
            assert_eq!(d.p, expected_p);
            assert_eq!(d.q, expected_q);
            assert_eq!(d.nonce, req_pq.nonce);
            assert_eq!(d.server_nonce, server_nonce);
            assert_eq!(d.dc, expected_dc);
            (d.new_nonce, d.dc, None)
        }
        mtp_enums::PQInnerData::TempDc(d) => {
            assert_eq!(d.pq, pq_bytes);
            assert_eq!(d.p, expected_p);
            assert_eq!(d.q, expected_q);
            assert_eq!(d.nonce, req_pq.nonce);
            assert_eq!(d.server_nonce, server_nonce);
            assert_eq!(d.dc, expected_dc);
            assert!(d.expires_in > 0);
            (d.new_nonce, d.dc, Some(d.expires_in))
        }
        other => panic!("unexpected PQInnerData variant: {other:?}"),
    };

    if matches!(behavior, DhServerBehavior::ServerDhParamsFail) {
        let h = spec_sha1(&[&new_nonce]);
        let mut new_nonce_hash = [0u8; 16];
        new_nonce_hash.copy_from_slice(&h[4..20]);
        let fail = mtp_enums::ServerDHParams::Fail(mtp_types::ServerDHParamsFail {
            nonce: req_pq.nonce,
            server_nonce,
            new_nonce_hash,
        })
        .to_bytes();
        let _ = peer.tx.send(Ok(encode_unencrypted_packet(
            server_unencrypted_msg_id(server_time, 2),
            &fail,
        )));
        return None;
    }

    let (tmp_aes_key, tmp_aes_iv) = spec_tmp_aes_key_iv(&new_nonce, &server_nonce);

    let dh_prime_bytes = if matches!(behavior, DhServerBehavior::BadDhPrime) {
        let mut bad = GOOD_PRIME;
        bad[255] ^= 0x01; // even -> composite
        bad
    } else {
        GOOD_PRIME
    };
    let dh_prime = BigUint::from_bytes_be(&dh_prime_bytes);
    let g: i32 = 3;
    let a = BigUint::from_bytes_be(&[0x5Au8; 256]) % (&dh_prime - 2u32) + 2u32;
    let g_a_bytes = if matches!(behavior, DhServerBehavior::BadGaOne) {
        vec![1u8]
    } else {
        let g_a = BigUint::from(g as u32).modpow(&a, &dh_prime);
        to_fixed_be::<256>(&g_a).to_vec()
    };

    let server_inner =
        mtp_enums::ServerDHInnerData::ServerDHInnerData(mtp_types::ServerDHInnerData {
            nonce: req_pq.nonce,
            server_nonce,
            g,
            dh_prime: dh_prime_bytes.to_vec(),
            g_a: g_a_bytes,
            server_time,
        })
        .to_bytes();
    let mut sha = spec_sha1(&[&server_inner]);
    if matches!(behavior, DhServerBehavior::TamperServerDhSha1) {
        sha[0] ^= 0xFF;
    }
    let unpadded = 20 + server_inner.len();
    let padded_len = (unpadded + 15) & !15;
    let mut plain_answer = vec![0x42u8; padded_len];
    plain_answer[0..20].copy_from_slice(&sha);
    plain_answer[20..unpadded].copy_from_slice(&server_inner);
    let encrypted_answer = aes_ige_encrypt(&plain_answer, &tmp_aes_key, &tmp_aes_iv);

    let server_dh_ok = mtp_enums::ServerDHParams::Ok(mtp_types::ServerDHParamsOk {
        nonce: req_pq.nonce,
        server_nonce,
        encrypted_answer,
    })
    .to_bytes();
    peer.tx
        .send(Ok(encode_unencrypted_packet(
            server_unencrypted_msg_id(server_time, 2),
            &server_dh_ok,
        )))
        .ok()?;

    // 3. Receive set_client_DH_params (possibly retried once on RetryOnceThenOk).
    let mut observed_retry_ids = Vec::new();
    let mut attempt = 0u32;
    loop {
        let pkt3 = peer.rx.recv().await?;
        assert_eq!(&pkt3[0..8], &[0u8; 8]);
        let client_msg_id3 = u64::from_le_bytes(pkt3[8..16].try_into().unwrap());
        assert_eq!(client_msg_id3 % 4, 0);
        let set_client = mtp_functions::SetClientDHParams::from_bytes(&pkt3[20..])
            .expect("set_client_DH_params");
        assert_eq!(set_client.nonce, req_pq.nonce);
        assert_eq!(set_client.server_nonce, server_nonce);

        let dec_client = aes_ige_decrypt(&set_client.encrypted_data, &tmp_aes_key, &tmp_aes_iv);
        let mut cur3 = Cursor::new(&dec_client[20..]);
        let mtp_enums::ClientDHInnerData::ClientDHInnerData(client_dh) =
            mtp_enums::ClientDHInnerData::deserialize(&mut cur3).expect("client_DH_inner_data");
        let consumed3 = cur3.position();
        let expected_sha3 = spec_sha1(&[&dec_client[20..20 + consumed3]]);
        assert_eq!(
            &dec_client[0..20],
            &expected_sha3,
            "client_DH_inner_data SHA-1 mismatch"
        );
        assert_eq!(client_dh.nonce, req_pq.nonce);
        assert_eq!(client_dh.server_nonce, server_nonce);
        observed_retry_ids.push(client_dh.retry_id);

        let g_b = BigUint::from_bytes_be(&client_dh.g_b);
        let auth_key_big = g_b.modpow(&a, &dh_prime);
        let auth_key = to_fixed_be::<256>(&auth_key_big);
        let key_sha1 = spec_sha1(&[&auth_key]);
        let mut aux_hash = [0u8; 8];
        aux_hash.copy_from_slice(&key_sha1[0..8]);
        let auth_key_id = u64::from_le_bytes(key_sha1[12..20].try_into().unwrap());

        let salt1 = u64::from_le_bytes(new_nonce[0..8].try_into().unwrap());
        let salt2 = u64::from_le_bytes(server_nonce[0..8].try_into().unwrap());
        let initial_salt = salt1 ^ salt2;

        attempt += 1;
        match behavior {
            DhServerBehavior::RetryOnceThenOk if attempt == 1 => {
                assert_eq!(client_dh.retry_id, 0);
                let retry = mtp_enums::SetClientDHParamsAnswer::DhGenRetry(mtp_types::DhGenRetry {
                    nonce: req_pq.nonce,
                    server_nonce,
                    new_nonce_hash2: spec_new_nonce_hash(&new_nonce, 2, &aux_hash),
                })
                .to_bytes();
                peer.tx
                    .send(Ok(encode_unencrypted_packet(
                        server_unencrypted_msg_id(server_time, 3),
                        &retry,
                    )))
                    .ok()?;
                continue;
            }
            DhServerBehavior::DhGenFail => {
                let fail = mtp_enums::SetClientDHParamsAnswer::DhGenFail(mtp_types::DhGenFail {
                    nonce: req_pq.nonce,
                    server_nonce,
                    new_nonce_hash3: spec_new_nonce_hash(&new_nonce, 3, &aux_hash),
                })
                .to_bytes();
                let _ = peer.tx.send(Ok(encode_unencrypted_packet(
                    server_unencrypted_msg_id(server_time, 3),
                    &fail,
                )));
                return None;
            }
            DhServerBehavior::WrongNewNonceHash1 => {
                let mut wrong = spec_new_nonce_hash(&new_nonce, 1, &aux_hash);
                wrong[0] ^= 0x01;
                let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(mtp_types::DhGenOk {
                    nonce: req_pq.nonce,
                    server_nonce,
                    new_nonce_hash1: wrong,
                })
                .to_bytes();
                let _ = peer.tx.send(Ok(encode_unencrypted_packet(
                    server_unencrypted_msg_id(server_time, 3),
                    &ok,
                )));
                return None;
            }
            _ => {
                let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(mtp_types::DhGenOk {
                    nonce: req_pq.nonce,
                    server_nonce,
                    new_nonce_hash1: spec_new_nonce_hash(&new_nonce, 1, &aux_hash),
                })
                .to_bytes();
                peer.tx
                    .send(Ok(encode_unencrypted_packet(
                        server_unencrypted_msg_id(server_time, 3 + attempt),
                        &ok,
                    )))
                    .ok()?;
                return Some(ServerDhResult {
                    auth_key,
                    auth_key_id,
                    initial_salt,
                    observed_retry_ids,
                    observed_dc,
                    observed_expires_in,
                });
            }
        }
    }
}

// ============================================================================
// Area 1: Full DH Auth Key Creation Wire Conformance
// ============================================================================

#[tokio::test]
async fn dh_exchange_persistent_key_matches_independent_spec_server() {
    let rsa = TestRsaKeypair::deterministic();
    let dc_options = make_dc_options_with_rsa(&rsa.pub_key);
    let server_time = (local_unix_secs() - 600) as i32; // local clock is 600s ahead
    let clock = MtprotoClock::new();

    let (mut client_conn, peer) = FakeConnection::pair();
    let server_task = tokio::spawn(run_spec_dh_server(
        peer,
        rsa,
        server_time,
        2,
        DhServerBehavior::Normal,
    ));

    let (managed, initial_salt) =
        create_single_auth_key(&mut client_conn, 2, 2, &dc_options, &clock, 0)
            .await
            .expect("persistent DH exchange must succeed");
    let server_res = server_task
        .await
        .expect("server task panicked")
        .expect("server must finish");

    assert_eq!(managed.key_type, AuthKeyType::Generated);
    assert_eq!(managed.expires_at, None);
    assert_eq!(managed.key.data, server_res.auth_key);
    assert_eq!(managed.key_id(), server_res.auth_key_id);
    assert_eq!(initial_salt, server_res.initial_salt);
    assert_eq!(server_res.observed_dc, 2);
    assert_eq!(server_res.observed_expires_in, None);
    assert_eq!(server_res.observed_retry_ids, vec![0]);

    // Bug 1 regression check: after DH syncs server_time when local clock was 600s ahead,
    // the very next client msg_id must immediately reflect server_time (within a few seconds),
    // NOT stay pinned to the pre-sync local+600 timestamp!
    let next_id = clock.next_msg_id();
    let next_secs = (next_id >> 32) as i32;
    assert_eq!(next_id % 4, 0);
    assert!(
        (next_secs - server_time).abs() <= 5,
        "post-DH msg_id time {next_secs} should track server_time {server_time}"
    );
}

#[tokio::test]
async fn dh_exchange_temporary_key_and_dh_gen_retry_conform_to_spec() {
    let rsa = TestRsaKeypair::deterministic();
    let dc_options = make_dc_options_with_rsa(&rsa.pub_key);
    let server_time = local_unix_secs() as i32;
    let clock = MtprotoClock::new();

    let (mut client_conn, peer) = FakeConnection::pair();
    let server_task = tokio::spawn(run_spec_dh_server(
        peer,
        rsa,
        server_time,
        10002, // test DC 2
        DhServerBehavior::RetryOnceThenOk,
    ));

    let (managed, initial_salt) =
        create_single_auth_key(&mut client_conn, 2, 10002, &dc_options, &clock, 86_400)
            .await
            .expect("temporary DH exchange with retry must succeed");
    let server_res = server_task
        .await
        .expect("server task panicked")
        .expect("server must finish");

    assert_eq!(managed.key_type, AuthKeyType::Temporary);
    assert!(managed.expires_at.is_some());
    assert_eq!(managed.key.data, server_res.auth_key);
    assert_eq!(managed.key_id(), server_res.auth_key_id);
    assert_eq!(initial_salt, server_res.initial_salt);
    assert_eq!(server_res.observed_dc, 10002);
    assert_eq!(server_res.observed_expires_in, Some(86_400));
    assert_eq!(server_res.observed_retry_ids.len(), 2);
    assert_eq!(server_res.observed_retry_ids[0], 0);
    assert_ne!(
        server_res.observed_retry_ids[1], 0,
        "dh_gen_retry must send previous aux_hash as retry_id"
    );
}

#[tokio::test]
async fn dh_exchange_rejects_protocol_failures_and_tampering() {
    let rsa = TestRsaKeypair::deterministic();
    let cases = [
        (
            "server_DH_params_fail",
            DhServerBehavior::ServerDhParamsFail,
        ),
        ("tampered SHA-1", DhServerBehavior::TamperServerDhSha1),
        ("bad dh_prime", DhServerBehavior::BadDhPrime),
        ("bad g_a == 1", DhServerBehavior::BadGaOne),
        ("dh_gen_fail", DhServerBehavior::DhGenFail),
        (
            "wrong new_nonce_hash1",
            DhServerBehavior::WrongNewNonceHash1,
        ),
    ];
    for (label, behavior) in cases {
        let dc_options = make_dc_options_with_rsa(&rsa.pub_key);
        let clock = MtprotoClock::new();
        let server_time = local_unix_secs() as i32;

        let (mut client_conn, peer) = FakeConnection::pair();
        let server_task = tokio::spawn(run_spec_dh_server(
            peer,
            rsa.clone(),
            server_time,
            2,
            behavior,
        ));

        let res = create_single_auth_key(&mut client_conn, 2, 2, &dc_options, &clock, 0).await;
        assert!(res.is_err(), "DH exchange must reject {label}");
        // Drop client_conn so the server's peer.rx.recv() sees EOF when the client aborts early.
        drop(client_conn);
        let _ = server_task.await;
    }
}

#[test]
fn unencrypted_packet_framing_checks_auth_key_zero_and_server_parity() {
    let body = mtp_functions::ReqPqMulti { nonce: [0x11; 16] }.to_bytes();
    let msg_id = server_unencrypted_msg_id(1_700_000_000, 1);
    let pkt = encode_unencrypted_packet(msg_id, &body);
    assert_eq!(&pkt[0..8], &[0u8; 8]);
    let (decoded_id, decoded_body) = decode_unencrypted_packet(&pkt).expect("valid");
    assert_eq!(decoded_id, msg_id);
    assert_eq!(decoded_body, body.as_slice());

    // Non-zero auth_key_id must fail.
    let mut bad_key = pkt.clone();
    bad_key[0] = 1;
    assert!(decode_unencrypted_packet(&bad_key).is_err());

    // Client-parity msg_id (% 4 == 0) in server unencrypted response must fail.
    let bad_parity = encode_unencrypted_packet(msg_id & !0x03, &body);
    assert!(decode_unencrypted_packet(&bad_parity).is_err());
}

// ============================================================================
// Area 2: MTProto 2.0 Encrypted Packet Direction & Padding Rejection
// ============================================================================

#[test]
fn encrypted_packet_both_directions_and_wrong_x_or_padding_rejected() {
    let mut raw_key = [0u8; 256];
    for (i, b) in raw_key.iter_mut().enumerate() {
        *b = ((i as u32 * 73 + 19) & 0xFF) as u8;
    }
    let key = AuthKey::new(raw_key);
    let salt = 0x1122_3344_5566_7788u64;
    let session_id = 0x99AA_BBCC_DDEE_FF01u64;
    let body = mtp_functions::Ping { ping_id: 42 }.to_bytes();

    // Client -> Server (x = 0)
    let c_msg_id = 0x6555_1234_0000_0004u64;
    let c_pkt = build_encrypted_packet(salt, session_id, c_msg_id, 1, &body, &key, Side::Client);
    let c_env = parse_encrypted_packet(&c_pkt, &key, Side::Client).expect("client decrypt");
    assert_eq!(c_env.server_salt, salt);
    assert_eq!(c_env.session_id, session_id);
    assert_eq!(c_env.msg_id, c_msg_id);
    assert_eq!(c_env.seq_no, 1);
    assert_eq!(c_env.body, body);

    // Wrong direction (Client packet decrypted as Server -> wrong x = 8 vs 0) must be rejected.
    assert!(
        parse_encrypted_packet(&c_pkt, &key, Side::Server).is_err(),
        "wrong-x (Client parsed as Server) must be rejected"
    );

    // Server -> Client (x = 8)
    let s_msg_id = 0x6555_1234_0000_0005u64;
    let s_pkt = build_encrypted_packet(salt, session_id, s_msg_id, 1, &body, &key, Side::Server);
    let s_env = parse_encrypted_packet(&s_pkt, &key, Side::Server).expect("server decrypt");
    assert_eq!(s_env.msg_id, s_msg_id);
    assert_eq!(s_env.body, body);

    // Wrong direction (Server packet decrypted as Client -> wrong x = 0 vs 8) must be rejected.
    assert!(
        parse_encrypted_packet(&s_pkt, &key, Side::Client).is_err(),
        "wrong-x (Server parsed as Client) must be rejected"
    );

    // Wrong padding length (8 bytes < 12 minimum): construct manually with exact MTProto 2.0 KDF.
    let mut plain = Vec::with_capacity(32 + body.len());
    plain.extend_from_slice(&salt.to_le_bytes());
    plain.extend_from_slice(&session_id.to_le_bytes());
    plain.extend_from_slice(&s_msg_id.to_le_bytes());
    plain.extend_from_slice(&1u32.to_le_bytes());
    plain.extend_from_slice(&(body.len() as u32).to_le_bytes());
    plain.extend_from_slice(&body);
    // Build raw MTProto 2.0 packets with intentionally invalid padding (4 < 12 and 1028 > 1024)
    // to verify parse_encrypted_packet rejects them even when msg_key matches!
    let raw_encrypt_with_any_pad = |plain: &[u8], pad: &[u8], side: Side| -> Vec<u8> {
        let x = match side {
            Side::Client => 0usize,
            Side::Server => 8usize,
        };
        let mut padded = Vec::with_capacity(plain.len() + pad.len());
        padded.extend_from_slice(plain);
        padded.extend_from_slice(pad);
        let large = spec_sha256(&[&key.data[88 + x..88 + x + 32], &padded]);
        let mut msg_key = [0u8; 16];
        msg_key.copy_from_slice(&large[8..24]);
        let sha_a = spec_sha256(&[&msg_key, &key.data[x..x + 36]]);
        let sha_b = spec_sha256(&[&key.data[40 + x..40 + x + 36], &msg_key]);
        let mut aes_key = [0u8; 32];
        aes_key[0..8].copy_from_slice(&sha_a[0..8]);
        aes_key[8..24].copy_from_slice(&sha_b[8..24]);
        aes_key[24..32].copy_from_slice(&sha_a[24..32]);
        let mut aes_iv = [0u8; 32];
        aes_iv[0..8].copy_from_slice(&sha_b[0..8]);
        aes_iv[8..24].copy_from_slice(&sha_a[8..24]);
        aes_iv[24..32].copy_from_slice(&sha_b[24..32]);
        let enc = aes_ige_encrypt(&padded, &aes_key, &aes_iv);
        let mut pkt = Vec::with_capacity(24 + enc.len());
        pkt.extend_from_slice(&key.key_id());
        pkt.extend_from_slice(&msg_key);
        pkt.extend_from_slice(&enc);
        pkt
    };

    assert_eq!(plain.len() % 16, 12); // 44 bytes -> 4 bytes pads to 48, 20 bytes to 64
    let short_pad = [0xEEu8; 4]; // < 12 bytes
    assert!(encrypt_data_v2_with_padding(&plain, &short_pad, &key, Side::Server).is_err());
    let bad_pad_pkt = raw_encrypt_with_any_pad(&plain, &short_pad, Side::Server);
    assert!(
        parse_encrypted_packet(&bad_pad_pkt, &key, Side::Server).is_err(),
        "padding < 12 bytes must be rejected"
    );

    let huge_pad = [0xEEu8; 1028]; // > 1024 bytes (44 + 1028 = 1072, div by 16)
    assert!(encrypt_data_v2_with_padding(&plain, &huge_pad, &key, Side::Server).is_err());
    let huge_pad_pkt = raw_encrypt_with_any_pad(&plain, &huge_pad, Side::Server);
    assert!(
        parse_encrypted_packet(&huge_pad_pkt, &key, Side::Server).is_err(),
        "padding > 1024 bytes must be rejected"
    );

    // Valid boundary paddings (20 bytes and 1012 bytes for 44-byte plaintext) must succeed.
    let ok_min = encrypt_data_v2_with_padding(&plain, &[0x11u8; 20], &key, Side::Server)
        .expect("valid min padding");
    assert!(parse_encrypted_packet(&ok_min, &key, Side::Server).is_ok());
    let ok_max = encrypt_data_v2_with_padding(&plain, &[0x22u8; 1012], &key, Side::Server)
        .expect("valid max padding");
    assert!(parse_encrypted_packet(&ok_max, &key, Side::Server).is_ok());
}

// ============================================================================
// Area 3: Message IDs, Time Sync, seq_no, Replay/Window Checks, Bad Msg Codes
// ============================================================================

fn make_session(clock: MtprotoClock, initial_salt: u64) -> SessionState {
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock,
        dcenter,
        initial_salt,
    )
}

#[test]
fn client_msg_id_divisible_by_4_monotonic_and_clock_correction() {
    let clock = MtprotoClock::new();
    let mut prev = 0u64;
    for _ in 0..256 {
        let id = clock.next_msg_id();
        assert_eq!(id % 4, 0, "client msg_id must be divisible by 4");
        assert!(id > prev, "client msg_id must be strictly monotonic");
        prev = id;
    }

    // Bug 1 regression test: clock running +600s fast, generates a msg_id, then learns real
    // server time via non-forced update (e.g. DH) OR forced update with unchanged shift.
    let fast_clock = MtprotoClock::with_shift(600);
    let ahead_id = fast_clock.next_msg_id();
    let real_server_time = local_unix_secs() as i32;
    assert!((ahead_id >> 32) as i32 >= real_server_time + 595);

    // Non-forced update with shift change must reset last_msg_id so next_msg_id drops to server time.
    assert!(fast_clock.update_server_time(real_server_time, false));
    let corrected_id = fast_clock.next_msg_id();
    assert_eq!(corrected_id % 4, 0);
    assert!(
        ((corrected_id >> 32) as i32 - real_server_time).abs() <= 5,
        "next_msg_id should re-anchor to server time after non-forced shift change"
    );

    // Forced update when shift_secs is already 0 (no change) must STILL reset last_msg_id
    // if last_msg_id was pushed into the future (e.g. rapid bursts or prior ahead clock).
    let clock2 = MtprotoClock::new();
    let _ = clock2.update_server_time(real_server_time + 500, true);
    let high_id = clock2.next_msg_id();
    let shift_now = clock2.time_shift_secs();
    // Simulate server reporting same shift after burst; forced update must reset last_msg_id.
    let _ = clock2.update_server_time((local_unix_secs() + shift_now) as i32, true);
    let after_forced = clock2.next_msg_id();
    assert_eq!(after_forced % 4, 0);
    assert!(
        after_forced <= high_id + 4
            || ((after_forced >> 32) as i32 - (high_id >> 32) as i32).abs() <= 2
    );
}

#[test]
fn seq_no_rules_for_content_related_vs_service_messages() {
    // Verify constructor classification matches tdesktop SerializedRequest::needAck().
    assert!(!need_ack_for_constructor(MsgContainer::CONSTRUCTOR_ID));
    assert!(!need_ack_for_constructor(
        mtp_types::MsgsAck::CONSTRUCTOR_ID
    ));
    assert!(!need_ack_for_constructor(
        mtp_types::HttpWait::CONSTRUCTOR_ID
    ));
    assert!(!need_ack_for_constructor(
        mtp_types::BadMsgNotification::CONSTRUCTOR_ID
    ));
    assert!(!need_ack_for_constructor(
        mtp_types::MsgsStateInfo::CONSTRUCTOR_ID
    ));
    assert!(!need_ack_for_constructor(
        mtp_types::MsgsAllInfo::CONSTRUCTOR_ID
    ));
    assert!(!need_ack_for_constructor(
        mtp_types::MsgDetailedInfo::CONSTRUCTOR_ID
    ));
    assert!(!need_ack_for_constructor(
        mtp_types::MsgNewDetailedInfo::CONSTRUCTOR_ID
    ));

    // Content-related RPCs and ack-bearing service messages need odd seq_no.
    let get_config = tl_functions::help::GetConfig {}.to_bytes();
    assert!(body_needs_ack(&get_config));
    assert!(need_ack_for_constructor(
        mtp_functions::Ping::CONSTRUCTOR_ID
    ));
    assert!(need_ack_for_constructor(
        mtp_functions::GetFutureSalts::CONSTRUCTOR_ID
    ));
    assert!(need_ack_for_constructor(
        mtp_types::MsgsStateReq::CONSTRUCTOR_ID
    ));

    let clock = MtprotoClock::new();
    let mut sess = make_session(clock.clone(), 0x1111_2222_3333_4444);

    // 1. Standalone content-related RPC -> seq_no = 2*0 + 1 = 1, counter -> 1.
    sess.enqueue_request(QueuedRequest::new(1, get_config.clone()));
    let out1 = sess.build_next_outbound().expect("rpc 1");
    assert_eq!(out1.msg_id % 4, 0);
    assert_eq!(out1.seq_no, 1);
    assert_eq!(sess.messages_counter(), 1);

    // 2. Receive a content-related server update (seq_no = 1) to queue msgs_ack.
    let now = clock.now_secs();
    let upd_body = tl_enums::Updates::TooLong.to_bytes();
    let outcome = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: 0x1111_2222_3333_4444,
        session_id: sess.session_id(),
        msg_id: server_msg_id(now, 1, false),
        seq_no: 1,
        body: upd_body,
    });
    assert!(outcome.has_pending_acks);

    // 3. Standalone msgs_ack -> non-content-related -> seq_no = 2*1 = 2, counter stays 1!
    let out2 = sess.build_next_outbound().expect("msgs_ack");
    assert_eq!(out2.seq_no, 2);
    assert_eq!(sess.messages_counter(), 1);
    let ack_ctor = u32::from_le_bytes(out2.body[0..4].try_into().unwrap());
    assert_eq!(ack_ctor, mtp_types::MsgsAck::CONSTRUCTOR_ID);

    // 4. Container holding ping (content-related, seq_no = 3) + RPC (content-related, seq_no = 5):
    //    outer msg_container is non-content-related, so outer seq_no is even (2*3 = 6)!
    sess.schedule_ping(77);
    sess.enqueue_request(QueuedRequest::new(2, get_config));
    let out3 = sess.build_next_outbound().expect("container");
    assert_eq!(
        out3.seq_no % 2,
        0,
        "msg_container outer seq_no must be even"
    );
    let inner = unpack_container(&out3.body).expect("valid container");
    assert_eq!(inner.len(), 2);
    assert_eq!(inner[0].seqno, 3);
    assert_eq!(inner[1].seqno, 5);
    assert_eq!(out3.seq_no, 6);
    assert_eq!(sess.messages_counter(), 3);
}

#[test]
fn server_msg_id_parity_time_window_and_duplicate_rejection() {
    // Parity check
    assert!(!is_valid_server_msg_id(0x6555_0000_0000_0000));
    assert!(is_valid_server_msg_id(0x6555_0000_0000_0001));
    assert!(!is_valid_server_msg_id(0x6555_0000_0000_0002));
    assert!(is_valid_server_msg_id(0x6555_0000_0000_0003));

    // Time window boundaries: tdesktop `session_private.cpp:1371-1372` uses [-300s, +60s]
    // (slightly wider than the MTProto doc's +30s future bound).
    let base = 1_700_000_000i32;
    assert!(!is_bad_server_time(base - 300, base));
    assert!(is_bad_server_time(base - 301, base));
    assert!(!is_bad_server_time(base + 60, base));
    assert!(is_bad_server_time(base + 61, base));

    let clock = MtprotoClock::new();
    let now = clock.now_secs();
    let salt = 0xAABB_CCDD_EEFF_0011u64;
    let mut sess = make_session(clock, salt);
    let sid = sess.session_id();
    let upd = tl_enums::Updates::TooLong.to_bytes();

    // Bad parity (% 4 == 0 and % 4 == 2) -> RestartConnection.
    for bad_rem in [0u64, 2u64] {
        let out = sess.handle_decrypted(DecryptedEnvelope {
            server_salt: salt,
            session_id: sid,
            msg_id: ((now as u64) << 32) | bad_rem,
            seq_no: 1,
            body: upd.clone(),
        });
        assert_eq!(out.action, Some(SessionAction::RestartConnection));
        assert!(out.events.is_empty());
    }

    // Too old (-301s) unsolicited update -> ResetSession, no event emitted.
    let out_old = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: salt,
        session_id: sid,
        msg_id: server_msg_id(now - 301, 1, false),
        seq_no: 1,
        body: upd.clone(),
    });
    assert_eq!(out_old.action, Some(SessionAction::ResetSession));
    assert!(out_old.events.is_empty());

    // Too new (+61s) unsolicited update -> ResetSession, no event emitted.
    let out_new = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: salt,
        session_id: sid,
        msg_id: server_msg_id(now + 61, 2, false),
        seq_no: 3,
        body: upd.clone(),
    });
    assert_eq!(out_new.action, Some(SessionAction::ResetSession));
    assert!(out_new.events.is_empty());

    // Valid update delivered once, then duplicate msg_id silently ignored.
    let good_id = server_msg_id(now, 10, false);
    let out_ok = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: salt,
        session_id: sid,
        msg_id: good_id,
        seq_no: 5,
        body: upd.clone(),
    });
    assert_eq!(out_ok.events.len(), 1);
    let out_dup = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: salt,
        session_id: sid,
        msg_id: good_id,
        seq_no: 5,
        body: upd.clone(),
    });
    assert!(
        out_dup.events.is_empty(),
        "duplicate msg_id must be ignored"
    );

    // Duplicate inside a msg_container is also ignored.
    let container_body = pack_container(vec![CoreMessage {
        msg_id: good_id as i64,
        seqno: 5,
        body: RawObject(upd),
    }]);
    let out_cdup = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: salt,
        session_id: sid,
        msg_id: server_msg_id(now, 11, false),
        seq_no: 6,
        body: container_body,
    });
    assert!(
        out_cdup.events.is_empty(),
        "duplicate inner msg_id inside container must be ignored"
    );

    // ReceivedIdsManager TooOld check (> IDS_BUFFER_SIZE newer ids).
    let mut mgr = ReceivedIdsManager::new();
    for i in 1..=(IDS_BUFFER_SIZE as u64 + 5) {
        let id = ((now as u64) << 32) | (i << 2) | 1;
        assert_eq!(mgr.register_msg_id(id, true), RegisterIdResult::Success);
    }
    mgr.shrink();
    let ancient = (((now - 10) as u64) << 32) | 1;
    assert_eq!(mgr.register_msg_id(ancient, true), RegisterIdResult::TooOld);
}

#[test]
fn bad_time_guards_future_salts_msg_resend_req_and_containers() {
    // Regression tests for Bug 5 and MsgContainer bad_time clearing.
    let clock = MtprotoClock::new();
    let now = clock.now_secs();
    let initial_salt = 0x1000_0000_0000_0001u64;
    let mut sess = make_session(clock.clone(), initial_salt);
    let sid = sess.session_id();

    // Send a request so we can test whether MsgResendReq with bad_time triggers a resend.
    let req_body = tl_functions::help::GetConfig {}.to_bytes();
    sess.enqueue_request(QueuedRequest::new(1, req_body));
    let sent = sess.build_next_outbound().expect("outbound rpc");
    assert_eq!(sess.to_send_len(), 0);
    assert_eq!(sess.have_sent_len(), 1);

    // 1. Spoofed FutureSalts outside valid time window with unknown req_msg_id must NOT overwrite salt.
    let bad_future_salts = mtp_enums::FutureSalts::FutureSalts(mtp_types::FutureSalts {
        req_msg_id: 0xDEAD_BEEF_0000_0004u64 as i64,
        now: now - 500,
        salts: vec![mtp_types::FutureSalt {
            valid_since: now - 600,
            valid_until: now + 3600,
            salt: 0xBAD0_BAD0_BAD0_BAD0u64 as i64,
        }],
    })
    .to_bytes();
    let _ = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: initial_salt,
        session_id: sid,
        msg_id: server_msg_id(now - 500, 1, true),
        seq_no: 0,
        body: bad_future_salts,
    });
    assert_eq!(
        sess.salts().active_salt(),
        initial_salt,
        "unmatched bad_time future_salts must not overwrite salt"
    );
    assert_eq!(sess.salts().future_salt_count(), 0);

    // 2. Spoofed MsgResendReq outside valid time window must be ignored (no resend queued).
    let bad_resend = mtp_enums::MsgResendReq::MsgResendReq(mtp_types::MsgResendReq {
        msg_ids: vec![sent.msg_id as i64],
    })
    .to_bytes();
    let out_resend = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: initial_salt,
        session_id: sid,
        msg_id: server_msg_id(now - 500, 2, false),
        seq_no: 0,
        body: bad_resend,
    });
    assert_eq!(out_resend.action, None);
    assert_eq!(
        sess.to_send_len(),
        0,
        "bad_time msg_resend_req must not re-queue requests"
    );

    // 3. Container with bad_time holding [empty msgs_ack, spoofed Updates]:
    //    the ignored empty msgs_ack must NOT clear info.bad_time before Updates is processed!
    let empty_ack = mtp_enums::MsgsAck::MsgsAck(mtp_types::MsgsAck { msg_ids: vec![] }).to_bytes();
    let spoofed_upd = tl_enums::Updates::TooLong.to_bytes();
    let bad_container = pack_container(vec![
        CoreMessage {
            msg_id: server_msg_id(now - 500, 3, false) as i64,
            seqno: 0,
            body: RawObject(empty_ack),
        },
        CoreMessage {
            msg_id: server_msg_id(now - 500, 4, false) as i64,
            seqno: 1,
            body: RawObject(spoofed_upd),
        },
    ]);
    let out_container = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: initial_salt,
        session_id: sid,
        msg_id: server_msg_id(now - 500, 5, false),
        seq_no: 0,
        body: bad_container,
    });
    assert_eq!(
        out_container.action,
        Some(SessionAction::ResetSession),
        "container must not clear bad_time after ignored inner message"
    );
    assert!(out_container.events.is_empty());
}

#[test]
fn bad_msg_notification_codes_16_and_17_resync_time_and_resend() {
    // Code 16: msg_id too low (client clock 600s behind server).
    {
        let server_now = local_unix_secs() as i32;
        let clock = MtprotoClock::with_shift(-600);
        let salt = 0x2222_3333_4444_5555u64;
        let mut sess = make_session(clock.clone(), salt);
        let sid = sess.session_id();

        let rpc = tl_functions::help::GetConfig {}.to_bytes();
        sess.enqueue_request(QueuedRequest::new(10, rpc));
        let sent = sess.build_next_outbound().expect("first send");
        assert!((sent.msg_id >> 32) as i32 <= server_now - 595);

        let notif16 =
            mtp_enums::BadMsgNotification::BadMsgNotification(mtp_types::BadMsgNotification {
                bad_msg_id: sent.msg_id as i64,
                bad_msg_seqno: sent.seq_no as i32,
                error_code: 16,
            })
            .to_bytes();
        let outcome = sess.handle_decrypted(DecryptedEnvelope {
            server_salt: salt,
            session_id: sid,
            msg_id: server_msg_id(server_now, 1, true),
            seq_no: 0,
            body: notif16,
        });
        assert_eq!(outcome.action, Some(SessionAction::FlushOutbound));

        let resent = sess.build_next_outbound().expect("resent after code 16");
        let resent_secs = (resent.msg_id >> 32) as i32;
        assert!(
            (resent_secs - server_now).abs() <= 5,
            "code 16 must resync clock forward (got {resent_secs}, server {server_now})"
        );
        // Resent requests are wrapped in a msg_container (tdesktop forceSendInContainer).
        let inner = unpack_container(&resent.body).expect("resend wrapped in container");
        assert_eq!(inner.len(), 1);
    }

    // Code 17: msg_id too high (client clock 600s ahead of server).
    {
        let server_now = local_unix_secs() as i32;
        let clock = MtprotoClock::with_shift(600);
        let salt = 0x2222_3333_4444_5556u64;
        let mut sess = make_session(clock.clone(), salt);
        let sid = sess.session_id();

        let rpc = tl_functions::help::GetConfig {}.to_bytes();
        sess.enqueue_request(QueuedRequest::new(11, rpc));
        let sent = sess.build_next_outbound().expect("first send");
        assert!((sent.msg_id >> 32) as i32 >= server_now + 595);

        let notif17 =
            mtp_enums::BadMsgNotification::BadMsgNotification(mtp_types::BadMsgNotification {
                bad_msg_id: sent.msg_id as i64,
                bad_msg_seqno: sent.seq_no as i32,
                error_code: 17,
            })
            .to_bytes();
        let outcome = sess.handle_decrypted(DecryptedEnvelope {
            server_salt: salt,
            session_id: sid,
            msg_id: server_msg_id(server_now, 1, true),
            seq_no: 0,
            body: notif17,
        });
        assert_eq!(outcome.action, Some(SessionAction::FlushOutbound));

        let resent = sess.build_next_outbound().expect("resent after code 17");
        let resent_secs = (resent.msg_id >> 32) as i32;
        assert!(
            (resent_secs - server_now).abs() <= 5,
            "code 17 must resync clock backward (got {resent_secs}, server {server_now})"
        );
        let inner = unpack_container(&resent.body).expect("resend wrapped in container");
        assert_eq!(inner.len(), 1);
        assert!(
            ((inner[0].msg_id as u64) >> 32) as i32 <= server_now + 5,
            "inner resent msg_id must use corrected time"
        );
    }
}

#[test]
fn bad_msg_notification_codes_32_and_33_reset_session_and_clear_stale_to_send_seq_no() {
    // Tests both code 32 (seq_no too low) and code 33 (seq_no too high), plus Bug 4 regression:
    // requests already in `to_send` from a prior `resend()` must also have their stale
    // msg_id/seq_no cleared on `reset_session()` so multi-batch sends never emit old seq_nos!
    for error_code in [32i32, 33i32] {
        let clock = MtprotoClock::new();
        let now = clock.now_secs();
        let salt = 0x3333_4444_5555_6666u64;
        let mut sess = make_session(clock, salt);
        let old_sid = sess.session_id();

        // Enqueue two large requests (> CUT_CONTAINER_ON_BYTES combined) so they split across
        // two outbound batches after reset_session().
        let big_payload = vec![0xAAu8; CUT_CONTAINER_ON_BYTES / 2 + 128];
        let body1 = mtp_functions::Ping { ping_id: 1 }.to_bytes();
        let mut req1 = QueuedRequest::new(1, big_payload.clone());
        req1.needs_layer = false;
        let mut req2 = QueuedRequest::new(2, big_payload);
        req2.needs_layer = false;
        let mut req3 = QueuedRequest::new(3, body1);
        req3.needs_layer = false;

        sess.enqueue_request(req1);
        sess.enqueue_request(req2);
        sess.enqueue_request(req3);

        // Send all 3 so they acquire non-zero seq_nos (1, 3, 5).
        let b1 = sess.build_next_outbound().expect("batch 1");
        let b2 = sess.build_next_outbound().expect("batch 2");
        assert!(sess.messages_counter() >= 3);

        // Move one request back into `to_send` via resend() BEFORE bad_msg_notification(32/33),
        // leaving it in `to_send` with a stale seq_no != None.
        assert!(sess.resend(b2.msg_id));
        assert!(sess.to_send_len() >= 1);

        let notif =
            mtp_enums::BadMsgNotification::BadMsgNotification(mtp_types::BadMsgNotification {
                bad_msg_id: b1.msg_id as i64,
                bad_msg_seqno: b1.seq_no as i32,
                error_code,
            })
            .to_bytes();
        let outcome = sess.handle_decrypted(DecryptedEnvelope {
            server_salt: salt,
            session_id: old_sid,
            msg_id: server_msg_id(now, 1, true),
            seq_no: 0,
            body: notif,
        });
        assert_eq!(
            outcome.action,
            Some(SessionAction::ResetSession),
            "code {error_code} must trigger ResetSession"
        );

        sess.reset_session();
        assert_ne!(sess.session_id(), old_sid);
        assert_eq!(sess.messages_counter(), 0);
        assert_eq!(sess.to_send_len(), 3);

        // Drain all outbound batches after reset and collect every sent content seq_no.
        let mut seen_seq_nos = Vec::new();
        while let Some(out) = sess.build_next_outbound() {
            assert_eq!(out.session_id, sess.session_id());
            if out.body.len() >= 4
                && u32::from_le_bytes(out.body[0..4].try_into().unwrap())
                    == MsgContainer::CONSTRUCTOR_ID
            {
                for m in unpack_container(&out.body).expect("container") {
                    seen_seq_nos.push(m.seqno as u32);
                }
            } else {
                seen_seq_nos.push(out.seq_no);
            }
        }
        // Bug 4 regression: seq_nos across ALL batches must strictly be [1, 3, 5] starting fresh!
        assert_eq!(
            seen_seq_nos,
            vec![1, 3, 5],
            "all requests across multi-batch reset must get fresh seq_nos [1, 3, 5]"
        );
    }
}

#[test]
fn bad_server_salt_48_overrides_cached_future_salts_and_flushes_inside_container() {
    // Regression tests for Bug 2 (SaltManager::set_salt vs cached future_salts) and
    // Bug 3 (bad_server_salt / bad_msg_notification inside msg_container for ping).

    // Unit-level check for SaltManager::set_salt (Bug 2):
    let now = 1_700_000_000i32;
    let stale_salt = 0xAAAA_0000_0000_0001u64;
    let new_salt = 0xBBBB_0000_0000_0002u64;
    let mut salts = SaltManager::new(stale_salt);
    salts.apply_future_salts(
        now,
        vec![
            mtp_types::FutureSalt {
                valid_since: now - 100,
                valid_until: now + 3600,
                salt: stale_salt as i64,
            },
            mtp_types::FutureSalt {
                valid_since: now + 3600,
                valid_until: now + 7200,
                salt: 0xCCCC_0000_0000_0003u64 as i64,
            },
        ],
    );
    assert_eq!(salts.current_salt(now), stale_salt);
    salts.set_salt(new_salt);
    assert_eq!(
        salts.current_salt(now),
        new_salt,
        "set_salt must not be reverted by stale cached future_salts"
    );

    // Full SessionState check: bad_server_salt(48) inside a msg_container for an initial ping
    // while stale future_salts are cached!
    let clock = MtprotoClock::new();
    let server_now = clock.now_secs();
    let mut sess = make_session(clock, stale_salt);
    let sid = sess.session_id();
    sess.salts_mut().apply_future_salts(
        server_now,
        vec![mtp_types::FutureSalt {
            valid_since: server_now - 100,
            valid_until: server_now + 3600,
            salt: stale_salt as i64,
        }],
    );

    sess.schedule_ping(999);
    let sent_ping = sess.build_next_outbound().expect("initial ping");
    assert_eq!(sent_ping.salt, stale_salt);
    assert_eq!(sess.to_send_len(), 0);

    // Server wraps bad_server_salt(48) inside a msg_container!
    let bad_salt_body = mtp_enums::BadMsgNotification::BadServerSalt(mtp_types::BadServerSalt {
        bad_msg_id: sent_ping.msg_id as i64,
        bad_msg_seqno: sent_ping.seq_no as i32,
        error_code: 48,
        new_server_salt: new_salt as i64,
    })
    .to_bytes();
    let container_body = pack_container(vec![CoreMessage {
        msg_id: server_msg_id(server_now, 1, true) as i64,
        seqno: 0,
        body: RawObject(bad_salt_body),
    }]);

    let outcome = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: new_salt,
        session_id: sid,
        msg_id: server_msg_id(server_now, 2, false),
        seq_no: 0,
        body: container_body,
    });
    // Bug 3 regression: FlushOutbound MUST be preserved through MsgContainer even when to_send is empty!
    assert_eq!(
        outcome.action,
        Some(SessionAction::FlushOutbound),
        "bad_server_salt inside msg_container for ping must return FlushOutbound"
    );

    // Bug 2 regression: the resent ping MUST use new_salt, not stale_salt from future_salts!
    let resent_ping = sess.build_next_outbound().expect("resent ping");
    assert_eq!(
        resent_ping.salt, new_salt,
        "resent ping must use new_server_salt instead of reverted future_salts entry"
    );

    // Also verify RpcResult delivery works cleanly after recovery.
    let rpc_body = tl_functions::help::GetConfig {}.to_bytes();
    sess.enqueue_request(QueuedRequest::new(50, rpc_body));
    let sent_rpc = sess.build_next_outbound().expect("rpc after salt fix");
    assert_eq!(sent_rpc.salt, new_salt);

    let rpc_res = Boxed(RpcResult {
        req_msg_id: sent_rpc.msg_id as i64,
        result: RawObject(tg_tl::BOOL_TRUE_ID.to_le_bytes().to_vec()),
    })
    .to_bytes();
    let res_out = sess.handle_decrypted(DecryptedEnvelope {
        server_salt: new_salt,
        session_id: sid,
        msg_id: server_msg_id(server_now, 3, true),
        seq_no: 1,
        body: rpc_res,
    });
    assert!(matches!(
        res_out.events.as_slice(),
        [SessionEvent::RpcResult { request_id: 50, .. }]
    ));
}
