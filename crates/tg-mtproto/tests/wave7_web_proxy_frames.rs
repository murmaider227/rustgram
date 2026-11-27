//! WebSocket frame decoders of the web-proxy bridge over buffers of many small frames
//! (TGB-3073): the result and the drained prefix must match frame-by-frame decoding.
//! The timing report is ignored by default; run it with
//! `cargo test -p tg-mtproto --test wave7_web_proxy_frames -- --ignored --nocapture`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use tg_mtproto::web_proxy::WebSocketDecoder;
use tg_mtproto::{
    WebProxyError, WebSocketMessage, decode_server_websocket_frames,
    decode_websocket_client_frames, encode_masked_websocket_frame,
};

const MASK: [u8; 4] = [0x37, 0xFA, 0x21, 0x3D];

fn payload(i: usize) -> Vec<u8> {
    (0..8).map(|j| (i * 31 + j) as u8).collect()
}

/// `count` masked client binary frames of 8 bytes each.
fn client_wire(count: usize) -> Vec<u8> {
    (0..count)
        .flat_map(|i| encode_masked_websocket_frame(0x02, &payload(i), MASK))
        .collect()
}

/// `count` unmasked server binary frames of 8 bytes each.
fn server_wire(count: usize) -> Vec<u8> {
    (0..count)
        .flat_map(|i| {
            let mut frame = vec![0x82, 8];
            frame.extend_from_slice(&payload(i));
            frame
        })
        .collect()
}

fn expected(count: usize) -> Vec<WebSocketMessage> {
    (0..count)
        .map(|i| WebSocketMessage::Binary(payload(i)))
        .collect()
}

#[test]
fn client_decoder_reads_many_small_frames_and_keeps_the_partial_tail() {
    let count = 5_000;
    let mut wire = client_wire(count);
    let partial = encode_masked_websocket_frame(0x02, &[1, 2, 3, 4, 5, 6, 7, 8], MASK);
    let tail = &partial[..partial.len() - 3];
    wire.extend_from_slice(tail);

    let messages = decode_websocket_client_frames(&mut wire).expect("decode");
    assert_eq!(messages, expected(count));
    assert_eq!(wire, tail, "only the incomplete frame stays in the buffer");

    // The rest of the frame arrives in the next read.
    wire.extend_from_slice(&partial[partial.len() - 3..]);
    let messages = decode_websocket_client_frames(&mut wire).expect("decode tail");
    assert_eq!(
        messages,
        vec![WebSocketMessage::Binary(vec![1, 2, 3, 4, 5, 6, 7, 8])]
    );
    assert!(wire.is_empty());
}

#[test]
fn client_decoder_keeps_fragment_state_across_frames_in_one_read() {
    let mut wire = encode_masked_websocket_frame(0x02, b"abc", MASK);
    // Clear FIN on the first fragment, then continuation frames; the last one has FIN.
    wire[0] = 0x02;
    let mut middle = encode_masked_websocket_frame(0x00, b"def", MASK);
    middle[0] = 0x00;
    wire.extend_from_slice(&middle);
    wire.extend_from_slice(&encode_masked_websocket_frame(0x00, b"gh", MASK));
    wire.extend_from_slice(&encode_masked_websocket_frame(0x09, b"p", MASK));

    let mut decoder = WebSocketDecoder::default();
    let messages = decoder.decode(&mut wire).expect("decode");
    assert_eq!(
        messages,
        vec![
            WebSocketMessage::Binary(b"abcdefgh".to_vec()),
            WebSocketMessage::Ping(b"p".to_vec()),
        ]
    );
    assert!(wire.is_empty());
}

#[test]
fn client_decoder_error_drains_the_frames_read_before_it() {
    let mut wire = client_wire(3);
    let bad_start = wire.len();
    // An unmasked client frame is a protocol error.
    wire.extend_from_slice(&[0x82, 0x01, 0xAA]);
    let err = decode_websocket_client_frames(&mut wire).expect_err("unmasked frame");
    assert!(matches!(err, WebProxyError::WebSocket { code: 1002, .. }));
    assert_eq!(
        wire.len(),
        3,
        "the three good frames ({bad_start} bytes) are consumed"
    );
    assert_eq!(wire, [0x82, 0x01, 0xAA]);

    // A 1-byte close payload fails after its frame was consumed.
    let mut wire = client_wire(2);
    wire.extend_from_slice(&encode_masked_websocket_frame(0x08, &[7], MASK));
    wire.extend_from_slice(&client_wire(1));
    let err = decode_websocket_client_frames(&mut wire).expect_err("1-byte close");
    assert!(matches!(err, WebProxyError::WebSocket { code: 1002, .. }));
    assert_eq!(wire, client_wire(1));
}

#[test]
fn client_decoder_stops_after_close() {
    let mut wire = client_wire(2);
    wire.extend_from_slice(&encode_masked_websocket_frame(0x08, &[0x03, 0xE8], MASK));
    wire.extend_from_slice(&client_wire(1));
    let messages = decode_websocket_client_frames(&mut wire).expect("decode");
    let mut want = expected(2);
    want.push(WebSocketMessage::Close(vec![0x03, 0xE8]));
    assert_eq!(messages, want);
    assert_eq!(wire, client_wire(1), "frames after Close stay unread");
}

#[test]
fn server_decoder_reads_many_small_frames_and_keeps_the_partial_tail() {
    let count = 5_000;
    let mut wire = server_wire(count);
    wire.extend_from_slice(&[0x82, 8, 1, 2]);
    let messages = decode_server_websocket_frames(&mut wire).expect("decode");
    assert_eq!(messages, expected(count));
    assert_eq!(wire, [0x82, 8, 1, 2]);
}

#[test]
fn server_decoder_error_drains_the_frames_read_before_it() {
    let mut wire = server_wire(4);
    // A masked server frame is a protocol error.
    wire.extend_from_slice(&[0x82, 0x81, 0, 0, 0, 0, 1]);
    let err = decode_server_websocket_frames(&mut wire).expect_err("masked server frame");
    assert!(matches!(err, WebProxyError::WebSocket { code: 1002, .. }));
    assert_eq!(wire, [0x82, 0x81, 0, 0, 0, 0, 1]);

    // An unknown opcode fails after its frame was consumed.
    let mut wire = server_wire(1);
    wire.extend_from_slice(&[0x83, 0]);
    wire.extend_from_slice(&server_wire(1));
    let err = decode_server_websocket_frames(&mut wire).expect_err("bad opcode");
    assert!(matches!(err, WebProxyError::WebSocket { code: 1002, .. }));
    assert_eq!(wire, server_wire(1));
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
fn many_small_frames_decode_timings() {
    for &count in &[1_000usize, 10_000, 50_000] {
        let client = client_wire(count);
        let server = server_wire(count);
        let iters = if count > 20_000 { 5 } else { 50 };
        let client_time = time_per_iter(iters, || {
            let mut wire = client.clone();
            black_box(decode_websocket_client_frames(&mut wire).ok());
        });
        let server_time = time_per_iter(iters, || {
            let mut wire = server.clone();
            black_box(decode_server_websocket_frames(&mut wire).ok());
        });
        println!(
            "TIMING frames={count:>6} (8-byte payloads, client {} B, server {} B) client_decode={client_time:?} server_decode={server_time:?}",
            client.len(),
            server.len()
        );
    }
}
