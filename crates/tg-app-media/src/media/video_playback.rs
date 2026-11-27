//! Shared video playback plumbing for the media viewer and Picture-in-Picture.
//!
//! The desktop binary decodes on a background thread ([`ThreadedDecoder`], like tdesktop's
//! `VideoTrackObject` thread) so the UI thread never waits for FFmpeg or for streamed bytes;
//! tests and screenshot harnesses keep decoding synchronously so each step is deterministic
//! (the same switch as `inline_media::enable_background_decode`).

use std::time::Duration;

use tg_video::{Decoder, FfmpegStreamDecoder, FramePopOutcome, StreamingPlayer, ThreadedDecoder};

/// Player type used by the media viewer and Picture-in-Picture.
pub type ViewerPlayer = StreamingPlayer<Box<dyn Decoder>>;

/// Longest wait between two playback steps (one 60 Hz frame).
const MAX_STEP_DELAY_MS: i64 = 16;

/// Wraps `decoder` for playback: on a worker thread in the app, inline in tests.
pub fn viewer_decoder(decoder: FfmpegStreamDecoder) -> tg_video::Result<Box<dyn Decoder>> {
    if tg_emoji::background::background_decode_enabled() {
        Ok(Box::new(ThreadedDecoder::spawn(decoder)?))
    } else {
        Ok(Box::new(decoder))
    }
}

/// Drops decoded audio when no sound output is attached (see `StreamingPlayer::attach_audio`),
/// so the queue does not grow for the whole video (about 11 MB per minute of 48 kHz stereo).
pub fn drop_unplayed_audio(player: &mut ViewerPlayer) {
    while player.pop_audio_frame().is_some() {}
}

/// When to step playback again: when the next frame is due, at most one 60 Hz frame later.
#[must_use]
pub fn next_step_delay(outcome: Option<&FramePopOutcome>) -> Duration {
    let ms = match outcome {
        Some(FramePopOutcome::Wait { delay_ms }) => (*delay_ms).clamp(1, MAX_STEP_DELAY_MS),
        _ => MAX_STEP_DELAY_MS,
    };
    Duration::from_millis(ms.unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_delay_follows_the_next_due_frame() {
        let wait = |delay_ms| FramePopOutcome::Wait { delay_ms };
        assert_eq!(next_step_delay(Some(&wait(4))), Duration::from_millis(4));
        assert_eq!(next_step_delay(Some(&wait(40))), Duration::from_millis(16));
        assert_eq!(next_step_delay(Some(&wait(0))), Duration::from_millis(1));
        assert_eq!(
            next_step_delay(Some(&FramePopOutcome::Empty)),
            Duration::from_millis(16)
        );
        assert_eq!(next_step_delay(None), Duration::from_millis(16));
    }
}
