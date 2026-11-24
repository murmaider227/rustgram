//! Audio player music listen duration tracker and `messages.reportMusicListen` RPC helper.
//!
//! Port of:
//! - `Telegram/SourceFiles/media/player/media_player_listen_tracker.h`
//! - `Telegram/SourceFiles/media/player/media_player_listen_tracker.cpp`

use tg_api::RpcClient;
use tg_audio::{AudioMsgId, AudioType, TrackState};
use tg_tl::{enums as tl_enums, functions as tl_fns};

/// Minimum listened duration (in seconds) before a track listen is reported (`kReportDurationSecondsMin = 3`).
pub const MIN_REPORT_DURATION_SECS: i32 = 3;

/// Pause timeout (in milliseconds) after which accumulated listen time is flushed (60 seconds).
pub const PAUSE_REPORT_TIMEOUT_MS: i64 = 60_000;

/// Completed music listen duration ready to report via `messages.reportMusicListen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenReport {
    /// Track identifier (`AudioMsgId`, containing document ID and context message ID).
    pub track_id: AudioMsgId,
    /// Accumulated active playback duration in whole seconds (`>= MIN_REPORT_DURATION_SECS`).
    pub duration_secs: i32,
}

/// Tracks active song playback intervals and flushes [`ListenReport`]s when a song stops,
/// switches tracks, or remains paused for 60 seconds (`Media::Player::MusicListenTracker`).
#[derive(Debug, Default, Clone)]
pub struct MusicListenTracker {
    active_track: Option<AudioMsgId>,
    listened_ms: i64,
    playing: bool,
    play_started_at_ms: i64,
    pause_deadline_ms: Option<i64>,
}

impl MusicListenTracker {
    /// Creates an idle [`MusicListenTracker`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            active_track: None,
            listened_ms: 0,
            playing: false,
            play_started_at_ms: 0,
            pause_deadline_ms: None,
        }
    }

    /// Feeds the current audio player [`TrackState`] and timestamp `now_ms`.
    ///
    /// Returns a [`ListenReport`] if finalizing a previous track or stopping the current track
    /// reached at least [`MIN_REPORT_DURATION_SECS`].
    pub fn update(
        &mut self,
        now_ms: i64,
        track_id: AudioMsgId,
        audio_type: AudioType,
        state: &TrackState,
    ) -> Option<ListenReport> {
        if audio_type != AudioType::Song || !track_id.is_valid() {
            return self.finalize(now_ms);
        }

        let mut pending_report = None;
        if self.active_track != Some(track_id) {
            pending_report = self.finalize(now_ms);
            self.active_track = Some(track_id);
            self.listened_ms = 0;
            self.playing = false;
            self.pause_deadline_ms = None;
        }

        let stopped = state.state.is_stopped_or_stopping();
        let playing = !stopped && !state.state.is_paused_or_pausing();

        if self.playing == playing {
            return pending_report;
        }

        if !self.playing {
            self.playing = true;
            self.play_started_at_ms = now_ms;
            self.pause_deadline_ms = None;
        } else {
            self.playing = false;
            self.listened_ms = self
                .listened_ms
                .saturating_add(now_ms.saturating_sub(self.play_started_at_ms).max(0));
            if stopped {
                let stopped_report = self.take_report();
                if stopped_report.is_some() {
                    return stopped_report;
                }
            } else {
                self.pause_deadline_ms = Some(now_ms.saturating_add(PAUSE_REPORT_TIMEOUT_MS));
            }
        }

        pending_report
    }

    /// Checks whether a paused track exceeded [`PAUSE_REPORT_TIMEOUT_MS`] and flushes it.
    pub fn poll_timeout(&mut self, now_ms: i64) -> Option<ListenReport> {
        let deadline = self.pause_deadline_ms?;
        if now_ms >= deadline {
            self.pause_deadline_ms = None;
            return self.take_report();
        }
        None
    }

    /// Finalizes the currently tracked song (e.g. on player close or shutdown).
    pub fn finalize(&mut self, now_ms: i64) -> Option<ListenReport> {
        if self.playing {
            self.playing = false;
            self.listened_ms = self
                .listened_ms
                .saturating_add(now_ms.saturating_sub(self.play_started_at_ms).max(0));
        }
        self.pause_deadline_ms = None;
        self.take_report()
    }

    fn take_report(&mut self) -> Option<ListenReport> {
        let track_id = self.active_track.take()?;
        let duration_secs = (std::mem::take(&mut self.listened_ms) / 1000) as i32;
        if duration_secs < MIN_REPORT_DURATION_SECS {
            return None;
        }
        Some(ListenReport {
            track_id,
            duration_secs,
        })
    }
}

/// Sends `messages.reportMusicListen` to Telegram over `client`.
pub async fn report_music_listen<C>(
    client: &C,
    document: tl_enums::InputDocument,
    listened_duration: i32,
) -> Result<bool, String>
where
    C: RpcClient,
{
    if listened_duration < MIN_REPORT_DURATION_SECS {
        return Ok(false);
    }
    client
        .invoke(&tl_fns::messages::ReportMusicListen {
            id: document,
            listened_duration,
        })
        .await
        .map_err(|e| {
            let inv: tg_api::InvocationError = e.into();
            format!("messages.reportMusicListen failed: {inv}")
        })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tg_api::{DcId, InvocationError, RpcClient};
    use tg_audio::{DocumentId, FullMsgId, MediaKind, MsgId, PeerId, PlaybackState};
    use tg_tl::{Deserializable, RemoteCall, Serializable, types as tl_types};
    use tokio::sync::broadcast;

    use super::*;

    type RpcResponseQueue = Arc<Mutex<VecDeque<Result<Vec<u8>, InvocationError>>>>;

    #[derive(Clone)]
    struct FakeListenRpc {
        responses: RpcResponseQueue,
        updates_tx: broadcast::Sender<tl_enums::Updates>,
    }

    impl FakeListenRpc {
        fn new() -> Self {
            let (updates_tx, _) = broadcast::channel(8);
            Self {
                responses: Arc::new(Mutex::new(VecDeque::new())),
                updates_tx,
            }
        }

        fn push_ok<T: Serializable>(&self, resp: &T) {
            self.responses
                .lock()
                .expect("lock")
                .push_back(Ok(resp.to_bytes()));
        }
    }

    impl RpcClient for FakeListenRpc {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            _req: &R,
        ) -> Result<R::Return, Self::Error> {
            let next = self
                .responses
                .lock()
                .expect("lock")
                .pop_front()
                .ok_or(InvocationError::Dropped)??;
            R::Return::from_bytes(&next).map_err(|e| InvocationError::Deserialize(e.to_string()))
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            _dc_id: DcId,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            self.invoke(req).await
        }

        fn main_dc_id(&self) -> DcId {
            2
        }

        fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
            Some(self.updates_tx.subscribe())
        }
    }

    fn track(doc_id: u64) -> AudioMsgId {
        AudioMsgId::new(
            DocumentId(doc_id),
            FullMsgId::new(PeerId(100), MsgId(doc_id as i64)),
            MediaKind::AudioFile,
            0,
            0,
        )
    }

    fn state_with(st: PlaybackState) -> TrackState {
        TrackState {
            state: st,
            ..TrackState::default()
        }
    }

    #[test]
    fn tracker_accumulates_across_pause_and_reports_on_timeout_or_stop() {
        let mut tracker = MusicListenTracker::new();
        let song = track(1);

        // Start playing at t=0.
        assert_eq!(
            tracker.update(
                0,
                song,
                AudioType::Song,
                &state_with(PlaybackState::Playing)
            ),
            None
        );
        // Pause at t=2_500ms (< 3s so not reported yet).
        assert_eq!(
            tracker.update(
                2_500,
                song,
                AudioType::Song,
                &state_with(PlaybackState::Paused)
            ),
            None
        );
        // Resume at t=10_000ms (before 60s timeout), play for another 2s -> total 4.5s.
        assert_eq!(
            tracker.update(
                10_000,
                song,
                AudioType::Song,
                &state_with(PlaybackState::Playing)
            ),
            None
        );
        // Stop at t=12_000ms -> emits 4s report.
        assert_eq!(
            tracker.update(
                12_000,
                song,
                AudioType::Song,
                &state_with(PlaybackState::Stopped)
            ),
            Some(ListenReport {
                track_id: song,
                duration_secs: 4,
            })
        );

        // Short listen (< 3s) is discarded.
        let song2 = track(2);
        assert_eq!(
            tracker.update(
                20_000,
                song2,
                AudioType::Song,
                &state_with(PlaybackState::Playing)
            ),
            None
        );
        assert_eq!(
            tracker.update(
                21_900,
                song2,
                AudioType::Song,
                &state_with(PlaybackState::Stopped)
            ),
            None
        );

        // Pause timeout flushes after 60s.
        let song3 = track(3);
        tracker.update(
            30_000,
            song3,
            AudioType::Song,
            &state_with(PlaybackState::Playing),
        );
        tracker.update(
            35_000,
            song3,
            AudioType::Song,
            &state_with(PlaybackState::Paused),
        );
        assert_eq!(tracker.poll_timeout(94_999), None);
        assert_eq!(
            tracker.poll_timeout(95_000),
            Some(ListenReport {
                track_id: song3,
                duration_secs: 5,
            })
        );
    }

    #[tokio::test]
    async fn report_music_listen_invokes_tl_rpc() {
        let client = FakeListenRpc::new();
        client.push_ok(&true);
        let input_doc = tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
            id: 42,
            access_hash: 99,
            file_reference: vec![1, 2, 3],
        });
        let ok = report_music_listen(&client, input_doc, 18)
            .await
            .expect("rpc ok");
        assert!(ok);
    }
}
