//! The resampler keeps its 3-frame history in place (TGB-3068 removed a per-call allocation);
//! the output must not depend on how the input is split into chunks, including chunks shorter
//! than the history.

use tg_audio::Resampler;

fn signal(frames: usize, channels: usize) -> Vec<f32> {
    (0..frames * channels)
        .map(|i| ((i as f32) * 0.013).sin() * 0.5 + ((i % 7) as f32) * 0.01)
        .collect()
}

#[test]
fn resampled_output_is_the_same_for_any_chunking() {
    for channels in [1usize, 2, 6] {
        let input = signal(5_000, channels);
        let whole = Resampler::resample_all(&input, 44_100, 48_000, channels);
        for pattern in [&[1usize][..], &[2], &[3], &[1, 2, 5, 1, 4096], &[2048]] {
            let mut r = Resampler::new(44_100, 48_000, channels);
            let mut out = Vec::new();
            let mut at = 0;
            let mut i = 0;
            while at < 5_000 {
                let frames = pattern[i % pattern.len()].min(5_000 - at);
                r.process(&input[at * channels..(at + frames) * channels], &mut out);
                at += frames;
                i += 1;
            }
            assert_eq!(
                out.len(),
                whole.len(),
                "channels {channels} pattern {pattern:?}"
            );
            let max_diff = out
                .iter()
                .zip(&whole)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                max_diff < 1e-6,
                "channels {channels} pattern {pattern:?}: max diff {max_diff}"
            );
        }
    }
}
