//! Comprehensive unit and integration tests for `tg-transport`.

use std::net::SocketAddr;
use std::time::Duration;

use hmac::{Hmac, Mac};
use rand::SeedableRng;
use rand::rngs::StdRng;
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use tg_transport::{
    Abridged, CLIENT_HEADER, CLIENT_HELLO_LIMIT, CLIENT_PART_SIZE, CLIENT_PREFIX,
    CONSTRUCTOR_REQ_PQ_MULTI, CONSTRUCTOR_RES_PQ, Connection, ConnectionConfig, Error, FrameKind,
    Full, HELLO_DIGEST_LENGTH, HttpConnection, HttpConnectionConfig, Intermediate, MtprotoSecret,
    OBFUSCATED_HEADER_SIZE, Obfuscated2, PaddedIntermediate, ProxyData, ProxyStatus, ProxyType,
    RaceCandidate, RaceOptions, RacedTransport, SERVER_HEADER, SERVER_HELLO_DIGEST_POSITION,
    SERVER_HELLO_PART1, SERVER_HELLO_PART3, TAG_ABRIDGED, TAG_INTERMEDIATE,
    TAG_PADDED_INTERMEDIATE, TlsStream, Transport, TransportErrorCode, TransportKind,
    build_unencrypted_req_pq_multi_with_msg_id, compute_priority,
    compute_web_proxy_bridge_capability, decode_web_proxy_link_secret, default_client_hello_rules,
    encode_web_proxy_link_secret, generate_client_hello_with_rules, is_good_start_nonce,
    normalize_web_proxy_base_path, normalize_web_proxy_host, parse_unencrypted_res_pq,
    prepare_greases, protocol_dc_id, race_connections, valid_web_proxy_base_path,
    verify_server_hello,
};

// ---------------------------------------------------------------------------
// 1. Framing known-answer & round-trip tests
// ---------------------------------------------------------------------------

#[test]
fn abridged_known_answer_short_and_long() {
    let mut t = Abridged::new();

    // Short payload: 12 bytes = 3 words (< 0x7f) -> 1-byte header 0x03.
    let short_payload = [0x11u8; 12];
    let mut wire = Vec::new();
    t.pack(&short_payload, &mut wire);
    assert_eq!(wire[0], 0x03);
    assert_eq!(&wire[1..], &short_payload);

    let unpacked = t.unpack(&wire).unwrap();
    assert_eq!(unpacked.kind, FrameKind::Payload);
    assert_eq!(unpacked.next_offset, 13);
    assert_eq!(unpacked.slice(&wire), &short_payload);

    // Incomplete input returns MissingBytes.
    assert!(matches!(t.unpack(&wire[..5]), Err(Error::MissingBytes)));
    assert!(matches!(t.unpack(&[]), Err(Error::MissingBytes)));

    // Long payload: 0x80 words = 512 bytes (>= 0x7f) -> 4-byte header [0x7f, 0x80, 0x00, 0x00].
    let long_payload = vec![0xABu8; 0x80 * 4];
    wire.clear();
    t.pack(&long_payload, &mut wire);
    assert_eq!(&wire[..4], &[0x7F, 0x80, 0x00, 0x00]);
    assert_eq!(&wire[4..], &long_payload[..]);

    let unpacked = t.unpack(&wire).unwrap();
    assert_eq!(unpacked.kind, FrameKind::Payload);
    assert_eq!(unpacked.next_offset, 4 + long_payload.len());
    assert_eq!(unpacked.slice(&wire), &long_payload[..]);

    // Quick-ack outgoing bit and incoming 4-byte big-endian token.
    t.set_quick_ack(true);
    wire.clear();
    t.pack(&short_payload, &mut wire);
    assert_eq!(wire[0], 0x83);

    let qa_token: u32 = 0x1234_5678;
    let qa_wire = (qa_token | 0x8000_0000).to_be_bytes();
    let qa_unpacked = t.unpack(&qa_wire).unwrap();
    assert_eq!(qa_unpacked.kind, FrameKind::QuickAck(qa_token));
    assert_eq!(qa_unpacked.next_offset, 4);

    // 4-byte negative error code (-404).
    let mut err_wire = vec![0x01];
    err_wire.extend_from_slice(&(-404i32).to_le_bytes());
    let err = t.unpack(&err_wire).unwrap_err();
    assert!(matches!(
        err,
        Error::ServerError(TransportErrorCode::AuthKeyNotFound)
    ));

    // 4-byte zero NOP.
    let nop_wire = [0x01, 0, 0, 0, 0];
    let nop = t.unpack(&nop_wire).unwrap();
    assert_eq!(nop.kind, FrameKind::Nop);
    assert_eq!(nop.next_offset, 5);
}

#[test]
fn intermediate_known_answer_and_errors() {
    let mut t = Intermediate::new();
    let payload = [0x42u8; 20];
    let mut wire = Vec::new();
    t.pack(&payload, &mut wire);
    assert_eq!(&wire[..4], &20u32.to_le_bytes());
    assert_eq!(&wire[4..], &payload);

    let unpacked = t.unpack(&wire).unwrap();
    assert_eq!(unpacked.kind, FrameKind::Payload);
    assert_eq!(unpacked.slice(&wire), &payload);
    assert_eq!(unpacked.next_offset, 24);

    // Quick-ack request bit on pack.
    t.set_quick_ack(true);
    wire.clear();
    t.pack(&payload, &mut wire);
    assert_eq!(&wire[..4], &(20u32 | 0x8000_0000).to_le_bytes());

    // Quick-ack token on unpack (4-byte LE with bit 31 set).
    let token = 0x0BAD_F00Du32;
    let qa_wire = (token | 0x8000_0000).to_le_bytes();
    let qa = t.unpack(&qa_wire).unwrap();
    assert_eq!(qa.kind, FrameKind::QuickAck(token));
    assert_eq!(qa.next_offset, 4);

    // Server error -429 (TransportFlood) and -444 (InvalidDc).
    let mut err429 = 4u32.to_le_bytes().to_vec();
    err429.extend_from_slice(&(-429i32).to_le_bytes());
    assert!(matches!(
        t.unpack(&err429),
        Err(Error::ServerError(TransportErrorCode::TransportFlood))
    ));

    let mut err444 = 4u32.to_le_bytes().to_vec();
    err444.extend_from_slice(&(-444i32).to_le_bytes());
    assert!(matches!(
        t.unpack(&err444),
        Err(Error::ServerError(TransportErrorCode::InvalidDc))
    ));
}

#[test]
fn padded_intermediate_padding_and_strip() {
    let mut t = PaddedIntermediate::new();

    // Deterministic KAT with 7 bytes of padding around an unencrypted req_pq_multi (40 bytes).
    let req = build_unencrypted_req_pq_multi_with_msg_id([0x55; 16], 0x1234_5678_0000_0000);
    let pad = [1u8, 2, 3, 4, 5, 6, 7];
    let mut wire = Vec::new();
    t.pack_with_padding(&req, &pad, &mut wire);
    assert_eq!(&wire[..4], &((req.len() + pad.len()) as u32).to_le_bytes());
    assert_eq!(&wire[4..4 + req.len()], &req[..]);
    assert_eq!(&wire[4 + req.len()..], &pad);

    let unpacked = t.unpack(&wire).unwrap();
    assert_eq!(unpacked.kind, FrameKind::Payload);
    assert_eq!(unpacked.next_offset, wire.len());
    // Partial word (7 % 4 = 3) trimmed by unpack; remaining 4 bytes stripped by MTProto helper.
    assert_eq!(unpacked.slice(&wire).len(), req.len() + 4);
    let stripped = PaddedIntermediate::strip_mtproto_padding(unpacked.slice(&wire));
    assert_eq!(stripped, &req[..]);

    // Random padding round-trip on an encrypted-shaped MTProto payload (8 + 16 + 32 = 56 bytes).
    let mut encrypted_like = vec![0u8; 56];
    encrypted_like[..8].copy_from_slice(&0xDEAD_BEEF_CAFE_BABEu64.to_le_bytes());
    for (i, b) in encrypted_like[8..].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7);
    }
    for _ in 0..32 {
        let mut buf = Vec::new();
        t.pack(&encrypted_like, &mut buf);
        let pad_len = buf.len() - 4 - encrypted_like.len();
        assert!(pad_len <= 15);
        let u = t.unpack(&buf).unwrap();
        assert_eq!(u.next_offset, buf.len());
        let recovered = PaddedIntermediate::strip_mtproto_padding(u.slice(&buf));
        assert_eq!(recovered, &encrypted_like[..]);
    }

    // 4-byte error code (-404) with 2 bytes of padding.
    let mut err_wire = Vec::new();
    t.pack_with_padding(&(-404i32).to_le_bytes(), &[0xAA, 0xBB], &mut err_wire);
    assert!(matches!(
        t.unpack(&err_wire),
        Err(Error::ServerError(TransportErrorCode::AuthKeyNotFound))
    ));
}

#[test]
fn full_transport_known_answer_seq_and_crc() {
    let mut sender = Full::new();
    let mut receiver = Full::new();

    let p0 = b"0123456789abcdef"; // 16 bytes
    let p1 = b"fedcba9876543210abcd"; // 20 bytes

    let mut wire = Vec::new();
    sender.pack(p0, &mut wire);
    sender.pack(p1, &mut wire);
    assert_eq!(sender.send_seq(), 2);

    // Known-answer check on first frame: len=28, seq=0, payload=p0, crc32.
    assert_eq!(&wire[0..4], &28u32.to_le_bytes());
    assert_eq!(&wire[4..8], &0u32.to_le_bytes());
    assert_eq!(&wire[8..24], p0);
    let expected_crc0 = crc32fast::hash(&wire[0..24]);
    assert_eq!(&wire[24..28], &expected_crc0.to_le_bytes());

    let u0 = receiver.unpack(&wire).unwrap();
    assert_eq!(u0.kind, FrameKind::Payload);
    assert_eq!(u0.slice(&wire), p0);
    assert_eq!(u0.next_offset, 28);

    let u1 = receiver.unpack(&wire[u0.next_offset..]).unwrap();
    assert_eq!(u1.kind, FrameKind::Payload);
    assert_eq!(u1.slice(&wire[u0.next_offset..]), p1);
    assert_eq!(receiver.recv_seq(), 2);

    // Corrupted CRC fails with CrcMismatch.
    let mut bad_crc = wire[..28].to_vec();
    bad_crc[10] ^= 0x01;
    let mut fresh_rx = Full::new();
    assert!(matches!(
        fresh_rx.unpack(&bad_crc),
        Err(Error::CrcMismatch { .. })
    ));

    // Wrong sequence number fails with SequenceMismatch.
    assert!(matches!(
        receiver.unpack(&wire[..28]),
        Err(Error::SequenceMismatch {
            expected: 2,
            actual: 0
        })
    ));

    // 4-byte negative error packet inside Full framing.
    let mut err_tx = Full::new();
    let mut err_wire = Vec::new();
    err_tx.pack(&(-404i32).to_le_bytes(), &mut err_wire);
    let mut err_rx = Full::new();
    assert!(matches!(
        err_rx.unpack(&err_wire),
        Err(Error::ServerError(TransportErrorCode::AuthKeyNotFound))
    ));
}

// ---------------------------------------------------------------------------
// 2. Obfuscated2 tests
// ---------------------------------------------------------------------------

#[test]
fn obfuscated2_forbidden_prefixes_and_roundtrip() {
    // Reject all forbidden first byte / first word / second word values.
    let mut bad = [0x11u8; 64];
    assert!(is_good_start_nonce(&bad));

    bad[0] = 0xEF;
    assert!(!is_good_start_nonce(&bad));

    for forbidden_first in [
        *b"HEAD",
        *b"POST",
        *b"GET ",
        [0xEE; 4],
        [0xDD; 4],
        [0x16, 0x03, 0x01, 0x02],
    ] {
        let mut n = [0x22u8; 64];
        n[0..4].copy_from_slice(&forbidden_first);
        assert!(
            !is_good_start_nonce(&n),
            "prefix {forbidden_first:02x?} should be rejected"
        );
    }

    let mut zero_second = [0x33u8; 64];
    zero_second[4..8].copy_from_slice(&[0, 0, 0, 0]);
    assert!(!is_good_start_nonce(&zero_second));

    // Random headers always satisfy is_good_start_nonce and decrypt cleanly on the server side.
    let secret = [0x7Au8; 16];
    for dc in [
        2i16,
        -2,
        protocol_dc_id(2, false, true),
        protocol_dc_id(4, true, true),
    ] {
        for sec in [None, Some(&secret)] {
            let (mut client, header) = Obfuscated2::generate(TAG_PADDED_INTERMEDIATE, dc, sec);
            assert_eq!(header.len(), OBFUSCATED_HEADER_SIZE);
            assert!(is_good_start_nonce(&header));

            let (mut server, tag, parsed_dc) = Obfuscated2::accept_client_header(&header, sec);
            assert_eq!(tag, TAG_PADDED_INTERMEDIATE);
            assert_eq!(parsed_dc, dc);

            // Bidirectional multi-block stream check.
            let c2s_plain = b"client-to-server MTProto payload spanning multiple AES CTR blocks!!";
            let mut c2s = c2s_plain.to_vec();
            client.encrypt_outgoing(&mut c2s);
            assert_ne!(&c2s[..], c2s_plain);
            server.decrypt_incoming(&mut c2s);
            assert_eq!(&c2s[..], c2s_plain);

            let s2c_plain = b"server-to-client MTProto response using reversed key and iv!!!!";
            let mut s2c = s2c_plain.to_vec();
            server.encrypt_outgoing(&mut s2c);
            assert_ne!(&s2c[..], s2c_plain);
            client.decrypt_incoming(&mut s2c);
            assert_eq!(&s2c[..], s2c_plain);
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Proxy secret & link parsing tests (including tdesktop KAT vectors)
// ---------------------------------------------------------------------------

#[test]
fn proxy_secrets_and_links() {
    // Plain 16-byte hex secret.
    let plain_hex = "000102030405060708090a0b0c0d0e0f";
    assert_eq!(
        ProxyData::mtproto_password_status(plain_hex),
        ProxyStatus::Valid
    );
    let p_plain = ProxyData::mtproto("1.2.3.4", 443, plain_hex);
    assert!(p_plain.valid());
    assert_eq!(
        p_plain.parsed_mtproto_secret().unwrap(),
        MtprotoSecret::Plain([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f
        ])
    );

    // Padded 17-byte `dd` hex secret.
    let dd_hex = "dd000102030405060708090a0b0c0d0e0f";
    let p_dd = ProxyData::mtproto("proxy.example.org", 8443, dd_hex);
    assert!(p_dd.valid());
    assert!(p_dd.try_custom_resolve());
    assert!(matches!(
        p_dd.parsed_mtproto_secret().unwrap(),
        MtprotoSecret::Padded(_)
    ));

    // Fake-TLS `ee` hex secret with domain "cloudflare.com".
    let ee_hex = "ee000102030405060708090a0b0c0d0e0f636c6f7564666c6172652e636f6d";
    let p_ee = ProxyData::mtproto("198.51.100.1", 443, ee_hex);
    assert!(p_ee.valid());
    assert!(!p_ee.try_custom_resolve());
    match p_ee.parsed_mtproto_secret().unwrap() {
        MtprotoSecret::FakeTls { key, domain } => {
            assert_eq!(key[0], 0x00);
            assert_eq!(key[15], 0x0F);
            assert_eq!(domain, b"cloudflare.com");
        }
        other => panic!("expected FakeTls, got {other:?}"),
    }

    // Base64url `ee` secret (0xEE -> starts with `7g`..`7v`).
    use base64::Engine as _;
    let raw_ee = p_ee.secret_from_mtproto_password();
    let ee_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw_ee);
    assert_eq!(
        ProxyData::mtproto_password_status(&ee_b64),
        ProxyStatus::Valid
    );
    let p_ee_b64 = ProxyData::mtproto("198.51.100.1", 443, &ee_b64);
    assert_eq!(p_ee_b64.secret_from_mtproto_password(), raw_ee);

    // Incorrect secret: user typed `ee` before base64url.
    let bad_ee_b64 = format!("ee{ee_b64}");
    assert_eq!(
        ProxyData::mtproto_password_status(&bad_ee_b64),
        ProxyStatus::IncorrectSecret
    );

    // Unsupported length (18 bytes hex starting with 0x11).
    let unsupported_hex = "11000102030405060708090a0b0c0d0e0f12";
    assert_eq!(
        ProxyData::mtproto_password_status(unsupported_hex),
        ProxyStatus::Unsupported
    );

    // Invalid short string.
    assert_eq!(
        ProxyData::mtproto_password_status("deadbeef"),
        ProxyStatus::Invalid
    );

    // Link parsing: tg://proxy, https://t.me/proxy, tg://socks, t.me/socks.
    let parsed_tg = ProxyData::from_link(&format!(
        "tg://proxy?server=1.2.3.4&port=443&secret={ee_hex}"
    ))
    .unwrap();
    assert_eq!(parsed_tg.proxy_type, ProxyType::Mtproto);
    assert_eq!(parsed_tg.host, "1.2.3.4");
    assert_eq!(parsed_tg.port, 443);
    assert_eq!(parsed_tg.password, ee_hex);
    assert!(parsed_tg.valid());

    let parsed_tme = ProxyData::from_link(&format!(
        "https://t.me/proxy?SERVER=proxy.test&Port=8443&Secret={dd_hex}#frag"
    ))
    .unwrap();
    assert_eq!(parsed_tme.proxy_type, ProxyType::Mtproto);
    assert_eq!(parsed_tme.host, "proxy.test");
    assert_eq!(parsed_tme.port, 8443);
    assert_eq!(parsed_tme.password, dd_hex);

    // `+` and `/` in base64 secret inside URL get normalized to `-` and `_`.
    let plus_secret = ee_b64.replace('-', "+").replace('_', "/");
    let parsed_plus = ProxyData::from_link(&format!(
        "t.me/proxy/?server=1.2.3.4&port=443&secret={plus_secret}"
    ))
    .unwrap();
    assert_eq!(parsed_plus.password, ee_b64);
    assert!(parsed_plus.valid());

    let parsed_socks = ProxyData::from_link(
        "https://t.me/socks?server=127.0.0.1&port=1080&user=alice&pass=s%3Aecret",
    )
    .unwrap();
    assert_eq!(parsed_socks.proxy_type, ProxyType::Socks5);
    assert_eq!(parsed_socks.host, "127.0.0.1");
    assert_eq!(parsed_socks.port, 1080);
    assert_eq!(parsed_socks.user, "alice");
    assert_eq!(parsed_socks.password, "s:ecret");
    assert!(parsed_socks.valid());

    // Round-trip `to_tg_link`.
    let link = parsed_socks.to_tg_link().unwrap();
    assert_eq!(ProxyData::from_link(&link).unwrap(), parsed_socks);
}

#[test]
fn web_proxy_known_answer_vectors_from_tdesktop() {
    // Exact debug assertions from `mtproto/mtproto_proxy_data.cpp:312-380`.
    assert_eq!(
        normalize_web_proxy_host(" Proxy.Example.COM "),
        "proxy.example.com"
    );
    assert_eq!(
        normalize_web_proxy_host("bücher.example"),
        "xn--bcher-kva.example"
    );
    assert_eq!(normalize_web_proxy_host("bücher.de"), "xn--bcher-kva.de");
    assert_eq!(
        normalize_web_proxy_host("xn--strae-oqa.example"),
        "xn--strae-oqa.example"
    );
    assert!(normalize_web_proxy_host("localhost").is_empty());
    assert!(normalize_web_proxy_host("127.0.0.1").is_empty());
    assert!(normalize_web_proxy_host("127.1").is_empty());
    assert!(normalize_web_proxy_host("0x7f.1").is_empty());
    assert!(normalize_web_proxy_host("0177.0.0.1").is_empty());
    assert!(normalize_web_proxy_host("1.2.3").is_empty());
    assert!(normalize_web_proxy_host("site.example:443").is_empty());
    assert!(normalize_web_proxy_host("site..example").is_empty());

    assert_eq!(
        normalize_web_proxy_base_path(" /dobry-cola-super-app/ "),
        "dobry-cola-super-app"
    );
    assert_eq!(normalize_web_proxy_base_path("a"), "a");
    assert!(normalize_web_proxy_base_path("").is_empty());
    assert!(normalize_web_proxy_base_path("-lead").is_empty());
    assert_eq!(normalize_web_proxy_base_path("MixedCase"), "MixedCase");
    assert_eq!(
        normalize_web_proxy_base_path(" /two/segments/ "),
        "two/segments"
    );
    assert!(normalize_web_proxy_base_path("with space").is_empty());
    assert!(normalize_web_proxy_base_path("empty//segment").is_empty());
    assert!(normalize_web_proxy_base_path("trailing//").is_empty());
    assert!(normalize_web_proxy_base_path("a/-lead").is_empty());
    assert!(normalize_web_proxy_base_path("per%20cent").is_empty());
    assert!(normalize_web_proxy_base_path("dot.ted").is_empty());
    assert!(normalize_web_proxy_base_path("..").is_empty());
    assert!(normalize_web_proxy_base_path(&"a".repeat(129)).is_empty());
    assert!(!normalize_web_proxy_base_path(&"a".repeat(128)).is_empty());
    assert!(valid_web_proxy_base_path(""));
    assert!(valid_web_proxy_base_path("/"));
    assert!(!valid_web_proxy_base_path("empty//segment"));

    let plain_secret = "8561944064fc730cbfa4473562d8ec59";
    let marked_secret = "cIVhlEBk_HMMv6RHNWLY7Fk";
    assert_eq!(
        decode_web_proxy_link_secret(marked_secret, true),
        plain_secret
    );
    assert_eq!(
        decode_web_proxy_link_secret(marked_secret, false),
        plain_secret
    );
    assert_eq!(
        decode_web_proxy_link_secret(plain_secret, false),
        plain_secret
    );
    assert!(decode_web_proxy_link_secret(plain_secret, true).is_empty());

    let mut web_proxy = ProxyData::none();
    web_proxy.proxy_type = ProxyType::Web;
    web_proxy.port = 443;
    web_proxy.set_web_address("https://proxy.example.com/my-app/");
    web_proxy.password = plain_secret.to_owned();
    assert_eq!(encode_web_proxy_link_secret(&web_proxy), marked_secret);

    let plain = [
        0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    let padded = [
        0xddu8, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
        0x0e, 0x0f,
    ];
    let path = "dobry-cola-super-app";
    assert_eq!(
        compute_web_proxy_bridge_capability("proxy.example.com", "", &plain),
        "MHLEY5PmW1GWqJkSrlmJpvJUiLhBH_QKy6yKg8a0JPk"
    );
    assert_eq!(
        compute_web_proxy_bridge_capability("proxy.example.com", "", &padded),
        "IpJrt3e7sKtzPyoXy6w-Zj6GGEvsvclN66JzQEfPYLA"
    );
    assert_eq!(
        compute_web_proxy_bridge_capability("proxy.example.com", path, &plain),
        "hHz99Xs93EN1j91G9gpNepXwGNNt5YdAFkEVk_LlqdQ"
    );
    assert_eq!(
        compute_web_proxy_bridge_capability("proxy.example.com", path, &padded),
        "TGUkZaevsavLbHvlNWipnRoYxgzZ51ioWvbxgGT3wHo"
    );
}

// ---------------------------------------------------------------------------
// 4. Fake-TLS ClientHello, ServerHello HMAC & record framing tests
// ---------------------------------------------------------------------------

#[test]
fn fake_tls_client_hello_structure_and_hmac() {
    let mut rng = StdRng::seed_from_u64(0xCAFE_BABE);

    // Verify GREASE invariants.
    for _ in 0..32 {
        let g = prepare_greases(&mut rng);
        for &b in &g {
            assert_eq!(b & 0x0F, 0x0A);
        }
        for pair in g.as_chunks::<2>().0 {
            assert_ne!(pair[0], pair[1]);
        }
    }

    let domain = b"www.google.com";
    let key = [0x3Cu8; 16];
    let ts: i32 = 1_700_000_123;
    let rules = default_client_hello_rules();
    let hello = generate_client_hello_with_rules(&rules, domain, &key, ts, &mut rng).unwrap();

    assert!(hello.data.len() <= CLIENT_HELLO_LIMIT);
    // TLS record header: Handshake (0x16), TLS 1.0 (0x03 0x01), 2-byte BE length.
    assert_eq!(&hello.data[0..3], b"\x16\x03\x01");
    let record_len = u16::from_be_bytes([hello.data[3], hello.data[4]]) as usize;
    assert_eq!(record_len + 5, hello.data.len());

    // Handshake header: ClientHello (0x01), 0x00, 2-byte BE length.
    assert_eq!(&hello.data[5..7], b"\x01\x00");
    let hs_len = u16::from_be_bytes([hello.data[7], hello.data[8]]) as usize;
    assert_eq!(hs_len + 4, record_len);

    // ClientHello version TLS 1.2 (0x03 0x03).
    assert_eq!(&hello.data[9..11], b"\x03\x03");

    // Digest sits at offset 11..43.
    assert_eq!(&hello.data[11..11 + HELLO_DIGEST_LENGTH], &hello.digest[..]);

    // Verify HMAC-SHA256 + timestamp XOR matches MTProxy server verification!
    let mut zeroed = hello.data.clone();
    zeroed[11..11 + HELLO_DIGEST_LENGTH].fill(0);
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
    mac.update(&zeroed);
    let expected_hmac = mac.finalize().into_bytes();

    // First 28 bytes match HMAC verbatim.
    assert_eq!(&hello.digest[..28], &expected_hmac[..28]);
    // Last 4 bytes XOR with expected_hmac[28..32] recover exact timestamp!
    let mut recovered_ts_bytes = [0u8; 4];
    for i in 0..4 {
        recovered_ts_bytes[i] = hello.digest[28 + i] ^ expected_hmac[28 + i];
    }
    assert_eq!(i32::from_le_bytes(recovered_ts_bytes), ts);

    // SNI domain bytes appear inside the ClientHello.
    assert!(hello.data.windows(domain.len()).any(|w| w == domain));
}

fn build_mock_server_hello(client_digest: &[u8; HELLO_DIGEST_LENGTH], key: &[u8; 16]) -> Vec<u8> {
    let mut sh = Vec::new();
    // Part 1: \x16\x03\x03 + 2-byte BE len (part2 = 60 bytes, covering digest at offset 11..43).
    sh.extend_from_slice(SERVER_HELLO_PART1);
    sh.extend_from_slice(&60u16.to_be_bytes());
    // Handshake header (6 bytes -> puts digest at 5 + 6 = 11) + 32-byte zeroed digest + 22 bytes.
    sh.extend_from_slice(&[0x02, 0x00, 0x00, 0x38, 0x03, 0x03]);
    assert_eq!(sh.len(), SERVER_HELLO_DIGEST_POSITION);
    sh.extend_from_slice(&[0u8; HELLO_DIGEST_LENGTH]);
    sh.extend_from_slice(&[0x44u8; 22]);

    // Part 3: ChangeCipherSpec + ApplicationData header + 2-byte BE len + part4 (16 bytes).
    sh.extend_from_slice(SERVER_HELLO_PART3);
    sh.extend_from_slice(&16u16.to_be_bytes());
    sh.extend_from_slice(&[0x99u8; 16]);

    // Compute HMAC-SHA256(key, client_digest || sh_with_zero_digest) and write at 11..43.
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(client_digest);
    mac.update(&sh);
    let digest = mac.finalize().into_bytes();
    sh[SERVER_HELLO_DIGEST_POSITION..SERVER_HELLO_DIGEST_POSITION + HELLO_DIGEST_LENGTH]
        .copy_from_slice(&digest);
    sh
}

#[tokio::test]
async fn fake_tls_stream_handshake_and_large_record_framing() {
    let key = [0x5Au8; 16];
    let domain = b"example.com";

    // Also verify tampered ServerHello fails verification.
    let fake_digest = [0x12u8; 32];
    let mut sh = build_mock_server_hello(&fake_digest, &key);
    verify_server_hello(&sh, &fake_digest, &key).unwrap();
    sh[20] ^= 0xFF;
    assert!(matches!(
        verify_server_hello(&sh, &fake_digest, &key),
        Err(Error::Tls(_))
    ));

    // Full async TlsStream test over an in-memory DuplexStream.
    let (client_io, mut server_io) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        // Read TLS ClientHello record.
        let mut head = [0u8; 5];
        server_io.read_exact(&mut head).await.unwrap();
        assert_eq!(&head[..3], b"\x16\x03\x01");
        let rec_len = u16::from_be_bytes([head[3], head[4]]) as usize;
        let mut body = vec![0u8; rec_len];
        server_io.read_exact(&mut body).await.unwrap();

        let mut client_digest = [0u8; 32];
        client_digest.copy_from_slice(&body[6..38]);

        // Send valid ServerHello.
        let server_hello = build_mock_server_hello(&client_digest, &key);
        server_io.write_all(&server_hello).await.unwrap();
        server_io.flush().await.unwrap();

        // Client sends ChangeCipherSpec prefix + chunked Application Data records (payload > 2878).
        let mut ccs = [0u8; 6];
        server_io.read_exact(&mut ccs).await.unwrap();
        assert_eq!(&ccs, CLIENT_PREFIX);

        let mut received = Vec::new();
        let mut record_count = 0usize;
        while received.len() < 4000 {
            let mut rh = [0u8; 5];
            server_io.read_exact(&mut rh).await.unwrap();
            assert_eq!(&rh[..3], CLIENT_HEADER);
            let len = u16::from_be_bytes([rh[3], rh[4]]) as usize;
            assert!(len <= CLIENT_PART_SIZE);
            let mut chunk = vec![0u8; len];
            server_io.read_exact(&mut chunk).await.unwrap();
            received.extend_from_slice(&chunk);
            record_count += 1;
        }
        assert!(record_count >= 2);

        // Send a 0-length record (must be skipped) followed by echoed payload.
        server_io.write_all(SERVER_HEADER).await.unwrap();
        server_io.write_all(&0u16.to_be_bytes()).await.unwrap();
        server_io.write_all(SERVER_HEADER).await.unwrap();
        server_io
            .write_all(&(received.len() as u16).to_be_bytes())
            .await
            .unwrap();
        server_io.write_all(&received).await.unwrap();
        server_io.flush().await.unwrap();
    });

    let mut tls = TlsStream::connect(client_io, domain, &key).await.unwrap();
    let prefix = [0xAAu8; 64];
    let payload = vec![0xBBu8; 4000 - 64];
    tls.write_framed(&prefix, &payload).await.unwrap();

    let mut echoed = Vec::new();
    let mut tmp = [0u8; 512];
    while echoed.len() < 4000 {
        let n = tls.read_payload(&mut tmp).await.unwrap();
        assert!(n > 0);
        echoed.extend_from_slice(&tmp[..n]);
    }
    assert_eq!(&echoed[..64], &prefix);
    assert_eq!(&echoed[64..], &payload[..]);

    let echoed_check = echoed.len();
    assert_eq!(echoed_check, 4000);
    server_task.await.unwrap();
}

#[tokio::test]
async fn mtproxy_fake_tls_end_to_end_connection() {
    let key = [
        0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0xD0, 0xE0, 0xF0,
        0x01,
    ];
    let secret_hex = "ee102030405060708090a0b0c0d0e0f0016578616d706c652e636f6d"; // ee + key + "example.com"
    let expected_dc = protocol_dc_id(4, true, false); // -4 (DC4 media)

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();

        // 1. Read fake-TLS ClientHello and verify HMAC.
        let mut head = [0u8; 5];
        sock.read_exact(&mut head).await.unwrap();
        assert_eq!(&head[..3], b"\x16\x03\x01");
        let rec_len = u16::from_be_bytes([head[3], head[4]]) as usize;
        let mut body = vec![0u8; rec_len];
        sock.read_exact(&mut body).await.unwrap();

        let mut client_digest = [0u8; 32];
        client_digest.copy_from_slice(&body[6..38]);

        // 2. Reply with signed ServerHello.
        let server_hello = build_mock_server_hello(&client_digest, &key);
        sock.write_all(&server_hello).await.unwrap();
        sock.flush().await.unwrap();

        // 3. Read ChangeCipherSpec + Application Data record containing 64-byte obfuscated2 header + first packet.
        let mut ccs = [0u8; 6];
        sock.read_exact(&mut ccs).await.unwrap();
        assert_eq!(&ccs, CLIENT_PREFIX);

        let mut rec_head = [0u8; 5];
        sock.read_exact(&mut rec_head).await.unwrap();
        assert_eq!(&rec_head[..3], CLIENT_HEADER);
        let app_len = u16::from_be_bytes([rec_head[3], rec_head[4]]) as usize;
        let mut app_data = vec![0u8; app_len];
        sock.read_exact(&mut app_data).await.unwrap();

        let mut obf_header = [0u8; OBFUSCATED_HEADER_SIZE];
        obf_header.copy_from_slice(&app_data[..OBFUSCATED_HEADER_SIZE]);
        let (mut obf, tag, dc) = Obfuscated2::accept_client_header(&obf_header, Some(&key));
        assert_eq!(tag, TAG_PADDED_INTERMEDIATE);
        assert_eq!(dc, expected_dc);

        let mut encrypted_pkt = app_data[OBFUSCATED_HEADER_SIZE..].to_vec();
        obf.decrypt_incoming(&mut encrypted_pkt);

        let mut transport = PaddedIntermediate::new();
        let unpacked = transport.unpack(&encrypted_pkt).unwrap();
        let stripped = PaddedIntermediate::strip_mtproto_padding(unpacked.slice(&encrypted_pkt));

        // Echo back an unencrypted resPQ with matching nonce.
        let mut req_nonce = [0u8; 16];
        req_nonce.copy_from_slice(&stripped[24..40]);
        let mut res_pq = Vec::with_capacity(56);
        res_pq.extend_from_slice(&0u64.to_le_bytes()); // auth_key_id = 0
        res_pq.extend_from_slice(&0x1234_5678_0000_0001i64.to_le_bytes()); // server msg_id (...01)
        res_pq.extend_from_slice(&36u32.to_le_bytes()); // body len
        res_pq.extend_from_slice(&CONSTRUCTOR_RES_PQ.to_le_bytes());
        res_pq.extend_from_slice(&req_nonce);
        res_pq.extend_from_slice(&[0xEEu8; 16]); // server_nonce

        let mut out_wire = Vec::new();
        transport.pack(&res_pq, &mut out_wire);
        obf.encrypt_outgoing(&mut out_wire);

        sock.write_all(SERVER_HEADER).await.unwrap();
        sock.write_all(&(out_wire.len() as u16).to_be_bytes())
            .await
            .unwrap();
        sock.write_all(&out_wire).await.unwrap();
        sock.flush().await.unwrap();
    });

    let cfg = ConnectionConfig {
        dc_id: expected_dc,
        proxy: ProxyData::mtproto("127.0.0.1", proxy_addr.port(), secret_hex),
        ..Default::default()
    };
    let mut conn = Connection::connect("149.154.167.50", 443, cfg)
        .await
        .unwrap();
    assert_eq!(conn.transport_kind(), TransportKind::PaddedIntermediate);
    let rtt = conn.probe_pq(Duration::from_secs(3)).await.unwrap();
    assert!(rtt < Duration::from_secs(3));
    assert!(conn.ping_time().is_some());

    server_task.await.unwrap();
}

// ---------------------------------------------------------------------------
// 5. SOCKS5, HTTP CONNECT, Plain HTTP, and Connection Racing tests
// ---------------------------------------------------------------------------

/// Spawn a tiny local mock obfuscated2 Intermediate echo server that echoes back every
/// MTProto frame it receives.
async fn spawn_mock_mtproto_echo_server(expected_dc: i16) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut header = [0u8; OBFUSCATED_HEADER_SIZE];
                if sock.read_exact(&mut header).await.is_err() {
                    return;
                }
                let (mut obf, tag, dc) = Obfuscated2::accept_client_header(&header, None);
                assert_eq!(dc, expected_dc);
                let mut transport = match tag {
                    t if t == TAG_ABRIDGED => TransportKind::Abridged.build(),
                    t if t == TAG_INTERMEDIATE => TransportKind::Intermediate.build(),
                    t if t == TAG_PADDED_INTERMEDIATE => TransportKind::PaddedIntermediate.build(),
                    _ => return,
                };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while let Ok(n) = sock.read(&mut tmp).await {
                    if n == 0 {
                        break;
                    }
                    obf.decrypt_incoming(&mut tmp[..n]);
                    buf.extend_from_slice(&tmp[..n]);
                    while let Ok(u) = transport.unpack(&buf) {
                        let payload = u.slice(&buf).to_vec();
                        buf.drain(..u.next_offset);
                        let mut out = Vec::new();
                        transport.pack(&payload, &mut out);
                        obf.encrypt_outgoing(&mut out);
                        if sock.write_all(&out).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    addr
}

/// Spawn a tiny local SOCKS5 proxy server supporting both No-Auth and RFC 1929 Username/Password.
async fn spawn_mock_socks5_server(required_auth: Option<(String, String)>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let auth = required_auth.clone();
            tokio::spawn(async move {
                let mut head = [0u8; 2];
                if client.read_exact(&mut head).await.is_err() || head[0] != 0x05 {
                    return;
                }
                let mut methods = vec![0u8; head[1] as usize];
                if client.read_exact(&mut methods).await.is_err() {
                    return;
                }
                if let Some((expected_user, expected_pass)) = auth {
                    if !methods.contains(&0x02) {
                        let _ = client.write_all(&[0x05, 0xFF]).await;
                        return;
                    }
                    let _ = client.write_all(&[0x05, 0x02]).await;
                    let mut auth_head = [0u8; 2];
                    if client.read_exact(&mut auth_head).await.is_err() {
                        return;
                    }
                    let mut user = vec![0u8; auth_head[1] as usize];
                    if client.read_exact(&mut user).await.is_err() {
                        return;
                    }
                    let mut plen = [0u8; 1];
                    if client.read_exact(&mut plen).await.is_err() {
                        return;
                    }
                    let mut pass = vec![0u8; plen[0] as usize];
                    if client.read_exact(&mut pass).await.is_err() {
                        return;
                    }
                    if user != expected_user.as_bytes() || pass != expected_pass.as_bytes() {
                        let _ = client.write_all(&[0x01, 0x01]).await;
                        return;
                    }
                    let _ = client.write_all(&[0x01, 0x00]).await;
                } else {
                    let _ = client.write_all(&[0x05, 0x00]).await;
                }

                // Read CONNECT request.
                let mut req = [0u8; 4];
                if client.read_exact(&mut req).await.is_err() || req[0] != 0x05 || req[1] != 0x01 {
                    return;
                }
                let target_host = match req[3] {
                    0x01 => {
                        let mut ip = [0u8; 4];
                        if client.read_exact(&mut ip).await.is_err() {
                            return;
                        }
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    0x04 => {
                        let mut ip = [0u8; 16];
                        if client.read_exact(&mut ip).await.is_err() {
                            return;
                        }
                        std::net::Ipv6Addr::from(ip).to_string()
                    }
                    0x03 => {
                        let mut dlen = [0u8; 1];
                        if client.read_exact(&mut dlen).await.is_err() {
                            return;
                        }
                        let mut domain = vec![0u8; dlen[0] as usize];
                        if client.read_exact(&mut domain).await.is_err() {
                            return;
                        }
                        String::from_utf8_lossy(&domain).into_owned()
                    }
                    _ => return,
                };
                let mut port_buf = [0u8; 2];
                if client.read_exact(&mut port_buf).await.is_err() {
                    return;
                }
                let target_port = u16::from_be_bytes(port_buf);

                let Ok(mut upstream) =
                    tokio::net::TcpStream::connect((target_host.as_str(), target_port)).await
                else {
                    let _ = client
                        .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await;
                    return;
                };
                let _ = client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
                    .await;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn socks5_connection_no_auth_and_user_pass() {
    let target = spawn_mock_mtproto_echo_server(2).await;

    // 1. SOCKS5 without authentication.
    let socks_no_auth = spawn_mock_socks5_server(None).await;
    let cfg = ConnectionConfig {
        transport: TransportKind::Intermediate,
        obfuscated: true,
        dc_id: 2,
        proxy: ProxyData::socks5("127.0.0.1", socks_no_auth.port(), "", ""),
        ..Default::default()
    };
    let mut conn = Connection::connect(&target.ip().to_string(), target.port(), cfg)
        .await
        .unwrap();
    let msg = b"hello through socks5 no-auth!!!!";
    conn.send(msg).await.unwrap();
    assert_eq!(conn.recv().await.unwrap(), msg);

    // 2. SOCKS5 with username/password auth.
    let socks_auth = spawn_mock_socks5_server(Some(("alice".into(), "secret123".into()))).await;
    let cfg_auth = ConnectionConfig {
        transport: TransportKind::Abridged,
        obfuscated: true,
        dc_id: 2,
        proxy: ProxyData::socks5("127.0.0.1", socks_auth.port(), "alice", "secret123"),
        ..Default::default()
    };
    let mut conn_auth = Connection::connect(&target.ip().to_string(), target.port(), cfg_auth)
        .await
        .unwrap();
    let msg2 = b"hello through socks5 with auth!!";
    conn_auth.send(msg2).await.unwrap();
    assert_eq!(conn_auth.recv().await.unwrap(), msg2);

    // 3. Wrong password returns ProxyAuthFailed.
    let cfg_bad = ConnectionConfig {
        transport: TransportKind::Intermediate,
        obfuscated: true,
        dc_id: 2,
        proxy: ProxyData::socks5("127.0.0.1", socks_auth.port(), "alice", "wrong"),
        ..Default::default()
    };
    let err = Connection::connect(&target.ip().to_string(), target.port(), cfg_bad)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::ProxyAuthFailed));
}

#[tokio::test]
async fn http_connect_proxy_tunnel() {
    let target = spawn_mock_mtproto_echo_server(3).await;

    // Tiny local HTTP CONNECT proxy requiring Basic auth `bob:pw`.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 512];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = client.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let req = String::from_utf8_lossy(&buf);
                if !req.contains("Proxy-Authorization: Basic Ym9iOnB3") {
                    let _ = client
                        .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                        .await;
                    return;
                }
                let target_authority = req
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                let mut upstream = tokio::net::TcpStream::connect(&target_authority)
                    .await
                    .unwrap();
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    // Valid credentials succeed.
    let cfg = ConnectionConfig {
        transport: TransportKind::Intermediate,
        obfuscated: true,
        dc_id: 3,
        proxy: ProxyData::http("127.0.0.1", proxy_addr.port(), "bob", "pw"),
        ..Default::default()
    };
    let mut conn = Connection::connect(&target.ip().to_string(), target.port(), cfg)
        .await
        .unwrap();
    let msg = b"hello via http connect proxy!!!!";
    conn.send(msg).await.unwrap();
    assert_eq!(conn.recv().await.unwrap(), msg);

    // Wrong credentials -> ProxyAuthFailed.
    let cfg_bad = ConnectionConfig {
        dc_id: 3,
        proxy: ProxyData::http("127.0.0.1", proxy_addr.port(), "bob", "nope"),
        ..Default::default()
    };
    assert!(matches!(
        Connection::connect(&target.ip().to_string(), target.port(), cfg_bad)
            .await
            .err(),
        Some(Error::ProxyAuthFailed)
    ));
}

#[tokio::test]
async fn plain_http_transport_post_and_chunked() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req_num = 0usize;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let Ok(n) = sock.read(&mut tmp).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let pos = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let headers = String::from_utf8_lossy(&buf[..pos]).into_owned();
            let content_len: usize = headers
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.eq_ignore_ascii_case("content-length") {
                        v.trim().parse().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            buf.drain(..pos + 4);
            while buf.len() < content_len {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body: Vec<u8> = buf.drain(..content_len).collect();
            req_num += 1;

            if req_num == 1 {
                // Reply with Content-Length.
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
                sock.write_all(&body).await.unwrap();
            } else if req_num == 2 {
                // Reply with chunked Transfer-Encoding.
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
                sock.write_all(&body).await.unwrap();
                sock.write_all(b"\r\n0\r\n\r\n").await.unwrap();
            } else {
                // Reply with HTTP 404 -> TransportErrorCode::AuthKeyNotFound (-404).
                sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
            }
        }
    });

    let mut http = HttpConnection::new(
        "127.0.0.1",
        HttpConnectionConfig {
            port: addr.port(),
            ..Default::default()
        },
    );
    let p1 = b"http-mtproto-request-one";
    assert_eq!(http.post(p1).await.unwrap(), p1);

    let p2 = b"http-mtproto-chunked-two";
    assert_eq!(http.post(p2).await.unwrap(), p2);

    let err = http.post(b"trigger-404!").await.unwrap_err();
    assert!(matches!(
        err,
        Error::ServerError(TransportErrorCode::AuthKeyNotFound)
    ));
}

#[tokio::test]
async fn connection_racing_prefers_higher_priority_and_falls_back() {
    assert_eq!(compute_priority(false, false, true, false), 2);
    assert_eq!(compute_priority(false, false, false, false), 1);
    assert_eq!(compute_priority(true, false, true, false), 1);
    assert_eq!(compute_priority(true, true, true, true), 4);

    let fast_low_server = spawn_mock_mtproto_echo_server(2).await;
    let fast_high_server = spawn_mock_mtproto_echo_server(2).await;

    // Scenario A: Both connect; candidate 1 has higher priority and wins.
    let candidates = vec![
        RaceCandidate {
            endpoint: tg_transport::RaceEndpoint::Tcp {
                address: "127.0.0.1".into(),
                port: fast_low_server.port(),
                config: ConnectionConfig::default(),
            },
            priority: 1,
            probe_pq: false,
        },
        RaceCandidate {
            endpoint: tg_transport::RaceEndpoint::Tcp {
                address: "127.0.0.1".into(),
                port: fast_high_server.port(),
                config: ConnectionConfig::default(),
            },
            priority: 3,
            probe_pq: false,
        },
    ];
    let winner = race_connections(
        candidates,
        RaceOptions {
            wait_for_better: Duration::from_millis(400),
            total_timeout: Duration::from_secs(3),
            probe_timeout: Duration::from_secs(2),
        },
    )
    .await
    .unwrap();
    assert_eq!(winner.candidate_index, 1);
    assert_eq!(winner.priority, 3);
    assert!(matches!(winner.transport, RacedTransport::Tcp(_)));

    // Scenario B: High-priority candidate fails (closed port), lower-priority candidate wins immediately.
    let closed_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);

    let candidates_b = vec![
        RaceCandidate {
            endpoint: tg_transport::RaceEndpoint::Tcp {
                address: "127.0.0.1".into(),
                port: fast_low_server.port(),
                config: ConnectionConfig::default(),
            },
            priority: 1,
            probe_pq: false,
        },
        RaceCandidate {
            endpoint: tg_transport::RaceEndpoint::Tcp {
                address: "127.0.0.1".into(),
                port: closed_port,
                config: ConnectionConfig {
                    connect_timeout: Duration::from_millis(300),
                    ..Default::default()
                },
            },
            priority: 5,
            probe_pq: false,
        },
    ];
    let winner_b = race_connections(
        candidates_b,
        RaceOptions {
            wait_for_better: Duration::from_millis(500),
            total_timeout: Duration::from_secs(3),
            probe_timeout: Duration::from_secs(2),
        },
    )
    .await
    .unwrap();
    assert_eq!(winner_b.candidate_index, 0);
}

// ---------------------------------------------------------------------------
// 6. Live anonymous DC2 network test (#[ignore] by default)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "connects to real Telegram DC2 (149.154.167.50:443) for anonymous req_pq_multi"]
async fn live_dc2_intermediate_obfuscated_req_pq_multi() {
    let cfg = ConnectionConfig {
        transport: TransportKind::Intermediate,
        obfuscated: true,
        dc_id: 2,
        ..Default::default()
    };
    let mut conn = Connection::connect("149.154.167.50", 443, cfg)
        .await
        .expect("failed to connect to DC2 149.154.167.50:443");

    let nonce = [
        0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98, 0xA9, 0xBA, 0xCB, 0xDC, 0xED, 0xFE,
        0x0F,
    ];
    let req = tg_transport::build_unencrypted_req_pq_multi(nonce);
    assert_eq!(
        u32::from_le_bytes([req[20], req[21], req[22], req[23]]),
        CONSTRUCTOR_REQ_PQ_MULTI
    );

    conn.send(&req).await.expect("send req_pq_multi failed");
    let reply = conn.recv().await.expect("recv resPQ failed");

    let constructor = u32::from_le_bytes([reply[20], reply[21], reply[22], reply[23]]);
    assert_eq!(constructor, CONSTRUCTOR_RES_PQ);

    let parsed = parse_unencrypted_res_pq(&reply).expect("failed to parse resPQ");
    assert_eq!(parsed.nonce, nonce);
}
