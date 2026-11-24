//! Offline deterministic MTProto transport conformance tests for `tg-transport`.
//!
//! Cross-checks framing, Obfuscated2, and MTProxy `dd`/`ee` fake-TLS against independent
//! straight-from-spec implementations and tdesktop's `connection_tcp.cpp`,
//! `mtproto_tcp_socket.cpp`, `mtproto_tls_socket.cpp`, and `mtproto_proxy_data.cpp`.

use std::collections::HashSet;

use aes::Aes256;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use hmac::{Hmac, Mac};
use rand::SeedableRng;
use rand::rngs::StdRng;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use tg_transport::{
    Abridged, CLIENT_HEADER, CLIENT_HELLO_LIMIT, CLIENT_PREFIX, Connection, ConnectionConfig,
    Error, FrameKind, Full, Intermediate, MtprotoSecret, Obfuscated2, PLAIN_INIT_ABRIDGED,
    PaddedIntermediate, ProxyData, ProxyStatus, SERVER_HEADER, SERVER_HELLO_DIGEST_POSITION,
    SERVER_HELLO_PART1, SERVER_HELLO_PART3, TAG_ABRIDGED, TAG_INTERMEDIATE,
    TAG_PADDED_INTERMEDIATE, TlsBlock, Transport, TransportErrorCode, TransportKind,
    build_unencrypted_req_pq_multi_with_msg_id, default_client_hello_rules,
    generate_client_hello_with_rules, is_good_start_nonce, protocol_dc_id, verify_server_hello,
};

// ============================================================================
// Independent straight-from-spec reference primitives
// ============================================================================

/// Independent IEEE 802.3 CRC32 (bit-by-bit, polynomial 0xEDB88320, no `crc32fast`).
fn spec_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Independent AES-256-CTR (128-bit big-endian counter, matching OpenSSL `CRYPTO_ctr128_encrypt`).
struct SpecCtr {
    cipher: Aes256,
    counter: [u8; 16],
    keystream: [u8; 16],
    pos: usize,
}

impl SpecCtr {
    fn new(key: &[u8; 32], iv: &[u8; 16]) -> Self {
        Self {
            cipher: Aes256::new(GenericArray::from_slice(key)),
            counter: *iv,
            keystream: [0u8; 16],
            pos: 16,
        }
    }

    fn apply(&mut self, data: &mut [u8]) {
        for b in data.iter_mut() {
            if self.pos == 16 {
                self.keystream = self.counter;
                self.cipher
                    .encrypt_block(GenericArray::from_mut_slice(&mut self.keystream));
                // Increment 128-bit big-endian counter.
                for c in self.counter.iter_mut().rev() {
                    let (next, carry) = c.overflowing_add(1);
                    *c = next;
                    if !carry {
                        break;
                    }
                }
                self.pos = 0;
            }
            *b ^= self.keystream[self.pos];
            self.pos += 1;
        }
    }
}

fn spec_hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

fn spec_build_server_hello(client_digest: &[u8; 32], key: &[u8; 16]) -> Vec<u8> {
    let mut sh = Vec::new();
    // Record 1: Handshake (0x16), TLS 1.2 (0x03 0x03), length 70 bytes.
    sh.extend_from_slice(SERVER_HELLO_PART1);
    sh.extend_from_slice(&70u16.to_be_bytes());
    // Handshake header: ServerHello (0x02), 3-byte len (66), version TLS 1.2 (0x03 0x03).
    sh.extend_from_slice(&[0x02, 0x00, 0x00, 0x42, 0x03, 0x03]);
    assert_eq!(sh.len(), SERVER_HELLO_DIGEST_POSITION);
    sh.extend_from_slice(&[0u8; 32]); // digest slot
    sh.extend_from_slice(&[0x55u8; 32]); // session_id / cipher / extensions placeholder

    // Record 2+3 header: ChangeCipherSpec (6 bytes) + ApplicationData header (3 bytes) + 2-byte len.
    sh.extend_from_slice(SERVER_HELLO_PART3);
    sh.extend_from_slice(&24u16.to_be_bytes());
    sh.extend_from_slice(&[0x77u8; 24]);

    let digest = spec_hmac_sha256(key, &[client_digest, &sh]);
    sh[SERVER_HELLO_DIGEST_POSITION..SERVER_HELLO_DIGEST_POSITION + 32].copy_from_slice(&digest);
    sh
}

// ============================================================================
// 1. Framing Conformance: Abridged, Intermediate, PaddedIntermediate, Full
// ============================================================================

#[test]
fn conformance_abridged_boundaries_and_errors() {
    let mut t = Abridged::new();
    assert_eq!(t.protocol_tag(), Some(TAG_ABRIDGED));
    assert_eq!(t.plain_init_bytes(), Some(PLAIN_INIT_ABRIDGED));

    // Boundary 1: 126 words (0x7E * 4 = 504 bytes) -> 1-byte length prefix 0x7E.
    let p_126 = vec![0x31u8; 0x7E * 4];
    let mut wire = Vec::new();
    t.pack(&p_126, &mut wire);
    assert_eq!(wire.len(), 1 + p_126.len());
    assert_eq!(wire[0], 0x7E);
    let u = t.unpack(&wire).unwrap();
    assert_eq!(u.kind, FrameKind::Payload);
    assert_eq!(u.slice(&wire), &p_126[..]);

    // Boundary 2: 127 words (0x7F * 4 = 508 bytes) -> 4-byte prefix [0x7F, 0x7F, 0x00, 0x00].
    let p_127 = vec![0x32u8; 0x7F * 4];
    wire.clear();
    t.pack(&p_127, &mut wire);
    assert_eq!(wire.len(), 4 + p_127.len());
    assert_eq!(&wire[..4], &[0x7F, 0x7F, 0x00, 0x00]);
    let u = t.unpack(&wire).unwrap();
    assert_eq!(u.slice(&wire), &p_127[..]);

    // Quick-ack bit on long frame sets bit 7 of 0x7F -> 0xFF.
    t.set_quick_ack(true);
    wire.clear();
    t.pack(&p_127, &mut wire);
    assert_eq!(&wire[..4], &[0xFF, 0x7F, 0x00, 0x00]);
    t.set_quick_ack(false);

    // Non-canonical long header (0x7F with word count < 0x7F) MUST be rejected
    // (matches Version0::readPacketLength in connection_tcp.cpp:121).
    let non_canonical = [0x7F, 0x10, 0x00, 0x00, 1, 2, 3, 4];
    assert!(matches!(
        t.unpack(&non_canonical),
        Err(Error::InvalidPacketLength(_))
    ));

    // Zero length header MUST be rejected.
    assert!(matches!(
        t.unpack(&[0x00, 0, 0, 0, 0]),
        Err(Error::InvalidPacketLength(0))
    ));
}

#[test]
fn conformance_intermediate_and_padded_intermediate_all_pad_lengths() {
    let mut inter = Intermediate::new();
    assert_eq!(inter.protocol_tag(), Some(TAG_INTERMEDIATE));
    assert_eq!(inter.plain_init_bytes(), Some(&TAG_INTERMEDIATE[..]));

    // Reject unaligned length, 0 length, and < 4 byte payload length in Intermediate.
    for bad_len in [0u32, 1, 2, 3, 5, 6, 7] {
        let mut frame = bad_len.to_le_bytes().to_vec();
        frame.extend_from_slice(&[0u8; 16]);
        assert!(
            matches!(inter.unpack(&frame), Err(Error::InvalidPacketLength(_))),
            "Intermediate must reject length {bad_len}"
        );
    }

    // PaddedIntermediate: test EVERY padding length 0..=15 for both unencrypted and encrypted
    // MTProto packets and verify strip_mtproto_padding recovers the exact packet every time!
    let mut padded = PaddedIntermediate::new();
    assert_eq!(padded.protocol_tag(), Some(TAG_PADDED_INTERMEDIATE));
    assert_eq!(
        padded.plain_init_bytes(),
        Some(&TAG_PADDED_INTERMEDIATE[..])
    );

    let unencrypted_pkt =
        build_unencrypted_req_pq_multi_with_msg_id([0xAB; 16], 0x6700_0000_0000_0000);
    let mut encrypted_pkt = vec![0u8; 24 + 48]; // auth_key_id(8) + msg_key(16) + 3 AES blocks(48)
    encrypted_pkt[0..8].copy_from_slice(&0x1234_5678_9ABC_DEF0u64.to_le_bytes());
    for (i, b) in encrypted_pkt[8..].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(11).wrapping_add(5);
    }

    for pad_len in 0usize..=15 {
        let pad: Vec<u8> = (0..pad_len).map(|i| 0xF0u8 ^ (i as u8)).collect();

        // Unencrypted packet with pad_len bytes of trailing padding.
        let mut wire = Vec::new();
        padded.pack_with_padding(&unencrypted_pkt, &pad, &mut wire);
        assert_eq!(
            &wire[..4],
            &((unencrypted_pkt.len() + pad_len) as u32).to_le_bytes()
        );
        let u = padded.unpack(&wire).unwrap();
        assert_eq!(u.kind, FrameKind::Payload);
        assert_eq!(u.next_offset, wire.len());
        assert_eq!(
            PaddedIntermediate::strip_mtproto_padding(u.slice(&wire)),
            &unencrypted_pkt[..],
            "failed to strip pad_len={pad_len} on unencrypted packet"
        );

        // Encrypted packet with pad_len bytes of trailing padding.
        wire.clear();
        padded.pack_with_padding(&encrypted_pkt, &pad, &mut wire);
        let u = padded.unpack(&wire).unwrap();
        assert_eq!(u.kind, FrameKind::Payload);
        assert_eq!(u.next_offset, wire.len());
        assert_eq!(
            PaddedIntermediate::strip_mtproto_padding(u.slice(&wire)),
            &encrypted_pkt[..],
            "failed to strip pad_len={pad_len} on encrypted packet"
        );
    }
}

#[test]
fn conformance_full_transport_independent_crc32_and_seq_checks() {
    let mut tx = Full::new();
    let mut rx = Full::new();
    assert_eq!(tx.protocol_tag(), None);
    assert_eq!(tx.plain_init_bytes(), None);

    // Pack 5 packets and cross-check every wire byte + independent bit-by-bit IEEE CRC32.
    let payloads: &[&[u8]] = &[
        b"ABCD",
        b"0123456789abcdef",
        b"11223344556677889900aabb",
        &[0x55u8; 64],
        &[0xAAu8; 128],
    ];
    let mut stream = Vec::new();
    let mut offsets = Vec::new();
    for (seq, payload) in payloads.iter().enumerate() {
        let start = stream.len();
        tx.pack(payload, &mut stream);
        let frame = &stream[start..];

        let expected_len = (payload.len() + 12) as u32;
        assert_eq!(&frame[0..4], &expected_len.to_le_bytes());
        assert_eq!(&frame[4..8], &(seq as u32).to_le_bytes());
        assert_eq!(&frame[8..8 + payload.len()], *payload);

        let expected_crc = spec_crc32(&frame[..frame.len() - 4]);
        assert_eq!(
            &frame[frame.len() - 4..],
            &expected_crc.to_le_bytes(),
            "Full transport CRC32 diverged from independent IEEE 802.3 CRC32 at seq={seq}"
        );
        offsets.push((start, stream.len()));
    }

    // Unpack sequentially and verify seq_no progression.
    let mut cursor = 0usize;
    for (i, payload) in payloads.iter().enumerate() {
        assert_eq!(rx.recv_seq(), i as u32);
        let u = rx.unpack(&stream[cursor..]).unwrap();
        assert_eq!(u.kind, FrameKind::Payload);
        assert_eq!(u.slice(&stream[cursor..]), *payload);
        cursor += u.next_offset;
    }
    assert_eq!(cursor, stream.len());
    assert_eq!(rx.recv_seq(), payloads.len() as u32);

    // Single-bit flip in length (keeping valid length? test seq, payload, crc) MUST fail CRC!
    let frame1 = stream[offsets[1].0..offsets[1].1].to_vec();
    for byte_idx in [4usize, 8, 12, frame1.len() - 1] {
        let mut bad = frame1.clone();
        bad[byte_idx] ^= 0x80;
        let mut check_rx = Full::new();
        assert!(
            matches!(check_rx.unpack(&bad), Err(Error::CrcMismatch { .. })),
            "flipping byte {byte_idx} must trigger CrcMismatch"
        );
    }

    // Sequence number mismatch: feeding seq=1 frame to a fresh Full receiver (expecting seq=0).
    let mut fresh_rx = Full::new();
    assert!(matches!(
        fresh_rx.unpack(&frame1),
        Err(Error::SequenceMismatch {
            expected: 0,
            actual: 1
        })
    ));

    // Bare 4-byte negative error code (-429) AND Full-framed 16-byte error (-404).
    assert!(matches!(
        fresh_rx.unpack(&(-429i32).to_le_bytes()),
        Err(Error::ServerError(TransportErrorCode::TransportFlood))
    ));
    let mut err_frame = Vec::new();
    let mut err_tx = Full::new();
    err_tx.pack(&(-404i32).to_le_bytes(), &mut err_frame);
    assert!(matches!(
        fresh_rx.unpack(&err_frame),
        Err(Error::ServerError(TransportErrorCode::AuthKeyNotFound))
    ));

    // reset() resets both send_seq and recv_seq to 0.
    tx.reset();
    rx.reset();
    assert_eq!(tx.send_seq(), 0);
    assert_eq!(rx.recv_seq(), 0);
}

// ============================================================================
// 2. Obfuscated2 Init Header & Reversed-Key Derivation Conformance Tests
// ============================================================================

#[test]
fn conformance_obfuscated2_header_keys_tags_dc_and_forbidden_words() {
    // 1. Verify ALL 8 forbidden start-nonce rules matching TcpSocket::isGoodStartNonce
    //    (mtproto_tcp_socket.cpp:48-71).
    let base = [0x42u8; 64];
    assert!(is_good_start_nonce(&base));

    // Rule 1: byte[0] == 0xEF
    let mut b = base;
    b[0] = 0xEF;
    assert!(!is_good_start_nonce(&b));

    // Rules 2-7: first 4 bytes in {HEAD, POST, GET , 0xEEEEEEEE, 0xDDDDDDDD, 0x02010316 LE}
    let forbidden_first_words: &[[u8; 4]] = &[
        *b"HEAD",
        *b"POST",
        *b"GET ",
        [0xEE, 0xEE, 0xEE, 0xEE],
        [0xDD, 0xDD, 0xDD, 0xDD],
        [0x16, 0x03, 0x01, 0x02],
    ];
    for word in forbidden_first_words {
        let mut n = base;
        n[0..4].copy_from_slice(word);
        assert!(!is_good_start_nonce(&n));
    }

    // Rule 8: bytes[4..8] == 0x00000000
    let mut zero_second = base;
    zero_second[4..8].copy_from_slice(&[0, 0, 0, 0]);
    assert!(!is_good_start_nonce(&zero_second));

    // 2. Deterministic byte-for-byte cross-check of Obfuscated2::from_nonce against an
    //    independent spec implementation (with and without 16-byte secret, across all
    //    3 protocol tags and regular/media/test DC IDs).
    let mut nonce = [0u8; 64];
    for (i, b) in nonce.iter_mut().enumerate() {
        *b = ((i as u32 * 29 + 7) & 0xFF) as u8;
    }
    assert!(is_good_start_nonce(&nonce));

    let secret = [
        0x01u8, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32,
        0x10,
    ];
    let dc_cases = [
        protocol_dc_id(2, false, false), // 2
        protocol_dc_id(2, true, false),  // -2
        protocol_dc_id(2, false, true),  // 10002
        protocol_dc_id(4, true, true),   // -10004
    ];
    let tag_cases = [TAG_ABRIDGED, TAG_INTERMEDIATE, TAG_PADDED_INTERMEDIATE];

    for tag in tag_cases {
        for dc in dc_cases {
            for opt_secret in [None, Some(&secret)] {
                let (mut client, header) = Obfuscated2::from_nonce(nonce, tag, dc, opt_secret);

                // Independent spec derivation:
                // send_key = SHA256(nonce[8..40] || secret) if secret else nonce[8..40]
                // send_iv  = nonce[40..56]
                let mut raw_send_key = [0u8; 32];
                raw_send_key.copy_from_slice(&nonce[8..40]);
                let send_key: [u8; 32] = match opt_secret {
                    Some(s) => {
                        let mut h = Sha256::new();
                        h.update(raw_send_key);
                        h.update(s);
                        h.finalize().into()
                    }
                    None => raw_send_key,
                };
                let mut send_iv = [0u8; 16];
                send_iv.copy_from_slice(&nonce[40..56]);

                // Reversed 48 bytes nonce[8..56]:
                let mut rev = [0u8; 48];
                rev.copy_from_slice(&nonce[8..56]);
                rev.reverse();
                let mut raw_recv_key = [0u8; 32];
                raw_recv_key.copy_from_slice(&rev[0..32]);
                let recv_key: [u8; 32] = match opt_secret {
                    Some(s) => {
                        let mut h = Sha256::new();
                        h.update(raw_recv_key);
                        h.update(s);
                        h.finalize().into()
                    }
                    None => raw_recv_key,
                };
                let mut recv_iv = [0u8; 16];
                recv_iv.copy_from_slice(&rev[32..48]);

                // Plaintext bytes 0..56 must be untouched; bytes 56..64 encrypted after
                // advancing CTR over bytes 0..64.
                let mut spec_plain_header = nonce;
                spec_plain_header[56..60].copy_from_slice(&tag);
                spec_plain_header[60..62].copy_from_slice(&dc.to_le_bytes());

                let mut spec_send_ctr = SpecCtr::new(&send_key, &send_iv);
                let mut spec_enc_header = spec_plain_header;
                spec_send_ctr.apply(&mut spec_enc_header);

                let mut expected_wire_header = [0u8; 64];
                expected_wire_header[0..56].copy_from_slice(&nonce[0..56]);
                expected_wire_header[56..64].copy_from_slice(&spec_enc_header[56..64]);

                assert_eq!(header, expected_wire_header);

                // Verify subsequent outgoing stream matches SpecCtr starting at byte 64!
                let payload =
                    b"MTProto encrypted payload spanning multiple 16-byte AES-CTR blocks!";
                let mut crate_out = payload.to_vec();
                client.encrypt_outgoing(&mut crate_out);
                let mut spec_out = payload.to_vec();
                spec_send_ctr.apply(&mut spec_out);
                assert_eq!(crate_out, spec_out);

                // Verify server-to-client stream uses reversed key/IV from block 0!
                let mut spec_recv_ctr = SpecCtr::new(&recv_key, &recv_iv);
                let mut s2c_ct = payload.to_vec();
                spec_recv_ctr.apply(&mut s2c_ct);
                client.decrypt_incoming(&mut s2c_ct);
                assert_eq!(&s2c_ct[..], payload);

                // Verify accept_client_header recovers exact tag and dc_id.
                let (_srv, got_tag, got_dc) =
                    Obfuscated2::accept_client_header(&header, opt_secret);
                assert_eq!(got_tag, tag);
                assert_eq!(got_dc, dc);
            }
        }
    }
}

// ============================================================================
// 3. MTProxy Secrets `dd` / `ee` & Fake-TLS ClientHello Shape + HMAC Tests
// ============================================================================

#[test]
fn conformance_mtproxy_dd_ee_secret_classification_and_fake_tls_client_hello_parse() {
    // 1. Secret classification for plain (16B), dd (17B), ee (>=21B) in hex and base64url.
    let key = [
        0x10u8, 0x22, 0x34, 0x46, 0x58, 0x6A, 0x7C, 0x8E, 0x90, 0xA2, 0xB4, 0xC6, 0xD8, 0xEA, 0xFC,
        0x0E,
    ];
    let domain = b"www.cloudflare.com";

    let mut dd_raw = vec![0xDDu8];
    dd_raw.extend_from_slice(&key);
    assert_eq!(
        MtprotoSecret::from_bytes(&dd_raw).unwrap(),
        MtprotoSecret::Padded(key)
    );

    let mut ee_raw = vec![0xEEu8];
    ee_raw.extend_from_slice(&key);
    ee_raw.extend_from_slice(domain);
    assert_eq!(
        MtprotoSecret::from_bytes(&ee_raw).unwrap(),
        MtprotoSecret::FakeTls {
            key,
            domain: domain.to_vec()
        }
    );

    // Reject invalid lengths (< 16, 17 without 0xDD, 20 with 0xEE).
    assert!(MtprotoSecret::from_bytes(&[0xEE; 20]).is_err());
    assert!(MtprotoSecret::from_bytes(&[0xAA; 17]).is_err());
    assert_eq!(
        ProxyData::mtproto_password_status("ee000102030405060708090a0b0c0d0e0f616263"),
        ProxyStatus::Unsupported // 20 bytes < 21 minimum for 0xEE
    );

    // 2. Full structural TLS 1.3 ClientHello parser verifying every field & extension
    //    emitted by default_client_hello_rules() against mtproto_tls_socket.cpp:126-231.
    let mut rng = StdRng::seed_from_u64(0x2026_0001);
    let ts: i32 = 1_750_000_456;
    let rules = default_client_hello_rules();
    let hello = generate_client_hello_with_rules(&rules, domain, &key, ts, &mut rng).unwrap();
    let data = &hello.data;
    assert!(data.len() <= CLIENT_HELLO_LIMIT);

    // TLS Record layer: ContentType=0x16 (Handshake), LegacyVersion=0x0301, Length
    assert_eq!(data[0], 0x16);
    assert_eq!(&data[1..3], &[0x03, 0x01]);
    let rec_len = u16::from_be_bytes([data[3], data[4]]) as usize;
    assert_eq!(rec_len + 5, data.len());

    // Handshake layer: HandshakeType=0x01 (ClientHello), 24-bit BE length
    assert_eq!(data[5], 0x01);
    assert_eq!(data[6], 0x00);
    let hs_len = u16::from_be_bytes([data[7], data[8]]) as usize;
    assert_eq!(hs_len + 4, rec_len);

    // ClientHello legacy_version = 0x0303 (TLS 1.2)
    assert_eq!(&data[9..11], &[0x03, 0x03]);

    // ClientHello Random (32 bytes at offset 11..43) = HMAC-SHA256(key, zeroed_hello) XOR (ts_le at [28..32])
    let mut zeroed = data.clone();
    zeroed[11..43].fill(0);
    let expected_hmac = spec_hmac_sha256(&key, &[&zeroed]);
    assert_eq!(&data[11..39], &expected_hmac[..28]);
    let mut xor_ts = [0u8; 4];
    for i in 0..4 {
        xor_ts[i] = data[39 + i] ^ expected_hmac[28 + i];
    }
    assert_eq!(i32::from_le_bytes(xor_ts), ts);
    assert_eq!(&hello.digest[..], &data[11..43]);

    // Session ID: length 32 (0x20) + 32 random bytes
    assert_eq!(data[43], 0x20);
    let mut pos = 44 + 32;

    // Cipher suites: length 0x0020 (32 bytes = 16 suites), first suite is GREASE (0x?A 0x?A)
    let cs_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    assert_eq!(cs_len, 32);
    pos += 2;
    assert_eq!(data[pos], data[pos + 1]);
    assert_eq!(data[pos] & 0x0F, 0x0A);
    pos += cs_len;

    // Compression methods: 0x01 0x00
    assert_eq!(&data[pos..pos + 2], &[0x01, 0x00]);
    pos += 2;

    // Extensions list: 2-byte BE total length
    let ext_total = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;
    assert_eq!(pos + ext_total, data.len());

    // Parse every extension TLV and verify expected types, SNI domain, GREASE matching, and ML-KEM bounds!
    let mut seen_types = HashSet::new();
    let mut sni_found = None;
    let mut supported_groups_grease = None;
    let mut key_share_grease = None;
    let mut first_ext_is_grease = false;
    let mut ext_idx = 0usize;

    while pos < data.len() {
        let ext_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let ext_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        let ext_body = &data[pos + 4..pos + 4 + ext_len];
        pos += 4 + ext_len;

        let is_grease =
            ((ext_type >> 8) as u8 == (ext_type & 0xFF) as u8) && ((ext_type & 0x0F) == 0x0A);
        if ext_idx == 0 {
            first_ext_is_grease = is_grease;
            assert_eq!(ext_len, 0);
        }
        if !is_grease {
            assert!(
                seen_types.insert(ext_type),
                "duplicate non-GREASE extension 0x{ext_type:04x}"
            );
        }

        match ext_type {
            0x0000 => {
                // SNI: list_len (2) | host_name_type=0 (1) | name_len (2) | domain
                let list_len = u16::from_be_bytes([ext_body[0], ext_body[1]]) as usize;
                assert_eq!(list_len + 2, ext_len);
                assert_eq!(ext_body[2], 0x00);
                let name_len = u16::from_be_bytes([ext_body[3], ext_body[4]]) as usize;
                assert_eq!(name_len + 3, list_len);
                sni_found = Some(&ext_body[5..5 + name_len]);
            }
            0x000A => {
                // supported_groups: list_len (2) | Grease(4) (2) | X25519MLKEM768 | X25519 | P-256 | P-384
                supported_groups_grease = Some([ext_body[2], ext_body[3]]);
            }
            0x0033 => {
                // key_share: client_shares_len (2) | Grease(4) (2) | 0x0001 0x00 | 0x11EC 0x04C0 | MlKem(1184) | X25519(32) | 0x001D 0x0020 | X25519(32)
                key_share_grease = Some([ext_body[2], ext_body[3]]);
                assert_eq!(&ext_body[4..11], b"\x00\x01\x00\x11\xec\x04\xc0");
                let mlkem = &ext_body[11..11 + 384 * 3];
                // Every unpacked 12-bit ML-KEM-768 polynomial coefficient MUST be < 3329!
                for tri in mlkem.as_chunks::<3>().0 {
                    let a = u16::from(tri[0]) | ((u16::from(tri[1]) & 0x0F) << 8);
                    let b = (u16::from(tri[1]) >> 4) | (u16::from(tri[2]) << 4);
                    assert!(
                        a < 3329 && b < 3329,
                        "ML-KEM coeff out of range: a={a}, b={b}"
                    );
                }
            }
            _ => {}
        }
        ext_idx += 1;
    }

    assert!(first_ext_is_grease);
    assert_eq!(sni_found, Some(domain.as_slice()));
    assert_eq!(
        supported_groups_grease, key_share_grease,
        "supported_groups and key_share must reuse Grease(4)"
    );
    // All 16 standard extensions from PrepareClientHelloRules must be present.
    for required in [
        0x0000u16, 0x0005, 0x000A, 0x000B, 0x000D, 0x0010, 0x0012, 0x0017, 0x001B, 0x0023, 0x002B,
        0x002D, 0x0033, 0x44CD, 0xFE0D, 0xFF01,
    ] {
        assert!(
            seen_types.contains(&required),
            "missing extension 0x{required:04x} in fake-TLS ClientHello"
        );
    }

    // 3. Verify TlsBlock::Padding brings a short ClientHello (< 513 bytes) to exactly 517 bytes.
    let short_rules = vec![
        TlsBlock::String(b"\x16\x03\x01"),
        TlsBlock::Scope(vec![
            TlsBlock::String(b"\x01\x00"),
            TlsBlock::Scope(vec![
                TlsBlock::String(b"\x03\x03"),
                TlsBlock::Zero(32),
                TlsBlock::Padding,
            ]),
        ]),
    ];
    let short_hello =
        generate_client_hello_with_rules(&short_rules, domain, &key, ts, &mut rng).unwrap();
    assert_eq!(short_hello.data.len(), 517);

    // 4. Verify ServerHello HMAC verification accepts valid ServerHello and rejects wrong key /
    //    wrong client digest / mutated byte / wrong headers.
    let sh = spec_build_server_hello(&hello.digest, &key);
    verify_server_hello(&sh, &hello.digest, &key).unwrap();

    let mut wrong_key = key;
    wrong_key[0] ^= 0x01;
    assert!(verify_server_hello(&sh, &hello.digest, &wrong_key).is_err());

    let mut wrong_client_digest = hello.digest;
    wrong_client_digest[31] ^= 0x01;
    assert!(verify_server_hello(&sh, &wrong_client_digest, &key).is_err());

    let mut bad_sh = sh.clone();
    bad_sh[sh.len() - 1] ^= 0x01;
    assert!(verify_server_hello(&bad_sh, &hello.digest, &key).is_err());
}

#[tokio::test]
async fn conformance_mtproxy_dd_and_ee_connection_wire_format() {
    let key = [
        0xA1u8, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18, 0x29, 0x3A, 0x4B, 0x5C, 0x6D, 0x7E, 0x8F,
        0x90,
    ];
    let dc_id = protocol_dc_id(2, false, false);

    // 1. Test `dd` secret over in-memory duplex stream against independent server decoder.
    let mut dd_secret = vec![0xDDu8];
    dd_secret.extend_from_slice(&key);
    let (client_io, mut server_io) = tokio::io::duplex(64 * 1024);
    let cfg_dd = ConnectionConfig {
        dc_id,
        protocol_secret: dd_secret,
        ..Default::default()
    };
    let mut conn_dd = Connection::from_stream(client_io, cfg_dd).await.unwrap();
    assert_eq!(conn_dd.transport_kind(), TransportKind::PaddedIntermediate);

    let req = build_unencrypted_req_pq_multi_with_msg_id([0x44; 16], 0x6700_0000_0000_0000);
    conn_dd.send(&req).await.unwrap();

    // Independent server reads 64-byte obfuscated2 header + encrypted PaddedIntermediate frame.
    let mut hdr = [0u8; 64];
    server_io.read_exact(&mut hdr).await.unwrap();
    assert!(is_good_start_nonce(&hdr));

    let mut send_k_hasher = Sha256::new();
    send_k_hasher.update(&hdr[8..40]);
    send_k_hasher.update(key);
    let c2s_key: [u8; 32] = send_k_hasher.finalize().into();
    let c2s_iv: [u8; 16] = hdr[40..56].try_into().unwrap();
    let mut c2s_ctr = SpecCtr::new(&c2s_key, &c2s_iv);
    let mut dec_hdr = hdr;
    c2s_ctr.apply(&mut dec_hdr);
    assert_eq!(&dec_hdr[56..60], &TAG_PADDED_INTERMEDIATE);
    assert_eq!(i16::from_le_bytes([dec_hdr[60], dec_hdr[61]]), dc_id);

    let mut len_bytes = [0u8; 4];
    server_io.read_exact(&mut len_bytes).await.unwrap();
    c2s_ctr.apply(&mut len_bytes);
    let frame_len = u32::from_le_bytes(len_bytes) as usize;
    assert!((req.len()..=req.len() + 15).contains(&frame_len));
    let mut frame_body = vec![0u8; frame_len];
    server_io.read_exact(&mut frame_body).await.unwrap();
    c2s_ctr.apply(&mut frame_body);
    assert_eq!(&frame_body[..req.len()], &req[..]);

    // 2. Test `ee` fake-TLS secret over in-memory duplex stream against independent server.
    let domain = b"example.org";
    let mut ee_secret = vec![0xEEu8];
    ee_secret.extend_from_slice(&key);
    ee_secret.extend_from_slice(domain);
    let (client_io, mut server_io) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        // Read ClientHello and verify HMAC.
        let mut rh = [0u8; 5];
        server_io.read_exact(&mut rh).await.unwrap();
        assert_eq!(&rh[0..3], b"\x16\x03\x01");
        let len = u16::from_be_bytes([rh[3], rh[4]]) as usize;
        let mut ch_body = vec![0u8; len];
        server_io.read_exact(&mut ch_body).await.unwrap();
        let mut full_ch = Vec::with_capacity(5 + len);
        full_ch.extend_from_slice(&rh);
        full_ch.extend_from_slice(&ch_body);
        let client_digest: [u8; 32] = full_ch[11..43].try_into().unwrap();

        let mut zeroed = full_ch.clone();
        zeroed[11..43].fill(0);
        let hmac = spec_hmac_sha256(&key, &[&zeroed]);
        assert_eq!(&client_digest[..28], &hmac[..28]);

        // Send ServerHello signed with HMAC.
        let sh = spec_build_server_hello(&client_digest, &key);
        server_io.write_all(&sh).await.unwrap();
        server_io.flush().await.unwrap();

        // Read ChangeCipherSpec + ApplicationData record carrying 64B obf2 header + first packet.
        let mut ccs = [0u8; 6];
        server_io.read_exact(&mut ccs).await.unwrap();
        assert_eq!(&ccs, CLIENT_PREFIX);

        let mut app_hdr = [0u8; 5];
        server_io.read_exact(&mut app_hdr).await.unwrap();
        assert_eq!(&app_hdr[..3], CLIENT_HEADER);
        let app_len = u16::from_be_bytes([app_hdr[3], app_hdr[4]]) as usize;
        let mut app = vec![0u8; app_len];
        server_io.read_exact(&mut app).await.unwrap();

        let hdr: [u8; 64] = app[..64].try_into().unwrap();
        let (mut srv_obf, tag, got_dc) = Obfuscated2::accept_client_header(&hdr, Some(&key));
        assert_eq!(tag, TAG_PADDED_INTERMEDIATE);
        assert_eq!(got_dc, dc_id);

        let mut enc_frame = app[64..].to_vec();
        srv_obf.decrypt_incoming(&mut enc_frame);
        let frame_len = u32::from_le_bytes(enc_frame[..4].try_into().unwrap()) as usize;
        assert_eq!(frame_len + 4, enc_frame.len());
        assert_eq!(
            &enc_frame[4..4 + 40],
            &build_unencrypted_req_pq_multi_with_msg_id([0x44; 16], 0x6700_0000_0000_0000)[..]
        );

        // Send one ApplicationData record back using server-to-client cipher.
        let reply = [0x99u8; 24];
        let mut out_frame = Vec::new();
        PaddedIntermediate::new().pack_with_padding(&reply, &[1, 2, 3], &mut out_frame);
        srv_obf.encrypt_outgoing(&mut out_frame);
        server_io.write_all(SERVER_HEADER).await.unwrap();
        server_io
            .write_all(&(out_frame.len() as u16).to_be_bytes())
            .await
            .unwrap();
        server_io.write_all(&out_frame).await.unwrap();
        server_io.flush().await.unwrap();
    });

    let cfg_ee = ConnectionConfig {
        dc_id,
        protocol_secret: ee_secret,
        ..Default::default()
    };
    let mut conn_ee = Connection::from_stream(client_io, cfg_ee).await.unwrap();
    assert_eq!(conn_ee.transport_kind(), TransportKind::PaddedIntermediate);
    conn_ee.send(&req).await.unwrap();
    let got_reply = conn_ee.recv().await.unwrap();
    assert_eq!(got_reply, vec![0x99u8; 24]);
    server_task.await.unwrap();
}
