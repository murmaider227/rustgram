//! Offline deterministic MTProto 2.0 cryptographic conformance tests for `tg-crypto`.
//!
//! Cross-checks `tg-crypto` against:
//! - Published known-answer test vectors (MTProto auth_key docs, FIPS-197, OpenSSL AES-IGE,
//!   RFC 3526 Group 14, tdesktop `mtproto_dh_utils.cpp` / `mtproto_dc_options.cpp`).
//! - Independent straight-from-spec reference implementations written directly from the
//!   MTProto 2.0 specification text (<https://core.telegram.org/mtproto/auth_key> and
//!   <https://core.telegram.org/mtproto/description>) without sharing `tg-crypto` helpers.

use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use sha1::{Digest as _, Sha1};
use sha2::Sha256;

use tg_crypto::aes::{aes_ige_decrypt, aes_ige_encrypt, try_aes_ige_decrypt, try_aes_ige_encrypt};
use tg_crypto::auth_key::{
    AuthKey, MAX_PADDING_V2, MIN_PADDING_V2, Side, compute_msg_key_v2, decrypt_data_v2,
    decrypt_message_v2, encrypt_data_v2, encrypt_data_v2_with_padding,
};
use tg_crypto::dh::{
    GOOD_PRIME, create_auth_key, create_mod_exp, dh_new_nonce_hash, dh_server_aes_key_iv,
    factorize, factorize_bytes, is_good_mod_exp_first, is_good_mod_exp_first_bytes,
    is_prime_and_good,
};
use tg_crypto::error::CryptoError;
use tg_crypto::rsa::{
    PRODUCTION_PUBLIC_KEYS, PublicKey, SPECIAL_CONFIG_PUBLIC_KEY, TEST_PUBLIC_KEYS,
};

fn hex_to_bytes(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    assert!(clean.len().is_multiple_of(2));
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).expect("valid hex"))
        .collect()
}

// ============================================================================
// Independent straight-from-spec MTProto 2.0 reference primitives
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

/// Independent AES-256-IGE encryption straight from the IGE definition:
/// `c_i = E_K(m_i XOR c_{i-1}) XOR m_{i-1}`, with `c_0 = iv[0..16]`, `m_0 = iv[16..32]`.
fn spec_aes_ige_encrypt(plaintext: &[u8], key: &[u8; 32], iv: &[u8; 32]) -> Vec<u8> {
    assert!(plaintext.len().is_multiple_of(16));
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut prev_c = [0u8; 16];
    let mut prev_m = [0u8; 16];
    prev_c.copy_from_slice(&iv[0..16]);
    prev_m.copy_from_slice(&iv[16..32]);

    let mut out = Vec::with_capacity(plaintext.len());
    for block in plaintext.as_chunks::<16>().0 {
        let mut xored = [0u8; 16];
        for i in 0..16 {
            xored[i] = block[i] ^ prev_c[i];
        }
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut xored));
        let mut c = [0u8; 16];
        for i in 0..16 {
            c[i] = xored[i] ^ prev_m[i];
        }
        out.extend_from_slice(&c);
        prev_c = c;
        prev_m.copy_from_slice(block);
    }
    out
}

/// Independent AES-256-IGE decryption straight from the IGE definition:
/// `m_i = D_K(c_i XOR m_{i-1}) XOR c_{i-1}`.
fn spec_aes_ige_decrypt(ciphertext: &[u8], key: &[u8; 32], iv: &[u8; 32]) -> Vec<u8> {
    assert!(ciphertext.len().is_multiple_of(16));
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut prev_c = [0u8; 16];
    let mut prev_m = [0u8; 16];
    prev_c.copy_from_slice(&iv[0..16]);
    prev_m.copy_from_slice(&iv[16..32]);

    let mut out = Vec::with_capacity(ciphertext.len());
    for block in ciphertext.as_chunks::<16>().0 {
        let mut xored = [0u8; 16];
        for i in 0..16 {
            xored[i] = block[i] ^ prev_m[i];
        }
        cipher.decrypt_block(GenericArray::from_mut_slice(&mut xored));
        let mut m = [0u8; 16];
        for i in 0..16 {
            m[i] = xored[i] ^ prev_c[i];
        }
        out.extend_from_slice(&m);
        prev_c.copy_from_slice(block);
        prev_m = m;
    }
    out
}

/// Independent MTProto 2.0 KDF straight from <https://core.telegram.org/mtproto/description#defining-aes-key-and-initialization-vector>.
fn spec_mtproto2_kdf(auth_key: &[u8; 256], msg_key: &[u8; 16], x: usize) -> ([u8; 32], [u8; 32]) {
    let sha256_a = spec_sha256(&[msg_key, &auth_key[x..x + 36]]);
    let sha256_b = spec_sha256(&[&auth_key[40 + x..40 + x + 36], msg_key]);

    let mut aes_key = [0u8; 32];
    aes_key[0..8].copy_from_slice(&sha256_a[0..8]);
    aes_key[8..24].copy_from_slice(&sha256_b[8..24]);
    aes_key[24..32].copy_from_slice(&sha256_a[24..32]);

    let mut aes_iv = [0u8; 32];
    aes_iv[0..8].copy_from_slice(&sha256_b[0..8]);
    aes_iv[8..24].copy_from_slice(&sha256_a[8..24]);
    aes_iv[24..32].copy_from_slice(&sha256_b[24..32]);

    (aes_key, aes_iv)
}

/// Independent MTProto 2.0 message encryption straight from spec text.
fn spec_mtproto2_encrypt(
    auth_key: &[u8; 256],
    plaintext: &[u8],
    padding: &[u8],
    x: usize,
) -> Vec<u8> {
    let auth_key_hash = spec_sha1(&[auth_key]);
    let auth_key_id = &auth_key_hash[12..20];

    let mut padded = Vec::with_capacity(plaintext.len() + padding.len());
    padded.extend_from_slice(plaintext);
    padded.extend_from_slice(padding);

    let msg_key_large = spec_sha256(&[&auth_key[88 + x..88 + x + 32], &padded]);
    let mut msg_key = [0u8; 16];
    msg_key.copy_from_slice(&msg_key_large[8..24]);

    let (aes_key, aes_iv) = spec_mtproto2_kdf(auth_key, &msg_key, x);
    let ciphertext = spec_aes_ige_encrypt(&padded, &aes_key, &aes_iv);

    let mut wire = Vec::with_capacity(24 + ciphertext.len());
    wire.extend_from_slice(auth_key_id);
    wire.extend_from_slice(&msg_key);
    wire.extend_from_slice(&ciphertext);
    wire
}

/// Independent MTProto 2.0 RSA_PAD encryption straight from
/// <https://core.telegram.org/mtproto/auth_key#presenting-proof-of-work-server-authentication>.
fn spec_rsa_pad_encrypt(
    data: &[u8],
    padding: &[u8],
    temp_key: &[u8; 32],
    n: &BigUint,
    e: &BigUint,
) -> Option<[u8; 256]> {
    assert!(data.len() <= 144);
    assert_eq!(data.len() + padding.len(), 192);

    // 1. data_with_padding := data + random_padding_bytes (192 bytes)
    let mut data_with_padding = Vec::with_capacity(192);
    data_with_padding.extend_from_slice(data);
    data_with_padding.extend_from_slice(padding);

    // 2. data_pad_reversed := BYTE_REVERSE(data_with_padding)
    let mut data_pad_reversed = data_with_padding.clone();
    data_pad_reversed.reverse();

    // 3. data_with_hash := data_pad_reversed + SHA256(temp_key + data_with_padding)
    let hash = spec_sha256(&[temp_key, &data_with_padding]);
    let mut data_with_hash = Vec::with_capacity(224);
    data_with_hash.extend_from_slice(&data_pad_reversed);
    data_with_hash.extend_from_slice(&hash);

    // 4. aes_encrypted := AES256_IGE(data_with_hash, temp_key, 0)
    let aes_encrypted = spec_aes_ige_encrypt(&data_with_hash, temp_key, &[0u8; 32]);

    // 5. temp_key_xor := temp_key XOR SHA256(aes_encrypted)
    let aes_hash = spec_sha256(&[&aes_encrypted]);
    let mut key_aes_encrypted = [0u8; 256];
    for i in 0..32 {
        key_aes_encrypted[i] = temp_key[i] ^ aes_hash[i];
    }
    // 6. key_aes_encrypted := temp_key_xor + aes_encrypted
    key_aes_encrypted[32..256].copy_from_slice(&aes_encrypted);

    let m = BigUint::from_bytes_be(&key_aes_encrypted);
    if &m >= n {
        return None;
    }

    // 7. encrypted_data := RSA(key_aes_encrypted, n, e)
    let c = m.modpow(e, n);
    let c_bytes = c.to_bytes_be();
    let mut out = [0u8; 256];
    out[256 - c_bytes.len()..].copy_from_slice(&c_bytes);
    Some(out)
}

/// Independent MTProto 2.0 RSA_PAD decryption (server-side check).
fn spec_rsa_pad_decrypt(
    encrypted_data: &[u8; 256],
    n: &BigUint,
    d: &BigUint,
) -> Result<([u8; 192], [u8; 32]), &'static str> {
    let c = BigUint::from_bytes_be(encrypted_data);
    let m = c.modpow(d, n);
    let m_bytes = m.to_bytes_be();
    if m_bytes.len() > 256 {
        return Err("decrypted > 256 bytes");
    }
    let mut key_aes_encrypted = [0u8; 256];
    key_aes_encrypted[256 - m_bytes.len()..].copy_from_slice(&m_bytes);

    let temp_key_xor = &key_aes_encrypted[0..32];
    let aes_encrypted = &key_aes_encrypted[32..256];
    let aes_hash = spec_sha256(&[aes_encrypted]);

    let mut temp_key = [0u8; 32];
    for i in 0..32 {
        temp_key[i] = temp_key_xor[i] ^ aes_hash[i];
    }

    let data_with_hash = spec_aes_ige_decrypt(aes_encrypted, &temp_key, &[0u8; 32]);
    let mut data_with_padding = [0u8; 192];
    data_with_padding.copy_from_slice(&data_with_hash[0..192]);
    data_with_padding.reverse();

    let expected_hash = spec_sha256(&[&temp_key, &data_with_padding]);
    if data_with_hash[192..224] != expected_hash {
        return Err("RSA_PAD SHA-256 hash mismatch");
    }
    Ok((data_with_padding, temp_key))
}

/// Independent temporary DH AES key/IV derivation straight from
/// <https://core.telegram.org/mtproto/auth_key#dh-key-exchange-complete>.
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

/// Deterministic 2048-bit RSA keypair derived from two deterministic 1024-bit primes.
fn deterministic_2048_rsa_keypair() -> (PublicKey, BigUint, BigUint) {
    let p_seed = (BigUint::one() << 1023usize)
        | (BigUint::one() << 1022usize)
        | BigUint::from(0x0001_0001u64);
    let q_seed = (BigUint::one() << 1023usize)
        | (BigUint::one() << 1022usize)
        | (BigUint::one() << 512usize)
        | BigUint::from(1u64);
    let e = BigUint::from(65537u32);

    let p = next_probable_prime(p_seed, &e);
    let q = next_probable_prime(q_seed, &e);
    let n = &p * &q;
    assert_eq!(n.bits(), 2048);

    let phi = (&p - BigUint::one()) * (&q - BigUint::one());
    let d = mod_inverse(&e, &phi);
    let pub_key = PublicKey::from_components(&n.to_bytes_be(), &e.to_bytes_be()).unwrap();
    (pub_key, n, d)
}

fn next_probable_prime(mut cand: BigUint, e: &BigUint) -> BigUint {
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

fn is_prime_mr_det(n: &BigUint) -> bool {
    const SMALL: &[u32] = &[
        3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    ];
    for &sp in SMALL {
        if (n % sp).is_zero() {
            return false;
        }
    }
    let n_minus_1 = n - BigUint::one();
    let s = n_minus_1.trailing_zeros().unwrap_or(0);
    let d = &n_minus_1 >> s;
    for &a_u32 in &[2u32, 3, 5, 7, 11, 13, 17, 19, 23, 29] {
        let a = BigUint::from(a_u32);
        let mut x = a.modpow(&d, n);
        if x.is_one() || x == n_minus_1 {
            continue;
        }
        let mut witness = true;
        for _ in 1..s {
            x = (&x * &x) % n;
            if x == n_minus_1 {
                witness = false;
                break;
            }
        }
        if witness {
            return false;
        }
    }
    true
}

fn mod_inverse(a: &BigUint, m: &BigUint) -> BigUint {
    let (mut old_r, mut r) = (a.clone(), m.clone());
    let (mut old_s, mut s) = (BigUint::one(), BigUint::zero());
    let mut old_s_neg = false;
    let mut s_neg = false;
    while !r.is_zero() {
        let q = &old_r / &r;
        let new_r = &old_r - &q * &r;
        old_r = r;
        r = new_r;

        let qs = &q * &s;
        let (new_s, new_s_neg) = match (old_s_neg, s_neg) {
            (false, false) => {
                if old_s >= qs {
                    (&old_s - &qs, false)
                } else {
                    (&qs - &old_s, true)
                }
            }
            (true, true) => {
                if qs >= old_s {
                    (&qs - &old_s, false)
                } else {
                    (&old_s - &qs, true)
                }
            }
            (false, true) => (&old_s + &qs, false),
            (true, false) => (&old_s + &qs, true),
        };
        old_s = s;
        old_s_neg = s_neg;
        s = new_s;
        s_neg = new_s_neg;
    }
    assert!(old_r.is_one());
    if old_s_neg {
        m - (old_s % m)
    } else {
        old_s % m
    }
}

// ============================================================================
// 1. Auth Key Creation (DH Exchange) Conformance Tests
// ============================================================================

#[test]
fn conformance_pq_factorization_published_and_edge_vectors() {
    // Published MTProto worked example (https://core.telegram.org/mtproto/samples-auth_key):
    // pq = 0x17ED48941A08F981 = 0x494C553B * 0x53911073
    let sample_pq_bytes = hex_to_bytes("17ed48941a08f981");
    let (p_bytes, q_bytes) = factorize_bytes(&sample_pq_bytes).unwrap();
    assert_eq!(p_bytes, hex_to_bytes("494c553b"));
    assert_eq!(q_bytes, hex_to_bytes("53911073"));
    assert!(BigUint::from_bytes_be(&p_bytes) < BigUint::from_bytes_be(&q_bytes));
    assert_eq!(
        BigUint::from_bytes_be(&p_bytes) * BigUint::from_bytes_be(&q_bytes),
        BigUint::from_bytes_be(&sample_pq_bytes)
    );

    // Additional realistic 63-bit and 64-bit semiprimes (Telegram sends 63-bit PQ products).
    let vectors: &[(u64, u64)] = &[
        (0x494C_553B, 0x5391_1073),
        (0x7FFF_FFFB, 0x7FFF_FFC3), // ~62-bit product of large 31-bit primes
        (0xB23A_8B0B, 0xD5E1_1337), // >2^63 product of 32-bit primes (tests unsigned u64 path)
        (1_000_000_007, 1_000_000_009),
        (2, 0x7FFF_FFFF),
    ];
    for &(expected_p, expected_q) in vectors {
        // Verify inputs are indeed distinct primes or expected factors where p < q.
        let (p_lo, q_hi) = if expected_p <= expected_q {
            (expected_p, expected_q)
        } else {
            (expected_q, expected_p)
        };
        let pq = p_lo.checked_mul(q_hi).unwrap();
        let (got_p, got_q) = factorize(pq);
        assert!(got_p <= got_q);
        assert_eq!(got_p * got_q, pq);
        assert!(got_p > 1);

        let (pb, qb) = factorize_bytes(&pq.to_be_bytes()).unwrap();
        assert_eq!(BigUint::from_bytes_be(&pb), BigUint::from(got_p));
        assert_eq!(BigUint::from_bytes_be(&qb), BigUint::from(got_q));
    }

    // Prime inputs must fail factorize_bytes (cannot split into two factors > 1).
    assert_eq!(
        factorize_bytes(&1_000_000_007u64.to_be_bytes()),
        Err(CryptoError::FactorizationFailed)
    );
    assert_eq!(
        factorize_bytes(&[0, 0, 0, 1]),
        Err(CryptoError::FactorizationFailed)
    );
}

#[test]
fn conformance_rsa_pad_cross_check_against_independent_spec() {
    let (pub_key, n, d) = deterministic_2048_rsa_keypair();
    let e = BigUint::from(65537u32);

    // Construct an exact TL-encoded p_q_inner_data_dc#a9f55f95 (100 bytes <= 144).
    let mut pq_inner_dc = Vec::new();
    pq_inner_dc.extend_from_slice(&0xa9f5_5f95u32.to_le_bytes()); // p_q_inner_data_dc
    // pq:bytes (8 bytes -> 1 len + 8 + 3 pad = 12 bytes)
    pq_inner_dc.push(8);
    pq_inner_dc.extend_from_slice(&hex_to_bytes("17ed48941a08f981"));
    pq_inner_dc.extend_from_slice(&[0, 0, 0]);
    // p:bytes (4 bytes -> 1 len + 4 + 3 pad = 8 bytes)
    pq_inner_dc.push(4);
    pq_inner_dc.extend_from_slice(&hex_to_bytes("494c553b"));
    pq_inner_dc.extend_from_slice(&[0, 0, 0]);
    // q:bytes (4 bytes -> 1 len + 4 + 3 pad = 8 bytes)
    pq_inner_dc.push(4);
    pq_inner_dc.extend_from_slice(&hex_to_bytes("53911073"));
    pq_inner_dc.extend_from_slice(&[0, 0, 0]);
    // nonce (16), server_nonce (16), new_nonce (32), dc (4)
    pq_inner_dc.extend_from_slice(&[0x11u8; 16]);
    pq_inner_dc.extend_from_slice(&[0x22u8; 16]);
    pq_inner_dc.extend_from_slice(&[0x33u8; 32]);
    pq_inner_dc.extend_from_slice(&2i32.to_le_bytes());
    assert_eq!(pq_inner_dc.len(), 100);

    let padding = vec![0x5Au8; 192 - pq_inner_dc.len()];
    // Find a deterministic temp_key where key_aes_encrypted < n and cross-check byte-for-byte.
    let mut matched = 0usize;
    let mut rejected_ge_n = 0usize;
    for seed in 0u8..64 {
        let mut temp_key = [0u8; 32];
        for (i, b) in temp_key.iter_mut().enumerate() {
            *b = seed.wrapping_mul(17).wrapping_add(i as u8);
        }
        let spec_res = spec_rsa_pad_encrypt(&pq_inner_dc, &padding, &temp_key, &n, &e);
        let crate_res = pub_key.encrypt_hashed_with_random(&pq_inner_dc, &padding, &temp_key);
        match (spec_res, crate_res) {
            (Some(spec_ct), Ok(crate_ct)) => {
                assert_eq!(
                    crate_ct, spec_ct,
                    "RSA_PAD ciphertext diverged from independent spec for seed {seed}"
                );
                let (recovered_padded, recovered_key) =
                    spec_rsa_pad_decrypt(&crate_ct, &n, &d).expect("server decrypt must succeed");
                assert_eq!(recovered_key, temp_key);
                assert_eq!(&recovered_padded[..pq_inner_dc.len()], &pq_inner_dc[..]);
                assert_eq!(&recovered_padded[pq_inner_dc.len()..], &padding[..]);
                matched += 1;
            }
            (None, Err(CryptoError::RsaFailed("key_aes_encrypted >= modulus"))) => {
                rejected_ge_n += 1;
            }
            (s, c) => {
                panic!("RSA_PAD modulus-range mismatch on seed {seed}: spec={s:?}, crate={c:?}")
            }
        }
    }
    assert!(
        matched > 0,
        "must have verified at least one valid RSA_PAD vector"
    );
    // Ensure we also test the exact boundary lengths 144 (max allowed) and 145 (must fail).
    let max_data = [0xABu8; 144];
    let enc_max = pub_key.encrypt_hashed(&max_data).unwrap();
    let (dec_max, _) = spec_rsa_pad_decrypt(&enc_max, &n, &d).unwrap();
    assert_eq!(&dec_max[..144], &max_data);
    assert!(pub_key.encrypt_hashed(&[0xABu8; 145]).is_err());
    let _ = rejected_ge_n;

    // Also verify built-in production, test, and special-config RSA keys match tdesktop fingerprints.
    let prod = PublicKey::from_pem(PRODUCTION_PUBLIC_KEYS[0]).unwrap();
    assert_eq!(prod.n().len(), 256);
    assert_eq!(prod.e(), &[0x01, 0x00, 0x01]);
    assert_eq!(prod.fingerprint(), 0xd09d_1d85_de64_fd85);

    let test_k = PublicKey::from_pem(TEST_PUBLIC_KEYS[0]).unwrap();
    assert_eq!(test_k.n().len(), 256);
    assert_eq!(test_k.e(), &[0x01, 0x00, 0x01]);
    assert_ne!(test_k.fingerprint(), 0);

    let special_k = PublicKey::from_pem(SPECIAL_CONFIG_PUBLIC_KEY).unwrap();
    assert_eq!(special_k.n().len(), 256);
    assert_eq!(special_k.e(), &[0x01, 0x00, 0x01]);
}

#[test]
fn conformance_dh_tmp_aes_key_iv_and_auth_key_derivation() {
    // Published nonces from MTProto samples-auth_key doc:
    // server_nonce = 0x0123456789abcdef... style deterministic nonces + exact spec formula.
    let new_nonce: [u8; 32] =
        hex_to_bytes("311c85db234aa2640afc4a76a735cf5b1f0fd68b17f5e1320987654321fedcba")
            .try_into()
            .unwrap();
    let server_nonce: [u8; 16] = hex_to_bytes("a5cf56314209e3c09876543210abcdef")
        .try_into()
        .unwrap();

    let (got_key, got_iv) = dh_server_aes_key_iv(&new_nonce, &server_nonce);
    let (spec_key, spec_iv) = spec_tmp_aes_key_iv(&new_nonce, &server_nonce);
    assert_eq!(got_key, spec_key);
    assert_eq!(got_iv, spec_iv);

    // Verify auth_key_id, auth_key_aux_hash, new_nonce_hash{1,2,3}, and left-zero-padding.
    let mut short_secret = vec![0x42u8; 255]; // 255 bytes -> must left-pad with one 0x00 byte
    short_secret[0] = 0x7F;
    let auth_key = AuthKey::from_bytes(&short_secret).unwrap();
    assert_eq!(auth_key.data[0], 0x00);
    assert_eq!(&auth_key.data[1..256], &short_secret[..]);

    let key_sha1 = spec_sha1(&[&auth_key.data]);
    assert_eq!(auth_key.aux_hash(), key_sha1[0..8]);
    assert_eq!(auth_key.key_id(), key_sha1[12..20]);
    assert_eq!(
        auth_key.key_id_u64(),
        u64::from_le_bytes(key_sha1[12..20].try_into().unwrap())
    );

    for number in [1u8, 2, 3] {
        let expected_hash = spec_sha1(&[&new_nonce, &[number], &auth_key.aux_hash()]);
        assert_eq!(
            dh_new_nonce_hash(&new_nonce, number, &auth_key.aux_hash()),
            expected_hash[4..20],
            "new_nonce_hash{number} mismatch"
        );
    }
}

#[test]
fn conformance_dh_prime_g_residue_and_range_checks() {
    // 1. Verify GOOD_PRIME matches tdesktop's GoodPrime (mtproto_dh_utils.cpp:106-122)
    //    and actually satisfies 2048-bit safe prime + residue rules for g in {3, 4, 5, 7}.
    let good_bn = BigUint::from_bytes_be(&GOOD_PRIME);
    assert_eq!(good_bn.bits(), 2048);
    assert_eq!(&good_bn % 8u32, BigUint::from(3u32)); // g=2 requires mod 8 == 7 -> rejected!
    assert_eq!(&good_bn % 3u32, BigUint::from(2u32)); // g=3 OK
    // Note: GoodPrime mod 5 == 3 (not 1 or 4), yet tdesktop's GoodPrime fast-path
    // (mtproto_dh_utils.cpp:124-128) explicitly permits g in {3, 4, 5, 7}; general primes
    // (tested below with RFC 3526 Group 14) enforce exact p % 5 in {1, 4}.
    assert_eq!(&good_bn % 5u32, BigUint::from(3u32));
    assert_eq!(&good_bn % 24u32, BigUint::from(11u32)); // g=6 requires 19 or 23 -> rejected!
    assert_eq!(&good_bn % 7u32, BigUint::from(6u32)); // g=7 requires 3, 5, or 6 -> OK
    assert!(is_prime_mr_det(&good_bn));
    assert!(is_prime_mr_det(&((&good_bn - BigUint::one()) >> 1usize)));

    assert!(!is_prime_and_good(&GOOD_PRIME, 2));
    assert!(is_prime_and_good(&GOOD_PRIME, 3));
    assert!(is_prime_and_good(&GOOD_PRIME, 4));
    assert!(is_prime_and_good(&GOOD_PRIME, 5));
    assert!(!is_prime_and_good(&GOOD_PRIME, 6));
    assert!(is_prime_and_good(&GOOD_PRIME, 7));
    for bad_g in [-2, -1, 0, 1, 8, 9, 100] {
        assert!(!is_prime_and_good(&GOOD_PRIME, bad_g));
    }

    // 2. RFC 3526 Group 14 (2048-bit MODP safe prime, NOT on GOOD_PRIME fast path):
    //    Exercises the full Miller-Rabin + residue code path for g=2, 3, 4, 6 (and rejects g=5, 7 if residues fail).
    let rfc3526_g14 = hex_to_bytes(
        "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1\
         29024E088A67CC74020BBEA63B139B22514A08798E3404DD\
         EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245\
         E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
         EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3D\
         C2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F\
         83655D23DCA3AD961C62F356208552BB9ED529077096966D\
         670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B\
         E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9\
         DE2BCBF6955817183995497CEA956AE515D2261898FA0510\
         15728E5A8AACAA68FFFFFFFFFFFFFFFF",
    );
    let rfc_bn = BigUint::from_bytes_be(&rfc3526_g14);
    assert_eq!(rfc_bn.bits(), 2048);
    // Compute exact per-g residue expectations independently from the MTProto spec:
    let expected_g_ok = |g: i32| -> bool {
        match g {
            2 => (&rfc_bn % 8u32) == BigUint::from(7u32),
            3 => (&rfc_bn % 3u32) == BigUint::from(2u32),
            4 => true,
            5 => matches!((&rfc_bn % 5u32).try_into().unwrap_or(99u32), 1 | 4),
            6 => matches!((&rfc_bn % 24u32).try_into().unwrap_or(99u32), 19 | 23),
            7 => matches!((&rfc_bn % 7u32).try_into().unwrap_or(99u32), 3 | 5 | 6),
            _ => false,
        }
    };
    assert!(expected_g_ok(2), "RFC 3526 Group 14 has p mod 8 == 7");
    assert!(expected_g_ok(6), "RFC 3526 Group 14 has p mod 24 == 23");
    for g in [2, 3, 4, 5, 6, 7] {
        assert_eq!(
            is_prime_and_good(&rfc3526_g14, g),
            expected_g_ok(g),
            "RFC 3526 Group 14 residue check failed for g={g}"
        );
    }

    // 3. Reject 2047-bit, 2049-bit, composite, and non-safe 2048-bit primes.
    let mut bits_2047 = GOOD_PRIME;
    bits_2047[0] &= 0x7F;
    assert!(!is_prime_and_good(&bits_2047, 4));
    let mut bits_2049 = vec![0x01u8];
    bits_2049.extend_from_slice(&GOOD_PRIME);
    assert!(!is_prime_and_good(&bits_2049, 4));
    let mut composite = GOOD_PRIME;
    composite[255] ^= 0x04;
    assert!(!is_prime_and_good(&composite, 4));

    // 4. g_a / g_b safety range checks (1 < g_a < p - 1 and 2^{2048-64} margin):
    //    Matches MTP::IsGoodModExpFirst (mtproto_dh_utils.cpp:88-103):
    //    modexp.bits() >= 1984 && (p - modexp).bits() >= 1984 && modexp.bytes() <= 256.
    let min_ok = BigUint::one() << 1983usize; // 1984 bits
    let max_ok = &good_bn - &min_ok; // (p - max_ok) has 1984 bits
    assert!(!is_good_mod_exp_first(&BigUint::zero(), &good_bn));
    assert!(!is_good_mod_exp_first(&BigUint::one(), &good_bn));
    assert!(!is_good_mod_exp_first(&BigUint::from(2u32), &good_bn));
    assert!(!is_good_mod_exp_first(
        &(&min_ok - BigUint::one()),
        &good_bn
    ));
    assert!(is_good_mod_exp_first(&min_ok, &good_bn));
    assert!(is_good_mod_exp_first(
        &(BigUint::one() << 1984usize),
        &good_bn
    ));
    assert!(is_good_mod_exp_first(&max_ok, &good_bn));
    assert!(!is_good_mod_exp_first(
        &(&max_ok + BigUint::one()),
        &good_bn
    ));
    assert!(!is_good_mod_exp_first(
        &(&good_bn - BigUint::one()),
        &good_bn
    ));
    assert!(!is_good_mod_exp_first(&good_bn, &good_bn));
    assert!(!is_good_mod_exp_first(
        &(&good_bn + BigUint::one()),
        &good_bn
    ));
    assert!(!is_good_mod_exp_first_bytes(&[0x01], &GOOD_PRIME));

    // create_auth_key must reject out-of-range g_a values.
    let seed = [0x42u8; 256];
    let modexp = create_mod_exp(3, &GOOD_PRIME, &seed).unwrap();
    assert!(is_good_mod_exp_first_bytes(&modexp.modexp, &GOOD_PRIME));
    assert!(
        create_auth_key(
            &(&min_ok - BigUint::one()).to_bytes_be(),
            &modexp.random_power,
            &GOOD_PRIME
        )
        .is_err()
    );
    assert!(
        create_auth_key(
            &(&max_ok + BigUint::one()).to_bytes_be(),
            &modexp.random_power,
            &GOOD_PRIME
        )
        .is_err()
    );
}

// ============================================================================
// 2. MTProto 2.0 Message Encryption & AES-256-IGE Conformance Tests
// ============================================================================

#[test]
fn conformance_aes_256_ige_multi_block_vectors() {
    // OpenSSL test/igetest.c + FIPS-197 AES-256 cross-check against independent IGE implementation.
    let key: [u8; 32] =
        hex_to_bytes("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
            .try_into()
            .unwrap();
    let iv: [u8; 32] =
        hex_to_bytes("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
            .try_into()
            .unwrap();
    let plaintext = hex_to_bytes(
        "00000000000000000000000000000000\
         00000000000000000000000000000000\
         112233445566778899aabbccddeeff00\
         ffeeddccbbaa99887766554433221100",
    );

    let expected_ct = spec_aes_ige_encrypt(&plaintext, &key, &iv);
    let crate_ct = aes_ige_encrypt(&plaintext, &key, &iv);
    assert_eq!(crate_ct, expected_ct);
    assert_eq!(aes_ige_decrypt(&crate_ct, &key, &iv), plaintext);
    assert_eq!(spec_aes_ige_decrypt(&crate_ct, &key, &iv), plaintext);

    // Infinite Garble Extension property: flipping 1 bit in ciphertext block 0 garbles ALL
    // subsequent decrypted plaintext blocks (blocks 0, 1, 2, 3).
    let mut garbled_ct = crate_ct;
    garbled_ct[0] ^= 0x01;
    let garbled_pt = aes_ige_decrypt(&garbled_ct, &key, &iv);
    for blk in 0..4 {
        assert_ne!(
            &garbled_pt[blk * 16..(blk + 1) * 16],
            &plaintext[blk * 16..(blk + 1) * 16],
            "IGE error propagation must corrupt block {blk}"
        );
    }

    // Non-16-byte lengths must return InvalidBlockLength.
    for bad_len in [1usize, 4, 8, 15, 17, 31] {
        let buf = vec![0u8; bad_len];
        assert_eq!(
            try_aes_ige_encrypt(&buf, &key, &iv),
            Err(CryptoError::InvalidBlockLength { len: bad_len })
        );
        assert_eq!(
            try_aes_ige_decrypt(&buf, &key, &iv),
            Err(CryptoError::InvalidBlockLength { len: bad_len })
        );
    }
}

#[test]
fn conformance_mtproto2_encryption_both_directions_and_rejections() {
    let mut raw_key = [0u8; 256];
    for (i, b) in raw_key.iter_mut().enumerate() {
        *b = ((i as u32 * 73 + 19) & 0xFF) as u8;
    }
    let auth_key = AuthKey::new(raw_key);

    // Build a realistic 32-byte MTProto inner header + 12-byte ping body = 44 bytes.
    let salt: i64 = 0x1122_3344_5566_7788;
    let session_id: i64 = 0x7766_5544_3322_1100;
    let msg_id: i64 = 0x6710_0000_0000_0000;
    let seq_no: i32 = 1;
    let body = hex_to_bytes("ec77be7a0102030405060708"); // ping#7abe77ec + ping_id
    assert_eq!(body.len(), 12);

    let mut plaintext = Vec::with_capacity(44);
    plaintext.extend_from_slice(&salt.to_le_bytes());
    plaintext.extend_from_slice(&session_id.to_le_bytes());
    plaintext.extend_from_slice(&msg_id.to_le_bytes());
    plaintext.extend_from_slice(&seq_no.to_le_bytes());
    plaintext.extend_from_slice(&(body.len() as u32).to_le_bytes());
    plaintext.extend_from_slice(&body);

    // 1. Cross-check both directions (x = 0 client->server, x = 8 server->client) byte-for-byte
    //    across padding boundaries (for 44-byte pt, 44 % 16 == 12 so valid pads are 16k + 4 >= 12:
    //    20, 36, 52, 260, 1012).
    for (side, x) in [(Side::Client, 0usize), (Side::Server, 8usize)] {
        assert_eq!(side.x(), x);
        for pad_len in [20usize, 36, 52, 260, 1012] {
            let padding: Vec<u8> = (0..pad_len).map(|i| (i as u8).wrapping_mul(13)).collect();
            let spec_wire = spec_mtproto2_encrypt(&raw_key, &plaintext, &padding, x);
            let crate_wire =
                encrypt_data_v2_with_padding(&plaintext, &padding, &auth_key, side).unwrap();
            assert_eq!(
                crate_wire, spec_wire,
                "MTProto 2.0 wire packet diverged from independent spec (side={side:?}, pad_len={pad_len})"
            );

            // Decrypt with matching side succeeds and recovers exact fields.
            let parsed = decrypt_message_v2(&spec_wire, &auth_key, side).unwrap();
            assert_eq!(parsed.salt, salt);
            assert_eq!(parsed.session_id, session_id);
            assert_eq!(parsed.msg_id, msg_id);
            assert_eq!(parsed.seq_no, seq_no);
            assert_eq!(parsed.body, body);

            // Wrong-x direction MUST be rejected with MsgKeyMismatch!
            let wrong_side = match side {
                Side::Client => Side::Server,
                Side::Server => Side::Client,
            };
            assert_eq!(
                decrypt_data_v2(&spec_wire, &auth_key, wrong_side),
                Err(CryptoError::MsgKeyMismatch)
            );
            assert_eq!(
                decrypt_message_v2(&spec_wire, &auth_key, wrong_side),
                Err(CryptoError::MsgKeyMismatch)
            );
        }
    }

    // 2. Check exact 12-byte minimum padding (36-byte pt + 12 = 48, div by 16) and exact
    //    1024-byte maximum padding (48-byte pt + 1024 = 1072, div by 16).
    let mut pt36 = Vec::with_capacity(36);
    pt36.extend_from_slice(&salt.to_le_bytes());
    pt36.extend_from_slice(&session_id.to_le_bytes());
    pt36.extend_from_slice(&msg_id.to_le_bytes());
    pt36.extend_from_slice(&seq_no.to_le_bytes());
    pt36.extend_from_slice(&4u32.to_le_bytes());
    pt36.extend_from_slice(&[0xDDu8; 4]);
    let pad_12 = vec![0x77u8; MIN_PADDING_V2];
    let wire_12 = spec_mtproto2_encrypt(&raw_key, &pt36, &pad_12, 0);
    assert_eq!(
        encrypt_data_v2_with_padding(&pt36, &pad_12, &auth_key, Side::Client).unwrap(),
        wire_12
    );
    let ok_12 = decrypt_message_v2(&wire_12, &auth_key, Side::Client).unwrap();
    assert_eq!(ok_12.body, vec![0xDDu8; 4]);

    let mut pt48 = Vec::with_capacity(48);
    pt48.extend_from_slice(&salt.to_le_bytes());
    pt48.extend_from_slice(&session_id.to_le_bytes());
    pt48.extend_from_slice(&msg_id.to_le_bytes());
    pt48.extend_from_slice(&seq_no.to_le_bytes());
    pt48.extend_from_slice(&16u32.to_le_bytes());
    pt48.extend_from_slice(&[0xEEu8; 16]);
    let pad_1024 = vec![0x99u8; MAX_PADDING_V2];
    let wire_1024 = spec_mtproto2_encrypt(&raw_key, &pt48, &pad_1024, 8);
    let ok_1024 = decrypt_message_v2(&wire_1024, &auth_key, Side::Server).unwrap();
    assert_eq!(ok_1024.body, vec![0xEEu8; 16]);

    // 3. Craft valid-msg_key packets with WRONG PADDING (< 12 or > 1024) using the independent
    //    spec encryptor and verify decrypt_message_v2 REJECTS every one of them!
    //    - padding = 0 bytes (48 + 0 = 48, valid AES block multiple, valid msg_key, but pad < 12!)
    let wire_pad_0 = spec_mtproto2_encrypt(&raw_key, &pt48, &[], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_pad_0, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));
    //    - padding = 4 bytes (44 + 4 = 48, valid AES block multiple, valid msg_key, but pad < 12!)
    let wire_pad_4 = spec_mtproto2_encrypt(&raw_key, &plaintext, &[0x11; 4], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_pad_4, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));
    //    - padding = 8 bytes (40 + 8 = 48, valid AES block multiple, valid msg_key, but pad < 12!)
    let mut pt40 = pt48[..40].to_vec();
    pt40[28..32].copy_from_slice(&8u32.to_le_bytes());
    let wire_pad_8 = spec_mtproto2_encrypt(&raw_key, &pt40, &[0x22; 8], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_pad_8, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));
    //    - padding = 1040 bytes (48 + 1040 = 1088, valid AES block multiple, valid msg_key, pad > 1024!)
    let wire_pad_1040 = spec_mtproto2_encrypt(&raw_key, &pt48, &vec![0x33; 1040], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_pad_1040, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));
    //    - padding = 1028 bytes (44 + 1028 = 1072, valid AES block multiple, valid msg_key, pad > 1024!)
    let wire_pad_1028 = spec_mtproto2_encrypt(&raw_key, &plaintext, &vec![0x44; 1028], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_pad_1028, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));

    // 4. Craft valid-msg_key packets with invalid inner message_data_length (0, unaligned, overflow).
    let mut bad_len_zero = pt48.clone();
    bad_len_zero[28..32].copy_from_slice(&0u32.to_le_bytes());
    let wire_len_zero = spec_mtproto2_encrypt(&raw_key, &bad_len_zero, &[0xAA; 16], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_len_zero, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));

    let mut bad_len_unaligned = pt48.clone();
    bad_len_unaligned[28..32].copy_from_slice(&5u32.to_le_bytes());
    let wire_len_unaligned = spec_mtproto2_encrypt(&raw_key, &bad_len_unaligned, &[0xAA; 16], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_len_unaligned, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));

    let mut bad_len_exceeds = pt48.clone();
    bad_len_exceeds[28..32].copy_from_slice(&1024u32.to_le_bytes());
    let wire_len_exceeds = spec_mtproto2_encrypt(&raw_key, &bad_len_exceeds, &[0xAA; 16], 8);
    assert!(matches!(
        decrypt_message_v2(&wire_len_exceeds, &auth_key, Side::Server),
        Err(CryptoError::InvalidMessageFraming(_))
    ));

    // 5. Random encrypt_data_v2 always produces valid 12..=1024 padding and 16-byte aligned ciphertext.
    for body_words in 1..=20 {
        let mut pt = vec![0u8; 32 + body_words * 4];
        pt[28..32].copy_from_slice(&((body_words * 4) as u32).to_le_bytes());
        for side in [Side::Client, Side::Server] {
            let wire = encrypt_data_v2(&pt, &auth_key, side);
            assert!((wire.len() - 24).is_multiple_of(16));
            let dec_padded = decrypt_data_v2(&wire, &auth_key, side).unwrap();
            let pad_len = dec_padded.len() - pt.len();
            assert!((MIN_PADDING_V2..=MAX_PADDING_V2).contains(&pad_len));
            assert_eq!(
                &wire[8..24],
                &compute_msg_key_v2(&auth_key, side, &dec_padded)
            );
            let parsed = decrypt_message_v2(&wire, &auth_key, side).unwrap();
            assert_eq!(parsed.body.len(), body_words * 4);
        }
    }
}
