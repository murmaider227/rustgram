//! Panic fuzz over call packets and signaling from the peer, relays and SFUs (TGB-3065).

use tg_fuzz::Fuzz;

use crate::crypto::{EncryptedConnection, TransportMessage};
use crate::rtp::{RtpHeader, VoipPacket};
use crate::signaling::SignalingMessage;
use crate::turn::SctpChunk;

const ITERATIONS: usize = 3_000;

fn auth_key() -> [u8; crate::crypto::AUTH_KEY_SIZE] {
    std::array::from_fn(|i| i as u8)
}

fn seed_bodies() -> Vec<Vec<u8>> {
    let messages = [
        TransportMessage::AudioData(vec![1, 2, 3, 4]),
        TransportMessage::VideoData(vec![5; 40]),
        TransportMessage::UnstructuredData(b"{\"a\":1}".to_vec()),
        TransportMessage::RemoteMediaState {
            audio_active: true,
            video_state: 2,
        },
        TransportMessage::VideoParameters { aspect_ratio: 1777 },
        TransportMessage::RemoteBatteryLow(true),
        TransportMessage::RemoteNetworkStatus {
            is_low_cost: true,
            is_low_data_requested: false,
        },
    ];
    let mut sender = EncryptedConnection::new_transport(auth_key(), true);
    let mut out = Vec::new();
    for (i, message) in messages.iter().enumerate() {
        let Ok(packet) = sender.prepare_for_sending(message, i as u64) else {
            continue;
        };
        let mut reader = EncryptedConnection::new_transport(auth_key(), false);
        if let Ok(body) = reader.decrypt_raw_packet(&packet.bytes) {
            out.push(body);
        }
    }
    if let Ok(packet) = sender.prepare_for_sending_raw_message(b"raw payload", true, 9) {
        let mut reader = EncryptedConnection::new_transport(auth_key(), false);
        if let Ok(body) = reader.decrypt_raw_packet(&packet.bytes) {
            out.push(body);
        }
    }
    assert!(!out.is_empty(), "seed packets must decrypt");
    out
}

#[test]
fn decrypted_transport_packets_do_not_panic() {
    let mut fuzz = Fuzz::new(0x7015_0001, ITERATIONS);
    let seeds = seed_bodies();
    let mut accepted = 0usize;
    for i in 0..fuzz.iterations() {
        let body = if fuzz.rng().one_in(5) {
            fuzz.bytes(64)
        } else {
            let seed = fuzz.rng().below(seeds.len());
            fuzz.mutate(&seeds[seed])
        };
        fuzz.check("encrypted transport", i, &body, || {
            let mut sender = EncryptedConnection::new_transport(auth_key(), true);
            let Ok(packet) = sender.encrypt_raw_packet(&body) else {
                return;
            };
            let mut receiver = EncryptedConnection::new_transport(auth_key(), false);
            if receiver.handle_incoming_packet(&packet).is_ok() {
                accepted += 1;
            }
            let mut receiver = EncryptedConnection::new_transport(auth_key(), false);
            let _ = receiver.handle_incoming_raw_packet(&packet);
            let _ = receiver.handle_incoming_packet(&body);
        });
    }
    assert!(accepted > 0, "no fuzzed packet reached the message parser");
    fuzz.finish();
}

#[test]
fn wire_packets_do_not_panic() {
    let mut fuzz = Fuzz::new(0x7015_0002, ITERATIONS * 4);
    let stun_like: Vec<u8> = {
        let mut v = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
        v.extend_from_slice(&[7; 12]);
        v.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x12, 0x34, 1, 2, 3, 4]);
        v
    };
    let rtp_like: Vec<u8> = {
        let mut v = vec![
            0x90, 111, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0xbe, 0xde, 0, 1, 0x10, 0x85, 0, 0,
        ];
        v.extend_from_slice(&[9; 20]);
        v
    };
    let prefix = [3u8; 12];
    for i in 0..fuzz.iterations() {
        let data = match fuzz.rng().below(4) {
            0 => fuzz.mutate(&stun_like),
            1 => fuzz.mutate(&rtp_like),
            2 => {
                let mut v = prefix.to_vec();
                v.extend(fuzz.bytes(40));
                v
            }
            _ => fuzz.bytes(80),
        };
        fuzz.check("wire parsers", i, &data, || {
            let _ = RtpHeader::parse(&data);
            let _ = VoipPacket::parse(&data);
            let _ = crate::rtp::parse_rtp_audio_level_ext(&data, 1);
            let _ = SctpChunk::parse(&data);
            let _ = crate::turn::decode_turn_channel_data(&data);
            let _ = crate::turn::parse_turn_message(&data, None);
            let _ = crate::turn::parse_turn_message(&data, Some(b"key"));
            let _ = crate::reflector::parse_stun_binding_response(&data);
            let _ = crate::reflector::decode_reflector_udp_packet(&prefix, &data);
            let _ = SignalingMessage::parse(&data);
        });
    }
    fuzz.finish();
}

#[test]
fn signaling_json_and_candidates_do_not_panic() {
    const TOKENS: &[&str] = &[
        "{",
        "}",
        "[",
        "]",
        ":",
        ",",
        "\"",
        "\"@type\"",
        "\"InitialSetup\"",
        "\"Candidates\"",
        "\"MediaState\"",
        "\"ufrag\"",
        "\"pwd\"",
        "\"fingerprints\"",
        "\"audio\"",
        "\"video\"",
        "\"ssrc\"",
        "\"ssrcGroups\"",
        "\"payloadTypes\"",
        "\"id\"",
        "\"name\"",
        "\"clockrate\"",
        "\"channels\"",
        "\"parameters\"",
        "\"feedbackTypes\"",
        "\"rtpExtensions\"",
        "\"uri\"",
        "\"candidates\"",
        "\"sdpString\"",
        "\"muted\"",
        "\"videoState\"",
        "\"lowBattery\"",
        "\"4294967296\"",
        "-1",
        "1e9",
        "null",
        "true",
        "candidate:1 1 udp 2122 1.2.3.4 50000 typ host",
        "a=",
        "typ",
        "relay",
        "raddr",
        "rport",
        "tcp",
        "65536",
        "::1",
        "[",
        "reflector-",
        ".",
    ];
    let mut fuzz = Fuzz::new(0x7015_0003, ITERATIONS * 4);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(TOKENS, 24);
        fuzz.check("signaling strings", i, &s, || {
            let _ = SignalingMessage::parse(s.as_bytes());
            let _ = crate::reflector::parse_sdp_candidate(&s);
            let _ = crate::reflector::parse_reflector_hostname(&s);
            let _ = crate::reflector::decode_hex_16(&s);
        });
    }
    fuzz.finish();
}

#[test]
fn reflector_peer_tag_with_multibyte_chars_is_rejected() {
    // Old code sliced `hex[i * 2..i * 2 + 2]` on any 32-byte string; the tag comes from the
    // server's RtcServer password.
    let tag = format!("{}\u{e9}a", "a".repeat(29));
    assert_eq!(tag.len(), 32);
    assert!(crate::reflector::decode_hex_16(&tag).is_err());
}
