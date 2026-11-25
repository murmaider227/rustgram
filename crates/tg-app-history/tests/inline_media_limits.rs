//! Regression tests for SEC-C3: inline photo decode enforces pixel-area limits.

use tg_app_history::history::inline_media::decode_inline_photo_rgba;

fn encode_1bit_gray_png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::One);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().expect("write png header");
        let row_bytes = (width as usize).div_ceil(8);
        let row = vec![0u8; row_bytes];
        let mut stream = writer.stream_writer().expect("png stream writer");
        use std::io::Write as _;
        for _ in 0..height {
            stream.write_all(&row).expect("write png row");
        }
        stream.finish().expect("finish png stream");
    }
    out
}

#[test]
fn inline_photo_decode_rejects_oversized_pixel_area_and_accepts_valid_png() {
    // Valid small PNG decodes cleanly.
    let small = encode_1bit_gray_png(16, 12);
    let (w, h, rgba) = decode_inline_photo_rgba(&small).expect("small PNG should decode");
    assert_eq!((w, h), (16, 12));
    assert_eq!(rgba.len(), 16 * 12 * 4);

    // 11_000 x 11_000 = 121_000_000 pixels > READ_MAX_AREA (12_032 * 9_024 = 108_576_768).
    // As a 1-bit grayscale PNG it fits in ~20 KiB and stays below image::Limits' 512 MiB
    // Luma8 decode cap, so raw `image::load_from_memory` would decode it; `read_image`
    // rejects it at the header dimension check before allocating pixels.
    let oversized = encode_1bit_gray_png(11_000, 11_000);
    assert!(
        decode_inline_photo_rgba(&oversized).is_none(),
        "oversized PNG exceeding READ_MAX_AREA must be rejected"
    );
}
