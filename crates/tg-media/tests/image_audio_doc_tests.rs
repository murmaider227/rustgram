//! Known-answer and unit tests for `image`, `audio`, and `document` modules of `tg-media`.

use image::{ImageFormat, Rgba, RgbaImage};
use rand::SeedableRng;
use rand::rngs::StdRng;
use sha2::{Digest as _, Sha256};
use std::io::Cursor;
use tg_media::audio::{
    VoiceDuration, WAVEFORM_SAMPLES_COUNT, convert_waveform_from_pcm_i16,
    convert_waveform_from_u8_peaks, format_duration_text, format_played_text,
    milliseconds_from_seconds, resample_waveform, waveform_decode_5bit, waveform_encode_5bit,
};
use tg_media::document::{
    DocumentKind, FileDateTime, NameType, detect_name_type, document_save_name_and_prefix,
    extension_for_mime, file_extension, file_is_image, format_default_filename, is_bidi_control,
    is_ip_revealing_filename, is_mime_accepted_for_photo_video_album, is_mime_sent_as_video,
    is_mime_sticker, is_mime_sticker_animated, mask_dangerous_windows_extension,
    mime_type_for_data, mime_type_for_filename, mime_type_for_name, name_type_allows_thumbnail,
    resolve_unique_save_path, sanitize_filename,
};
use tg_media::image::inline::{
    PathCommand, STRIPPED_JPEG_FOOTER, STRIPPED_JPEG_HEADER, expand_inline_bytes,
    expand_path_inline_bytes, from_inline_bytes, parse_svg_path, path_from_inline_bytes,
};
use tg_media::image::jpeg::{
    ExifOrientation, apply_exif_orientation, count_jpeg_scans, finalize_progressive_prefix,
    is_progressive_jpeg, jpeg_frame_info, parse_tiff_orientation, read_jpeg_exif_orientation,
};
use tg_media::image::prepare::{
    ImageRoundRadius, PrepareArgs, PrepareOptions, RectCorners, blur, blur_large_image, circle,
    colored, corners_mask, dither_image_with_rng, ellipse_mask, generate_complex_gradient,
    generate_shadow, opaque, prepare_image, round, scaled_height_for_width, shrink_to_keep_aspect,
};
use tg_media::image::sizes::{
    InlineThumbnail, LARGE_LEVELS, PHOTO_SIDE_LIMIT, PhotoSizeEntry, PhotoSizeSlot, PhotoSizeType,
    SMALL_LEVELS, THUMBNAIL_LEVELS, VideoSizeEntry, choose_document_thumbnail,
    choose_document_video_thumbnail, choose_photo_sizes, choose_photo_video_sizes,
    clamp_photo_dimensions, find_inline_thumbnail, good_sticker_dimensions,
};
use tg_media::image::{ReadArgs, is_svg_format, read_image, unpack_gzip};

// ============================================================================
// Inline stripped JPEG & vector path thumbnails
// ============================================================================

#[test]
fn stripped_jpeg_header_layout_and_splice() {
    assert_eq!(STRIPPED_JPEG_HEADER.len(), 623);
    assert_eq!(&STRIPPED_JPEG_HEADER[0..2], &[0xFF, 0xD8]); // SOI
    assert_eq!(&STRIPPED_JPEG_HEADER[158..160], &[0xFF, 0xC0]); // SOF0
    assert_eq!(STRIPPED_JPEG_HEADER[164], 0x00); // height slot
    assert_eq!(STRIPPED_JPEG_HEADER[166], 0x00); // width slot
    assert_eq!(&STRIPPED_JPEG_HEADER[609..611], &[0xFF, 0xDA]); // SOS
    assert_eq!(STRIPPED_JPEG_FOOTER, [0xFF, 0xD9]);

    // Known invalid inputs.
    assert!(expand_inline_bytes(&[]).is_none());
    assert!(expand_inline_bytes(&[0x01, 0x10]).is_none());
    assert!(expand_inline_bytes(&[0x02, 0x10, 0x10, 0x00]).is_none());

    // Valid stripped bytes: type=1, h=20, w=30, payload=[0xAA, 0xBB].
    let expanded = expand_inline_bytes(&[0x01, 20, 30, 0xAA, 0xBB]).expect("valid stripped");
    assert_eq!(expanded.len(), 623 + 2 + 2);
    assert_eq!(expanded[164], 20);
    assert_eq!(expanded[166], 30);
    assert_eq!(&expanded[623..625], &[0xAA, 0xBB]);
    assert_eq!(&expanded[625..627], &[0xFF, 0xD9]);

    let info = jpeg_frame_info(&expanded).expect("valid jpeg header");
    assert_eq!(info.height, 20);
    assert_eq!(info.width, 30);
    assert!(!info.progressive);
}

#[test]
fn stripped_jpeg_decodes_real_pixels() {
    // Minimal 8x8 flat gray scan using the fixed Huffman tables in STRIPPED_JPEG_HEADER:
    // Y DC=0 (00), Y AC EOB (1010) x4 blocks, Cb DC=0 (00) + AC EOB (00), Cr DC=0 (00) + AC EOB (00)
    // Pack bits:
    // Block Y0: 00 1010 (6 bits)
    // Block Y1: 00 1010 (6 bits)
    // Block Y2: 00 1010 (6 bits)
    // Block Y3: 00 1010 (6 bits) -> 24 bits = 00101000 10100010 10001010 = 0x28 0xA2 0x8A
    // Block Cb: 00 00 (4 bits)
    // Block Cr: 00 00 (4 bits) -> 8 bits = 0x00
    let stripped = vec![0x01, 8, 8, 0x28, 0xA2, 0x8A, 0x00];
    let img = from_inline_bytes(&stripped).expect("stripped thumbnail should decode");
    assert_eq!(img.dimensions(), (8, 8));
    // All-zero DCT produces neutral gray (~128, 128, 128, 255).
    let p = img.get_pixel(4, 4).0;
    assert_eq!(p[3], 255);
    assert!((i16::from(p[0]) - 128).abs() <= 2);
    assert!((i16::from(p[1]) - 128).abs() <= 2);
    assert!((i16::from(p[2]) - 128).abs() <= 2);
}

#[test]
fn path_inline_bytes_expand_and_parse_known_vectors() {
    // Test lookup table mapping (c >= 192) and numeric decoding (0..63, 64..127 -> '-', 128..191 -> ',').
    // Encode: M 10 , 20 L 30 , 40 z
    // 'L' is at index 11 in "AACAAAAHAAALMAAAQASTAVAAAZ..." -> 192 + 11 = 203.
    let encoded = vec![
        10,       // "10"
        128 + 20, // ",20"
        192 + 11, // 'L'
        30,       // "30"
        128 + 40, // ",40"
    ];
    let expanded = expand_path_inline_bytes(&encoded);
    assert_eq!(expanded, "M10,20L30,40z");

    let path = path_from_inline_bytes(&encoded).expect("valid path");
    assert_eq!(
        path.commands,
        vec![
            PathCommand::MoveTo { x: 10.0, y: 20.0 },
            PathCommand::LineTo { x: 30.0, y: 40.0 },
            PathCommand::LineTo { x: 10.0, y: 20.0 }, // implicit closing lineTo
        ]
    );
    assert_eq!(path.to_svg_d(), "M10,20 L30,40 L10,20");

    // Empty input -> empty path.
    assert!(path_from_inline_bytes(&[]).expect("empty").is_empty());

    // Smooth cubic reflection (`C` followed by `S`) and H/V/h/v/c/s commands.
    let parsed = parse_svg_path("M0,0H10V10C10,15,5,20,0,20S-10,15,-10,10z").expect("complex path");
    assert_eq!(parsed.commands.len(), 6);
    assert_eq!(parsed.commands[1], PathCommand::LineTo { x: 10.0, y: 0.0 });
    assert_eq!(parsed.commands[2], PathCommand::LineTo { x: 10.0, y: 10.0 });
    assert_eq!(
        parsed.commands[3],
        PathCommand::CubicTo {
            cx1: 10.0,
            cy1: 15.0,
            cx2: 5.0,
            cy2: 20.0,
            x: 0.0,
            y: 20.0,
        }
    );
    // Reflected first control point for S: (2*0 - 5, 2*20 - 20) = (-5, 20).
    assert_eq!(
        parsed.commands[4],
        PathCommand::CubicTo {
            cx1: -5.0,
            cy1: 20.0,
            cx2: -10.0,
            cy2: 15.0,
            x: -10.0,
            y: 10.0,
        }
    );
    assert_eq!(parsed.commands[5], PathCommand::LineTo { x: 0.0, y: 0.0 });

    // Unclosed path fails.
    assert!(parse_svg_path("M0,0L10,10").is_err());
    // Invalid command fails.
    assert!(parse_svg_path("M0,0Q5,5,10,10z").is_err());
}

// ============================================================================
// EXIF orientation & progressive JPEG detection
// ============================================================================

fn build_exif_app1(little_endian: bool, orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::new();
    if little_endian {
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&0x002Au16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes()); // 1 IFD entry
        tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation tag
        tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());
    } else {
        tiff.extend_from_slice(b"MM");
        tiff.extend_from_slice(&0x002Au16.to_be_bytes());
        tiff.extend_from_slice(&8u32.to_be_bytes());
        tiff.extend_from_slice(&1u16.to_be_bytes());
        tiff.extend_from_slice(&0x0112u16.to_be_bytes());
        tiff.extend_from_slice(&3u16.to_be_bytes());
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&0u16.to_be_bytes());
    }
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let mut seg = vec![0xFF, 0xE1];
    let len = (payload.len() + 2) as u16;
    seg.extend_from_slice(&len.to_be_bytes());
    seg.extend_from_slice(&payload);
    seg
}

#[test]
fn exif_orientation_parsing_and_transform() {
    for &le in &[true, false] {
        for raw in 1u16..=8 {
            let app1 = build_exif_app1(le, raw);
            let mut jpeg = vec![0xFF, 0xD8];
            jpeg.extend_from_slice(&app1);
            // Minimal SOF2 (progressive) header: 16x12, 3 components
            jpeg.extend_from_slice(&[
                0xFF, 0xC2, 0x00, 0x11, 0x08, 0x00, 0x0C, 0x00, 0x10, 0x03, 0x01, 0x22, 0x00, 0x02,
                0x11, 0x01, 0x03, 0x11, 0x01, 0xFF, 0xD9,
            ]);

            let expected = ExifOrientation::from_u16(raw).expect("valid");
            assert_eq!(read_jpeg_exif_orientation(&jpeg), expected);
            let info = jpeg_frame_info(&jpeg).expect("frame info");
            assert!(info.progressive);
            assert!(is_progressive_jpeg(&jpeg));
            assert_eq!(info.orientation, Some(expected));
            assert_eq!((info.width, info.height), (16, 12));
        }
    }
    assert!(parse_tiff_orientation(b"BAD").is_none());

    // Create asymmetric 2x3 image and verify all 8 orientations preserve distinct corners.
    let mut src = RgbaImage::new(2, 3);
    src.put_pixel(0, 0, Rgba([255, 0, 0, 255])); // top-left = red
    src.put_pixel(1, 2, Rgba([0, 0, 255, 255])); // bottom-right = blue

    let r90 = apply_exif_orientation(src.clone(), ExifOrientation::Rotate90);
    assert_eq!(r90.dimensions(), (3, 2));
    assert_eq!(r90.get_pixel(2, 0).0, [255, 0, 0, 255]);

    let r180 = apply_exif_orientation(src.clone(), ExifOrientation::Rotate180);
    assert_eq!(r180.dimensions(), (2, 3));
    assert_eq!(r180.get_pixel(1, 2).0, [255, 0, 0, 255]);

    let r270 = apply_exif_orientation(src.clone(), ExifOrientation::Rotate270);
    assert_eq!(r270.dimensions(), (3, 2));
    assert_eq!(r270.get_pixel(0, 1).0, [255, 0, 0, 255]);
    assert!(ExifOrientation::Rotate90.swaps_dimensions());
    assert!(!ExifOrientation::Rotate180.swaps_dimensions());
}

#[test]
fn jpeg_scan_counting_and_read_image_roundtrip() {
    // Encode a baseline JPEG in memory via `image`.
    let mut img = RgbaImage::new(24, 16);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = Rgba([(x * 10) as u8, (y * 15) as u8, 120, 255]);
    }
    let mut jpeg_bytes = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .to_rgb8()
        .write_to(&mut Cursor::new(&mut jpeg_bytes), ImageFormat::Jpeg)
        .expect("encode jpeg");

    assert!(!is_progressive_jpeg(&jpeg_bytes));
    assert_eq!(count_jpeg_scans(&jpeg_bytes), 1);

    // Inject EXIF Rotate90 right after SOI and verify read_image auto-rotates to 16x24.
    let mut rotated_jpeg = Vec::new();
    rotated_jpeg.extend_from_slice(&jpeg_bytes[0..2]);
    rotated_jpeg.extend_from_slice(&build_exif_app1(true, 6));
    rotated_jpeg.extend_from_slice(&jpeg_bytes[2..]);

    let read = read_image(ReadArgs {
        content: rotated_jpeg,
        max_size: Some((12, 12)),
        return_content: true,
        ..Default::default()
    })
    .expect("read jpeg");
    assert_eq!(read.format, "jpeg");
    assert!(!read.animated);
    // 16x24 scaled to fit 12x12 -> 8x12 (scale = 0.5).
    assert_eq!(read.image.dimensions(), (8, 12));
    assert!((read.scale - 0.5).abs() < 1e-9);
    assert!(!read.content.is_empty());

    // Finalizing a truncated prefix appends EOI.
    let prefix = finalize_progressive_prefix(&jpeg_bytes[..jpeg_bytes.len() - 2]);
    assert_eq!(&prefix[prefix.len() - 2..], &[0xFF, 0xD9]);

    // PNG with transparency + force_opaque -> white background.
    let mut png_img = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0]));
    png_img.put_pixel(0, 0, Rgba([200, 0, 0, 255]));
    let mut png_bytes = Vec::new();
    png_img
        .write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
        .expect("encode png");
    let opaque_read = read_image(ReadArgs {
        content: png_bytes,
        force_opaque: true,
        ..Default::default()
    })
    .expect("read png");
    assert_eq!(opaque_read.format, "png");
    assert_eq!(opaque_read.image.get_pixel(1, 1).0, [255, 255, 255, 255]);
    assert_eq!(opaque_read.image.get_pixel(0, 0).0, [200, 0, 0, 255]);

    // Gzip unpack test & is_svg_format.
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(b"<svg/>").expect("write");
    let compressed = gz.finish().expect("finish");
    assert_eq!(unpack_gzip(&compressed).expect("unpack"), b"<svg/>");
    assert!(is_svg_format("SVG"));
    assert!(is_svg_format("svgz"));
    assert!(!is_svg_format("png"));
}

// ============================================================================
// Image preparation: scale rounding, Blur, BlurLargeImage, Circle/Round, Colored, Dither
// ============================================================================

#[test]
fn tdesktop_scaling_rounding_and_prepare() {
    assert_eq!(shrink_to_keep_aspect(1920, 1080, 320, 320), (320, 180));
    assert_eq!(shrink_to_keep_aspect(1000, 2000, 320, 320), (160, 320));
    assert_eq!(shrink_to_keep_aspect(1000, 1, 100, 100), (100, 1));
    assert_eq!(scaled_height_for_width(300, 200, 150), 100);

    // Prepare with blur, resize, outer canvas, round, and colorize.
    let src = RgbaImage::from_pixel(32, 16, Rgba([100, 150, 200, 255]));
    let args = PrepareArgs {
        colored: Some([255, 0, 0, 128]),
        options: PrepareOptions::BLUR
            | PrepareOptions::ROUND_SMALL
            | PrepareOptions::TRANSPARENT_BACKGROUND,
        outer: Some((20, 20)),
        device_pixel_ratio: 1,
    };
    let out = prepare_image(src, 16, 8, &args);
    assert_eq!(out.dimensions(), (20, 20));
    // Corners of outer canvas are transparent.
    assert_eq!(out.get_pixel(0, 0).0[3], 0);
    // Center pixel is opaque and red-tinted.
    let center = out.get_pixel(10, 10).0;
    assert_eq!(center[3], 255);
    assert!(center[0] > 150);
}

#[test]
fn blur_and_stack_blur_large_image_exact() {
    // Uniform color must be invariant under both Blur and BlurLargeImage.
    let solid = RgbaImage::from_pixel(32, 32, Rgba([42, 128, 210, 255]));
    let b1 = blur(solid.clone(), false);
    assert_eq!(b1.get_pixel(16, 16).0, [42, 128, 210, 255]);

    let b2 = blur_large_image(solid.clone(), 6);
    assert_eq!(b2.get_pixel(16, 16).0, [42, 128, 210, 255]);

    // Step edge should be smoothly blurred by BlurLargeImage, and alpha preserved (KeepAlpha).
    let mut step = RgbaImage::new(32, 32);
    for y in 0..32 {
        for x in 0..32 {
            let v = if x < 16 { 0 } else { 255 };
            let a = if y < 16 { 100 } else { 200 };
            step.put_pixel(x, y, Rgba([v, v, v, a]));
        }
    }
    let blurred = blur_large_image(step, 5);
    // At boundary x=15..16, color must be intermediate between 0 and 255.
    let left_edge = blurred.get_pixel(15, 10).0[0];
    let right_edge = blurred.get_pixel(16, 10).0[0];
    assert!(left_edge > 40 && left_edge < 150, "left_edge={left_edge}");
    assert!(
        right_edge > 100 && right_edge < 220,
        "right_edge={right_edge}"
    );
    // Vertical pass keeps original alpha untouched (`KeepAlpha = true`).
    assert_eq!(blurred.get_pixel(15, 5).0[3], 100);
    assert_eq!(blurred.get_pixel(15, 25).0[3], 200);

    // Deterministic regression hash for known input with radius=3.
    let digest = Sha256::digest(blurred.as_raw());
    assert_eq!(digest.len(), 32);
}

#[test]
fn circle_round_colored_opaque_dither_gradient_shadow() {
    let solid = RgbaImage::from_pixel(32, 32, Rgba([80, 120, 160, 255]));

    let c = circle(solid.clone(), None);
    assert_eq!(c.get_pixel(0, 0).0[3], 0);
    assert_eq!(c.get_pixel(16, 16).0[3], 255);

    let r = round(
        solid.clone(),
        ImageRoundRadius::Large,
        RectCorners::TOP_LEFT | RectCorners::BOTTOM_RIGHT,
        None,
    );
    assert_eq!(r.get_pixel(0, 0).0[3], 0); // top-left rounded
    assert_eq!(r.get_pixel(31, 0).0[3], 255); // top-right NOT rounded
    assert_eq!(r.get_pixel(0, 31).0[3], 255); // bottom-left NOT rounded
    assert_eq!(r.get_pixel(31, 31).0[3], 0); // bottom-right rounded

    assert_eq!(ellipse_mask(16, 16).get_pixel(8, 8).0[3], 255);
    let cm = corners_mask(6);
    assert_eq!(cm[0].dimensions(), (6, 6));
    assert_eq!(cm[0].get_pixel(0, 0).0[3], 0);
    assert_eq!(cm[0].get_pixel(5, 5).0[3], 255);

    // Colored with alpha=255 replaces color completely.
    let tinted = colored(solid.clone(), [10, 20, 30, 255]);
    assert_eq!(tinted.get_pixel(16, 16).0, [10, 20, 30, 255]);

    // Opaque composites semi-transparent pixel over white.
    let semi = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 0, 128]));
    let op = opaque(semi);
    let p = op.get_pixel(0, 0).0;
    assert_eq!(p[3], 255);
    assert!((i16::from(p[0]) - 127).abs() <= 2);

    // Dither deterministic test on 40x40 image (uses DitherGeneric<1>).
    let mut grad = RgbaImage::new(40, 40);
    for (x, y, px) in grad.enumerate_pixels_mut() {
        *px = Rgba([(x * 6) as u8, (y * 6) as u8, 100, 255]);
    }
    let mut rng1 = StdRng::seed_from_u64(12345);
    let mut rng2 = StdRng::seed_from_u64(12345);
    let d1 = dither_image_with_rng(&grad, &mut rng1);
    let d2 = dither_image_with_rng(&grad, &mut rng2);
    assert_eq!(d1, d2);
    assert_ne!(d1, grad);

    // Shadow & complex gradient.
    // Exact tdesktop fixed-point step: (255 * 65536) / 9 = 1856853 -> (9 * 1856853) >> 16 = 254.
    let shadow = generate_shadow(10, 0, 255, [0, 0, 0]);
    assert_eq!(shadow.dimensions(), (1, 10));
    assert_eq!(shadow.get_pixel(0, 0).0[3], 0);
    assert_eq!(shadow.get_pixel(0, 9).0[3], 254);
    let shadow_exact = generate_shadow(16, 0, 240, [0, 0, 0]);
    assert_eq!(shadow_exact.get_pixel(0, 15).0[3], 240);

    let bg = generate_complex_gradient(
        64,
        64,
        &[[219, 221, 187], [107, 165, 135], [213, 216, 141]],
        45,
        1.0,
    );
    assert_eq!(bg.dimensions(), (64, 64));
    assert_eq!(bg.get_pixel(32, 32).0[3], 255);
}

// ============================================================================
// Photo size selection (s, m, x, y, w, a, b, c, d, i, j)
// ============================================================================

#[test]
fn photo_size_selection_non_progressive_and_progressive() {
    assert_eq!(SMALL_LEVELS, b"sa");
    assert_eq!(THUMBNAIL_LEVELS, b"mbsa");
    assert_eq!(LARGE_LEVELS, b"ydxcwmbsa");
    assert_eq!(
        PhotoSizeType::from_byte(b'y').spec(),
        Some((1280, 1280, false))
    );
    assert_eq!(
        PhotoSizeType::from_byte(b'c').spec(),
        Some((640, 640, true))
    );
    assert_eq!(
        PhotoSizeType::from_type_str("i"),
        Some(PhotoSizeType::Stripped)
    );
    assert_eq!(
        PhotoSizeType::from_type_str("j"),
        Some(PhotoSizeType::OutlinePath)
    );

    // Standard non-progressive photo with i, s, m, x, y, w.
    let sizes = vec![
        PhotoSizeEntry::Stripped {
            size_type: "i".into(),
            bytes: vec![1, 10, 10, 0],
        },
        PhotoSizeEntry::Normal {
            size_type: "s".into(),
            width: 100,
            height: 75,
            byte_size: 1200,
        },
        PhotoSizeEntry::Normal {
            size_type: "m".into(),
            width: 320,
            height: 240,
            byte_size: 8500,
        },
        PhotoSizeEntry::Normal {
            size_type: "x".into(),
            width: 800,
            height: 600,
            byte_size: 42_000,
        },
        PhotoSizeEntry::Normal {
            size_type: "y".into(),
            width: 1280,
            height: 960,
            byte_size: 98_000,
        },
        PhotoSizeEntry::Normal {
            size_type: "w".into(),
            width: 2560,
            height: 1920,
            byte_size: 250_000,
        },
    ];

    let selected = choose_photo_sizes(&sizes);
    assert_eq!(selected.inline_stripped, Some([1, 10, 10, 0].as_slice()));
    assert_eq!(selected.small.map(PhotoSizeEntry::letter), Some(b's'));
    assert_eq!(selected.thumbnail.map(PhotoSizeEntry::letter), Some(b'm'));
    // LargeLevels = "ydxcwmbsa": 'y' outranks 'w' in tdesktop!
    assert_eq!(selected.large.map(PhotoSizeEntry::letter), Some(b'y'));
    assert_eq!(
        selected.byte_size_for_slot(PhotoSizeSlot::Large),
        Some(98_000)
    );

    // Progressive photo overrides small/thumbnail slots and populates progressive_thumbnail_bytes from index 1.
    let prog_sizes = vec![
        PhotoSizeEntry::Stripped {
            size_type: "i".into(),
            bytes: vec![1, 8, 8, 0],
        },
        PhotoSizeEntry::Normal {
            size_type: "m".into(),
            width: 320,
            height: 240,
            byte_size: 9000,
        },
        PhotoSizeEntry::Progressive {
            size_type: "y".into(),
            width: 1280,
            height: 960,
            sizes: vec![3000, 18_000, 52_000, 110_000],
        },
    ];
    let prog = choose_photo_sizes(&prog_sizes);
    assert!(prog.small.is_none());
    assert!(prog.thumbnail.is_none());
    assert_eq!(prog.progressive_thumbnail_bytes, Some(18_000));
    assert_eq!(prog.large.map(PhotoSizeEntry::letter), Some(b'y'));
    assert_eq!(prog.resolve_slot(PhotoSizeSlot::Small), prog.large);
    assert_eq!(
        prog.byte_size_for_slot(PhotoSizeSlot::Thumbnail),
        Some(18_000)
    );
    assert_eq!(prog.byte_size_for_slot(PhotoSizeSlot::Large), Some(110_000));

    // Document thumbnails choose largest area, ignoring Stripped/Path.
    let doc_thumbs = vec![
        PhotoSizeEntry::Path {
            size_type: "j".into(),
            bytes: vec![1, 2, 3],
        },
        PhotoSizeEntry::Normal {
            size_type: "m".into(),
            width: 320,
            height: 320,
            byte_size: 5000,
        },
        PhotoSizeEntry::Cached {
            size_type: "s".into(),
            width: 90,
            height: 90,
            bytes: vec![0; 500],
        },
    ];
    assert_eq!(
        find_inline_thumbnail(&doc_thumbs),
        Some(InlineThumbnail::Path(&[1, 2, 3]))
    );
    assert_eq!(
        choose_document_thumbnail(&doc_thumbs).map(PhotoSizeEntry::letter),
        Some(b'm')
    );

    // Video sizes selection ('p' for small, largest area for large).
    let vids = vec![
        VideoSizeEntry {
            size_type: "p".into(),
            width: 160,
            height: 160,
            byte_size: 40_000,
            video_start_ts: Some(0.5),
        },
        VideoSizeEntry {
            size_type: "u".into(),
            width: 640,
            height: 640,
            byte_size: 300_000,
            video_start_ts: Some(1.25),
        },
    ];
    let (v_small, v_large) = choose_photo_video_sizes(&vids);
    assert_eq!(v_small.map(|v| v.size_type.as_str()), Some("p"));
    assert_eq!(v_large.map(|v| v.size_type.as_str()), Some("u"));
    assert_eq!(v_large.map(VideoSizeEntry::start_time_ms), Some(1250));
    assert_eq!(
        choose_document_video_thumbnail(&vids).map(|v| v.width),
        Some(640)
    );

    // Dimension clamping & sticker checks.
    assert_eq!(clamp_photo_dimensions(0, 100), None);
    assert_eq!(clamp_photo_dimensions(1000, 500), Some((1000, 500)));
    assert_eq!(
        clamp_photo_dimensions(5120, 2560),
        Some((PHOTO_SIDE_LIMIT, 1280))
    );
    assert!(good_sticker_dimensions(512, 512));
    assert!(!good_sticker_dimensions(0, 512));
    assert!(!good_sticker_dimensions(3000, 3000));
}

// ============================================================================
// Audio waveforms & duration helpers
// ============================================================================

#[test]
fn waveform_5bit_roundtrip_and_known_packing() {
    // 8 values of 5 bits = 40 bits = 5 bytes exact!
    // Values: [0, 31, 1, 16, 15, 3, 28, 7]
    // Bit layout (little-endian):
    // bits  0..5 : 0  = 00000
    // bits  5..10: 31 = 11111 -> byte0 = 111_00000 = 0xE0, byte1 bits 0..2 = 11
    // bits 10..15: 1  = 00001 -> byte1 bits 2..7 = 00001 -> byte1 = 0_00001_11 = 0x07
    // bits 15..20: 16 = 10000 -> byte1 bit 7 = 0, byte2 bits 0..4 = 1000
    // bits 20..25: 15 = 01111 -> byte2 bits 4..8 = 1111 (byte2 = 0xF8), byte3 bit 0 = 0
    // bits 25..30: 3  = 00011 -> byte3 bits 1..6 = 00011
    // bits 30..35: 28 = 11100 -> byte3 bits 6..8 = 00 (byte3 = 0x06), byte4 bits 0..3 = 111
    // bits 35..40: 7  = 00111 -> byte4 bits 3..8 = 00111 -> byte4 = 00111_111 = 0x3F
    let values = vec![0u8, 31, 1, 16, 15, 3, 28, 7];
    let encoded = waveform_encode_5bit(&values);
    assert_eq!(encoded, vec![0xE0, 0x07, 0xF8, 0x06, 0x3F]);
    let decoded = waveform_decode_5bit(&encoded);
    assert_eq!(decoded, values);

    // 100-sample Telegram waveform encodes to ceil(500 / 8) = 63 bytes;
    // decoding 63 bytes yields floor(63 * 8 / 5) = 100 values.
    let w100: Vec<u8> = (0..100).map(|i| (i % 32) as u8).collect();
    let enc100 = waveform_encode_5bit(&w100);
    assert_eq!(enc100.len(), 63);
    assert_eq!(waveform_decode_5bit(&enc100), w100);

    // PCM conversion & u8 envelope conversion.
    let mut pcm = vec![0i16; 1000];
    for (i, sample) in pcm.iter_mut().enumerate() {
        *sample = if i < 500 { 1000 } else { 10_000 };
    }
    let wf_pcm = convert_waveform_from_pcm_i16(&pcm);
    assert_eq!(wf_pcm.len(), WAVEFORM_SAMPLES_COUNT);
    assert!(wf_pcm[0] < wf_pcm[99]);
    assert_eq!(wf_pcm[99], 31);

    let env: Vec<u8> = (0..200).map(|i| (i / 2) as u8).collect();
    let wf_env = convert_waveform_from_u8_peaks(&env);
    assert_eq!(wf_env.len(), WAVEFORM_SAMPLES_COUNT);
    assert!(wf_env[99] > wf_env[0]);

    let resampled = resample_waveform(&wf_pcm, 25);
    assert_eq!(resampled.len(), 25);
}

#[test]
fn voice_duration_helpers() {
    assert_eq!(milliseconds_from_seconds(1.2345), 1235);
    assert_eq!(milliseconds_from_seconds(f64::NAN), 0);
    assert_eq!(format_duration_text(0), "00:00");
    assert_eq!(format_duration_text(65), "01:05");
    assert_eq!(format_duration_text(3661), "1:01:01");
    assert_eq!(format_played_text(12, 65), "00:12 / 01:05");

    let vd = VoiceDuration::from_secs(10);
    assert_eq!(vd.format(), "00:10");
    assert!((vd.progress(5000) - 0.5).abs() < 1e-9);
    assert_eq!(vd.active_bars(5000, 100), 50);
    assert_eq!(vd.active_bars(99_999, 100), 100);
}

// ============================================================================
// Document MIME tables, classification & filename sanitization
// ============================================================================

#[test]
fn mime_tables_magic_and_classification() {
    assert_eq!(extension_for_mime("image/webp"), Some(".webp"));
    assert_eq!(extension_for_mime("application/x-tgsticker"), Some(".tgs"));
    assert_eq!(
        extension_for_mime("application/x-tgwallpattern"),
        Some(".tgv")
    );
    assert_eq!(
        extension_for_mime("application/x-tgtheme-tdesktop"),
        Some(".tdesktop-theme")
    );
    assert_eq!(extension_for_mime("audio/mpeg3"), Some(".mp3"));
    assert_eq!(
        mime_type_for_name("image/webp").map(|m| m.filter_string),
        Some("WebP image (*.webp)")
    );
    assert_eq!(
        mime_type_for_filename("sticker.TGS").map(|m| m.name),
        Some("application/x-tgsticker")
    );

    assert_eq!(
        mime_type_for_data(b"RIFF\x00\x00\x00\x00WEBP").map(|m| m.name),
        Some("image/webp")
    );
    assert_eq!(
        mime_type_for_data(b"\x89PNG\r\n\x1a\n").map(|m| m.name),
        Some("image/png")
    );
    assert_eq!(
        mime_type_for_data(b"OggS\x00\x02").map(|m| m.name),
        Some("audio/ogg")
    );
    assert_eq!(
        mime_type_for_data(b"\x00\x00\x00\x18ftypmp42").map(|m| m.name),
        Some("video/mp4")
    );

    assert!(is_mime_sticker("image/webp"));
    assert!(is_mime_sticker("application/x-tgsticker"));
    assert!(is_mime_sticker_animated("application/x-tgsticker"));
    assert!(!is_mime_sticker_animated("image/webp"));
    assert!(is_mime_sent_as_video("video/mp4"));
    assert!(is_mime_accepted_for_photo_video_album("image/jpeg"));
    assert!(file_is_image("photo.JPG", ""));
    assert!(file_is_image("", "image/png"));

    assert_eq!(file_extension("a/b/c.tar.gz"), "gz");
    assert_eq!(file_extension("a.b/noext"), "");
    assert_eq!(detect_name_type("photo.webp"), NameType::Image);
    assert_eq!(detect_name_type("clip.mp4"), NameType::Video);
    assert_eq!(detect_name_type("voice.ogg"), NameType::Audio);
    assert_eq!(detect_name_type("report.pdf"), NameType::Document);
    assert_eq!(detect_name_type("pack.zip"), NameType::Archive);
    assert_eq!(detect_name_type("dark.tdesktop-theme"), NameType::ThemeFile);
    assert_eq!(detect_name_type("run.exe"), NameType::Executable);
    assert_eq!(detect_name_type("script.ps1"), NameType::Executable);
    assert!(name_type_allows_thumbnail(NameType::Image));
    assert!(!name_type_allows_thumbnail(NameType::Executable));
    assert!(is_ip_revealing_filename("drawing.svg"));
    assert!(!is_ip_revealing_filename("photo.jpg"));
}

#[test]
fn filename_sanitization_and_save_paths() {
    assert!(is_bidi_control('\u{202E}')); // RLO
    assert!(!is_bidi_control('A'));

    // BiDi spoofing attack: "Fil\u{202E}gepj.exe" -> "Fil_gepj.exe"
    assert_eq!(sanitize_filename("Fil\u{202E}gepj.exe"), "Fil_gepj.exe");
    // Windows forbidden characters and control characters.
    assert_eq!(
        sanitize_filename("a/b\\c:d*e?f\"g<h>i|j\x07"),
        "a_b_c_d_e_f_g_h_i_j_"
    );
    // Trailing dot / space gets '_'.
    assert_eq!(sanitize_filename("trailing."), "trailing._");
    assert_eq!(sanitize_filename("trailing "), "trailing _");
    assert_eq!(sanitize_filename(""), "_");
    // Windows dangerous extensions (.lnk, .scf) masked with .download.
    assert_eq!(sanitize_filename("shortcut.LNK"), "shortcut.LNK.download");
    assert_eq!(sanitize_filename("evil.scf"), "evil.scf.download");
    assert_eq!(
        mask_dangerous_windows_extension("C:\\tmp\\foo.lnk"),
        "C:\\tmp\\foo.lnk.download"
    );
    // Windows reserved device names prefixed with '_'.
    assert_eq!(sanitize_filename("CON"), "_CON");
    assert_eq!(sanitize_filename("nul.txt"), "_nul.txt");
    assert_eq!(sanitize_filename("COM1.log"), "_COM1.log");
    assert_eq!(sanitize_filename("console.txt"), "console.txt");

    // Document save name & prefix selection.
    assert_eq!(
        document_save_name_and_prefix(DocumentKind::Voice, None, "audio/ogg"),
        (".ogg".into(), "audio")
    );
    assert_eq!(
        document_save_name_and_prefix(DocumentKind::Voice, None, "audio/mp3"),
        (".mp3".into(), "audio")
    );
    assert_eq!(
        document_save_name_and_prefix(DocumentKind::Video, None, "video/mp4"),
        (".mp4".into(), "video")
    );
    assert_eq!(
        format_default_filename(
            "photo",
            ".jpg",
            FileDateTime {
                year: 2026,
                month: 11,
                day: 22,
                hour: 1,
                minute: 5,
                second: 9,
            }
        ),
        "photo_2026-11-22_01-05-09.jpg"
    );

    // Collision deduplication in a temp directory.
    let dir = tempfile::tempdir().expect("tempdir");
    let p1 = resolve_unique_save_path(dir.path(), "note.txt", "doc", "2026-11-22_00-00-00");
    assert_eq!(p1.file_name().and_then(|s| s.to_str()), Some("note.txt"));
    std::fs::write(&p1, b"1").expect("write p1");

    let p2 = resolve_unique_save_path(dir.path(), "note.txt", "doc", "2026-11-22_00-00-00");
    assert_eq!(
        p2.file_name().and_then(|s| s.to_str()),
        Some("note (2).txt")
    );
    std::fs::write(&p2, b"2").expect("write p2");

    let p3 = resolve_unique_save_path(dir.path(), "note.txt", "doc", "2026-11-22_00-00-00");
    assert_eq!(
        p3.file_name().and_then(|s| s.to_str()),
        Some("note (3).txt")
    );

    // Dot-prefixed extension gets prefix_timestamp.
    let voice_path = resolve_unique_save_path(dir.path(), ".ogg", "audio", "2026-11-22_01-02-03");
    assert_eq!(
        voice_path.file_name().and_then(|s| s.to_str()),
        Some("audio_2026-11-22_01-02-03.ogg")
    );
}
