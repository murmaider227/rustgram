//! Wave 6 (TGB-3064): untrusted voice files and waveforms must never panic.

use std::io::Cursor;

use opus_pure::Application;
use tg_audio::opus_codec::{
    OggOpusStreamDecoder, decode_ogg_opus_to_f32, encode_ogg_opus_i16, is_ogg_opus_header,
};
use tg_audio::waveform::{decode_5bit, encode_5bit};

/// Deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n.max(1) as u64)) as usize
    }
}

/// 300 ms of a 440 Hz tone as a mono 48 kHz voice message.
fn voice_note() -> Vec<u8> {
    let pcm: Vec<i16> = (0..14_400)
        .map(|i| ((f64::from(i) * 440.0 * std::f64::consts::TAU / 48_000.0).sin() * 8000.0) as i16)
        .collect();
    encode_ogg_opus_i16(&pcm, 48_000, 1, 32_000, Application::Voip).expect("encode")
}

/// Byte offset of the last Ogg page ("OggS" capture pattern).
fn last_page(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .rposition(|w| w == b"OggS")
        .expect("ogg page")
}

/// The duration comes from the last page's granule position, a 64-bit value read from the file.
/// A huge granule overflowed `frames * 1000` (a panic in debug builds, a negative duration in
/// release).
#[test]
fn huge_last_granule_gives_a_sane_duration() {
    let mut bytes = voice_note();
    let page = last_page(&bytes);
    bytes[page + 6..page + 14].copy_from_slice(&0x7FFF_FFFF_FFFF_FFF0i64.to_le_bytes());
    let decoder = OggOpusStreamDecoder::new(Cursor::new(bytes)).expect("header still parses");
    assert!(decoder.duration_ms() >= 0, "{}", decoder.duration_ms());
    assert!(decoder.total_frames() >= 0);
}

#[test]
fn mutated_voice_notes_never_panic() {
    let seed = voice_note();
    let (pcm, rate, channels, duration) = decode_ogg_opus_to_f32(&seed).expect("seed decodes");
    assert_eq!((rate, channels), (48_000, 1));
    assert!(!pcm.is_empty() && duration > 0);

    let mut rng = Rng(0x5DEE_CE66_D1CE_4E5B);
    for round in 0..400 {
        let mut bytes = seed.clone();
        match round % 4 {
            0 => {
                let at = rng.below(bytes.len());
                bytes[at] ^= (rng.next() as u8) | 1;
            }
            1 => {
                let keep = rng.below(bytes.len());
                bytes.truncate(keep);
            }
            2 => {
                // Page header fields: version, flags, granule, serial, sequence, segment count.
                let page = last_page(&bytes[..bytes.len() / (1 + rng.below(3))]);
                let at = page + 4 + rng.below(23);
                if at < bytes.len() {
                    bytes[at] = rng.next() as u8;
                }
            }
            _ => {
                let at = rng.below(bytes.len());
                let len = rng.below(32).min(bytes.len() - at);
                for b in &mut bytes[at..at + len] {
                    *b = rng.next() as u8;
                }
            }
        }
        let _ = is_ogg_opus_header(&bytes);
        if let Ok(mut decoder) = OggOpusStreamDecoder::new(Cursor::new(bytes.clone())) {
            assert!(decoder.duration_ms() >= 0);
            let _ = decoder.seek_ms(150);
            while let Ok(Some(_)) = decoder.read_next_i16() {}
        }
        let _ = decode_ogg_opus_to_f32(&bytes);
    }
}

#[test]
fn ogg_header_check_survives_any_prefix() {
    let seed = voice_note();
    assert!(is_ogg_opus_header(&seed));
    for len in 0..seed.len().min(128) {
        assert!(!is_ogg_opus_header(&seed[..len]) || len >= 36);
    }
    let mut bytes = seed[..64].to_vec();
    bytes[26] = 0xFF; // segment count pointing far past the prefix
    assert!(!is_ogg_opus_header(&bytes));
}

#[test]
fn waveform_5bit_round_trips_and_decodes_any_bytes() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    for len in 0..300 {
        let values: Vec<u8> = (0..len).map(|_| (rng.next() % 32) as u8).collect();
        let packed = encode_5bit(&values);
        let unpacked = decode_5bit(&packed);
        assert_eq!(&unpacked[..values.len()], &values[..], "len {len}");
        let noise: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        assert!(decode_5bit(&noise).iter().all(|&v| v < 32));
    }
}
