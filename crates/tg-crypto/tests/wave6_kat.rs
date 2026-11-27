//! Known-answer tests pinning the exact MTProto 2.0 packet bytes before the single-buffer
//! encrypt/decrypt rewrite (TGB-3059). The digests were recorded from the previous
//! implementation; any change in output bytes fails here.

use tg_crypto::{
    AuthKey, Side, decrypt_data_v2, decrypt_message_v2, encrypt_data_v2,
    encrypt_data_v2_with_padding, encrypt_packet_v2_in_place, sha256,
};

fn test_key() -> AuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    AuthKey::new(data)
}

fn inner_plaintext(body_len: usize) -> Vec<u8> {
    let mut pt = Vec::with_capacity(32 + body_len);
    pt.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    pt.extend_from_slice(&0x7766_5544_3322_1100u64.to_le_bytes());
    pt.extend_from_slice(&0x6710_0000_0000_0004u64.to_le_bytes());
    pt.extend_from_slice(&3u32.to_le_bytes());
    pt.extend_from_slice(&(body_len as u32).to_le_bytes());
    pt.extend((0..body_len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(i as u8 >> 3)));
    pt
}

fn padding_for(plain_len: usize, extra_blocks: usize) -> Vec<u8> {
    let rem = plain_len % 16;
    let mut pad = if rem == 0 { 0 } else { 16 - rem };
    if pad < 12 {
        pad += 16;
    }
    pad += extra_blocks * 16;
    (0..pad)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(5))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `(body_len, extra padding blocks, side, sha256 of the wire packet)`.
const CASES: &[(usize, usize, Side, &str)] = &[
    (
        4,
        0,
        Side::Client,
        "ec252852e8e5bcf589da5b30c425d05a26fdbe74d13275180cd021d9224f0d61",
    ),
    (
        12,
        15,
        Side::Server,
        "8a4f81280550e980e1a55ac161baeeb3128b716e1fa3fa321156be0dfa59cb2d",
    ),
    (
        1000,
        3,
        Side::Client,
        "04a244826830f02376d4fff40ced183311e114f21157a512dbe1b32648c01f4c",
    ),
    (
        512 * 1024,
        7,
        Side::Server,
        "2ab30b02012a90b590fe67af78807e57d4c90731e0b8380a444dfbc733b77e7b",
    ),
];

#[test]
fn encrypt_v2_wire_bytes_match_recorded_digests() {
    let key = test_key();
    for &(body_len, extra, side, digest) in CASES {
        let pt = inner_plaintext(body_len);
        let pad = padding_for(pt.len(), extra);
        let wire = encrypt_data_v2_with_padding(&pt, &pad, &key, side).unwrap();
        assert_eq!(wire.len(), 24 + pt.len() + pad.len());
        assert_eq!(
            hex(&sha256(&[&wire])),
            digest,
            "wire digest changed for body_len={body_len} extra={extra} side={side:?}"
        );

        let mut in_place = vec![0u8; 24];
        in_place.extend_from_slice(&pt);
        in_place.extend_from_slice(&pad);
        encrypt_packet_v2_in_place(&mut in_place, &key, side).unwrap();
        assert_eq!(
            in_place, wire,
            "in-place encryption differs (body_len={body_len})"
        );
    }
}

#[test]
fn decrypt_v2_recovers_exact_plaintext_and_fields() {
    let key = test_key();
    for &(body_len, extra, side, _) in CASES {
        let pt = inner_plaintext(body_len);
        let pad = padding_for(pt.len(), extra);
        let wire = encrypt_data_v2_with_padding(&pt, &pad, &key, side).unwrap();

        let decrypted = decrypt_data_v2(&wire, &key, side).unwrap();
        let mut expected = pt.clone();
        expected.extend_from_slice(&pad);
        assert_eq!(decrypted, expected);

        let msg = decrypt_message_v2(&wire, &key, side).unwrap();
        assert_eq!(msg.salt, 0x1122_3344_5566_7788);
        assert_eq!(msg.session_id, 0x7766_5544_3322_1100);
        assert_eq!(msg.msg_id, 0x6710_0000_0000_0004);
        assert_eq!(msg.seq_no, 3);
        assert_eq!(msg.body, pt[32..]);
    }
}

#[test]
fn decrypt_v2_rejects_bad_packets_with_the_same_errors() {
    use tg_crypto::CryptoError;
    let key = test_key();
    let pt = inner_plaintext(64);
    let pad = padding_for(pt.len(), 1);
    let wire = encrypt_data_v2_with_padding(&pt, &pad, &key, Side::Server).unwrap();
    let flip = |at: usize| {
        let mut w = wire.clone();
        w[at] ^= 1;
        w
    };
    let cases: Vec<(Vec<u8>, CryptoError)> = vec![
        (
            Vec::new(),
            CryptoError::InvalidLength("MTProto 2.0 packet shorter than 40 bytes"),
        ),
        (
            wire[..39].to_vec(),
            CryptoError::InvalidLength("MTProto 2.0 packet shorter than 40 bytes"),
        ),
        (
            wire[..wire.len() - 1].to_vec(),
            CryptoError::InvalidBlockLength {
                len: wire.len() - 25,
            },
        ),
        (flip(0), CryptoError::AuthKeyIdMismatch),
        (flip(10), CryptoError::MsgKeyMismatch),
        (flip(wire.len() - 1), CryptoError::MsgKeyMismatch),
    ];
    for (bad, expected) in cases {
        assert_eq!(
            decrypt_data_v2(&bad, &key, Side::Server),
            Err(expected.clone())
        );
        assert_eq!(decrypt_message_v2(&bad, &key, Side::Server), Err(expected));
    }
    assert_eq!(
        decrypt_message_v2(&wire, &key, Side::Client),
        Err(CryptoError::MsgKeyMismatch)
    );
}

#[test]
fn encrypt_v2_random_padding_stays_in_protocol_range() {
    let key = test_key();
    for body_len in (4..=96).step_by(4) {
        for _ in 0..8 {
            let pt = inner_plaintext(body_len);
            let wire = encrypt_data_v2(&pt, &key, Side::Client);
            let padding = wire.len() - 24 - pt.len();
            assert!(
                (12..=267).contains(&padding),
                "padding {padding} for body {body_len}"
            );
            assert_eq!((pt.len() + padding) % 16, 0);
            let msg = decrypt_message_v2(&wire, &key, Side::Client).unwrap();
            assert_eq!(msg.body, pt[32..]);
        }
    }
}
