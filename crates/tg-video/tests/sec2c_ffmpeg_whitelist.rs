//! Regression tests for SEC2-C W4: FFmpeg decode shim `format_whitelist` and `codec_whitelist`.

use tg_video::{FfmpegMedia, FfmpegOpenOptions};

/// Builds a valid minimal AIFF container (`FORM ... AIFF`, `pcm_s16be`, 44.1 kHz mono, 64 frames).
///
/// Without `format_whitelist`, FFmpeg's `aiff` demuxer opens and decodes this cleanly.
/// With `TG_FORMAT_WHITELIST`, the shim must refuse the non-whitelisted `aiff` container.
fn build_minimal_aiff_pcm16() -> Vec<u8> {
    let sample_frames: u32 = 64;
    let pcm_bytes_len = (sample_frames as usize) * 2;
    let ssnd_payload_len = 8 + pcm_bytes_len;
    let comm_payload_len = 18_usize;
    let form_size = (4 + 8 + comm_payload_len + 8 + ssnd_payload_len) as u32;

    let mut out = Vec::with_capacity(8 + form_size as usize);
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&form_size.to_be_bytes());
    out.extend_from_slice(b"AIFF");

    // COMM chunk: 1 channel, 64 frames, 16-bit, 44100.0 Hz (80-bit IEEE 754 extended).
    out.extend_from_slice(b"COMM");
    out.extend_from_slice(&(comm_payload_len as u32).to_be_bytes());
    out.extend_from_slice(&1_u16.to_be_bytes());
    out.extend_from_slice(&sample_frames.to_be_bytes());
    out.extend_from_slice(&16_u16.to_be_bytes());
    out.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

    // SSND chunk: offset=0, blockSize=0, followed by silent PCM samples.
    out.extend_from_slice(b"SSND");
    out.extend_from_slice(&(ssnd_payload_len as u32).to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    out.resize(out.len() + pcm_bytes_len, 0);
    out
}

/// Builds a valid minimal WAV container with standard `pcm_s16le` (whitelisted container + codec).
fn build_minimal_wav_pcm16() -> Vec<u8> {
    let data_len: u32 = 128;
    let riff_size: u32 = 36 + data_len;
    let mut out = Vec::with_capacity(8 + riff_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1_u16.to_le_bytes()); // mono
    out.extend_from_slice(&44_100_u32.to_le_bytes());
    out.extend_from_slice(&88_200_u32.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.resize(out.len() + data_len as usize, 0);
    out
}

/// Builds a valid minimal WAV container carrying MS-ADPCM (`0x0002`, decoder `adpcm_ms`):
/// whitelisted container (`wav`), non-whitelisted codec (`adpcm_ms`).
fn build_minimal_wav_adpcm_ms() -> Vec<u8> {
    let fmt_len: u32 = 50;
    let data_len: u32 = 256;
    let riff_size: u32 = 4 + (8 + fmt_len) + (8 + data_len);
    let mut out = Vec::with_capacity(8 + riff_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&fmt_len.to_le_bytes());
    out.extend_from_slice(&0x0002_u16.to_le_bytes()); // WAVE_FORMAT_ADPCM (adpcm_ms)
    out.extend_from_slice(&1_u16.to_le_bytes()); // mono
    out.extend_from_slice(&8_000_u32.to_le_bytes());
    out.extend_from_slice(&4_096_u32.to_le_bytes());
    out.extend_from_slice(&256_u16.to_le_bytes()); // block_align
    out.extend_from_slice(&4_u16.to_le_bytes()); // bits_per_sample
    out.extend_from_slice(&32_u16.to_le_bytes()); // cbSize
    out.extend_from_slice(&500_u16.to_le_bytes()); // wSamplesPerBlock
    out.extend_from_slice(&7_u16.to_le_bytes()); // wNumCoef
    for (c1, c2) in [
        (256_i16, 0_i16),
        (512, -256),
        (0, 0),
        (192, 64),
        (240, 0),
        (460, -208),
        (392, -232),
    ] {
        out.extend_from_slice(&c1.to_le_bytes());
        out.extend_from_slice(&c2.to_le_bytes());
    }

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.resize(out.len() + data_len as usize, 0);
    out
}

#[test]
fn sec2c_w4_decode_shim_refuses_non_whitelisted_container_and_codec() {
    let opts = FfmpegOpenOptions {
        open_video: false,
        open_audio: true,
        ..FfmpegOpenOptions::default()
    };

    // 1. Non-whitelisted container (AIFF with PCM audio) must be refused by `format_whitelist`.
    let aiff = build_minimal_aiff_pcm16();
    let aiff_res = FfmpegMedia::open_bytes(aiff, opts);
    assert!(
        aiff_res.is_err(),
        "non-whitelisted AIFF container must be refused by FFmpeg format_whitelist"
    );

    // 2. Whitelisted container (WAV) with non-whitelisted codec (adpcm_ms) must be refused by `codec_whitelist`.
    let adpcm_wav = build_minimal_wav_adpcm_ms();
    let adpcm_res = FfmpegMedia::open_bytes(adpcm_wav, opts);
    assert!(
        adpcm_res.is_err(),
        "non-whitelisted adpcm_ms codec inside WAV must be refused by FFmpeg codec_whitelist"
    );

    // 3. Whitelisted container (WAV) with whitelisted codec (pcm_s16le) opens cleanly.
    let pcm_wav = build_minimal_wav_pcm16();
    let media = FfmpegMedia::open_bytes(pcm_wav, opts)
        .expect("whitelisted WAV + pcm_s16le must still open cleanly");
    assert!(media.info().has_audio);
    assert_eq!(media.info().format_name, "wav");
    assert_eq!(media.info().audio_codec_name, "pcm_s16le");
}
