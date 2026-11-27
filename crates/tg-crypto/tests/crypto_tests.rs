//! Comprehensive known-answer, round-trip, and negative tests for `tg-crypto`.

use num_bigint::BigUint;
use num_traits::{One, Zero};
use tg_crypto::aes::{
    CtrState, aes_cbc_decrypt, aes_cbc_encrypt, aes_ctr_encrypt, aes_ige_decrypt,
    aes_ige_decrypt_in_place, aes_ige_encrypt, aes_ige_encrypt_in_place, try_aes_ige_decrypt,
    try_aes_ige_encrypt,
};
use tg_crypto::auth_key::{
    AuthKey, Side, compute_msg_key_v2, decrypt_data_v1, decrypt_data_v2, decrypt_message_v2,
    encrypt_data_v1, encrypt_data_v2, encrypt_data_v2_with_padding,
};
use tg_crypto::dh::{
    GOOD_PRIME, create_auth_key, create_mod_exp, dh_new_nonce_hash, dh_server_aes_key_iv,
    factorize, factorize_bytes, is_good_mod_exp_first, is_good_mod_exp_first_bytes,
    is_prime_and_good, mod_exp,
};
use tg_crypto::error::CryptoError;
use tg_crypto::hash::{
    Md5Hasher, Sha256Hasher, const_time_eq, hmac_sha256, hmac_sha512, md5, md5_bytes, md5_hex,
    pbkdf2_hmac_sha1, pbkdf2_hmac_sha512, pbkdf2_hmac_sha512_64, sha1, sha1_bytes, sha256,
    sha256_bytes, sha512, sha512_bytes,
};
use tg_crypto::local::{
    PasscodeKdf, PasscodeKdfKind, StorageEncryptionKey, TDF_MAGIC, aes_decrypt_local,
    aes_encrypt_local, create_legacy_local_key, create_local_key, create_passcode_key,
    decrypt_local_data, encrypt_local_data, tdf_checksum, verify_tdf_checksum,
};
use tg_crypto::random::{random_array, random_bytes, random_u64};
use tg_crypto::rsa::{
    PRODUCTION_PUBLIC_KEYS, PublicKey, SPECIAL_CONFIG_PUBLIC_KEY, TEST_PUBLIC_KEYS,
    builtin_production_keys, builtin_test_keys, find_by_fingerprints,
};
use tg_crypto::srp::{
    PasswordKdfAlgoModPow, SecureSecretAlgo, check_secret_bytes, compute_check,
    compute_check_with_hash, compute_check_with_random, compute_digest, compute_hash,
    compute_secure_secret_hash, count_secure_secret_id, decrypt_secure_secret,
    encrypt_secure_secret, generate_secret_bytes, validate_new_algo,
    validate_new_secure_secret_algo,
};

fn hex_to_bytes(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    assert!(clean.len().is_multiple_of(2));
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

// ============================================================================
// Hashes, HMAC, PBKDF2
// ============================================================================

#[test]
fn kat_sha1_sha256_sha512_md5() {
    // SHA-1 (FIPS 180-4)
    assert_eq!(
        bytes_to_hex(&sha1_bytes(b"")),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    assert_eq!(
        bytes_to_hex(&sha1_bytes(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(sha1(&[b"a", b"b", b"c"]), sha1_bytes(b"abc"));

    // SHA-256 (FIPS 180-4)
    assert_eq!(
        bytes_to_hex(&sha256_bytes(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        bytes_to_hex(&sha256_bytes(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(sha256(&[b"ab", b"c"]), sha256_bytes(b"abc"));
    let mut sha_stream = Sha256Hasher::new();
    sha_stream.update(b"a");
    sha_stream.update(b"bc");
    assert_eq!(sha_stream.finalize(), sha256_bytes(b"abc"));

    // SHA-512 (FIPS 180-4)
    assert_eq!(
        bytes_to_hex(&sha512_bytes(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
         2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(sha512(&[b"a", b"bc"]), sha512_bytes(b"abc"));

    // MD5 (RFC 1321)
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5_hex(b"message digest"),
        "f96b697d7cb7938d525a2f31aaf161d0"
    );
    assert_eq!(md5(&[b"message ", b"digest"]), md5_bytes(b"message digest"));

    let mut hasher = Md5Hasher::new();
    hasher.update(b"message ");
    hasher.update(b"digest");
    assert_eq!(hasher.finalize(), md5_bytes(b"message digest"));
}

#[test]
fn kat_hmac_and_pbkdf2() {
    // RFC 4231 Test Case 2 ("Jefe", "what do ya want for nothing?")
    assert_eq!(
        bytes_to_hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    assert_eq!(
        bytes_to_hex(&hmac_sha512(b"Jefe", b"what do ya want for nothing?")),
        "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554\
         9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
    );

    // RFC 6070 PBKDF2-HMAC-SHA1
    let mut out20 = [0u8; 20];
    pbkdf2_hmac_sha1(b"password", b"salt", 1, &mut out20);
    assert_eq!(
        bytes_to_hex(&out20),
        "0c60c80f961f0e71f3a9b524af6012062fe037a6"
    );
    pbkdf2_hmac_sha1(b"password", b"salt", 4096, &mut out20);
    assert_eq!(
        bytes_to_hex(&out20),
        "4b007901b765489abead49d926f721d065a429c1"
    );

    // PBKDF2-HMAC-SHA512 known-answer test (RFC 7914 / standard vector)
    let out64 = pbkdf2_hmac_sha512_64(b"password", b"salt", 1);
    assert_eq!(
        bytes_to_hex(&out64),
        "867f70cf1ade02cff3752599a3a53dc4af34c7a669815ae5d513554e1c8cf252\
         c02d470a285a0501bad999bfe943c08f050235d7d68b1da55e63f73b60a57fce"
    );
    let mut out32 = [0u8; 32];
    pbkdf2_hmac_sha512(b"password", b"salt", 4096, &mut out32);
    assert_eq!(
        bytes_to_hex(&out32),
        "d197b1b33db0143e018b12f3d1d1479e6cdebdcc97c5c0f87f6902e072f457b5"
    );

    assert!(const_time_eq(b"hello", b"hello"));
    assert!(!const_time_eq(b"hello", b"hellp"));
    assert!(!const_time_eq(b"hello", b"hell"));
}

// ============================================================================
// AES-256-IGE, AES-256-CTR, AES-256-CBC
// ============================================================================

#[test]
fn kat_aes_256_ige() {
    // FIPS-197 Appendix C.3 AES-256 single block:
    // With zero IV (y0 = 0, x0 = 0), 1-block IGE equals raw AES-256-ECB.
    let key: [u8; 32] =
        hex_to_bytes("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
            .try_into()
            .unwrap();
    let pt1 = hex_to_bytes("00112233445566778899aabbccddeeff");
    let expected_ct1 = hex_to_bytes("8ea2b7ca516745bfeafc49904b496089");
    let zero_iv = [0u8; 32];

    let ct = aes_ige_encrypt(&pt1, &key, &zero_iv);
    assert_eq!(ct, expected_ct1);
    assert_eq!(aes_ige_decrypt(&ct, &key, &zero_iv), pt1);

    // Verify 2-block IGE chaining:
    // y1 = E(x1 ^ y0) ^ x0
    // y2 = E(x2 ^ y1) ^ x1
    // Choose x1 = pt1 ^ y0, so E(x1 ^ y0) = expected_ct1 and y1 = expected_ct1 ^ x0.
    // Choose x2 = pt1 ^ y1, so E(x2 ^ y1) = expected_ct1 and y2 = expected_ct1 ^ x1!
    let mut iv = [0u8; 32];
    for (i, b) in iv.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    let y0 = &iv[0..16];
    let x0 = &iv[16..32];

    let x1: Vec<u8> = (0..16).map(|i| pt1[i] ^ y0[i]).collect();
    let y1: Vec<u8> = (0..16).map(|i| expected_ct1[i] ^ x0[i]).collect();
    let x2: Vec<u8> = (0..16).map(|i| pt1[i] ^ y1[i]).collect();
    let y2: Vec<u8> = (0..16).map(|i| expected_ct1[i] ^ x1[i]).collect();

    let mut two_blocks = Vec::with_capacity(32);
    two_blocks.extend_from_slice(&x1);
    two_blocks.extend_from_slice(&x2);

    let mut expected_two = Vec::with_capacity(32);
    expected_two.extend_from_slice(&y1);
    expected_two.extend_from_slice(&y2);

    let encrypted = aes_ige_encrypt(&two_blocks, &key, &iv);
    assert_eq!(encrypted, expected_two);
    assert_eq!(aes_ige_decrypt(&encrypted, &key, &iv), two_blocks);

    // In-place round-trip
    let mut buf = two_blocks.clone();
    aes_ige_encrypt_in_place(&mut buf, &key, &iv).unwrap();
    assert_eq!(buf, expected_two);
    aes_ige_decrypt_in_place(&mut buf, &key, &iv).unwrap();
    assert_eq!(buf, two_blocks);

    // Reject non-block-aligned lengths
    assert_eq!(
        try_aes_ige_encrypt(&[1, 2, 3], &key, &iv),
        Err(CryptoError::InvalidBlockLength { len: 3 })
    );
    assert_eq!(
        try_aes_ige_decrypt(&[0u8; 17], &key, &iv),
        Err(CryptoError::InvalidBlockLength { len: 17 })
    );
}

#[test]
fn kat_aes_256_ctr_nist_and_streaming() {
    // NIST SP 800-38A F.5.5 (AES-256-CTR)
    let key: [u8; 32] =
        hex_to_bytes("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4")
            .try_into()
            .unwrap();
    let iv: [u8; 16] = hex_to_bytes("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff")
        .try_into()
        .unwrap();
    let plaintext = hex_to_bytes(
        "6bc1bee22e409f96e93d7e117393172a\
         ae2d8a571e03ac9c9eb76fac45af8e51\
         30c81c46a35ce411e5fbc1191a0a52ef\
         f69f2445df4f9b17ad2b417be66c3710",
    );
    let expected = hex_to_bytes(
        "601ec313775789a5b7a7f504bbf3d228\
         f443e3ca4d62b59aca84e990cacaf5c5\
         2b0930daa23de94ce87017ba2d84988d\
         dfc9c58db67aada613c2dd08457941a6",
    );

    // One-shot
    let mut ct = plaintext.clone();
    let mut state = CtrState::new(key, iv);
    aes_ctr_encrypt(&mut ct, &mut state);
    assert_eq!(ct, expected);

    // Streaming with irregular chunk sizes (1, 3, 17, 13, 30 bytes = 64 bytes)
    let mut streamed = plaintext.clone();
    let mut state_stream = CtrState::new(key, iv);
    let chunks = [1usize, 3, 17, 13, 30];
    let mut pos = 0;
    for len in chunks {
        state_stream.encrypt(&mut streamed[pos..pos + len]);
        pos += len;
    }
    assert_eq!(streamed, expected);

    // Decrypt byte-by-byte
    let mut dec = streamed;
    let mut state_dec = CtrState::new(key, iv);
    for b in &mut dec {
        state_dec.decrypt(std::slice::from_mut(b));
    }
    assert_eq!(dec, plaintext);

    // Verify 128-bit counter wraparound from all-0xff -> all-0x00
    let mut wrap_state = CtrState::new(key, [0xff; 16]);
    let mut block = [0u8; 32];
    wrap_state.encrypt(&mut block);
    let mut zero_counter = [0u8; 16];
    zero_counter[15] = 1; // incremented twice: ff..ff -> 00..00 -> 00..01
    assert_eq!(wrap_state.ivec, zero_counter);
}

#[test]
fn kat_aes_256_cbc_nist() {
    // NIST SP 800-38A F.2.5 (AES-256-CBC)
    let key: [u8; 32] =
        hex_to_bytes("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4")
            .try_into()
            .unwrap();
    let iv: [u8; 16] = hex_to_bytes("000102030405060708090a0b0c0d0e0f")
        .try_into()
        .unwrap();
    let plaintext = hex_to_bytes(
        "6bc1bee22e409f96e93d7e117393172a\
         ae2d8a571e03ac9c9eb76fac45af8e51",
    );
    let expected = hex_to_bytes(
        "f58c4c04d6e5f1ba779eabfb5f7bfbd6\
         9cfc4e967edb808d679f777bc6702c7d",
    );
    let ct = aes_cbc_encrypt(&plaintext, &key, &iv).unwrap();
    assert_eq!(ct, expected);
    assert_eq!(aes_cbc_decrypt(&ct, &key, &iv).unwrap(), plaintext);
}

// ============================================================================
// AuthKey & MTProto 2.0 / 1.0
// ============================================================================

#[test]
fn auth_key_ids_and_mtproto_v2_kdf_kat() {
    let mut raw = [0u8; 256];
    for (i, b) in raw.iter_mut().enumerate() {
        *b = i as u8;
    }
    let key = AuthKey::new(raw);
    let full_sha1 = sha1_bytes(&raw);
    assert_eq!(key.aux_hash(), full_sha1[0..8]);
    assert_eq!(key.key_id(), full_sha1[12..20]);
    assert_eq!(key.key_id_u64(), u64::from_le_bytes(key.key_id()));
    assert_eq!(key.aux_hash_u64(), u64::from_le_bytes(key.aux_hash()));

    // Left-padding in AuthKey::from_bytes
    let short_key = AuthKey::from_bytes(&[0xab, 0xcd]).unwrap();
    assert_eq!(&short_key.data()[..254], &[0u8; 254]);
    assert_eq!(&short_key.data()[254..], &[0xab, 0xcd]);
    assert!(AuthKey::from_bytes(&[0u8; 257]).is_err());

    // Verify MTProto 2.0 key derivation matches https://core.telegram.org/mtproto/description
    let msg_key: [u8; 16] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        0x00,
    ];
    for side in [Side::Client, Side::Server] {
        let x = match side {
            Side::Client => 0usize,
            Side::Server => 8usize,
        };
        let sha256_a = sha256(&[&msg_key, &raw[x..x + 36]]);
        let sha256_b = sha256(&[&raw[40 + x..40 + x + 36], &msg_key]);
        let mut expected_key = [0u8; 32];
        expected_key[0..8].copy_from_slice(&sha256_a[0..8]);
        expected_key[8..24].copy_from_slice(&sha256_b[8..24]);
        expected_key[24..32].copy_from_slice(&sha256_a[24..32]);

        let mut expected_iv = [0u8; 32];
        expected_iv[0..8].copy_from_slice(&sha256_b[0..8]);
        expected_iv[8..24].copy_from_slice(&sha256_a[8..24]);
        expected_iv[24..32].copy_from_slice(&sha256_b[24..32]);

        let (aes_key, aes_iv) = key.prepare_aes_v2(&msg_key, side);
        assert_eq!(aes_key, expected_key);
        assert_eq!(aes_iv, expected_iv);
        assert_eq!(key.part_for_msg_key(side), &raw[88 + x..88 + x + 32]);
    }

    // Verify MTProto 1.0 key derivation matches prepareAES_oldmtp
    for side in [Side::Client, Side::Server] {
        let x = side.x();
        let sha1_a = sha1(&[&msg_key, &raw[x..x + 32]]);
        let sha1_b = sha1(&[&raw[32 + x..48 + x], &msg_key, &raw[48 + x..64 + x]]);
        let sha1_c = sha1(&[&raw[64 + x..96 + x], &msg_key]);
        let sha1_d = sha1(&[&msg_key, &raw[96 + x..128 + x]]);

        let mut expected_key = [0u8; 32];
        expected_key[0..8].copy_from_slice(&sha1_a[0..8]);
        expected_key[8..20].copy_from_slice(&sha1_b[8..20]);
        expected_key[20..32].copy_from_slice(&sha1_c[4..16]);

        let mut expected_iv = [0u8; 32];
        expected_iv[0..12].copy_from_slice(&sha1_a[8..20]);
        expected_iv[12..20].copy_from_slice(&sha1_b[0..8]);
        expected_iv[20..24].copy_from_slice(&sha1_c[16..20]);
        expected_iv[24..32].copy_from_slice(&sha1_d[0..8]);

        let (aes_key, aes_iv) = key.prepare_aes_v1(&msg_key, side);
        assert_eq!(aes_key, expected_key);
        assert_eq!(aes_iv, expected_iv);
    }
}

#[test]
fn mtproto_v2_encrypt_decrypt_roundtrip_and_tamper() {
    let key = AuthKey::new(random_array::<256>());
    let other_key = AuthKey::new(random_array::<256>());

    // Build a valid 32-byte MTProto inner header + 8-byte body = 40 bytes.
    let salt: i64 = 0x0102_0304_0506_0708;
    let session_id: i64 = 0x1122_3344_5566_7788;
    let msg_id: i64 = 0x6700_0000_0000_0001;
    let seq_no: i32 = 3;
    let body = b"12345678"; // 8 bytes, multiple of 4

    let mut plaintext = Vec::new();
    plaintext.extend_from_slice(&salt.to_le_bytes());
    plaintext.extend_from_slice(&session_id.to_le_bytes());
    plaintext.extend_from_slice(&msg_id.to_le_bytes());
    plaintext.extend_from_slice(&seq_no.to_le_bytes());
    plaintext.extend_from_slice(&(body.len() as u32).to_le_bytes());
    plaintext.extend_from_slice(body);

    for side in [Side::Client, Side::Server] {
        let encrypted = encrypt_data_v2(&plaintext, &key, side);
        assert_eq!(&encrypted[0..8], &key.key_id());
        assert!((encrypted.len() - 24).is_multiple_of(16));

        let decrypted_padded = decrypt_data_v2(&encrypted, &key, side).unwrap();
        assert!(decrypted_padded.starts_with(&plaintext));
        let pad_len = decrypted_padded.len() - plaintext.len();
        assert!((12..=1024).contains(&pad_len));

        // Verify msg_key explicitly
        let expected_mk = compute_msg_key_v2(&key, side, &decrypted_padded);
        assert_eq!(&encrypted[8..24], &expected_mk);

        // Full framed message parse
        let parsed = decrypt_message_v2(&encrypted, &key, side).unwrap();
        assert_eq!(parsed.salt, salt);
        assert_eq!(parsed.session_id, session_id);
        assert_eq!(parsed.msg_id, msg_id);
        assert_eq!(parsed.seq_no, seq_no);
        assert_eq!(parsed.body, body);

        // Wrong side fails msg_key verification
        let opposite = match side {
            Side::Client => Side::Server,
            Side::Server => Side::Client,
        };
        assert_eq!(
            decrypt_data_v2(&encrypted, &key, opposite),
            Err(CryptoError::MsgKeyMismatch)
        );

        // Wrong key fails auth_key_id check
        assert_eq!(
            decrypt_data_v2(&encrypted, &other_key, side),
            Err(CryptoError::AuthKeyIdMismatch)
        );

        // Flip a ciphertext bit -> MsgKeyMismatch
        let mut tampered = encrypted.clone();
        tampered[24] ^= 0x01;
        assert_eq!(
            decrypt_data_v2(&tampered, &key, side),
            Err(CryptoError::MsgKeyMismatch)
        );
    }

    // Deterministic padding & rejection of invalid padding sizes
    let padding_24 = [0x42u8; 24]; // 40 + 24 = 64 (multiple of 16)
    let enc_det =
        encrypt_data_v2_with_padding(&plaintext, &padding_24, &key, Side::Client).unwrap();
    let msg_det = decrypt_message_v2(&enc_det, &key, Side::Client).unwrap();
    assert_eq!(msg_det.body, body);

    assert!(encrypt_data_v2_with_padding(&plaintext, &[0u8; 8], &key, Side::Client).is_err());
    assert!(encrypt_data_v2_with_padding(&plaintext, &[0u8; 1032], &key, Side::Client).is_err());

    // MTProto 1.0 round-trip
    let enc_v1 = encrypt_data_v1(&plaintext, &key, Side::Client);
    let (dec_v1, mk_v1) = decrypt_data_v1(&enc_v1, &key, Side::Client).unwrap();
    assert_eq!(&dec_v1[..plaintext.len()], &plaintext[..]);
    assert_eq!(&sha1_bytes(&plaintext)[4..20], &mk_v1);
}

// ============================================================================
// RSA
// ============================================================================

#[test]
fn rsa_builtin_keys_pem_and_fingerprints() {
    let prod = builtin_production_keys();
    assert_eq!(prod.len(), PRODUCTION_PUBLIC_KEYS.len());
    assert_eq!(prod[0].n().len(), 256);
    assert_eq!(prod[0].e(), &[0x01, 0x00, 0x01]); // 65537
    // Known Telegram production RSA public key fingerprint for kPublicRSAKeys[0]
    // (MIIBCgKCAQEA6LszBcC1LG...)
    assert_eq!(prod[0].fingerprint(), 0xd09d_1d85_de64_fd85);
    assert_eq!(prod[0].fingerprint_i64(), -3_414_540_481_677_951_611);

    // Independently verify TL-serialized (bytes(n) || bytes(e)) SHA-1 lower 64 bits
    let mut tl = Vec::new();
    tl.extend_from_slice(&[254, 0, 1, 0]); // TL bytes header for length 256
    tl.extend_from_slice(prod[0].n());
    tl.push(3); // TL bytes header for length 3 (e = 0x010001)
    tl.extend_from_slice(prod[0].e());
    let fp_sha = sha1_bytes(&tl);
    assert_eq!(
        prod[0].fingerprint(),
        u64::from_le_bytes(fp_sha[12..20].try_into().unwrap())
    );

    let test_keys = builtin_test_keys();
    assert_eq!(test_keys.len(), TEST_PUBLIC_KEYS.len());
    assert_eq!(test_keys[0].n().len(), 256);
    assert_eq!(test_keys[0].e(), &[0x01, 0x00, 0x01]);
    assert_ne!(test_keys[0].fingerprint(), prod[0].fingerprint());

    let special = PublicKey::from_pem(SPECIAL_CONFIG_PUBLIC_KEY).unwrap();
    assert_eq!(special.n().len(), 256);
    assert_eq!(special.e(), &[0x01, 0x00, 0x01]);

    // find_by_fingerprints lookup
    let found = find_by_fingerprints(&prod, &[12345, prod[0].fingerprint_i64()]).unwrap();
    assert_eq!(found, &prod[0]);
    assert!(find_by_fingerprints(&prod, &[12345]).is_none());

    // Build an X.509 SPKI PEM ("-----BEGIN PUBLIC KEY-----") wrapping the same PKCS#1 key
    // and verify SPKI parsing produces the exact same PublicKey and fingerprint.
    let pkcs1_b64: String = PRODUCTION_PUBLIC_KEYS[0]
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    let spki_pem = wrap_pkcs1_b64_as_spki_pem(&pkcs1_b64);
    let from_spki = PublicKey::from_pem(&spki_pem).unwrap();
    assert_eq!(from_spki, prod[0]);

    // Malformed PEMs rejected
    assert!(PublicKey::from_pem("not a pem").is_err());
    assert!(
        PublicKey::from_pem("-----BEGIN RSA PUBLIC KEY-----\nAAAA\n-----END RSA PUBLIC KEY-----")
            .is_err()
    );
}

#[test]
fn rsa_pad_legacy_and_oaep_end_to_end_with_private_key() {
    // Construct a deterministic 2048-bit RSA keypair from two 1024-bit safe-like primes
    // (or verify by generating a valid 2048-bit modulus from two primes p_rsa, q_rsa).
    let (pub_key, n, d) = test_2048_rsa_keypair();

    // 1. Test RSA_PAD (encrypt_hashed) end-to-end: encrypt then decrypt with private exponent d.
    let payload = b"p_q_inner_data_dc_test_payload_bytes_0123456789abcdef";
    let encrypted = pub_key.encrypt_hashed(payload).unwrap();
    let decrypted_bn = BigUint::from_bytes_be(&encrypted).modpow(&d, &n);
    let mut key_aes_encrypted = [0u8; 256];
    let dec_bytes = decrypted_bn.to_bytes_be();
    key_aes_encrypted[256 - dec_bytes.len()..].copy_from_slice(&dec_bytes);

    let temp_key_xor = &key_aes_encrypted[0..32];
    let aes_encrypted = &key_aes_encrypted[32..256];
    let aes_hash = sha256_bytes(aes_encrypted);
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
    assert_eq!(&data_with_padding[..payload.len()], payload);

    // Reject payload > 144 bytes
    assert!(pub_key.encrypt_hashed(&[0u8; 145]).is_err());

    // 2. Test legacy plain RSA encryption (encrypt_legacy) end-to-end.
    let legacy_enc = pub_key.encrypt_legacy(payload).unwrap();
    let legacy_dec_bn = BigUint::from_bytes_be(&legacy_enc).modpow(&d, &n);
    let mut legacy_dec = [0u8; 256];
    let lbytes = legacy_dec_bn.to_bytes_be();
    legacy_dec[256 - lbytes.len()..].copy_from_slice(&lbytes);
    assert_eq!(legacy_dec[0], 0);
    assert_eq!(&legacy_dec[1..21], &sha1_bytes(payload));
    assert_eq!(&legacy_dec[21..21 + payload.len()], payload);
    assert!(pub_key.encrypt_legacy(&[0u8; 236]).is_err());

    // 3. Test RSA-OAEP (encrypt_oaep) end-to-end.
    let oaep_msg = b"telegram passport credentials secret";
    let oaep_ct = pub_key.encrypt_oaep(oaep_msg).unwrap();
    assert_eq!(oaep_ct.len(), 256);
    let em_bn = BigUint::from_bytes_be(&oaep_ct).modpow(&d, &n);
    let mut em = [0u8; 256];
    let em_bytes = em_bn.to_bytes_be();
    em[256 - em_bytes.len()..].copy_from_slice(&em_bytes);
    assert_eq!(em[0], 0);
    let masked_seed = &em[1..21];
    let masked_db = &em[21..256];
    let seed_mask = test_mgf1_sha1(masked_db, 20);
    let seed: Vec<u8> = (0..20).map(|i| masked_seed[i] ^ seed_mask[i]).collect();
    let db_mask = test_mgf1_sha1(&seed, 235);
    let db: Vec<u8> = (0..235).map(|i| masked_db[i] ^ db_mask[i]).collect();
    assert_eq!(&db[..20], &sha1_bytes(b""));
    let sep = db[20..].iter().position(|&b| b != 0).unwrap() + 20;
    assert_eq!(db[sep], 0x01);
    assert_eq!(&db[sep + 1..], oaep_msg);
}

// ============================================================================
// PQ Factorization & DH
// ============================================================================

#[test]
fn pq_factorization_known_vectors_and_big() {
    // Classic MTProto documentation example: 0x17ED48941A08F981 = 0x494C553B * 0x53911073
    let pq: u64 = 0x17ED_4894_1A08_F981;
    let (p, q) = factorize(pq);
    assert_eq!(p, 0x494C_553B);
    assert_eq!(q, 0x5391_1073);
    assert_eq!(p * q, pq);

    // Edge cases & various sizes
    assert_eq!(factorize(15), (3, 5));
    assert_eq!(factorize(4), (2, 2));
    assert_eq!(factorize(17), (1, 17)); // prime
    assert_eq!(factorize(1), (1, 1));

    // > 2^63 semiprime (two 32-bit primes whose product is > 2^63)
    let p_hi: u64 = 3_000_000_019;
    let q_hi: u64 = 4_000_000_007;
    let pq_hi = p_hi * q_hi;
    assert!(pq_hi > (1u64 << 63));
    assert_eq!(factorize(pq_hi), (p_hi, q_hi));

    // factorize_bytes for 64-bit semiprime
    let (pb, qb) = factorize_bytes(&pq.to_be_bytes()).unwrap();
    assert_eq!(BigUint::from_bytes_be(&pb), BigUint::from(0x494C_553Bu64));
    assert_eq!(BigUint::from_bytes_be(&qb), BigUint::from(0x5391_1073u64));

    // >8-byte pq must be rejected before factorizing (SEC-C4)
    let big_p = BigUint::from(1_000_000_007u64);
    let big_q = BigUint::from(1_000_000_000_039u64); // product ~ 10^21 > 2^64 (9 bytes)
    let big_pq = &big_p * &big_q;
    assert!(big_pq.to_bytes_be().len() > 8);
    assert!(matches!(
        factorize_bytes(&big_pq.to_bytes_be()),
        Err(CryptoError::InvalidLength(_))
    ));
}

#[test]
fn factorize_bytes_rejects_oversized_16_byte_pq_quickly() {
    // 16-byte odd value with no small prime factors (would spin in BigUint Pollard-rho
    // for a very long time if not rejected upfront by the 8-byte cap).
    let pq_16: [u8; 16] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0x61, // 2^128 - 159 (prime)
    ];
    let start = std::time::Instant::now();
    let res = factorize_bytes(&pq_16);
    let elapsed = start.elapsed();
    assert!(
        matches!(res, Err(CryptoError::InvalidLength(_))),
        "expected InvalidLength for 16-byte pq, got {res:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "16-byte pq rejection took too long: {elapsed:?}"
    );

    // Even a 9-byte buffer with leading zero padding must be rejected by byte length.
    let padded_9 = [0x00, 0x17, 0xed, 0x48, 0x94, 0x1a, 0x08, 0xf9, 0x81];
    assert!(matches!(
        factorize_bytes(&padded_9),
        Err(CryptoError::InvalidLength(_))
    ));
}

#[test]
fn dh_validation_and_full_key_exchange() {
    // Known good prime fast path
    for g in [3, 4, 5, 7] {
        assert!(is_prime_and_good(&GOOD_PRIME, g));
    }
    // Bad generators for GOOD_PRIME
    for bad_g in [-1, 0, 1, 2, 6, 8, 99] {
        assert!(!is_prime_and_good(&GOOD_PRIME, bad_g));
    }

    // Reject wrong bit lengths
    assert!(!is_prime_and_good(&[0xffu8; 128], 3));
    assert!(!is_prime_and_good(&[0xffu8; 257], 3));
    let mut short_2047 = GOOD_PRIME;
    short_2047[0] &= 0x7f; // 2047 bits
    assert!(!is_prime_and_good(&short_2047, 3));

    // Reject composite 2048-bit numbers
    let mut even_p = GOOD_PRIME;
    even_p[255] &= 0xfe;
    assert!(!is_prime_and_good(&even_p, 4));
    let mut comp_p = GOOD_PRIME;
    comp_p[255] ^= 0x02; // almost certainly composite
    assert!(!is_prime_and_good(&comp_p, 4));

    // Non-fast-path 2048-bit safe prime: RFC 3526 Group 14 (p % 8 == 7, safe prime).
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
    assert_eq!(rfc3526_g14.len(), 256);
    assert!(is_prime_and_good(&rfc3526_g14, 2));
    assert!(is_prime_and_good(&rfc3526_g14, 4));

    // Reject a 2048-bit prime that is NOT a safe prime (p == 1 mod 4 => (p-1)/2 is even)
    let mut non_safe = (BigUint::one() << 2047usize) + BigUint::from(1u32);
    loop {
        if is_prime_small_test(&non_safe) {
            break;
        }
        non_safe += BigUint::from(4u32); // keeps non_safe == 1 (mod 4)
    }
    assert_eq!(non_safe.bits(), 2048);
    assert!(!is_prime_and_good(&non_safe.to_bytes_be(), 4));

    // Range checks for g_a / g_b (IsGoodModExpFirst)
    let prime_bn = BigUint::from_bytes_be(&GOOD_PRIME);
    let boundary = BigUint::one() << 1983usize; // bits() == 1984 == 2048 - 64
    assert!(!is_good_mod_exp_first(&BigUint::zero(), &prime_bn));
    assert!(!is_good_mod_exp_first(&BigUint::one(), &prime_bn));
    assert!(!is_good_mod_exp_first(
        &(&boundary - BigUint::one()),
        &prime_bn
    ));
    assert!(is_good_mod_exp_first(&boundary, &prime_bn));
    assert!(is_good_mod_exp_first(&(&prime_bn - &boundary), &prime_bn));
    assert!(!is_good_mod_exp_first(
        &(&prime_bn - &boundary + BigUint::one()),
        &prime_bn
    ));
    assert!(!is_good_mod_exp_first(&prime_bn, &prime_bn));
    assert!(!is_good_mod_exp_first_bytes(&[1u8], &GOOD_PRIME));

    // Full simulated DH exchange (client + server)
    let seed_a = random_array::<256>();
    let seed_b = random_array::<256>();
    let modexp_a = create_mod_exp(3, &GOOD_PRIME, &seed_a).unwrap();
    let modexp_b = create_mod_exp(3, &GOOD_PRIME, &seed_b).unwrap();

    let key_from_a =
        create_auth_key(&modexp_b.modexp, &modexp_a.random_power, &GOOD_PRIME).unwrap();
    let key_from_b =
        create_auth_key(&modexp_a.modexp, &modexp_b.random_power, &GOOD_PRIME).unwrap();
    assert_eq!(key_from_a, key_from_b);
    let auth_key_a = AuthKey::from_bytes(&key_from_a).unwrap();
    let auth_key_b = AuthKey::from_bytes(&key_from_b).unwrap();
    assert_eq!(auth_key_a, auth_key_b);

    // create_auth_key rejects bad g_a
    assert!(create_auth_key(&[1u8], &modexp_b.random_power, &GOOD_PRIME).is_err());

    // mod_exp sanity
    assert_eq!(
        mod_exp(&[2], &[10], &1000u16.to_be_bytes()).unwrap(),
        vec![24]
    );
    assert!(mod_exp(&[2], &[10], &[0]).is_err());

    // DH server AES key/IV and new_nonce_hash
    let new_nonce = [0xAAu8; 32];
    let server_nonce = [0x55u8; 16];
    let (k, iv) = dh_server_aes_key_iv(&new_nonce, &server_nonce);
    let s1 = sha1(&[&new_nonce, &server_nonce]);
    let s2 = sha1(&[&server_nonce, &new_nonce]);
    let s3 = sha1(&[&new_nonce, &new_nonce]);
    assert_eq!(&k[0..20], &s1);
    assert_eq!(&k[20..32], &s2[0..12]);
    assert_eq!(&iv[0..8], &s2[12..20]);
    assert_eq!(&iv[8..28], &s3);
    assert_eq!(&iv[28..32], &new_nonce[0..4]);

    let aux = auth_key_a.aux_hash();
    let nh1 = dh_new_nonce_hash(&new_nonce, 1, &aux);
    assert_eq!(nh1, sha1(&[&new_nonce, &[1u8], &aux])[4..20]);
}

// ============================================================================
// SRP 2FA & Telegram Passport
// ============================================================================

#[test]
fn srp_full_handshake_verification_and_bad_params() {
    let algo = PasswordKdfAlgoModPow {
        salt1: b"test_salt1_bytes_0123456789".to_vec(),
        salt2: b"test_salt2_bytes_abcdef".to_vec(),
        g: 3,
        p: GOOD_PRIME.to_vec(),
    };
    let password = b"correct horse battery staple";
    let srp_id: i64 = 0x1234_5678_9abc_def0;

    // 1. Independent check of compute_hash
    let h1 = sha256(&[&algo.salt1, password, &algo.salt1]);
    let h2 = sha256(&[&algo.salt2, &h1, &algo.salt2]);
    let h3 = pbkdf2_hmac_sha512_64(&h2, &algo.salt1, 100_000);
    let expected_x = sha256(&[&algo.salt2, &h3, &algo.salt2]);

    let x_bytes = compute_hash(&algo, password);
    assert_eq!(x_bytes, expected_x);

    // 2. Compute verifier v = g^x mod p
    let v_bytes = compute_digest(&algo, password).unwrap();
    let p = BigUint::from_bytes_be(&algo.p);
    let g = BigUint::from(algo.g as u32);
    let x = BigUint::from_bytes_be(&x_bytes);
    let v = BigUint::from_bytes_be(&v_bytes);
    assert_eq!(v, g.modpow(&x, &p));

    // 3. Independent server-side SRP-6a setup: B = (k*v + g^b) mod p
    let mut g_padded = [0u8; 256];
    g_padded[255] = algo.g as u8;
    let k = BigUint::from_bytes_be(&sha256(&[&GOOD_PRIME, &g_padded]));
    let b_secret = BigUint::from_bytes_be(&random_array::<256>());
    let g_b_pub = g.modpow(&b_secret, &p);
    let srp_b_bn = ((&k * &v) + &g_b_pub) % &p;
    let mut srp_b = [0u8; 256];
    let b_raw = srp_b_bn.to_bytes_be();
    srp_b[256 - b_raw.len()..].copy_from_slice(&b_raw);

    // 4. Client computes InputCheckPasswordSrp
    let check = compute_check(&algo, &srp_b, srp_id, password).unwrap();
    assert_eq!(check.srp_id, srp_id);

    // 5. Independent server verification of M1
    let a_pub = BigUint::from_bytes_be(&check.a);
    let u = BigUint::from_bytes_be(&sha256(&[&check.a, &srp_b]));
    let s_server = ((&a_pub * v.modpow(&u, &p)) % &p).modpow(&b_secret, &p);
    let mut s_server_padded = [0u8; 256];
    let s_raw = s_server.to_bytes_be();
    s_server_padded[256 - s_raw.len()..].copy_from_slice(&s_raw);
    let k_server = sha256_bytes(&s_server_padded);

    let hp = sha256_bytes(&GOOD_PRIME);
    let hg = sha256_bytes(&g_padded);
    let mut hp_xor_hg = [0u8; 32];
    for i in 0..32 {
        hp_xor_hg[i] = hp[i] ^ hg[i];
    }
    let m1_server = sha256(&[
        &hp_xor_hg,
        &sha256_bytes(&algo.salt1),
        &sha256_bytes(&algo.salt2),
        &check.a,
        &srp_b,
        &k_server,
    ]);
    assert_eq!(check.m1, m1_server);

    // Deterministic check with explicit `a`
    let fixed_a = [0x33u8; 256];
    let det1 = compute_check_with_random(&algo, &srp_b, srp_id, &x_bytes, &fixed_a).unwrap();
    let det2 = compute_check_with_random(&algo, &srp_b, srp_id, &x_bytes, &fixed_a).unwrap();
    assert_eq!(det1, det2);

    // Wrong password hash produces different M1
    let wrong_x = sha256_bytes(b"wrong");
    let wrong_check = compute_check_with_hash(&algo, &srp_b, srp_id, &wrong_x).unwrap();
    assert_ne!(wrong_check.m1, m1_server);

    // Bad SRP parameters rejected
    assert!(compute_check_with_hash(&algo, &[0u8; 256], srp_id, &x_bytes).is_err()); // B == 0
    assert!(compute_check_with_hash(&algo, &GOOD_PRIME, srp_id, &x_bytes).is_err()); // B == p
    let kv_mod_p = (&k * &v) % &p;
    assert!(compute_check_with_hash(&algo, &kv_mod_p.to_bytes_be(), srp_id, &x_bytes).is_err()); // g_b == 0 -> fails IsGoodModExpFirst

    let mut bad_algo = algo.clone();
    bad_algo.g = 2; // invalid for GOOD_PRIME
    assert!(compute_check_with_hash(&bad_algo, &srp_b, srp_id, &x_bytes).is_err());

    // validate_new_algo appends 32 random bytes to salt1
    let orig_len = algo.salt1.len();
    let extended = validate_new_algo(algo);
    assert_eq!(extended.salt1.len(), orig_len + 32);
}

#[test]
fn passport_secure_secret_hashing_and_encryption() {
    let salt = b"passport_salt_0123456789".to_vec();
    let pwd = b"my_passport_password";

    let algo_sha512 = SecureSecretAlgo::Sha512 { salt: salt.clone() };
    assert_eq!(
        compute_secure_secret_hash(&algo_sha512, pwd),
        sha512(&[&salt, pwd, &salt])
    );

    let algo_pbkdf2 = SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt: salt.clone() };
    let pbkdf2_hash = compute_secure_secret_hash(&algo_pbkdf2, pwd);
    assert_eq!(pbkdf2_hash, pbkdf2_hmac_sha512_64(pwd, &salt, 100_000));

    // validate_new_secure_secret_algo extends PBKDF2 salt by 32 and rejects SHA512
    assert!(validate_new_secure_secret_algo(algo_sha512).is_err());
    let validated = validate_new_secure_secret_algo(algo_pbkdf2).unwrap();
    match validated {
        SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt: new_salt } => {
            assert_eq!(new_salt.len(), salt.len() + 32);
        }
        _ => panic!("expected PBKDF2 variant"),
    }

    // Generate secret, verify mod-255 == 239 checksum, encrypt/decrypt round-trip
    let secret = generate_secret_bytes();
    assert!(check_secret_bytes(&secret));
    let encrypted = encrypt_secure_secret(&secret, &pbkdf2_hash).unwrap();
    let decrypted = decrypt_secure_secret(&encrypted, &pbkdf2_hash).unwrap();
    assert_eq!(decrypted, secret);

    // Wrong hash fails mod-255 checksum (probability 254/255; test corrupted secret directly too)
    let mut bad_secret = secret;
    bad_secret[0] = bad_secret[0].wrapping_add(1);
    assert!(!check_secret_bytes(&bad_secret));
    assert!(encrypt_secure_secret(&bad_secret, &pbkdf2_hash).is_err());

    // Secret ID matches first 8 bytes of SHA256(secret) as LE u64
    let expected_id = u64::from_le_bytes(sha256_bytes(&secret)[0..8].try_into().unwrap());
    assert_eq!(count_secure_secret_id(&secret), expected_id);
}

// ============================================================================
// Local Storage (tdata) Crypto
// ============================================================================

#[test]
fn local_storage_keys_encryption_tdf_and_ctr() {
    let salt = [0x22u8; 32];

    // 1. Modern local key (empty passcode -> 1 iteration)
    let empty_key = create_local_key(b"", &salt);
    let empty_prehash = sha512(&[&salt, b"", &salt]);
    let mut expected_empty = [0u8; 256];
    pbkdf2_hmac_sha512(&empty_prehash, &salt, 1, &mut expected_empty);
    assert_eq!(*empty_key.data(), expected_empty);

    // Modern local key (non-empty passcode -> 100_000 iterations)
    let pwd_key = create_local_key(b"local-passcode", &salt);
    let pwd_prehash = sha512(&[&salt, b"local-passcode", &salt]);
    let mut expected_pwd = [0u8; 256];
    pbkdf2_hmac_sha512(&pwd_prehash, &salt, 100_000, &mut expected_pwd);
    assert_eq!(*pwd_key.data(), expected_pwd);

    // 2. Legacy local key (empty -> 4 iters, non-empty -> 4000 iters)
    let legacy_empty = create_legacy_local_key(b"", &salt);
    let mut expected_leg_empty = [0u8; 256];
    pbkdf2_hmac_sha1(b"", &salt, 4, &mut expected_leg_empty);
    assert_eq!(*legacy_empty.data(), expected_leg_empty);

    let legacy_pwd = create_legacy_local_key(b"old-passcode", &salt);
    let mut expected_leg_pwd = [0u8; 256];
    pbkdf2_hmac_sha1(b"old-passcode", &salt, 4_000, &mut expected_leg_pwd);
    assert_eq!(*legacy_pwd.data(), expected_leg_pwd);

    // 3. Memory-hard PasscodeKdf (Argon2id and Scrypt with fast test costs + validation)
    let fast_argon2 = PasscodeKdf {
        kind: PasscodeKdfKind::Argon2id,
        memory: 64,
        time: 1,
        parallel: 1,
    };
    assert!(fast_argon2.is_valid() && fast_argon2.cost_within_limits());
    let k_a1 = create_passcode_key(b"secret", &salt, fast_argon2).unwrap();
    let k_a2 = create_passcode_key(b"secret", &salt, fast_argon2).unwrap();
    assert_eq!(k_a1, k_a2);

    let fast_scrypt = PasscodeKdf {
        kind: PasscodeKdfKind::Scrypt,
        memory: 64,
        time: 1,
        parallel: 1,
    };
    assert!(fast_scrypt.is_valid() && fast_scrypt.cost_within_limits());
    let k_s = create_passcode_key(b"secret", &salt, fast_scrypt).unwrap();
    assert_ne!(k_a1, k_s);

    // Reject empty passcode, short salt, excessive costs
    assert!(create_passcode_key(b"", &salt, fast_argon2).is_err());
    assert!(create_passcode_key(b"secret", &[1u8; 7], fast_argon2).is_err());
    assert!(
        !PasscodeKdf {
            kind: PasscodeKdfKind::Argon2id,
            memory: 500_000,
            time: 1,
            parallel: 1,
        }
        .cost_within_limits()
    );
    assert!(PasscodeKdf::default_argon2id().is_valid());
    assert!(PasscodeKdf::default_argon2id().cost_within_limits());
    assert!(PasscodeKdf::default_scrypt().is_valid());
    assert!(PasscodeKdf::default_scrypt().cost_within_limits());

    // 4. Verify aes_encrypt_local / aes_decrypt_local use x = 8 (Side::Server)
    let key128 = [0x99u8; 16];
    let block = [0x42u8; 32];
    let (aes_k, aes_iv) = empty_key.prepare_aes_v1(&key128, Side::Server);
    let raw_enc = aes_encrypt_local(&block, &empty_key, &key128).unwrap();
    assert_eq!(raw_enc, aes_ige_encrypt(&block, &aes_k, &aes_iv));
    assert_eq!(
        aes_decrypt_local(&raw_enc, &empty_key, &key128).unwrap(),
        block
    );

    // 5. High-level encrypt_local_data / decrypt_local_data round-trips
    for payload in [
        &b""[..],
        &b"x"[..],
        &b"123456789012"[..],
        &b"1234567890123456"[..],
        &[0xabu8; 100][..],
    ] {
        let enc = encrypt_local_data(payload, &empty_key).unwrap();
        assert!(enc.len() > 16 && enc.len().is_multiple_of(16));
        let dec = decrypt_local_data(&enc, &empty_key).unwrap();
        assert_eq!(dec, payload);

        // Wrong key fails
        assert!(decrypt_local_data(&enc, &legacy_empty).is_err());

        // Tampered byte fails
        let mut bad = enc.clone();
        bad[16] ^= 1;
        assert!(decrypt_local_data(&bad, &empty_key).is_err());
    }

    // 6. TDF checksum
    let tdf_payload = b"tdata serialized payload";
    let version = 5_008_000i32;
    let csum = tdf_checksum(tdf_payload, version).unwrap();
    let expected_csum = md5(&[
        tdf_payload,
        &(tdf_payload.len() as i32).to_le_bytes(),
        &version.to_le_bytes(),
        &TDF_MAGIC,
    ]);
    assert_eq!(csum, expected_csum);
    assert!(verify_tdf_checksum(tdf_payload, version, &csum));
    let mut bad_csum = csum;
    bad_csum[0] ^= 0xff;
    assert!(!verify_tdf_checksum(tdf_payload, version, &bad_csum));

    // 7. StorageEncryptionKey + StorageCtrState offset consistency
    let cache_salt = [0x77u8; 64];
    for key_len in [StorageEncryptionKey::SIZE_V2, StorageEncryptionKey::SIZE_V1] {
        let enc_key = StorageEncryptionKey::new(vec![0x5a; key_len]).unwrap();
        let ctr = enc_key.prepare_ctr_state(&cache_salt);

        let original: Vec<u8> = (0u8..64).collect();
        let mut full = original.clone();
        ctr.encrypt(&mut full, 0).unwrap();

        // Encrypting the second half directly at offset 32 must match `full[32..64]`
        let mut second_half = original[32..64].to_vec();
        ctr.encrypt(&mut second_half, 32).unwrap();
        assert_eq!(second_half, full[32..64]);

        // Decrypt back
        ctr.decrypt(&mut full, 0).unwrap();
        assert_eq!(full, original);
    }
    assert!(StorageEncryptionKey::new(vec![0; 32]).is_err());
}

#[test]
fn random_sanity() {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    random_bytes(&mut a);
    random_bytes(&mut b);
    assert_ne!(a, [0u8; 32]);
    assert_ne!(a, b);
    assert_ne!(random_u64(), random_u64());
}

// ============================================================================
// Test helpers (independent RSA keypair, SPKI wrapper, MGF1)
// ============================================================================

fn test_mgf1_sha1(seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut c = 0u32;
    while out.len() < len {
        let h = sha1(&[seed, &c.to_be_bytes()]);
        let take = (len - out.len()).min(20);
        out.extend_from_slice(&h[..take]);
        c += 1;
    }
    out
}

/// Build a deterministic 2048-bit RSA keypair `(PublicKey, n, d)` for end-to-end tests.
fn test_2048_rsa_keypair() -> (PublicKey, BigUint, BigUint) {
    // Two deterministic 1024-bit primes found by incrementing from 2^1023 + fixed offsets.
    // Using 2^1023 + offset where both are verified prime via small Miller-Rabin here so
    // the product is guaranteed 2048 bits and `e = 65537` is coprime to phi(n).
    let base_p =
        (BigUint::one() << 1023usize) + (BigUint::one() << 1022usize) + BigUint::from(1u32);
    let base_q = (BigUint::one() << 1023usize)
        + (BigUint::one() << 1022usize)
        + (BigUint::one() << 500usize)
        + BigUint::from(1u32);
    let p = next_prime(base_p);
    let q = next_prime(base_q);
    let n = &p * &q;
    assert_eq!(n.bits(), 2048);

    let phi = (&p - BigUint::one()) * (&q - BigUint::one());
    let e = BigUint::from(65537u32);
    let d = mod_inverse(&e, &phi);
    assert_eq!((&e * &d) % &phi, BigUint::one());

    let pub_key = PublicKey::from_components(&n.to_bytes_be(), &e.to_bytes_be()).unwrap();
    (pub_key, n, d)
}

fn next_prime(mut candidate: BigUint) -> BigUint {
    let two = BigUint::from(2u32);
    if (&candidate & BigUint::one()).is_zero() {
        candidate += BigUint::one();
    }
    while !is_prime_small_test(&candidate) {
        candidate += &two;
    }
    candidate
}

fn is_prime_small_test(n: &BigUint) -> bool {
    const BASES: &[u32] = &[2, 3, 5, 7, 11, 13, 17, 19, 23, 29];
    for &p in BASES {
        if (n % p).is_zero() {
            return false;
        }
    }
    let n_minus_one = n - BigUint::one();
    let s = n_minus_one.trailing_zeros().unwrap_or(0);
    let d = &n_minus_one >> s;
    'outer: for &base in BASES {
        let a = BigUint::from(base);
        let mut x = a.modpow(&d, n);
        if x.is_one() || x == n_minus_one {
            continue;
        }
        for _ in 1..s {
            x = (&x * &x) % n;
            if x == n_minus_one {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

fn mod_inverse(a: &BigUint, m: &BigUint) -> BigUint {
    use num_bigint::BigInt;
    let mut t = BigInt::zero();
    let mut new_t = BigInt::one();
    let mut r = BigInt::from(m.clone());
    let mut new_r = BigInt::from(a.clone());
    while !new_r.is_zero() {
        let q = &r / &new_r;
        let next_t = &t - &q * &new_t;
        t = new_t;
        new_t = next_t;
        let next_r = &r - &q * &new_r;
        r = new_r;
        new_r = next_r;
    }
    assert_eq!(r, BigInt::one());
    if t < BigInt::zero() {
        t += BigInt::from(m.clone());
    }
    t.to_biguint().unwrap()
}

fn wrap_pkcs1_b64_as_spki_pem(pkcs1_b64: &str) -> String {
    let pkcs1_der = test_b64_decode(pkcs1_b64);
    // AlgorithmIdentifier: SEQUENCE { OID rsaEncryption, NULL }
    let alg_id: &[u8] = &[
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    let mut bit_string = Vec::with_capacity(1 + pkcs1_der.len());
    bit_string.push(0x00);
    bit_string.extend_from_slice(&pkcs1_der);
    let bit_tlv = der_tlv(0x03, &bit_string);

    let mut spki_body = Vec::with_capacity(alg_id.len() + bit_tlv.len());
    spki_body.extend_from_slice(alg_id);
    spki_body.extend_from_slice(&bit_tlv);
    let spki_der = der_tlv(0x30, &spki_body);
    let b64 = test_b64_encode(&spki_der);
    format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----")
}

fn der_tlv(tag: u8, val: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = val.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xff {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push((len & 0xff) as u8);
    }
    out.extend_from_slice(val);
    out
}

fn test_b64_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    for chunk in bytes.as_chunks::<4>().0 {
        let mut v = [0u8; 4];
        let mut pad = 0;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                pad += 1;
            } else {
                v[i] = match b {
                    b'A'..=b'Z' => b - b'A',
                    b'a'..=b'z' => b - b'a' + 26,
                    b'0'..=b'9' => b - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => panic!("bad b64"),
                };
            }
        }
        let n =
            ((v[0] as u32) << 18) | ((v[1] as u32) << 12) | ((v[2] as u32) << 6) | (v[3] as u32);
        out.push(((n >> 16) & 0xff) as u8);
        if pad < 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if pad < 1 {
            out.push((n & 0xff) as u8);
        }
    }
    out
}

fn test_b64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
