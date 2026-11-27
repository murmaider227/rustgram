//! Downloads the user starts or cancels on a message's media (TGB-2982, TGB-2958).
//!
//! Port origin:
//! - `Photo::textState` / `Photo::draw` (`history/view/media/history_view_photo.cpp`): a click
//!   on a photo that is downloading cancels it (`_cancell`), a click on one that is neither
//!   loaded nor loading starts it (`_savel`), and the circle in its centre is
//!   `msgDateImgBgOver` while the pointer is over that link.
//! - Voice messages are never downloaded automatically (`Data::AutoDownload::Should`); the
//!   first play streams the file (`Media::Streaming::Player` over the document's loader, TGB-3000)
//!   and `Document::updateStatusText` shows the size only while the file is not in the cache.
//!
//! Internal design differs from tdesktop: click handlers there call the loader directly.
//! Painting here has no backend, so a click queues a [`MediaDownloadAction`] that the history
//! widget hands to the backend once per frame, before it paints. A voice track that is not
//! cached starts at once and decodes on a background thread that waits for the loader the widget
//! hands over ([`VoiceStreamSlot`]); the mixer plays silence until data comes. A backend that
//! cannot stream gets the old path: the play downloads the file and plays when it is there.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_audio::{AudioError, AudioInput, AudioMsgId, AudioType, StreamedInputOpener};
use tg_video::{RangeLoader, RangeReadSeek};

use super::{
    HistoryMediaRuntime, MSG_DATE_IMG_BG, PHOTO_RADIAL_SIZE, PhotoLoadState, VoiceLoadState,
};

/// `msgDateImgBgOver` (`#00000074`): the circle with the pointer over its link.
pub(super) const MSG_DATE_IMG_BG_OVER: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 0x74);
/// `st::msgFileOverDuration`: how long the circle takes to change colour.
pub const MSG_FILE_OVER_DURATION_SECS: f32 = 0.2;

/// What a click on a photo does (`Photo::textState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoLink {
    /// Loaded (or its state is unknown): opens the media viewer (`_openl`).
    Open,
    /// Downloading: cancels the download (`_cancell`).
    Cancel,
    /// Not loaded and not downloading: starts the download (`_savel`).
    Save,
}

/// A download request the history widget passes to its backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDownloadAction {
    /// `HistoryBackend::start_media_download`.
    Start,
    /// `HistoryBackend::cancel_media_download`.
    Cancel,
    /// `HistoryBackend::open_media_stream`, answered with
    /// [`HistoryMediaRuntime::provide_voice_stream`].
    Stream,
}

/// How long a started voice track waits for the widget to hand over its loader.
const STREAM_LOADER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
enum StreamLoader {
    #[default]
    Waiting,
    Ready(Arc<dyn RangeLoader>),
    Unavailable,
}

/// The loader a streamed voice track waits for (TGB-3000): its decoder thread blocks in
/// [`Self::wait`] until the widget asks the backend and hands the answer over.
#[derive(Default)]
pub(super) struct VoiceStreamSlot {
    loader: Mutex<StreamLoader>,
    changed: Condvar,
}

impl fmt::Debug for VoiceStreamSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VoiceStreamSlot")
    }
}

impl VoiceStreamSlot {
    fn set(&self, loader: Option<Arc<dyn RangeLoader>>) {
        *self.loader.lock().unwrap_or_else(PoisonError::into_inner) =
            loader.map_or(StreamLoader::Unavailable, StreamLoader::Ready);
        self.changed.notify_all();
    }

    fn is_waiting(&self) -> bool {
        matches!(
            *self.loader.lock().unwrap_or_else(PoisonError::into_inner),
            StreamLoader::Waiting
        )
    }

    fn wait(&self, timeout: Duration) -> Option<Arc<dyn RangeLoader>> {
        let guard = self.loader.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |l| matches!(l, StreamLoader::Waiting))
            .unwrap_or_else(PoisonError::into_inner);
        match &*guard {
            StreamLoader::Ready(loader) => Some(Arc::clone(loader)),
            StreamLoader::Waiting | StreamLoader::Unavailable => None,
        }
    }

    /// The input of a streamed track: waits for the loader, then reads the file as it arrives.
    pub(super) fn opener(self: &Arc<Self>, ext: String) -> StreamedInputOpener {
        let slot = Arc::clone(self);
        Arc::new(move || {
            let loader = slot
                .wait(STREAM_LOADER_TIMEOUT)
                .ok_or_else(|| AudioError::SourceNotFound("voice stream unavailable".into()))?;
            let reader = RangeReadSeek::new(loader)
                .map_err(|e| AudioError::SourceNotFound(e.to_string()))?;
            let size = reader.size();
            Ok(AudioInput {
                reader: Box::new(reader),
                byte_len: Some(size),
                hint_ext: Some(ext.clone()),
            })
        })
    }
}

/// A voice message the user started while its file was not cached.
#[derive(Debug, Clone, Copy)]
struct PendingVoicePlay {
    peer_id: PeerId,
    msg_id: MsgId,
    seek_frac: Option<f64>,
}

/// A round video message clicked while its file was not downloaded (TGB-2997): it starts with
/// sound once the file is there.
#[derive(Debug, Clone)]
struct PendingRoundPlay {
    peer_id: PeerId,
    msg_id: MsgId,
    sender_name: String,
    duration_secs: u32,
    clicked_at_ms: i64,
}

/// What registering a voice track needs again once its real bytes are there.
#[derive(Debug, Clone)]
struct VoiceMeta {
    sender_name: String,
    duration_secs: u32,
}

/// Download requests and the voice message waiting for its file.
#[derive(Debug, Default)]
pub(super) struct MediaDownloads {
    actions: Vec<(MsgId, MediaDownloadAction)>,
    pending_voice: Option<PendingVoicePlay>,
    pending_round: Option<PendingRoundPlay>,
    voice_meta: HashMap<MsgId, VoiceMeta>,
    /// The voice track last checked by [`HistoryMediaRuntime::guard_unloaded_voice`].
    guarded: Option<AudioMsgId>,
    /// Loaders of the streamed voice tracks, by message.
    voice_streams: HashMap<MsgId, Arc<VoiceStreamSlot>>,
    /// Messages whose loader was asked from the backend.
    stream_requested: HashSet<MsgId>,
    /// Voice messages whose registered track streams its file.
    streamed_tracks: HashSet<MsgId>,
    /// The backend cannot stream: voice messages download in full before they play.
    streams_unavailable: bool,
}

/// Circle colour between `msgDateImgBg` and `msgDateImgBgOver` (`anim::brush`).
#[must_use]
pub(super) fn circle_bg(over: f32) -> Color32 {
    let (from, to) = (
        f32::from(MSG_DATE_IMG_BG.a()),
        f32::from(MSG_DATE_IMG_BG_OVER.a()),
    );
    let alpha = from + (to - from) * over.clamp(0.0, 1.0);
    Color32::from_rgba_premultiplied(0, 0, 0, alpha.round() as u8)
}

/// `historyFileThumbDownload` (`history_file_download.png`): a down arrow, 2 px strokes.
fn paint_download_arrow(painter: &Painter, center: Pos2, color: Color32) {
    let stroke = Stroke::new(2.0, color);
    let tip = center + Vec2::new(0.0, 7.0);
    let ends = [
        center + Vec2::new(0.0, -8.0),
        center + Vec2::new(-6.5, 0.5),
        center + Vec2::new(6.5, 0.5),
    ];
    for end in ends {
        painter.line_segment([end, tip], stroke);
        painter.circle_filled(end, 1.0, color);
    }
    painter.circle_filled(tip, 1.0, color);
}

impl HistoryMediaRuntime {
    /// What a click on the photo of `msg_id` does.
    #[must_use]
    pub fn photo_link(&self, msg_id: MsgId) -> PhotoLink {
        match self
            .descriptors
            .get(&msg_id)
            .and_then(|d| d.photo_load.as_ref())
        {
            Some(PhotoLoadState::Loading { .. }) => PhotoLink::Cancel,
            Some(PhotoLoadState::NotLoaded) => PhotoLink::Save,
            Some(PhotoLoadState::Ready { .. } | PhotoLoadState::Pending) | None => PhotoLink::Open,
        }
    }

    /// Queues a download request for the backend.
    pub fn queue_download(&mut self, msg_id: MsgId, action: MediaDownloadAction) {
        self.downloads.actions.push((msg_id, action));
    }

    /// Download requests queued since the last call.
    pub fn take_download_actions(&mut self) -> Vec<(MsgId, MediaDownloadAction)> {
        std::mem::take(&mut self.downloads.actions)
    }

    /// The not loaded state of a photo (`Photo::draw` with `!loaded && !_data->loading()`):
    /// the circle with the download arrow.
    pub(super) fn paint_photo_download(painter: &Painter, rect: Rect, over: f32) {
        let center = rect.center();
        painter.circle_filled(center, PHOTO_RADIAL_SIZE / 2.0, circle_bg(over));
        paint_download_arrow(painter, center, Color32::WHITE);
    }

    /// Whether the voice file of `msg_id` is in the media cache: `Some(false)` while it has to
    /// be downloaded, `None` for data without a cache (demo).
    #[must_use]
    pub fn voice_loaded(&self, msg_id: MsgId) -> Option<bool> {
        let desc = self.descriptors.get(&msg_id)?;
        if desc.audio_bytes.is_some() {
            Some(true)
        } else {
            desc.voice_load.map(|_| false)
        }
    }

    /// Whether `msg_id` was started and waits for its file.
    #[must_use]
    pub fn voice_play_pending(&self, msg_id: MsgId) -> bool {
        self.downloads
            .pending_voice
            .is_some_and(|p| p.msg_id == msg_id)
    }

    /// The voice message (or round video message) waiting for its file, polled by the history
    /// widget.
    #[must_use]
    pub fn pending_voice_msg(&self) -> Option<MsgId> {
        self.downloads
            .pending_voice
            .map(|p| p.msg_id)
            .or_else(|| self.downloads.pending_round.as_ref().map(|p| p.msg_id))
    }

    /// Whether the round video `msg_id` is not downloaded and waits for a click to download
    /// (its autoplay is off): tdesktop shows the play button over the thumbnail then.
    #[must_use]
    pub(super) fn round_waits_for_click(&self, msg_id: MsgId) -> bool {
        self.descriptors
            .get(&msg_id)
            .is_some_and(|d| d.voice_load == Some(VoiceLoadState::NotLoaded))
            && !self
                .downloads
                .pending_round
                .as_ref()
                .is_some_and(|p| p.msg_id == msg_id)
    }

    /// A click on a round video that is not downloaded: downloads it (unless it already
    /// downloads) and starts it with sound when it is there. Returns `false` for data without
    /// a download state (nothing to wait for).
    pub(super) fn request_round_play(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        sender_name: &str,
        duration_secs: u32,
        now_ms: i64,
    ) -> bool {
        let Some(load) = self.descriptors.get(&msg_id).and_then(|d| d.voice_load) else {
            return false;
        };
        self.downloads.pending_round = Some(PendingRoundPlay {
            peer_id,
            msg_id,
            sender_name: sender_name.to_owned(),
            duration_secs,
            clicked_at_ms: now_ms,
        });
        if load == VoiceLoadState::NotLoaded {
            self.queue_download(msg_id, MediaDownloadAction::Start);
        }
        true
    }

    /// Starts the pending round video once its file is registered.
    pub(super) fn play_pending_round_if_loaded(&mut self, msg_id: MsgId) {
        let ready = self
            .descriptors
            .get(&msg_id)
            .is_some_and(|d| d.video_data.is_some() || d.audio_bytes.is_some());
        if !ready {
            return;
        }
        let Some(pending) = self.downloads.pending_round.take_if(|p| p.msg_id == msg_id) else {
            return;
        };
        self.click_round_video(
            pending.peer_id,
            msg_id,
            &pending.sender_name,
            pending.duration_secs,
            pending.clicked_at_ms,
        );
    }

    pub(super) fn remember_voice_meta(
        &mut self,
        msg_id: MsgId,
        sender_name: &str,
        duration_secs: u32,
    ) {
        self.downloads.voice_meta.insert(
            msg_id,
            VoiceMeta {
                sender_name: sender_name.to_owned(),
                duration_secs,
            },
        );
    }

    /// Whether the not cached voice message `msg_id` plays from a stream (TGB-3000).
    pub(super) fn streams_voice(&self, msg_id: MsgId) -> bool {
        !self.downloads.streams_unavailable && self.voice_loaded(msg_id) == Some(false)
    }

    /// The loader slot of `msg_id`'s streamed track.
    pub(super) fn voice_stream_slot(&mut self, msg_id: MsgId) -> Arc<VoiceStreamSlot> {
        Arc::clone(self.downloads.voice_streams.entry(msg_id).or_default())
    }

    /// Asks the backend (on the next frame) for the loader of `msg_id`, once.
    pub(super) fn request_voice_stream(&mut self, msg_id: MsgId) {
        let slot = self.voice_stream_slot(msg_id);
        if slot.is_waiting() && self.downloads.stream_requested.insert(msg_id) {
            self.queue_download(msg_id, MediaDownloadAction::Stream);
        }
    }

    /// The backend's answer to [`MediaDownloadAction::Stream`]. Without a loader the backend
    /// cannot stream: the voice message that was to play downloads in full and plays then.
    pub fn provide_voice_stream(&mut self, msg_id: MsgId, loader: Option<Arc<dyn RangeLoader>>) {
        let streamed = loader.is_some();
        self.voice_stream_slot(msg_id).set(loader);
        if streamed {
            return;
        }
        self.downloads.streams_unavailable = true;
        // Tracks registered to stream fail at once and register again as before.
        for (id, slot) in std::mem::take(&mut self.downloads.voice_streams) {
            slot.set(None);
            if id != msg_id {
                self.registered_audio.remove(&id);
            }
        }
        self.downloads.stream_requested.clear();
        self.downloads.streamed_tracks.clear();
        let Some(audio_id) = self.registered_audio.remove(&msg_id) else {
            return;
        };
        let player = self.audio_player_bar.player();
        if player.current(AudioType::Voice) != audio_id
            || player
                .get_state(AudioType::Voice)
                .state
                .is_stopped_or_stopping()
        {
            return;
        }
        self.audio_player_bar
            .player_mut()
            .stop(AudioType::Voice, false);
        let peer_id = self.active_peer;
        self.request_voice_play(peer_id, msg_id, None);
    }

    /// Forgets the stream of `msg_id` once its file is in the cache.
    pub(super) fn drop_voice_stream(&mut self, msg_id: MsgId) {
        self.downloads.voice_streams.remove(&msg_id);
        self.downloads.stream_requested.remove(&msg_id);
        self.downloads.streamed_tracks.remove(&msg_id);
    }

    /// The input of `msg_id`'s streamed track; marks the registration as streamed.
    pub(super) fn voice_stream_opener(&mut self, msg_id: MsgId) -> tg_audio::StreamedInputOpener {
        let ext = self
            .descriptors
            .get(&msg_id)
            .and_then(|d| d.audio_ext.clone())
            .unwrap_or_else(|| "ogg".to_owned());
        self.downloads.streamed_tracks.insert(msg_id);
        self.voice_stream_slot(msg_id).opener(ext)
    }

    /// A voice track registered before the message was known to be not cached plays a
    /// stand-in: switch the same track (same id, so the playlist keeps it) to the stream.
    pub(super) fn stream_registered_voice(&mut self, msg_id: MsgId) {
        if !self.streams_voice(msg_id)
            || self.downloads.streamed_tracks.contains(&msg_id)
            || !self.downloads.voice_meta.contains_key(&msg_id)
        {
            return;
        }
        let Some(&audio_id) = self.registered_audio.get(&msg_id) else {
            return;
        };
        let open = self.voice_stream_opener(msg_id);
        self.audio_backend.stream_track(audio_id, open);
    }

    /// A play (or seek) of a voice message whose file is not cached and cannot be streamed:
    /// starts the download and plays when it arrives. A second click before that takes the
    /// play back, like a pause.
    pub(super) fn request_voice_play(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        seek_frac: Option<f64>,
    ) {
        if self.voice_play_pending(msg_id) && seek_frac.is_none() {
            self.downloads.pending_voice = None;
            return;
        }
        self.downloads.pending_voice = Some(PendingVoicePlay {
            peer_id,
            msg_id,
            seek_frac,
        });
        self.queue_download(msg_id, MediaDownloadAction::Start);
    }

    /// Plays the pending voice message once its bytes are registered.
    pub(super) fn play_pending_voice_if_loaded(&mut self, msg_id: MsgId) {
        if !self.voice_play_pending(msg_id) || self.voice_loaded(msg_id) != Some(true) {
            return;
        }
        let Some(pending) = self.downloads.pending_voice.take() else {
            return;
        };
        let Some(meta) = self.downloads.voice_meta.get(&msg_id).cloned() else {
            return;
        };
        self.toggle_or_seek_voice(
            pending.peer_id,
            msg_id,
            &meta.sender_name,
            meta.duration_secs,
            pending.seek_frac,
        );
    }

    /// The playlist moved on to a voice message whose file is not cached (the previous one
    /// ended, or the bar's next / previous button). Its streamed track already plays and waits
    /// for its loader: ask the backend for it, the bar stays open. Without streaming the track
    /// is only a stand-in, so stop it and play the message through the download path instead.
    pub fn guard_unloaded_voice(&mut self) {
        let player = self.audio_player_bar.player();
        let current = player.current(AudioType::Voice);
        if self.downloads.guarded == Some(current) {
            return;
        }
        self.downloads.guarded = Some(current);
        if player
            .get_state(AudioType::Voice)
            .state
            .is_stopped_or_stopping()
        {
            return;
        }
        let Some(msg_id) = self
            .registered_audio
            .iter()
            .find_map(|(msg_id, id)| (*id == current).then_some(*msg_id))
        else {
            return;
        };
        if self.voice_loaded(msg_id) != Some(false) {
            return;
        }
        if self.streams_voice(msg_id) {
            self.request_voice_stream(msg_id);
            return;
        }
        self.audio_player_bar
            .player_mut()
            .stop(AudioType::Voice, false);
        self.downloads.guarded = None;
        let peer_id = self.active_peer;
        self.request_voice_play(peer_id, msg_id, None);
    }
}
