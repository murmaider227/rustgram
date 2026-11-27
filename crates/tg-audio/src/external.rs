//! PCM fed from outside the mixer: the sound of a streamed video.
//!
//! tdesktop's `Media::Streaming::AudioTrack` decodes on the streaming thread and hands packets
//! to the mixer (`Mixer::feedFromExternal`); the mixer plays them as they arrive and reports
//! `waitingForData` when it runs dry. [`ExternalPcmFeed`] is that hand-over: the video decoder
//! pushes decoded frames from its own thread, and [`ExternalPcmStream`] is the
//! [`AudioStream`] the mixer's video slot reads.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::error::AudioResult;
use crate::mixer::{AudioStream, PcmChunk};
use crate::types::DEFAULT_FREQUENCY;

/// Most audio a feed keeps when nothing plays it (for example with no output device): older
/// audio is dropped so the queue stays bounded.
pub const EXTERNAL_FEED_MAX_MS: i64 = 20_000;

#[derive(Default)]
struct FeedState {
    chunks: VecDeque<PcmChunk>,
    queued_ms: i64,
    first_pts_ms: Option<i64>,
    ended: bool,
}

/// Shared queue of decoded PCM for one video's sound. Clones share the queue.
#[derive(Clone, Default)]
pub struct ExternalPcmFeed {
    state: Arc<Mutex<FeedState>>,
}

impl std::fmt::Debug for ExternalPcmFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.lock();
        f.debug_struct("ExternalPcmFeed")
            .field("chunks", &st.chunks.len())
            .field("queued_ms", &st.queued_ms)
            .field("first_pts_ms", &st.first_pts_ms)
            .field("ended", &st.ended)
            .finish()
    }
}

fn chunk_ms(chunk: &PcmChunk) -> i64 {
    let frames = chunk.samples.len() / chunk.channels.max(1);
    (frames as i64 * 1000) / i64::from(chunk.sample_rate.max(1))
}

impl ExternalPcmFeed {
    /// Creates an empty feed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues interleaved `samples` that start at media position `pts_ms`.
    pub fn push(&self, samples: Vec<f32>, sample_rate: u32, channels: usize, pts_ms: i64) {
        if samples.is_empty() || sample_rate == 0 || channels == 0 {
            return;
        }
        let chunk = PcmChunk {
            samples,
            sample_rate,
            channels,
        };
        let mut st = self.lock();
        st.first_pts_ms.get_or_insert(pts_ms);
        st.queued_ms += chunk_ms(&chunk);
        st.chunks.push_back(chunk);
        while st.queued_ms > EXTERNAL_FEED_MAX_MS && st.chunks.len() > 1 {
            if let Some(old) = st.chunks.pop_front() {
                st.queued_ms -= chunk_ms(&old);
            }
        }
    }

    /// Marks the end of the sound: the mixer finishes the track once the queue is played.
    pub fn finish(&self) {
        self.lock().ended = true;
    }

    /// Drops everything queued (a seek); the next pushed frame starts the sound again.
    pub fn clear(&self) {
        let mut st = self.lock();
        st.chunks.clear();
        st.queued_ms = 0;
        st.first_pts_ms = None;
        st.ended = false;
    }

    /// Media position of the first frame pushed since creation or the last [`Self::clear`].
    #[must_use]
    pub fn first_pts_ms(&self) -> Option<i64> {
        self.lock().first_pts_ms
    }

    /// Duration of the audio queued and not yet read by the mixer.
    #[must_use]
    pub fn queued_ms(&self) -> i64 {
        self.lock().queued_ms
    }

    /// A mixer stream reading this feed.
    #[must_use]
    pub fn stream(&self) -> ExternalPcmStream {
        ExternalPcmStream { feed: self.clone() }
    }
}

/// [`AudioStream`] over an [`ExternalPcmFeed`]. It cannot seek: the video player seeks the
/// decoder and refills the feed instead.
#[derive(Debug, Clone)]
pub struct ExternalPcmStream {
    feed: ExternalPcmFeed,
}

impl AudioStream for ExternalPcmStream {
    fn sample_rate(&self) -> u32 {
        self.feed
            .lock()
            .chunks
            .front()
            .map_or(DEFAULT_FREQUENCY, |c| c.sample_rate)
    }

    fn channels(&self) -> usize {
        self.feed.lock().chunks.front().map_or(2, |c| c.channels)
    }

    fn duration_ms(&self) -> i64 {
        0
    }

    fn seek_ms(&mut self, _position_ms: i64) -> AudioResult<()> {
        Ok(())
    }

    fn read_chunk(&mut self) -> AudioResult<Option<PcmChunk>> {
        let mut st = self.feed.lock();
        match st.chunks.pop_front() {
            Some(chunk) => {
                st.queued_ms -= chunk_ms(&chunk);
                Ok(Some(chunk))
            }
            None if st.ended => Ok(None),
            None => Ok(Some(PcmChunk {
                samples: Vec::new(),
                sample_rate: DEFAULT_FREQUENCY,
                channels: 2,
            })),
        }
    }

    fn waiting_for_data(&self) -> bool {
        let st = self.feed.lock();
        st.chunks.is_empty() && !st.ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::{ExternalSyncPoint, MixerCore};
    use crate::types::{AudioMsgId, PlaybackState};
    use std::time::{Duration, Instant};

    fn tone(ms: i64, rate: u32) -> Vec<f32> {
        let frames = (ms * i64::from(rate) / 1000) as usize;
        (0..frames)
            .flat_map(|i| {
                let v = ((i as f32) * 0.05).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    fn render(mixer: &mut MixerCore, ms: usize, at: Instant) -> Vec<f32> {
        let mut buf = vec![0.0_f32; ms * 48 * 2];
        let _ = mixer.render_stereo_48k_at(&mut buf, at);
        buf
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
    }

    #[test]
    fn video_sound_plays_from_the_feed_and_reports_where_it_is() {
        let feed = ExternalPcmFeed::new();
        feed.push(tone(200, 48_000), 48_000, 2, 1_000);
        let id = AudioMsgId::for_video();
        let mut mixer = MixerCore::new();
        mixer
            .play(id, Box::new(feed.stream()), 1_000, 1.0, false)
            .expect("play");

        let t0 = Instant::now();
        let first = render(&mut mixer, 50, t0);
        assert!(rms(&first) > 0.05, "the video sound is audible");
        assert_eq!(
            mixer.external_sync_point(id),
            Some(ExternalSyncPoint {
                audio: id,
                position_ms: 1_000,
                at: t0
            })
        );
        let t1 = t0 + Duration::from_millis(50);
        let _ = render(&mut mixer, 50, t1);
        let point = mixer.external_sync_point(id).expect("sync point");
        assert_eq!((point.position_ms, point.at), (1_050, t1));
        assert_eq!(
            mixer.external_sync_point(AudioMsgId::for_video()),
            None,
            "another video's sound has no sync point"
        );
    }

    #[test]
    fn a_dry_feed_plays_silence_and_holds_the_position_until_data_arrives() {
        let feed = ExternalPcmFeed::new();
        feed.push(tone(100, 44_100), 44_100, 2, 0);
        let id = AudioMsgId::for_video();
        let mut mixer = MixerCore::new();
        mixer
            .play(id, Box::new(feed.stream()), 0, 1.0, false)
            .expect("play");
        let t0 = Instant::now();
        let _ = render(&mut mixer, 100, t0);
        // The resampler keeps a few samples back, so the queue is now (nearly) dry.
        for i in 1..=5 {
            let silent = render(&mut mixer, 20, t0 + Duration::from_millis(100 + 20 * i));
            assert!(rms(&silent) < 0.05, "no data, no sound");
        }
        let held = mixer.external_sync_point(id).map(|p| p.position_ms);
        assert!(
            held.is_some_and(|ms| (80..=100).contains(&ms)),
            "position holds at the end of the data: {held:?}"
        );
        assert_eq!(
            mixer.current_state(crate::types::AudioType::Video).state,
            PlaybackState::Playing,
            "a dry feed does not end the track"
        );

        feed.push(tone(100, 44_100), 44_100, 2, 100);
        let t2 = t0 + Duration::from_millis(300);
        let resumed = render(&mut mixer, 20, t2);
        assert!(rms(&resumed) > 0.05, "sound resumes with new data");
        let point = mixer.external_sync_point(id).expect("sync point");
        assert_eq!(point.at, t2);
        assert!((90..=110).contains(&point.position_ms), "{point:?}");

        feed.finish();
        for i in 0..10 {
            let _ = render(&mut mixer, 20, t2 + Duration::from_millis(20 * i));
        }
        assert!(
            mixer
                .current_state(crate::types::AudioType::Video)
                .state
                .is_stopped(),
            "a finished feed ends the track"
        );
    }

    #[test]
    fn the_feed_stays_bounded_when_nothing_plays_it() {
        let feed = ExternalPcmFeed::new();
        for i in 0..300 {
            feed.push(tone(100, 48_000), 48_000, 2, i * 100);
        }
        assert!(feed.queued_ms() <= EXTERNAL_FEED_MAX_MS);
        assert_eq!(feed.first_pts_ms(), Some(0));
        feed.clear();
        assert_eq!((feed.queued_ms(), feed.first_pts_ms()), (0, None));
    }
}
