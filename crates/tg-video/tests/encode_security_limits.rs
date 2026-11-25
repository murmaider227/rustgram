//! Regression tests for SEC-C5: FFmpeg encode shim protocol whitelist and dimension/stride limits.

use std::path::Path;

use tg_video::{VideoEncodeConfig, generate_test_clip, transcode_video_file};

#[test]
fn encode_shim_rejects_non_file_protocols_via_whitelist() {
    let dir = std::env::temp_dir().join(format!("rustgram-sec-c5-proto-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let out = dir.join("out.mp4");

    // Non-file protocols (data:, concat:, http:) must be rejected by FFmpeg's
    // `protocol_whitelist=file` rather than opened or parsed.
    let data_res = transcode_video_file(
        Path::new("data:application/octet-stream,AAAA"),
        &out,
        &VideoEncodeConfig::default(),
    );
    let err_msg = format!("{data_res:?}");
    assert!(
        data_res.is_err(),
        "data: protocol input must be rejected by protocol_whitelist"
    );
    assert!(
        err_msg.to_ascii_lowercase().contains("whitelist")
            || err_msg.to_ascii_lowercase().contains("protocol")
            || err_msg.contains("Invalid argument")
            || err_msg.contains("avformat_open_input"),
        "expected protocol_whitelist rejection error, got: {err_msg}"
    );

    let src = dir.join("valid_for_concat.mp4");
    generate_test_clip(&src, 32, 32, 10, 200, false).expect("generate valid clip");

    // `concat:<valid_file>` would be opened and transcoded on the old code (no whitelist);
    // with `protocol_whitelist=file` FFmpeg rejects it immediately.
    let concat_uri = format!("concat:{}", src.display());
    let concat_res =
        transcode_video_file(Path::new(&concat_uri), &out, &VideoEncodeConfig::default());
    let concat_err = format!("{concat_res:?}");
    assert!(
        concat_res.is_err(),
        "concat: protocol input over a valid file must be rejected by protocol_whitelist"
    );
    assert!(
        concat_err.to_ascii_lowercase().contains("whitelist")
            || concat_err.to_ascii_lowercase().contains("protocol")
            || concat_err.contains("Invalid argument")
            || concat_err.contains("avformat_open_input"),
        "expected protocol_whitelist rejection error, got: {concat_err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn encode_shim_rejects_oversized_dimensions_and_transcodes_valid_clip() {
    let dir = std::env::temp_dir().join(format!("rustgram-sec-c5-dims-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let src = dir.join("valid_src.mp4");
    let dst_bad = dir.join("oversized_out.mp4");
    let dst_ok = dir.join("rotated_ok.mp4");

    // Oversized synthetic clip (> 4K MAX_FRAME_AREA) is rejected upfront.
    let huge_gen = generate_test_clip(&dst_bad, 8192, 8192, 10, 200, false);
    assert!(
        huge_gen.is_err(),
        "8192x8192 clip exceeding 4K area limit must be rejected"
    );

    // Valid small clip generates cleanly via local `file` protocol.
    generate_test_clip(&src, 64, 48, 10, 300, false).expect("valid test clip should generate");

    // Transcoding with oversized target dimensions (> 4K MAX_FRAME_AREA) must fail before encoding.
    let oversized_cfg = VideoEncodeConfig {
        target_width: 8192,
        target_height: 8192,
        ..VideoEncodeConfig::default()
    };
    let oversized_res = transcode_video_file(&src, &dst_bad, &oversized_cfg);
    assert!(
        oversized_res.is_err(),
        "oversized target output dimensions must be rejected"
    );

    // Normal transcode with RGBA transform (rotation + flip + crop) succeeds with all checks active.
    let ok_cfg = VideoEncodeConfig {
        rotation: 90,
        flipped: true,
        crop_x: 4,
        crop_y: 4,
        crop_w: 40,
        crop_h: 32,
        remove_audio: true,
        ..VideoEncodeConfig::default()
    };
    let ok_res = transcode_video_file(&src, &dst_ok, &ok_cfg)
        .expect("valid local transcode with RGBA transform must succeed");
    assert_eq!(ok_res.output_width, 32);
    assert_eq!(ok_res.output_height, 40);
    assert!(ok_res.file_size > 0);

    let _ = std::fs::remove_dir_all(&dir);
}
