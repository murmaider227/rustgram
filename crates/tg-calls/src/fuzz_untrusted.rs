//! Panic fuzz over call signaling, group call JSON and conference blockchain data from other
//! participants and the server (TGB-3065).

use tg_fuzz::Fuzz;

use crate::group::e2e::{ChainBlock, E2eCall, GroupBroadcast};
use crate::types::UserId;

const ITERATIONS: usize = 2_000;

fn seed_blocks() -> Vec<Vec<u8>> {
    let mut call = E2eCall::new(UserId::from_tl(1));
    let mut seeds = Vec::new();
    if let Ok(block) = call.make_join_block() {
        seeds.push(block);
    }
    assert!(!seeds.is_empty(), "join block seed must build");
    seeds
}

#[test]
fn chain_blocks_and_broadcasts_do_not_panic() {
    let mut fuzz = Fuzz::new(0xCA11_0001, ITERATIONS);
    let seeds = seed_blocks();
    for i in 0..fuzz.iterations() {
        let block = if fuzz.rng().one_in(5) {
            fuzz.bytes(200)
        } else {
            let seed = fuzz.rng().below(seeds.len());
            fuzz.mutate(&seeds[seed])
        };
        let subchain = fuzz.rng().int(2) as i32;
        let index_after_last = fuzz.rng().int(3) as i32;
        fuzz.check("chain data", i, &block, || {
            let _ = ChainBlock::from_tl_bytes(&block);
            let _ = GroupBroadcast::from_tl_bytes(&block);
            let mut call = E2eCall::new(UserId::from_tl(2));
            let _ = call.apply_blocks(
                subchain,
                index_after_last,
                std::slice::from_ref(&block),
                false,
            );
            let _ = call.apply_blocks(0, 1, &[seeds[0].clone(), block.clone()], true);
            let _ = call.take_actions();
            let _ = call.emoji_fingerprint();
            let encrypt_decrypt = call.encrypt_decrypt();
            let _ = encrypt_decrypt.decrypt_frame(UserId::from_tl(1), 0, &block);
        });
    }
    fuzz.finish();
}

#[test]
fn signaling_envelopes_and_json_do_not_panic() {
    const JSON: &[&str] = &[
        "{",
        "}",
        "[",
        "]",
        ":",
        ",",
        "\"",
        "\"transport\"",
        "\"ufrag\"",
        "\"pwd\"",
        "\"fingerprints\"",
        "\"candidates\"",
        "\"video\"",
        "\"audio\"",
        "\"ssrc\"",
        "\"ssrc-groups\"",
        "\"sources\"",
        "\"semantics\"",
        "\"payload-types\"",
        "\"id\"",
        "\"name\"",
        "\"clockrate\"",
        "\"channels\"",
        "\"rtcp-fbs\"",
        "\"rtp-hdrexts\"",
        "\"uri\"",
        "\"stream\"",
        "\"rtmp\"",
        "\"groupCallMessage\"",
        "\"_\"",
        "\"random_id\"",
        "\"message\"",
        "\"text\"",
        "\"entities\"",
        "\"offset\"",
        "\"length\"",
        "\"type\"",
        "-1",
        "4294967296",
        "1e999",
        "null",
        "true",
        "\"\u{e9}\"",
    ];
    let mut fuzz = Fuzz::new(0xCA11_0002, ITERATIONS * 5);
    let key: [u8; crate::signaling::AUTH_KEY_SIZE] = std::array::from_fn(|i| (i * 7) as u8);
    for i in 0..fuzz.iterations() {
        let json = fuzz.text(JSON, 30);
        let bytes = fuzz.bytes(120);
        fuzz.check("json and envelopes", i, &(&json, &bytes), || {
            let _ = crate::group::payload::parse_join_response(&json);
            let _ = crate::group::messages::deserialize_encrypted_message(json.as_bytes());
            let _ = crate::group::extract_conference_slug(&json);
            for x in [0, 8] {
                let _ = crate::signaling::decrypt_ctr_envelope(&key, &bytes, x);
            }
            let mut channel = crate::signaling::SignalingCipher::new(key, false);
            let _ = channel.decrypt(&bytes);
            let mut sender = crate::signaling::SignalingCipher::new(key, true);
            if let Ok(packet) = sender.encrypt(json.as_bytes()) {
                let mut receiver = crate::signaling::SignalingCipher::new(key, false);
                let _ = receiver.decrypt(&packet);
            }
        });
    }
    fuzz.finish();
}
