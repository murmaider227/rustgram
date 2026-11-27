//! Where a [`crate::StreamingPlayer`] sends its decoded sound, and the audio clock it follows.
//!
//! tdesktop's `Media::Streaming::Player` owns an `AudioTrack` that feeds the mixer from the
//! streaming thread, and its `VideoTrack` takes the time from the mixer's played position
//! (`Mixer::getExternalSyncTimePoint`, used by `VideoTrackObject::trackTime` with
//! `syncVideoByAudio`). These traits are that seam: `tg-video` knows no audio device, the app
//! plugs in its mixer.

use std::sync::Arc;

use crate::streaming::clock::TimePoint;
use crate::types::AudioFrame;

/// Receives decoded audio frames in decode order. Called on whichever thread decodes: the
/// [`crate::ThreadedDecoder`] worker in the app, the UI thread with a synchronous decoder.
pub trait AudioSink: Send + Sync {
    /// Queues one decoded frame for playback.
    fn push(&self, frame: AudioFrame);

    /// Drops every frame pushed so far (a seek). A decoder that pushes from its own thread
    /// calls this atomically with the seek, so no frame from before the seek survives it and
    /// no frame from after it is lost.
    fn clear(&self);
}

/// The sound output of one player (tdesktop's `AudioTrack` plus the mixer's video slot).
///
/// The player drives it: it pauses and resumes it with playback (also while waiting for data),
/// resets it on a seek, forwards speed and volume, and asks it each step where the sound is.
pub trait PlayerAudio: Send {
    /// The sink decoded frames go to (shared with the decoding thread).
    fn sink(&self) -> Arc<dyn AudioSink>;

    /// Stops the sound for a seek. The player calls it before the decoder seeks; the queued
    /// frames are then dropped through [`AudioSink::clear`], and the first frame pushed
    /// afterwards restarts the sound.
    fn reset(&mut self);

    /// Plays (`true`) or pauses (`false`) the sound.
    fn set_playing(&mut self, playing: bool);

    /// Playback speed multiplier.
    fn set_speed(&mut self, speed: f64);

    /// Volume `0.0..=1.0`.
    fn set_volume(&mut self, volume: f64);

    /// Where the sound is heard now: `track_time_ms` reached the listener at `world_time_ms`
    /// (in the caller's world clock). `None` until the output has played a sample.
    fn sync_point(&mut self, world_now_ms: i64) -> Option<TimePoint>;
}
