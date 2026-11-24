use tg_calls::dh::{
    DH_GOOD_PRIME as GOOD_PRIME, DhConfig, DhConfigCache, RANDOM_POWER_SIZE, compute_ga_hash,
    compute_key_fingerprint, compute_key_sha_for_fingerprint, fill_auth_key_data,
    finish_dh_key_exchange, generate_mod_exp_first_with_rng, is_dh_prime_good as is_prime_and_good,
    is_good_dh_mod_exp_first as is_good_mod_exp_first_bytes, verify_ga_hash,
};
use tg_calls::error::{DhError, SignalingError};
use tg_calls::signaling::{
    ChannelType, MSG_KEY_SIZE, SignalingCipher, aes_256_ctr_apply, compute_msg_key,
    encrypt_raw_mtproto2, prepare_aes_key_iv,
};
use tg_calls::types::DhConfigResponse;
use tg_crypto::hash::sha1;

#[test]
fn good_prime_passes_fast_path_and_full_safe_prime_check() {
    // Fast path accepts GOOD_PRIME with g in {3, 4, 5, 7}.
    for g in [3, 4, 5, 7] {
        assert!(is_prime_and_good(&GOOD_PRIME, g), "g={g} should pass");
    }

    // Invalid generators rejected even for GOOD_PRIME.
    for bad_g in [-1, 0, 1, 2, 6, 8, 100] {
        assert!(!is_prime_and_good(&GOOD_PRIME, bad_g));
    }

    // Mutating the LSB makes GOOD_PRIME even (composite) -> rejected.
    let mut composite = GOOD_PRIME;
    composite[255] ^= 0x01;
    assert!(!is_prime_and_good(&composite, 3));

    // Wrong bit length (too short) -> rejected.
    assert!(!is_prime_and_good(&GOOD_PRIME[1..], 3));
}

#[test]
fn is_good_mod_exp_first_enforces_2048_minus_64_bit_bounds() {
    // Smallest integer with exactly (2048 - 64) = 1984 bits is 2^1983.
    // In big-endian 256-byte representation, bit 1983 is bit 7 of byte index 8 (256 - 248 = 8).
    let mut min_ok = [0u8; 256];
    min_ok[8] = 0x80;

    let mut below_min = [0u8; 256];
    below_min[8] = 0x7F;
    below_min[9..].fill(0xFF);

    // Compute prime - min_ok and prime - min_ok + 1 on 256-byte big-endian arrays.
    let mut max_ok = GOOD_PRIME;
    let mut borrow: u16 = 0;
    for i in (0..256).rev() {
        let a = u16::from(max_ok[i]);
        let b = u16::from(min_ok[i]) + borrow;
        if a >= b {
            max_ok[i] = (a - b) as u8;
            borrow = 0;
        } else {
            max_ok[i] = (a + 256 - b) as u8;
            borrow = 1;
        }
    }
    let mut above_max = max_ok;
    for b in above_max.iter_mut().rev() {
        let (v, carry) = b.overflowing_add(1);
        *b = v;
        if !carry {
            break;
        }
    }

    assert!(!is_good_mod_exp_first_bytes(&[0u8], &GOOD_PRIME));
    assert!(!is_good_mod_exp_first_bytes(&[1u8], &GOOD_PRIME));
    assert!(!is_good_mod_exp_first_bytes(&below_min, &GOOD_PRIME));
    assert!(is_good_mod_exp_first_bytes(&min_ok, &GOOD_PRIME));
    assert!(is_good_mod_exp_first_bytes(&max_ok, &GOOD_PRIME));
    assert!(!is_good_mod_exp_first_bytes(&above_max, &GOOD_PRIME));
    assert!(!is_good_mod_exp_first_bytes(&GOOD_PRIME, &GOOD_PRIME));
}

#[test]
fn fill_auth_key_data_left_pads_short_keys_and_rejects_oversized() {
    let short = [0xAAu8, 0xBB, 0xCC];
    let padded = fill_auth_key_data(&short).expect("short key pads");
    assert_eq!(&padded[..253], &[0u8; 253]);
    assert_eq!(&padded[253..], &[0xAA, 0xBB, 0xCC]);

    let exact = [0x5Au8; 256];
    let filled = fill_auth_key_data(&exact).expect("256-byte key fits");
    assert_eq!(filled, exact);

    let too_long = [0x11u8; 257];
    assert_eq!(
        fill_auth_key_data(&too_long),
        Err(DhError::AuthKeyTooLarge { actual: 257 })
    );
}

#[test]
fn compute_key_fingerprint_known_answer_matches_tdesktop_sha1_lower_64_le() {
    // Known answer: all-zero 256-byte key.
    // SHA1([0u8; 256]) = c42a3184... let's verify against direct SHA-1 bytes[12..20] LE.
    let key = [0u8; 256];
    let hash = sha1(&[&key]);
    let expected = u64::from_le_bytes(hash[12..20].try_into().unwrap());
    assert_eq!(compute_key_fingerprint(&key), expected);

    // Known answer with sequential bytes 0x00..=0xFF.
    let mut seq_key = [0u8; 256];
    for (i, b) in seq_key.iter_mut().enumerate() {
        *b = i as u8;
    }
    let seq_hash = sha1(&[&seq_key]);
    // Explicitly check the exact bit-shifting expression from calls_call.cpp:193-200.
    let cpp_fp = (u64::from(seq_hash[19]) << 56)
        | (u64::from(seq_hash[18]) << 48)
        | (u64::from(seq_hash[17]) << 40)
        | (u64::from(seq_hash[16]) << 32)
        | (u64::from(seq_hash[15]) << 24)
        | (u64::from(seq_hash[14]) << 16)
        | (u64::from(seq_hash[13]) << 8)
        | u64::from(seq_hash[12]);
    assert_eq!(compute_key_fingerprint(&seq_key), cpp_fp);
}

#[test]
fn full_dh_key_exchange_agreement_between_caller_and_callee() {
    let config = DhConfig {
        version: 1,
        g: 3,
        p: GOOD_PRIME.to_vec(),
    };
    let server_seed_a = [0x11u8; RANDOM_POWER_SIZE];
    let server_seed_b = [0x22u8; RANDOM_POWER_SIZE];

    let first_a = generate_mod_exp_first_with_rng(&config, &server_seed_a, |buf| {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3).wrapping_add(7);
        }
    })
    .expect("caller modexp");

    let first_b = generate_mod_exp_first_with_rng(&config, &server_seed_b, |buf| {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(5).wrapping_add(13);
        }
    })
    .expect("callee modexp");

    // Commitment g_a_hash = SHA256(g_a).
    let g_a_hash = compute_ga_hash(&first_a.modexp);
    assert!(verify_ga_hash(&first_a.modexp, &g_a_hash).is_ok());

    let mut bad_ga = first_a.modexp.clone();
    bad_ga[0] ^= 0xFF;
    assert_eq!(
        verify_ga_hash(&bad_ga, &g_a_hash),
        Err(DhError::GaHashMismatch)
    );

    // Caller finishes with g_b; callee finishes with g_a.
    let (key_a, fp_a) = finish_dh_key_exchange(&config, &first_b.modexp, &first_a.random_power)
        .expect("caller auth key");
    let (key_b, fp_b) = finish_dh_key_exchange(&config, &first_a.modexp, &first_b.random_power)
        .expect("callee auth key");

    assert_eq!(key_a, key_b);
    assert_eq!(fp_a, fp_b);
    assert_ne!(fp_a, 0);

    // Emoji fingerprint hash SHA256(auth_key || g_a) matches on both sides.
    let sha_a = compute_key_sha_for_fingerprint(&key_a, &first_a.modexp);
    let sha_b = compute_key_sha_for_fingerprint(&key_b, &first_a.modexp);
    assert_eq!(sha_a, sha_b);
}

#[test]
fn dh_config_cache_validates_and_handles_not_modified() {
    let mut cache = DhConfigCache::new();
    assert_eq!(cache.version(), 0);

    // NotModified on empty cache fails.
    assert_eq!(
        cache.update(DhConfigResponse::NotModified {
            random: vec![0x42; RANDOM_POWER_SIZE],
        }),
        Err(DhError::MissingCachedConfig)
    );

    // Bad prime fails.
    assert_eq!(
        cache.update(DhConfigResponse::Config {
            g: 3,
            p: vec![0x01; 256],
            version: 1,
            random: vec![0x42; RANDOM_POWER_SIZE],
        }),
        Err(DhError::InvalidParameters)
    );

    // Wrong random size fails.
    assert_eq!(
        cache.update(DhConfigResponse::Config {
            g: 3,
            p: GOOD_PRIME.to_vec(),
            version: 1,
            random: vec![0x42; 32],
        }),
        Err(DhError::InvalidRandomLength {
            expected: 256,
            actual: 32,
        })
    );

    // Valid config updates cache.
    let random = cache
        .update(DhConfigResponse::Config {
            g: 3,
            p: GOOD_PRIME.to_vec(),
            version: 42,
            random: vec![0xAB; RANDOM_POWER_SIZE],
        })
        .expect("valid dh config");
    assert_eq!(random.len(), RANDOM_POWER_SIZE);
    assert_eq!(cache.version(), 42);

    // Subsequent NotModified succeeds using cached p/g.
    let random2 = cache
        .update(DhConfigResponse::NotModified {
            random: vec![0xCD; RANDOM_POWER_SIZE],
        })
        .expect("not modified uses cache");
    assert_eq!(random2, vec![0xCD; RANDOM_POWER_SIZE]);
}

#[test]
fn aes_ctr_and_signaling_cipher_roundtrip_and_replay_protection() {
    // Signaling cipher outgoing <-> incoming roundtrip (AES-256-CTR + seq_be(4) || payload).
    let mut auth_key = [0u8; 256];
    for (i, b) in auth_key.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut caller = SignalingCipher::new(auth_key, true);
    let mut callee = SignalingCipher::new(auth_key, false);

    // Verify directional x offsets match MTProto 2.0 signaling convention.
    assert_eq!(ChannelType::Signaling.send_x(true), 128);
    assert_eq!(ChannelType::Signaling.recv_x(false), 128);
    assert_eq!(ChannelType::Signaling.send_x(false), 136);
    assert_eq!(ChannelType::Signaling.recv_x(true), 136);
    assert_eq!(ChannelType::Transport.send_x(true), 0);
    assert_eq!(ChannelType::Transport.recv_x(false), 0);
    assert_eq!(ChannelType::Transport.send_x(false), 8);
    assert_eq!(ChannelType::Transport.recv_x(true), 8);

    // KDF (PrepareAesKeyIv with 16-byte CTR IV) & msg_key consistency check.
    let raw_plain = b"\x00\x00\x00\x01hello-tgcalls";
    let msg_key = compute_msg_key(&auth_key, raw_plain, 128);
    let (aes_k, aes_iv) = prepare_aes_key_iv(&auth_key, &msg_key, 128);
    assert_eq!(aes_iv.len(), 16);
    let packet = encrypt_raw_mtproto2(&auth_key, raw_plain, 128).unwrap();
    assert_eq!(packet.len(), MSG_KEY_SIZE + raw_plain.len());
    assert_eq!(&packet[..MSG_KEY_SIZE], &msg_key);
    assert_eq!(
        aes_256_ctr_apply(&packet[MSG_KEY_SIZE..], &aes_k, &aes_iv),
        raw_plain
    );

    // Encrypt two packets from caller and deliver out-of-order (pkt2 then pkt1) to callee.
    let payload1 = br#"{"@type":"InitialSetup"}"#;
    let payload2 = br#"{"@type":"Candidates"}"#;
    let pkt1 = caller.encrypt(payload1).unwrap();
    let pkt2 = caller.encrypt(payload2).unwrap();
    // Exact tgcalls wire length: 16-byte msg_key + 4-byte seq + payload (no padding).
    assert_eq!(pkt1.len(), MSG_KEY_SIZE + 4 + payload1.len());
    assert_eq!(pkt2.len(), MSG_KEY_SIZE + 4 + payload2.len());

    let dec2 = callee.decrypt(&pkt2).unwrap();
    assert_eq!(dec2.seq, 2);
    assert_eq!(dec2.payload, payload2);

    let dec1 = callee.decrypt(&pkt1).unwrap();
    assert_eq!(dec1.seq, 1);
    assert_eq!(dec1.payload, payload1);

    // Replaying pkt1 or pkt2 is rejected.
    assert_eq!(
        callee.decrypt(&pkt1),
        Err(SignalingError::ReplayDetected(1))
    );
    assert_eq!(
        callee.decrypt(&pkt2),
        Err(SignalingError::ReplayDetected(2))
    );

    // Reverse direction (callee -> caller) works with opposite x offset.
    let reply = callee.encrypt(br#"{"@type":"MediaState"}"#).unwrap();
    let dec_reply = caller.decrypt(&reply).unwrap();
    assert_eq!(dec_reply.seq, 1);
    assert_eq!(dec_reply.payload, br#"{"@type":"MediaState"}"#);

    // Tampered ciphertext is rejected by msg_key verification.
    let mut tampered = caller.encrypt(b"hello").unwrap();
    tampered[20] ^= 0x01;
    assert_eq!(
        callee.decrypt(&tampered),
        Err(SignalingError::MsgKeyMismatch)
    );

    // Too-short packet rejected.
    assert_eq!(
        callee.decrypt(&[0u8; 20]),
        Err(SignalingError::PacketTooShort(20))
    );
}
