//! Wave 6 (TGB-3064): Lottie frame cache header validation, context reuse, and mutated blobs.

use std::time::Instant;

use tg_lottie::{Cache, CacheReadContext, FrameBuffer, FrameRenderResult, FrameRequest, Size};

const FRAMES: u32 = 6;

/// Premultiplied test frame that changes a little between frames (so XOR deltas are used).
fn frame(size: Size, index: u32) -> FrameBuffer {
    let mut f = FrameBuffer::new(size);
    let w = size.width as usize;
    for (i, px) in f.data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        let a = if (x + y) % 5 == 0 { 128u8 } else { 255 };
        let r = ((x * 255) / w) as u8;
        let g = ((y * 7 + index as usize * 13) % 256) as u8;
        let b = ((x ^ y) as u8).wrapping_add(index as u8);
        let pm = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        *px = [pm(r), pm(g), pm(b), a];
    }
    f
}

/// Encodes `FRAMES` frames of a 32x32 animation for `request` and returns the serialized blob.
fn encoded_blob(original: Size, request: FrameRequest) -> Vec<u8> {
    let mut cache = Cache::new(Vec::new(), request, None);
    cache.init(original, 30, FRAMES, request);
    let size = cache_size(original, request);
    for i in 0..FRAMES {
        cache.append_frame(&frame(size, i), request, i);
    }
    cache.finalize_encoding();
    cache.serialized_bytes().to_vec()
}

fn cache_size(original: Size, request: FrameRequest) -> Size {
    if request.is_empty() {
        Size::new(original.width & !7, original.height & !7)
    } else {
        request.size(original, 8)
    }
}

fn header_i32(blob: &[u8], index: usize) -> i32 {
    let at = index * 4;
    i32::from_be_bytes([blob[at], blob[at + 1], blob[at + 2], blob[at + 3]])
}

fn set_header_i32(blob: &mut [u8], index: usize, value: i32) {
    blob[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn blob_round_trips_with_and_without_request() {
    for request in [
        FrameRequest::default(),
        FrameRequest::new(Size::new(32, 32)),
    ] {
        let blob = encoded_blob(Size::new(32, 32), request);
        let mut cache = Cache::new(blob, request, None);
        assert_eq!(cache.frames_ready(), FRAMES, "request {request:?}");
        let mut dst = FrameBuffer::new(Size::new(32, 32));
        for i in 0..FRAMES {
            assert_eq!(
                cache.render_frame(&mut dst, request, i),
                FrameRenderResult::Ok
            );
        }
    }
}

/// The cached frame size is stored separately from the original size. tdesktop rejects a header
/// whose cached size is not the one the request derives from the original; without a request the
/// port accepted any size, so a corrupt header could make `allocate` reserve gigabytes.
#[test]
fn header_with_cached_size_not_derived_from_original_is_rejected() {
    let request = FrameRequest::default();
    let mut blob = encoded_blob(Size::new(32, 32), request);
    assert_eq!((header_i32(&blob, 1), header_i32(&blob, 2)), (32, 32));
    // Claim a 16x16 original: a 32x32 cached frame cannot come from it.
    set_header_i32(&mut blob, 3, 16);
    set_header_i32(&mut blob, 4, 16);
    let cache = Cache::new(blob, request, None);
    assert_eq!(cache.frames_ready(), 0);
}

#[test]
fn keep_up_context_matches_primary_context() {
    let request = FrameRequest::new(Size::new(32, 32));
    let blob = encoded_blob(Size::new(32, 32), request);
    let mut cache = Cache::new(blob, request, None);
    let mut primary = FrameBuffer::new(Size::new(32, 32));
    let mut secondary = FrameBuffer::new(Size::new(32, 32));
    let mut context = CacheReadContext::default();
    for i in 0..FRAMES {
        // The secondary context catches up with the primary one, then both render frame i + 1.
        cache.keep_up_context(&mut context);
        let next = (i + 1) % FRAMES;
        let a = cache.render_frame_with_context(&mut context, &mut secondary, request, next);
        let b = cache.render_frame(&mut primary, request, i);
        assert_eq!(b, FrameRenderResult::Ok);
        assert_eq!(a, FrameRenderResult::Ok);
    }
    // After the loop both renderers decoded the same final frame sequence.
    cache.keep_up_context(&mut context);
    let mut again = FrameBuffer::new(Size::new(32, 32));
    assert_eq!(
        cache.render_frame_with_context(&mut context, &mut again, request, 0),
        FrameRenderResult::Ok
    );
    assert_eq!(
        cache.render_frame(&mut primary, request, 0),
        FrameRenderResult::Ok
    );
    assert_eq!(again.data, primary.data);
}

#[test]
fn mutated_blobs_never_panic() {
    let request = FrameRequest::new(Size::new(32, 32));
    let seed = encoded_blob(Size::new(32, 32), request);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut dst = FrameBuffer::new(Size::new(32, 32));
    for round in 0..3000u32 {
        let mut blob = seed.clone();
        match round % 3 {
            0 => {
                let at = (next() as usize) % blob.len();
                blob[at] ^= (next() as u8) | 1;
            }
            1 => {
                let keep = (next() as usize) % blob.len();
                blob.truncate(keep);
            }
            _ => {
                // Header fields: random values in each.
                let field = (next() as usize) % 8;
                set_header_i32(&mut blob, field, next() as i32);
            }
        }
        let mut cache = Cache::new(blob, request, None);
        for i in 0..FRAMES {
            let _ = cache.render_frame(&mut dst, request, i);
        }
    }
}

/// Prints the per-frame cost of replaying the cache (decompress + XOR + YUV decode) and of
/// `keep_up_context`; numbers go to the TGB-3064 report, the test asserts nothing about time.
#[test]
fn cache_replay_timing() {
    let request = FrameRequest::new(Size::new(512, 512));
    let original = Size::new(512, 512);
    let mut cache = Cache::new(Vec::new(), request, None);
    cache.init(original, 60, 60, request);
    for i in 0..60 {
        cache.append_frame(&frame(original, i), request, i);
    }
    cache.finalize_encoding();
    let blob = cache.serialized_bytes().to_vec();
    let mut cache = Cache::new(blob, request, None);
    let mut dst = FrameBuffer::new(original);
    let rounds = 10u32;
    let start = Instant::now();
    for _ in 0..rounds {
        for i in 0..60 {
            assert_eq!(
                cache.render_frame(&mut dst, request, i),
                FrameRenderResult::Ok
            );
        }
    }
    let per_frame = start.elapsed() / (rounds * 60);
    let mut context = CacheReadContext::default();
    cache.keep_up_context(&mut context);
    let start = Instant::now();
    for _ in 0..600 {
        cache.keep_up_context(&mut context);
    }
    let keep_up = start.elapsed() / 600;
    println!("lottie cache 512x512: render_frame {per_frame:?}/frame, keep_up_context {keep_up:?}");
}
