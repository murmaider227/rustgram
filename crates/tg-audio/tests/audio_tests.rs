//! Comprehensive unit and integration tests for `tg-audio`.

use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use opus_pure::Application;
use rand::SeedableRng;
use rand::rngs::StdRng;
use tg_audio::device::{
    DEFAULT_DEVICE_ID, DeviceChangeReason, DeviceInfo, DeviceManager, DeviceResolver, DeviceType,
};
use tg_audio::dsp::{Resampler, TimeStretcher, f32_to_i16};
use tg_audio::float_controller::{
    FloatColumn, FloatController, FloatCorner, Point, Rect, SectionArea, Size, compute_out_ratio,
};
use tg_audio::local_cache::{
    LOCAL_SOUND_MAX_DURATION_MS, LOCAL_SOUND_SAMPLE_RATE, LocalCache, LocalDiskCache,
    convert_and_cut_to_wav, encode_wav_pcm_s16le,
};
use tg_audio::mixer::{AudioStream, MemoryPcmStream, MixerCore, MixerEvent, compute_volume};
use tg_audio::opus_codec::{
    OPUS_VOICE_BITRATE_BPS, OggOpusStreamDecoder, concat_ogg_opus, decode_ogg_opus_to_f32,
    decode_ogg_opus_to_i16, encode_ogg_opus_i16, is_ogg_opus_header, trim_ogg_opus,
};
use tg_audio::player::{
    PlayerEvent, PlayerInstance, PlaylistNavigator, SeekingPhase, ShuffleState,
    compute_seek_position_ms, save_last_playback_position,
};
use tg_audio::sound_track::SoundTrack;
use tg_audio::source::{
    InMemoryPlaylistProvider, InMemoryPositionStore, MemoryAudioSource, PlaylistContext,
    PlaylistEntry, PlaylistProvider, ResumePositionStore,
};
use tg_audio::types::{
    AudioMsgId, AudioType, DEFAULT_FREQUENCY, FADE_DURATION_MS, MediaKind, OrderMode,
    PlaybackState, RepeatMode, SPEED_MAX, SPEED_MIN, SUPPRESS_RATIO_ALL, SUPPRESS_RATIO_SONG,
    TrackState, clamp_speed, equal_speeds, speed_dependent_position, speed_independent_position,
};
use tg_audio::waveform::{
    CaptureWaveformCollector, collect_waveform_from_bins, compute_waveform_from_i16, decode_5bit,
    encode_5bit, normalize_peaks, read_one_sample_i16, read_one_sample_u8,
};
use tg_audio::{AudioDecoder, CaptureConfig, VoiceCapturePipeline, count_waveform_from_bytes};
use tg_audio::{DocumentId, FullMsgId, MsgId, PeerId};

/// Generates a mono sine wave as `i16` PCM.
fn generate_sine_i16(
    freq_hz: f64,
    sample_rate: u32,
    duration_secs: f64,
    amplitude: f64,
) -> Vec<i16> {
    let total = (f64::from(sample_rate) * duration_secs).round() as usize;
    (0..total)
        .map(|i| {
            let t = (i as f64) / f64::from(sample_rate);
            let s = (2.0 * PI * freq_hz * t).sin() * amplitude * 32767.0;
            s.round().clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

/// Generates an interleaved `f32` sine wave (mono or stereo).
fn generate_sine_f32(
    freq_hz: f64,
    sample_rate: u32,
    duration_secs: f64,
    channels: usize,
    amplitude: f32,
) -> Vec<f32> {
    let frames = (f64::from(sample_rate) * duration_secs).round() as usize;
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let t = (i as f64) / f64::from(sample_rate);
        let s = (2.0 * PI * freq_hz * t).sin() as f32 * amplitude;
        for _ in 0..channels {
            out.push(s);
        }
    }
    out
}

/// Estimates fundamental frequency of a single-channel periodic signal via positive-going zero crossings.
fn estimate_freq_hz(samples: &[f32], channels: usize, sample_rate: u32) -> f64 {
    let frames = samples.len() / channels;
    let mut crossings = 0usize;
    let mut first = None;
    let mut last = 0usize;
    for f in 1..frames {
        let prev = samples[(f - 1) * channels];
        let curr = samples[f * channels];
        if prev <= 0.0 && curr > 0.0 {
            if first.is_none() {
                first = Some(f);
            }
            last = f;
            crossings += 1;
        }
    }
    let Some(start) = first else {
        return 0.0;
    };
    if crossings <= 1 || last <= start {
        return 0.0;
    }
    let periods = (crossings - 1) as f64;
    let elapsed_secs = ((last - start) as f64) / f64::from(sample_rate);
    periods / elapsed_secs
}

fn rms_f32(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum_sq / samples.len() as f64).sqrt()
}

// ============================================================================
// 1. Ogg Opus encode / decode / seek / edit round-trip tests
// ============================================================================

#[test]
fn opus_sine_wave_round_trip_duration_and_amplitude() {
    let duration_secs = 2.0;
    let amplitude = 0.5;
    let pcm = generate_sine_i16(440.0, DEFAULT_FREQUENCY, duration_secs, amplitude);

    let ogg_bytes = encode_ogg_opus_i16(
        &pcm,
        DEFAULT_FREQUENCY,
        1,
        OPUS_VOICE_BITRATE_BPS,
        Application::Voip,
    )
    .expect("Ogg Opus encode should succeed");

    assert!(is_ogg_opus_header(&ogg_bytes), "must have OggS + OpusHead");

    let (decoded, rate, channels, decoded_ms) =
        decode_ogg_opus_to_f32(&ogg_bytes).expect("Ogg Opus decode should succeed");
    assert_eq!(rate, DEFAULT_FREQUENCY);
    assert_eq!(channels, 1);

    // Check duration within ±60 ms (Opus frame boundary + pre-skip).
    assert!(
        (decoded_ms - 2000).abs() <= 60,
        "decoded duration {decoded_ms} ms not close to 2000 ms"
    );

    // Check rough amplitude in the steady middle section (skipping initial/final 100 ms).
    let skip = (DEFAULT_FREQUENCY / 10) as usize;
    let mid = &decoded[skip..decoded.len().saturating_sub(skip)];
    let expected_rms = amplitude / 2.0f64.sqrt(); // ~0.3535
    let actual_rms = rms_f32(mid);
    assert!(
        (actual_rms - expected_rms).abs() < 0.08,
        "decoded RMS {actual_rms:.4} too far from expected {expected_rms:.4}"
    );

    // Check fundamental frequency survived Opus compression accurately.
    let est_freq = estimate_freq_hz(mid, 1, DEFAULT_FREQUENCY);
    assert!(
        (est_freq - 440.0).abs() < 8.0,
        "decoded frequency {est_freq:.2} Hz too far from 440 Hz"
    );

    // Also verify unified AudioDecoder opens & decodes the same Ogg Opus buffer.
    let mut dec = AudioDecoder::from_bytes(ogg_bytes, Some("ogg".into()))
        .expect("AudioDecoder should open Ogg Opus");
    assert_eq!(dec.sample_rate(), DEFAULT_FREQUENCY);
    assert_eq!(dec.channels(), 1);
    assert!(
        (dec.duration_ms() - 2000).abs() <= 60,
        "probed duration {} ms not close to 2000 ms",
        dec.duration_ms()
    );

    // Seek to 1000 ms and confirm remaining decoded length is ~1000 ms.
    dec.seek_ms(1000).expect("Ogg Opus seek should succeed");
    let mut rem_samples = 0usize;
    while let Some(chunk) = dec.read_chunk().expect("read after seek") {
        rem_samples += chunk.samples.len();
    }
    let rem_ms = ((rem_samples as i64) * 1000) / i64::from(DEFAULT_FREQUENCY);
    assert!(
        (rem_ms - 1000).abs() <= 80,
        "remaining duration after seek to 1000 ms was {rem_ms} ms"
    );
}

#[test]
fn opus_trim_and_concat() {
    let pcm_a = generate_sine_i16(440.0, DEFAULT_FREQUENCY, 1.5, 0.4);
    let pcm_b = generate_sine_i16(660.0, DEFAULT_FREQUENCY, 1.0, 0.4);

    let ogg_a = encode_ogg_opus_i16(&pcm_a, DEFAULT_FREQUENCY, 1, 32_000, Application::Voip)
        .expect("encode A");
    let ogg_b = encode_ogg_opus_i16(&pcm_b, DEFAULT_FREQUENCY, 1, 32_000, Application::Voip)
        .expect("encode B");

    let trimmed = trim_ogg_opus(&ogg_a, 250, 1250).expect("trim A to 1.0s");
    assert!((trimmed.duration_ms - 1000).abs() <= 50);
    assert_eq!(trimmed.waveform.len(), 100);
    assert!(is_ogg_opus_header(&trimmed.content));

    let joined = concat_ogg_opus(&trimmed.content, &ogg_b).expect("concat A+B");
    assert!((joined.duration_ms - 2000).abs() <= 80);
    assert_eq!(joined.waveform.len(), 100);
    let (_, _, _, dec_ms) = decode_ogg_opus_to_i16(&joined.content).expect("decode joined");
    assert!((dec_ms - 2000).abs() <= 100);
}

// ============================================================================
// 2. Voice capture pipeline tests (media_audio_capture.cpp)
// ============================================================================

#[test]
fn capture_pipeline_skip_fade_level_metering_and_waveform() {
    let mut pipe = VoiceCapturePipeline::telegram_default().expect("init pipeline");
    let pcm = generate_sine_i16(440.0, DEFAULT_FREQUENCY, 2.0, 0.6);

    // Push in 50 ms chunks (matching OpenAL 50 ms timer in media_audio_capture.cpp:347).
    let chunk_size = (DEFAULT_FREQUENCY / 20) as usize; // 2400 samples = 50 ms
    let mut updates = Vec::new();
    for chunk in pcm.chunks(chunk_size) {
        if let Some(u) = pipe.push_mono_i16(chunk).expect("push chunk") {
            updates.push(u);
        }
    }

    // Should have received level updates roughly every >100 ms (~12-18 updates in 2 s).
    assert!(
        updates.len() >= 10,
        "expected >=10 updates, got {}",
        updates.len()
    );
    // Updates during the first 400 ms (19,200 samples) must report level == 0.
    for u in updates.iter().filter(|u| u.samples <= 19_200) {
        assert_eq!(
            u.level, 0,
            "level must be 0 during 400ms skip window: {u:?}"
        );
    }
    // Updates after 700 ms (33,600 samples) should report strong signal level (~0.6 * 32767 ≈ 19660).
    let steady_levels: Vec<u16> = updates
        .iter()
        .filter(|u| u.samples >= 40_000)
        .map(|u| u.level)
        .collect();
    assert!(!steady_levels.is_empty());
    assert!(
        steady_levels.iter().all(|&lvl| lvl > 15_000),
        "steady capture levels too low: {steady_levels:?}"
    );

    // Pause returns intermediate snapshot with non-empty waveform.
    let snap = pipe.set_paused(true).expect("pause snapshot");
    assert!(snap.duration_ms > 0);
    assert!(pipe.is_paused());
    // Samples pushed while paused are ignored.
    assert!(pipe.push_mono_i16(&pcm[..chunk_size]).unwrap().is_none());
    pipe.set_paused(false);

    let result = pipe.finish().expect("finish capture");
    assert!(is_ogg_opus_header(&result.bytes));
    assert_eq!(result.waveform.len(), 100);
    assert_eq!(result.waveform_5bit.len(), 63);
    assert!((result.duration_ms - 2000).abs() <= 40);

    // First ~20 bars correspond to the 400 ms muted skip window (400 / 2000 = 20%).
    assert!(
        result.waveform[..18].iter().all(|&b| b == 0),
        "first 400ms skip window should be silent in waveform: {:?}",
        &result.waveform[..25]
    );
    // Middle bars should be active.
    assert!(
        result.waveform[40..75].iter().all(|&b| b >= 15),
        "middle bars should reflect sine amplitude: {:?}",
        &result.waveform[40..75]
    );
    // Final bar should taper toward 0 from the 300ms tail fade-out.
    assert!(
        result.waveform[99] < result.waveform[60],
        "tail fade-out should reduce final waveform bar"
    );

    // Verify the produced .ogg file decodes cleanly back to ~2s mono 48 kHz.
    let mut reader = OggOpusStreamDecoder::new(std::io::Cursor::new(&result.bytes))
        .expect("decode captured .ogg");
    assert_eq!(reader.sample_rate(), 48_000);
    assert_eq!(reader.channels(), 1);
    assert!((reader.duration_ms() - 2000).abs() <= 60);
    assert!(reader.read_next_f32().unwrap().is_some());
}

#[test]
fn capture_pipeline_rejects_under_one_second_and_enforces_max_duration() {
    // Under 1 second produces empty bytes (media_audio_capture.cpp:505).
    let mut short = VoiceCapturePipeline::telegram_default().unwrap();
    let half_sec = generate_sine_i16(440.0, DEFAULT_FREQUENCY, 0.5, 0.5);
    short.push_mono_i16(&half_sec).unwrap();
    let res = short.finish().unwrap();
    assert!(res.bytes.is_empty());
    assert!(res.waveform.is_empty());
    assert_eq!(res.duration_ms, 0);

    // Max duration enforcement with a 2-second cap.
    let cfg = CaptureConfig {
        max_duration_secs: 2,
        ..CaptureConfig::default()
    };
    let mut capped = VoiceCapturePipeline::new(cfg).unwrap();
    let three_secs = generate_sine_i16(440.0, DEFAULT_FREQUENCY, 3.0, 0.5);
    let upd = capped
        .push_mono_i16(&three_secs)
        .unwrap()
        .expect("finish update");
    assert!(upd.finished);
    assert!(capped.is_finished());
    assert_eq!(capped.total_samples(), 2 * i64::from(DEFAULT_FREQUENCY));
}

// ============================================================================
// 3. Waveform algorithm & 5-bit packing known-answer tests
// ============================================================================

#[test]
fn waveform_known_answers_and_5bit_packing() {
    // Sample readers (media_audio.h:403-409).
    assert_eq!(read_one_sample_u8(0x80), 0);
    assert_eq!(read_one_sample_u8(0xFF), 127 * 256);
    assert_eq!(read_one_sample_u8(0x00), 128 * 256);
    assert_eq!(read_one_sample_i16(0), 0);
    assert_eq!(read_one_sample_i16(-1234), 1234);
    assert_eq!(read_one_sample_i16(i16::MIN), 32768);

    // normalize_peaks below floor (2500):
    // peaks = [0, 1250, 2500], avg = 1250, avg*1.8 = 2250 < 2500 -> peak = 2500
    // bars: 0*31/2500 = 0, 1250*31/2500 = 15, 2500*31/2500 = 31.
    assert_eq!(normalize_peaks(&[0, 1250, 2500]), vec![0, 15, 31]);

    // normalize_peaks above floor (2500):
    // peaks = [10000, 10000, 10000, 30000], sum = 60000, avg = 15000, peak = int(15000*1.8) = 27000
    // 10000*31/27000 = 11, min(30000, 27000)*31/27000 = 31.
    assert_eq!(
        normalize_peaks(&[10_000, 10_000, 10_000, 30_000]),
        vec![11, 11, 11, 31]
    );

    // CollectWaveform known answer with < 100 bins returns empty.
    assert!(collect_waveform_from_bins(&[50u8; 99]).is_empty());

    // CollectWaveform with exactly 100 bins of uniform 100 (sample = 100*256 = 25600):
    // avg = 25600, peak = int(25600 * 1.8) = 46080, bar = (25600 * 31) / 46080 = 17.
    let uniform = collect_waveform_from_bins(&[100u8; 100]);
    assert_eq!(uniform.len(), 100);
    assert!(uniform.iter().all(|&v| v == 17));

    // Exact 5-bit little-endian packing known vector:
    // Values [0, 1, 2, 3, 4, 5, 6, 7] (8 * 5 = 40 bits = 5 bytes):
    // Bitstream LSB-first:
    //   0: 00000 (bits 0..5)
    //   1: 00001 (bits 5..10) -> byte0 = 0b001_00000 = 0x20, byte1 low 2 bits = 0b00
    //   2: 00010 (bits 10..15) -> byte1 bits 2..7 = 0b00010 << 2 = 0x08
    //   3: 00011 (bits 15..20) -> byte1 bit 7 = 1 (0x88), byte2 low 4 bits = 0b0001
    //   4: 00100 (bits 20..25) -> byte2 high 4 bits = 0b0100 << 4 = 0x41, byte3 bit 0 = 0
    //   5: 00101 (bits 25..30) -> byte3 bits 1..6 = 0b00101 << 1 = 0x0A
    //   6: 00110 (bits 30..35) -> byte3 bits 6..8 = 0b10 << 6 = 0x8A, byte4 low 3 bits = 0b001
    //   7: 00111 (bits 35..40) -> byte4 high 5 bits = 0b00111 << 3 = 0x39
    let sample_vals = vec![0u8, 1, 2, 3, 4, 5, 6, 7];
    let packed = encode_5bit(&sample_vals);
    assert_eq!(packed, vec![0x20, 0x88, 0x41, 0x8A, 0x39]);
    assert_eq!(decode_5bit(&packed), sample_vals);

    // Full 100-sample waveform packs to 63 bytes and round-trips losslessly.
    let wave100: Vec<u8> = (0..100).map(|i| (i % 32) as u8).collect();
    let packed100 = encode_5bit(&wave100);
    assert_eq!(packed100.len(), 63);
    assert_eq!(decode_5bit(&packed100), wave100);

    // CaptureWaveformCollector incremental vs batch consistency on constant-amplitude signal.
    let mut collector = CaptureWaveformCollector::new(48_000);
    let const_pcm = vec![16_384i16; 48_000]; // 1 second = 100 bins of 480 samples
    collector.push_samples(&const_pcm);
    assert_eq!(collector.bin_count(), 100);
    let collected = collector.collect();
    assert_eq!(collected.len(), 100);
    // 16384 / 256 = 64 -> 64*256 = 16384 -> peak = int(16384*1.8) = 29491 -> 16384*31/29491 = 17.
    assert!(collected.iter().all(|&v| v == 17));
    assert_eq!(compute_waveform_from_i16(&const_pcm, 1), collected);
}

// ============================================================================
// 4. Playlist ordering rules (Default, Reverse, Shuffle, Repeat, Voice, TTL)
// ============================================================================

fn make_audio(doc: u64, msg: i64, kind: MediaKind) -> AudioMsgId {
    AudioMsgId::new(
        DocumentId(doc),
        FullMsgId::new(PeerId(100), MsgId(msg)),
        kind,
        5_000,
        0,
    )
}

#[test]
fn playlist_default_reverse_and_repeat_rules() {
    let entries: Vec<PlaylistEntry> = (1..=4)
        .map(|i| PlaylistEntry::new(make_audio(i, i as i64, MediaKind::AudioFile)))
        .collect();
    let mut rng = StdRng::seed_from_u64(42);
    let mut nav = PlaylistNavigator::new(AudioType::Song);
    nav.set_entries(entries.clone(), &entries[0].id, OrderMode::Default);

    // Default + Repeat::None walks 0 -> 1 -> 2 -> 3 -> None.
    assert!(!nav.previous_available(RepeatMode::None, OrderMode::Default));
    assert!(nav.next_available(RepeatMode::None, OrderMode::Default));
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Default, false, &mut rng),
        Some(entries[1].id)
    );
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Default, false, &mut rng),
        Some(entries[2].id)
    );
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Default, false, &mut rng),
        Some(entries[3].id)
    );
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Default, false, &mut rng),
        None
    );

    // Repeat::All wraps 3 -> 0 and 0 -> 3.
    assert_eq!(
        nav.step(1, RepeatMode::All, OrderMode::Default, false, &mut rng),
        Some(entries[0].id)
    );
    assert_eq!(
        nav.step(-1, RepeatMode::All, OrderMode::Default, false, &mut rng),
        Some(entries[3].id)
    );

    // OrderMode::Reverse walks backwards on +1 (3 -> 2 -> 1 -> 0 -> None).
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Reverse, false, &mut rng),
        Some(entries[2].id)
    );
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Reverse, false, &mut rng),
        Some(entries[1].id)
    );
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Reverse, false, &mut rng),
        Some(entries[0].id)
    );
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Reverse, false, &mut rng),
        None
    );
    // Reverse + Repeat::All wraps 0 -> 3 on +1.
    assert_eq!(
        nav.step(1, RepeatMode::All, OrderMode::Reverse, false, &mut rng),
        Some(entries[3].id)
    );
}

#[test]
fn playlist_voice_ignores_song_modes_skips_ttl_and_handles_unread() {
    let entries = vec![
        PlaylistEntry::new(make_audio(1, 10, MediaKind::VoiceMessage)).with_unread(false),
        PlaylistEntry::new(make_audio(2, 11, MediaKind::VoiceMessage))
            .with_ttl(true)
            .with_unread(true),
        PlaylistEntry::new(make_audio(3, 12, MediaKind::VideoMessage)).with_unread(false),
        PlaylistEntry::new(make_audio(4, 13, MediaKind::VoiceMessage)).with_unread(true),
    ];
    let mut rng = StdRng::seed_from_u64(7);
    let mut nav = PlaylistNavigator::new(AudioType::Voice);
    nav.set_entries(entries.clone(), &entries[0].id, OrderMode::Reverse);

    // Even if Reverse + Repeat::All is passed, Voice always forces Default + Repeat::None!
    assert_eq!(nav.effective_order(OrderMode::Reverse), OrderMode::Default);
    assert_eq!(nav.effective_repeat(RepeatMode::All), RepeatMode::None);

    // Stepping forward from 0 skips index 1 (has_ttl == true) and lands on 2 (round video).
    assert_eq!(
        nav.step(1, RepeatMode::All, OrderMode::Reverse, false, &mut rng),
        Some(entries[2].id)
    );
    // Next lands on 3, then stops (no wrap despite Repeat::All).
    assert_eq!(
        nav.step(1, RepeatMode::All, OrderMode::Reverse, false, &mut rng),
        Some(entries[3].id)
    );
    assert_eq!(
        nav.step(1, RepeatMode::All, OrderMode::Reverse, false, &mut rng),
        None
    );

    // With only_unread_voice == true, stepping from 0 skips 1 (TTL) AND 2 (already read) -> 3!
    nav.locate(&entries[0].id);
    assert_eq!(
        nav.step(1, RepeatMode::None, OrderMode::Default, true, &mut rng),
        Some(entries[3].id)
    );
}

#[test]
fn playlist_shuffle_visits_all_without_repeats_and_supports_back_forward() {
    let mut rng = StdRng::seed_from_u64(12345);
    let mut shuffle = ShuffleState::new(5);

    let mut visited = vec![0usize];
    let mut current = 0usize;
    while let Some(next) = shuffle.step(current, 1, false, &mut rng) {
        visited.push(next);
        current = next;
    }
    assert_eq!(
        visited.len(),
        5,
        "shuffle must visit every track exactly once"
    );
    let mut sorted = visited.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 1, 2, 3, 4]);

    // Stepping backward replays exact reverse history.
    let prev1 = shuffle.step(current, -1, false, &mut rng).expect("back 1");
    assert_eq!(prev1, visited[3]);
    let prev2 = shuffle.step(prev1, -1, false, &mut rng).expect("back 2");
    assert_eq!(prev2, visited[2]);
    // Stepping forward replays exact forward history.
    let fwd1 = shuffle.step(prev2, 1, false, &mut rng).expect("fwd 1");
    assert_eq!(fwd1, visited[3]);

    // With repeat_all == true, shuffle continues indefinitely without returning None.
    let mut cur = fwd1;
    for _ in 0..30 {
        let next = shuffle
            .step(cur, 1, true, &mut rng)
            .expect("repeat_all shuffle must always yield next");
        assert!(next < 5);
        cur = next;
    }
}

// ============================================================================
// 5. Speed / seek math, Resampler & Pitch-preserving TimeStretcher tests
// ============================================================================

#[test]
fn speed_and_seek_math_known_answers() {
    assert_eq!(clamp_speed(0.1), SPEED_MIN);
    assert_eq!(clamp_speed(5.0), SPEED_MAX);
    assert_eq!(clamp_speed(f64::NAN), 1.0);
    assert!(equal_speeds(1.70, 1.74));
    assert!(!equal_speeds(1.70, 1.80));

    // At 2.0x speed, 48,000 speed-dependent samples == 96,000 normal samples.
    assert_eq!(speed_independent_position(48_000, 2.0), 96_000);
    assert_eq!(speed_dependent_position(96_000, 2.0), 48_000);

    // At 0.5x speed, 96,000 speed-dependent samples == 48,000 normal samples.
    assert_eq!(speed_independent_position(96_000, 0.5), 48_000);
    assert_eq!(speed_dependent_position(48_000, 0.5), 96_000);

    // At 1.5x speed: 32,000 * 1.5 = 48,000.
    assert_eq!(speed_independent_position(32_000, 1.5), 48_000);
    assert_eq!(speed_dependent_position(48_000, 1.5), 32_000);

    // Seek progress math:
    assert_eq!(compute_seek_position_ms(0.0, 10_000), 0);
    assert_eq!(compute_seek_position_ms(0.25, 10_000), 2_500);
    assert_eq!(compute_seek_position_ms(1.0, 10_000), 10_000);
    assert_eq!(compute_seek_position_ms(1.5, 10_000), 10_000);
    assert_eq!(compute_seek_position_ms(-0.2, 10_000), 0);
    assert_eq!(compute_seek_position_ms(0.5, 0), 0);
}

#[test]
fn time_stretcher_scales_duration_and_preserves_pitch() {
    let input = generate_sine_f32(440.0, DEFAULT_FREQUENCY, 1.5, 2, 0.5);
    let in_frames = input.len() / 2;

    for speed in [0.5, 1.5, 2.0] {
        let mut stretcher = TimeStretcher::new(DEFAULT_FREQUENCY, 2, speed);
        let mut out = Vec::new();
        for chunk in input.chunks(4096) {
            stretcher.process(chunk, &mut out);
        }
        stretcher.flush(&mut out);

        let out_frames = out.len() / 2;
        let expected_frames = ((in_frames as f64) / speed).round() as usize;
        let len_err_ratio =
            (out_frames as f64 - expected_frames as f64).abs() / (expected_frames as f64);
        assert!(
            len_err_ratio < 0.08,
            "speed {speed}x: out_frames={out_frames}, expected≈{expected_frames} (err={len_err_ratio:.3})"
        );

        // Trim 15% from each edge and verify pitch stays near 440 Hz (not 440 * speed!).
        let margin = (out_frames / 6) * 2;
        let mid = &out[margin..out.len() - margin];
        let freq = estimate_freq_hz(mid, 2, DEFAULT_FREQUENCY);
        assert!(
            (freq - 440.0).abs() < 15.0,
            "speed {speed}x altered pitch: estimated {freq:.1} Hz (expected ~440 Hz)"
        );
    }
}

#[test]
fn resampler_44100_to_48000_preserves_frequency_and_duration() {
    let input = generate_sine_f32(440.0, 44_100, 1.0, 2, 0.6);
    let out = Resampler::resample_all(&input, 44_100, 48_000, 2);
    let out_frames = out.len() / 2;
    assert!(
        (out_frames as i64 - 48_000).abs() <= 16,
        "resampled frames {out_frames} not ~48000"
    );
    let freq = estimate_freq_hz(&out[4000..out.len() - 4000], 2, 48_000);
    assert!(
        (freq - 440.0).abs() < 4.0,
        "resampled frequency {freq:.2} Hz not ~440 Hz"
    );
}

// ============================================================================
// 6. Mixer, fades, cubic volume curve, suppression, and UI SoundTrack tests
// ============================================================================

#[test]
fn mixer_fades_volume_suppression_and_sound_tracks() {
    // Cubic volume curve check (`media_audio.cpp:251-263`).
    assert!((compute_volume(AudioType::Song, 0.5, 1.0, 1.0, 1.0) - 0.125).abs() < 1e-9);
    assert!(
        (compute_volume(AudioType::Song, 1.0, 1.0, 1.0, SUPPRESS_RATIO_SONG)
            - SUPPRESS_RATIO_SONG.powi(3))
        .abs()
            < 1e-9
    );
    assert!(
        (compute_volume(AudioType::Voice, 1.0, 1.0, SUPPRESS_RATIO_ALL, 1.0)
            - SUPPRESS_RATIO_ALL.powi(3))
        .abs()
            < 1e-9
    );

    let mut mixer = MixerCore::new();
    let song_id = make_audio(10, 1, MediaKind::AudioFile);
    let song_pcm = vec![0.5f32; (DEFAULT_FREQUENCY as usize) * 2 * 2]; // 2 seconds stereo DC 0.5
    mixer
        .play(
            song_id,
            Box::new(MemoryPcmStream::new(song_pcm, DEFAULT_FREQUENCY, 2)),
            0,
            1.0,
            false,
        )
        .unwrap();

    // Pause with 500 ms fade-out -> state becomes Pausing, ramps down to Paused after 500 ms.
    assert!(mixer.pause(song_id, false));
    assert_eq!(
        mixer.current_state(AudioType::Song).state,
        PlaybackState::Pausing
    );

    let mut first_10ms = vec![0.0f32; 480 * 2];
    mixer.render_stereo_48k(&mut first_10ms);
    let early_amp = first_10ms[0];
    assert!(
        early_amp > 0.45,
        "fade-out should start near full amplitude"
    );

    // Render remaining ~500 ms so fade completes -> Paused.
    let fade_frames = ((FADE_DURATION_MS * i64::from(DEFAULT_FREQUENCY)) / 1000) as usize;
    let mut buf = vec![0.0f32; fade_frames * 2];
    let events = mixer.render_stereo_48k(&mut buf);
    assert_eq!(
        mixer.current_state(AudioType::Song).state,
        PlaybackState::Paused
    );
    assert!(events.contains(&MixerEvent::StateChanged(song_id, PlaybackState::Paused)));

    // Resume with 500 ms fade-in -> starts near 0 and reaches Playing.
    assert!(mixer.resume(song_id, false));
    assert_eq!(
        mixer.current_state(AudioType::Song).state,
        PlaybackState::Resuming
    );
    let mut resume_head = vec![0.0f32; 64 * 2];
    mixer.render_stereo_48k(&mut resume_head);
    assert!(resume_head[0].abs() < 0.05, "fade-in must start near 0");

    // Playing a Voice suppresses Song down to SUPPRESS_RATIO_SONG (0.05) over 500 ms.
    let voice_id = make_audio(20, 2, MediaKind::VoiceMessage);
    let voice_pcm = vec![0.25f32; (DEFAULT_FREQUENCY as usize) * 2]; // 1 second stereo
    mixer
        .play(
            voice_id,
            Box::new(MemoryPcmStream::new(voice_pcm, DEFAULT_FREQUENCY, 2)),
            0,
            1.0,
            false,
        )
        .unwrap();
    let mut suppress_buf = vec![0.0f32; fade_frames * 2];
    mixer.render_stereo_48k(&mut suppress_buf);
    let (_, mult_song) = mixer.suppression_multipliers();
    assert!(
        (mult_song - SUPPRESS_RATIO_SONG).abs() < 0.01,
        "song multiplier should reach {SUPPRESS_RATIO_SONG}, got {mult_song}"
    );

    // Global suppression (`suppress_all`) ducks Voice to SUPPRESS_RATIO_ALL (0.2).
    mixer.suppress_all(1_000);
    let mut attack_buf = vec![0.0f32; (DEFAULT_FREQUENCY as usize / 5) * 2]; // 200 ms > 150 ms attack
    mixer.render_stereo_48k(&mut attack_buf);
    let (mult_all, _) = mixer.suppression_multipliers();
    assert!(
        (mult_all - SUPPRESS_RATIO_ALL).abs() < 0.01,
        "all multiplier should reach {SUPPRESS_RATIO_ALL}, got {mult_all}"
    );

    // UI SoundTrack with peak metering & one-shot completion event.
    let chime_pcm = generate_sine_f32(880.0, DEFAULT_FREQUENCY, 0.1, 1, 0.8);
    let mut sound = SoundTrack::from_pcm_f32(&chime_pcm, DEFAULT_FREQUENCY, 1, 10).unwrap();
    assert_eq!(sound.length_ms(), 100);
    sound.play_once(Some(0.5), 100);
    let sid = mixer.play_sound_track(sound, false, Some(0.5));
    let mut chime_out = vec![0.0f32; (DEFAULT_FREQUENCY as usize / 5) * 2]; // 200 ms
    let chime_events = mixer.render_stereo_48k(&mut chime_out);
    assert!(chime_events.contains(&MixerEvent::SoundTrackFinished(sid)));

    // After stopping everything and rendering 600 ms of silence, should_detach_device() is true.
    mixer.stop_and_clear();
    let mut silence = vec![0.0f32; ((DEFAULT_FREQUENCY as usize) * 6 / 10) * 2];
    mixer.render_stereo_48k(&mut silence);
    assert!(mixer.should_detach_device());
}

// ============================================================================
// 7. High-level PlayerInstance: autoplay, pause-on-call, voice/song, resume pos
// ============================================================================

#[test]
fn player_instance_voice_autoplay_call_pause_and_resume_positions() {
    let source = Arc::new(MemoryAudioSource::new());
    let positions = Arc::new(InMemoryPositionStore::new());
    let playlists = Arc::new(InMemoryPlaylistProvider::new());
    let mixer = Arc::new(Mutex::new(MixerCore::new()));

    // Create two short 0.2s Ogg Opus voice messages and one 0.5s WAV song.
    let v1_ogg = encode_ogg_opus_i16(
        &generate_sine_i16(440.0, DEFAULT_FREQUENCY, 0.2, 0.4),
        DEFAULT_FREQUENCY,
        1,
        32_000,
        Application::Voip,
    )
    .unwrap();
    let v2_ogg = encode_ogg_opus_i16(
        &generate_sine_i16(550.0, DEFAULT_FREQUENCY, 0.2, 0.4),
        DEFAULT_FREQUENCY,
        1,
        32_000,
        Application::Voip,
    )
    .unwrap();
    let song_wav = encode_wav_pcm_s16le(
        &f32_to_i16(&generate_sine_f32(330.0, 44_100, 1.0, 2, 0.4)),
        44_100,
        2,
    );

    let v1 = make_audio(101, 1, MediaKind::VoiceMessage);
    let v2 = make_audio(102, 2, MediaKind::VoiceMessage);
    let s1 = make_audio(201, 3, MediaKind::AudioFile);

    source.insert(v1.document_id, v1_ogg, Some("ogg".into()));
    source.insert(v2.document_id, v2_ogg, Some("ogg".into()));
    source.insert(s1.document_id, song_wav, Some("wav".into()));

    let ctx = PlaylistContext {
        peer_id: PeerId(100),
        ..PlaylistContext::default()
    };
    playlists.set_playlist(
        ctx,
        AudioType::Voice,
        vec![
            PlaylistEntry::new(v1).with_unread(true),
            PlaylistEntry::new(v2).with_unread(true),
        ],
    );
    playlists.set_playlist(ctx, AudioType::Song, vec![PlaylistEntry::new(s1)]);

    let mut player = PlayerInstance::new(
        Arc::clone(&mixer),
        None,
        source,
        Arc::clone(&positions) as Arc<dyn ResumePositionStore>,
        Arc::clone(&playlists) as Arc<dyn PlaylistProvider>,
    );

    // Start playing Song, then start Voice 1 -> Song must pause automatically.
    player.play_item(s1, Some(ctx)).unwrap();
    assert_eq!(
        player.get_state(AudioType::Song).state,
        PlaybackState::Playing
    );

    player.play_item(v1, Some(ctx)).unwrap();
    assert_eq!(player.active_type(), AudioType::Voice);
    assert!(
        player
            .get_state(AudioType::Song)
            .state
            .is_paused_or_pausing()
    );
    assert_eq!(playlists.marked_read().len(), 1);

    // Incoming call pauses active Voice; ending call resumes it.
    player.set_in_call(true).unwrap();
    assert!(
        player
            .get_state(AudioType::Voice)
            .state
            .is_paused_or_pausing()
    );
    player.set_in_call(false).unwrap();
    assert!(player.get_state(AudioType::Voice).state.show_pause_icon());

    // Render until Voice 1 finishes -> player should auto-advance to Voice 2!
    let mut mix_buf = vec![0.0f32; 4800 * 2]; // 100 ms per step
    let mut switched = false;
    for _ in 0..20 {
        let evs = mixer.lock().unwrap().render_stereo_48k(&mut mix_buf);
        player.handle_mixer_events(&evs).unwrap();
        for pev in player.take_events() {
            if matches!(pev, PlayerEvent::SwitchToNext { from, to } if from == v1 && to == v2.context_id)
            {
                switched = true;
            }
        }
        if switched {
            break;
        }
    }
    assert!(switched, "Voice 1 finishing should auto-advance to Voice 2");
    assert_eq!(player.current(AudioType::Voice), v2);
    assert_eq!(playlists.marked_read().len(), 2);

    // Render until Voice 2 finishes -> Song should resume automatically!
    let mut finished_voices = false;
    for _ in 0..20 {
        let evs = mixer.lock().unwrap().render_stereo_48k(&mut mix_buf);
        player.handle_mixer_events(&evs).unwrap();
        for pev in player.take_events() {
            if pev == PlayerEvent::TracksFinished(AudioType::Voice) {
                finished_voices = true;
            }
        }
        if finished_voices {
            break;
        }
    }
    assert!(
        finished_voices,
        "Voice 2 finishing should emit TracksFinished(Voice)"
    );
    assert!(
        player.get_state(AudioType::Song).state.show_pause_icon(),
        "Song should resume after voice playlist completes"
    );

    // Seeking lifecycle events & state.
    player.start_seeking(AudioType::Song);
    assert!(player.is_seeking(AudioType::Song));
    player.update_seeking(AudioType::Song, 0.5).unwrap();
    assert!((player.get_state(AudioType::Song).position_ms() - 500).abs() <= 20);
    player.finish_seeking(AudioType::Song, 0.5).unwrap();
    assert!(!player.is_seeking(AudioType::Song));
    let seek_events = player.take_events();
    assert!(seek_events.contains(&PlayerEvent::SeekingChanged {
        audio_type: AudioType::Song,
        phase: SeekingPhase::Finish,
    }));

    // Resume position threshold rules (`SaveLastPlaybackPosition`):
    // Music < 20 min does NOT save position; music >= 20 min saves whole seconds in ms.
    let short_song = make_audio(300, 10, MediaKind::AudioFile);
    let long_song = make_audio(301, 11, MediaKind::AudioFile);
    let video = make_audio(302, 12, MediaKind::VideoFile);

    let short_state = TrackState {
        id: short_song,
        state: PlaybackState::Paused,
        position: 30 * 48_000,
        length: 600 * 48_000, // 10 min < 20 min
        frequency: 48_000,
        ..TrackState::default()
    };
    save_last_playback_position(&short_song, &short_state, positions.as_ref());
    assert_eq!(positions.get_position_ms(short_song.document_id), 0);

    let long_state = TrackState {
        id: long_song,
        state: PlaybackState::Paused,
        position: 125 * 48_000 + 24_000, // 125.5 s -> truncates to 125_000 ms (matches C++)
        length: 1_500 * 48_000,          // 25 min >= 20 min
        frequency: 48_000,
        ..TrackState::default()
    };
    save_last_playback_position(&long_song, &long_state, positions.as_ref());
    assert_eq!(positions.get_position_ms(long_song.document_id), 125_000);

    let video_state = TrackState {
        id: video,
        state: PlaybackState::Playing,
        position: 42 * 48_000,
        length: 90 * 48_000, // 90 s >= 60 s
        frequency: 48_000,
        ..TrackState::default()
    };
    save_last_playback_position(&video, &video_state, positions.as_ref());
    assert_eq!(positions.get_position_ms(video.document_id), 42_000);
}

// ============================================================================
// 8. LocalCache / LocalDiskCache & Symphonia WAV decoding tests
// ============================================================================

#[test]
fn local_cache_converts_cuts_to_3s_44100_wav_and_caches_to_disk() {
    // Build a 5-second 48 kHz mono WAV -> convert_and_cut_to_wav must resample to 44.1 kHz
    // and truncate to 3.0 seconds.
    let long_pcm = generate_sine_i16(600.0, 48_000, 5.0, 0.5);
    let long_wav = encode_wav_pcm_s16le(&long_pcm, 48_000, 1);

    let cut_wav = convert_and_cut_to_wav(&long_wav).expect("convert_and_cut_to_wav");
    assert_eq!(&cut_wav[..4], b"RIFF");
    assert_eq!(&cut_wav[8..12], b"WAVE");

    // Verify decoded output is 44.1 kHz mono and <= 3000 ms.
    let dec = AudioDecoder::from_bytes(cut_wav.clone(), Some("wav".into()))
        .expect("Symphonia should decode generated WAV");
    assert_eq!(dec.sample_rate(), LOCAL_SOUND_SAMPLE_RATE);
    assert_eq!(dec.channels(), 1);
    assert!(
        dec.duration_ms() <= LOCAL_SOUND_MAX_DURATION_MS
            && dec.duration_ms() >= LOCAL_SOUND_MAX_DURATION_MS - 50,
        "cut WAV duration {} ms expected ~3000 ms",
        dec.duration_ms()
    );

    // Also test count_waveform_from_bytes on WAV via Symphonia.
    let wf = count_waveform_from_bytes(cut_wav, Some("wav".into())).expect("waveform from WAV");
    assert_eq!(wf.len(), 100);

    // LocalCache fallback to default sound (DocumentId(0)) when primary bytes are invalid.
    let mut cache = LocalCache::new();
    let fallback_wav = encode_wav_pcm_s16le(&generate_sine_i16(800.0, 44_100, 0.2, 0.5), 44_100, 1);
    let sound = cache.sound(
        DocumentId(0xBEEF),
        || b"not valid audio".to_vec(),
        Some(|| fallback_wav.clone()),
    );
    assert!(sound.is_valid());
    assert_eq!(sound.id, DocumentId(0));

    // LocalDiskCache writes TD_Default.wav and TD_CAFE.wav.
    let dir = std::env::temp_dir().join(format!("tg-audio-cache-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut disk = LocalDiskCache::new(&dir).expect("create disk cache");
    let def_name = disk.name(&sound).unwrap().expect("default name");
    assert_eq!(def_name, "TD_Default");
    let def_path = disk.path(&sound).unwrap().expect("default path");
    assert!(def_path.exists());

    let custom = cache.sound::<_, fn() -> Vec<u8>>(DocumentId(0xCAFE), || fallback_wav, None);
    assert_eq!(custom.id, DocumentId(0xCAFE));
    let custom_name = disk.name(&custom).unwrap().expect("custom name");
    assert_eq!(custom_name, "TD_CAFE");
    let _ = std::fs::remove_dir_all(&dir);
}

// ============================================================================
// 9. FloatController non-visual layout & drag tests
// ============================================================================

#[test]
fn float_controller_visibility_snapping_and_dismissal() {
    let parent = Size::new(1000, 700);
    let widget = Size::new(200, 200);
    let mut ctrl = FloatController::new(parent, widget);
    ctrl.set_layout(
        parent,
        vec![
            SectionArea {
                column: FloatColumn::First,
                available_rect: Rect::new(0, 0, 300, 700),
            },
            SectionArea {
                column: FloatColumn::Second,
                available_rect: Rect::new(300, 0, 700, 700),
            },
        ],
    );

    // out_ratio checks.
    assert!(
        (compute_out_ratio(Rect::new(100, 100, 200, 200), Rect::new(0, 0, 1000, 700)) - 1.0).abs()
            < 1e-9
    );
    assert!(
        (compute_out_ratio(Rect::new(-100, 100, 200, 200), Rect::new(0, 0, 1000, 700)) - 0.5).abs()
            < 1e-9
    );
    assert!(
        (compute_out_ratio(Rect::new(-120, 100, 200, 200), Rect::new(0, 0, 1000, 700)) - 0.4).abs()
            < 1e-9
    );

    let msg = FullMsgId::new(PeerId(1), MsgId(55));
    ctrl.set_current_round_video(Some(msg));
    ctrl.update_visibility(true, true); // visible in chat history -> float hidden
    assert!(!ctrl.current().unwrap().visible);

    ctrl.update_visibility(true, false); // scrolled away -> float visible at Second/TopRight
    assert!(ctrl.current().unwrap().visible);
    assert_eq!(ctrl.current().unwrap().position, Point::new(800, 0));

    // Drag near bottom-left of First column -> snaps to First / BottomLeft without closing.
    ctrl.drag_to(Point::new(20, 480));
    let outcome = ctrl.finish_drag().expect("snap outcome");
    assert!(!outcome.closed);
    assert_eq!(outcome.column, FloatColumn::First);
    assert_eq!(outcome.corner, FloatCorner::BottomLeft);
    assert_eq!(outcome.target_position, Point::new(0, 500));

    // Drag >50% off the left edge -> dismisses and reports closed_item.
    ctrl.drag_to(Point::new(-130, 480));
    let dismissed = ctrl.finish_drag().expect("dismiss outcome");
    assert!(dismissed.closed);
    assert_eq!(dismissed.closed_item, Some(msg));
}

// ============================================================================
// 10. DeviceResolver & hardware enumeration tests (gracefully skip if no device)
// ============================================================================

#[test]
fn device_resolver_tracks_default_and_handles_disconnect_reconnect() {
    let devs = vec![
        DeviceInfo {
            id: "dev-a".into(),
            name: "Speakers".into(),
            device_type: DeviceType::Playback,
            is_default: true,
            inactive: false,
        },
        DeviceInfo {
            id: "dev-b".into(),
            name: "Headphones".into(),
            device_type: DeviceType::Playback,
            is_default: false,
            inactive: false,
        },
    ];

    let mut resolver = DeviceResolver::new(DeviceType::Playback, DEFAULT_DEVICE_ID);
    assert!(resolver.resolve(&devs, "dev-a", DeviceChangeReason::Manual));
    assert!(resolver.current().is_default());
    assert_eq!(resolver.current().value, "dev-a");

    // Switching OS default updates resolved ID automatically.
    assert!(resolver.resolve(&devs, "dev-b", DeviceChangeReason::Connected));
    assert_eq!(resolver.current().value, "dev-b");
    assert!(resolver.current().is_default());

    // Selecting a concrete preferred device pins to it.
    assert!(resolver.set_saved_id("dev-b", &devs, "dev-a"));
    assert!(!resolver.current().is_default());
    assert_eq!(resolver.current().value, "dev-b");

    // Disconnecting preferred device falls back to OS default with Disconnected reason.
    let only_a = vec![devs[0].clone()];
    assert!(resolver.resolve(&only_a, "dev-a", DeviceChangeReason::Manual));
    assert!(resolver.current().is_default());
    assert_eq!(resolver.current().value, "dev-a");
    assert_eq!(
        resolver.last_change_reason(),
        DeviceChangeReason::Disconnected
    );

    // Reconnecting preferred device switches back automatically with Connected reason.
    assert!(resolver.resolve(&devs, "dev-a", DeviceChangeReason::Manual));
    assert!(!resolver.current().is_default());
    assert_eq!(resolver.current().value, "dev-b");
    assert_eq!(resolver.last_change_reason(), DeviceChangeReason::Connected);
}

#[test]
fn cpal_device_enumeration_skips_when_no_audio_device() {
    let mgr = DeviceManager::new();
    if !mgr.has_playback_device() {
        eprintln!("skipping hardware playback check: no audio output device available");
        return;
    }
    let playback_devices = mgr
        .devices(DeviceType::Playback)
        .expect("enumerate playback");
    assert!(
        !playback_devices.is_empty(),
        "default playback device exists so device list should be non-empty"
    );
    let _ = mgr
        .open_cpal_device(DeviceType::Playback)
        .expect("open default playback device");
}
