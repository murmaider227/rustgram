//! Focused timings for the send/receive hot path (TGB-3059). Ignored by default; run with
//! `cargo test -p tg-mtproto --test wave6_timing -- --ignored --nocapture`.
//! Prints numbers only; it asserts nothing about speed.

use std::hint::black_box;
use std::time::{Duration, Instant};

use tg_crypto::{AuthKey, Side};
use tg_mtproto::message::{build_encrypted_packet, gzip_pack_if_smaller, parse_encrypted_packet};

fn key() -> AuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(29).wrapping_add(11);
    }
    AuthKey::new(data)
}

/// Pseudo-random bytes (incompressible, like an already-compressed media file part).
fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn time_per_iter(iters: u32, mut f: impl FnMut()) -> Duration {
    f();
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    start.elapsed() / iters
}

#[test]
#[ignore = "timing report, run manually"]
fn send_receive_hot_path_timings() {
    let key = key();
    for &len in &[256usize, 16 * 1024, 512 * 1024] {
        let body = noise(len);
        let iters = if len > 100_000 { 40 } else { 2_000 };
        let build = time_per_iter(iters, || {
            black_box(build_encrypted_packet(
                1,
                2,
                0x6710_0000_0000_0004,
                3,
                black_box(&body),
                &key,
                Side::Client,
            ));
        });
        let server_wire =
            build_encrypted_packet(1, 2, 0x6710_0000_0000_0005, 3, &body, &key, Side::Server);
        let parse = time_per_iter(iters, || {
            black_box(parse_encrypted_packet(black_box(&server_wire), &key, Side::Server).ok());
        });
        println!(
            "TIMING body={len:>7} build_encrypted_packet={build:?} parse_encrypted_packet={parse:?}"
        );
    }
}

#[test]
#[ignore = "timing report, run manually"]
fn outgoing_gzip_timings() {
    let random_part = noise(512 * 1024);
    let gz = time_per_iter(20, || {
        black_box(gzip_pack_if_smaller(black_box(&random_part)));
    });
    println!(
        "TIMING gzip_pack_if_smaller incompressible 512 KiB: {gz:?} (out {} bytes)",
        gzip_pack_if_smaller(&random_part).len()
    );
    let text: Vec<u8> = b"Telegram desktop port message text "
        .iter()
        .copied()
        .cycle()
        .take(4096)
        .collect();
    let gz_text = time_per_iter(2_000, || {
        black_box(gzip_pack_if_smaller(black_box(&text)));
    });
    println!(
        "TIMING gzip_pack_if_smaller text 4 KiB: {gz_text:?} (out {} bytes)",
        gzip_pack_if_smaller(&text).len()
    );
}

/// TGB-3074: 256-byte packets, where hashing the 256-byte key for `auth_key_id` on every
/// packet was a visible share of the work. Best of five runs to cut scheduler noise.
#[test]
#[ignore = "timing report, run manually"]
fn small_packet_key_id_timings() {
    let key = key();
    let body = noise(256);
    let server_wire =
        build_encrypted_packet(1, 2, 0x6710_0000_0000_0005, 3, &body, &key, Side::Server);
    let best = |f: &mut dyn FnMut()| {
        (0..5)
            .map(|_| time_per_iter(20_000, &mut *f))
            .min()
            .unwrap_or_default()
    };
    let key_id = best(&mut || {
        black_box(black_box(&key).key_id());
    });
    let build = best(&mut || {
        black_box(build_encrypted_packet(
            1,
            2,
            0x6710_0000_0000_0004,
            3,
            black_box(&body),
            &key,
            Side::Client,
        ));
    });
    let parse = best(&mut || {
        black_box(parse_encrypted_packet(black_box(&server_wire), &key, Side::Server).ok());
    });
    println!(
        "TIMING body=256 key_id={key_id:?} build_encrypted_packet={build:?} parse_encrypted_packet={parse:?}"
    );
}
