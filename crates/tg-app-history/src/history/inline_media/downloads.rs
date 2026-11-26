//! Downloads the user starts or cancels on a message's media (TGB-2982, TGB-2958).
//!
//! Port origin:
//! - `Photo::textState` / `Photo::draw` (`history/view/media/history_view_photo.cpp`): a click
//!   on a photo that is downloading cancels it (`_cancell`), a click on one that is neither
//!   loaded nor loading starts it (`_savel`), and the circle in its centre is
//!   `msgDateImgBgOver` while the pointer is over that link.
//! - Voice messages are never downloaded automatically (`Data::AutoDownload::Should`); the
//!   first play fetches the file (tdesktop streams it) and `Document::updateStatusText` shows
//!   the size only while the file is not in the cache.
//!
//! Internal design differs from tdesktop: click handlers there call the loader directly.
//! Painting here has no backend, so a click queues a [`MediaDownloadAction`] that the history
//! widget hands to the backend once per frame, before it paints; a voice message that is not
//! cached is played when its bytes arrive instead of through a streaming reader.

use std::collections::HashMap;

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_audio::{AudioMsgId, AudioType};

use super::{HistoryMediaRuntime, MSG_DATE_IMG_BG, PHOTO_RADIAL_SIZE, PhotoLoadState};

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
}

/// A voice message the user started while its file was not cached.
#[derive(Debug, Clone, Copy)]
struct PendingVoicePlay {
    peer_id: PeerId,
    msg_id: MsgId,
    seek_frac: Option<f64>,
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
    voice_meta: HashMap<MsgId, VoiceMeta>,
    /// The voice track last checked by [`HistoryMediaRuntime::guard_unloaded_voice`].
    guarded: Option<AudioMsgId>,
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

    /// The voice message waiting for its file, polled by the history widget.
    #[must_use]
    pub fn pending_voice_msg(&self) -> Option<MsgId> {
        self.downloads.pending_voice.map(|p| p.msg_id)
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

    /// A play (or seek) of a voice message whose file is not cached: starts the download and
    /// plays when it arrives. A second click before that takes the play back, like a pause.
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
    /// ended, or the bar's next / previous button): its registered track is only a stand-in, so
    /// stop it and play the message through the download path instead.
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
        self.audio_player_bar
            .player_mut()
            .stop(AudioType::Voice, false);
        self.downloads.guarded = None;
        let peer_id = self.active_peer;
        self.request_voice_play(peer_id, msg_id, None);
    }
}
