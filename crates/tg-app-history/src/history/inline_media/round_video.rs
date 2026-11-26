//! Round video messages ("video notes") in chat history: playback state and painting.
//!
//! Port of the round path of `HistoryView::Gif` (`history/view/media/history_view_gif.cpp`):
//! the circle shows the first frame (or the blurred document thumbnail while downloading), loops
//! muted with the mute badge while visible, restarts with sound and the circular progress ring
//! (`HistoryView::VideoMessageSeek`) on click, pauses / resumes on the next click, and returns to
//! the muted loop when the sound playback ends. The remaining-time pill sits in the circle's
//! bottom-left corner; the time / views pill and the right actions live outside the circle
//! (`tg_history::element` and `history::inner`).

use super::*;
use crate::history::sections_model::RoundVideoAttachment;
use crate::history::video_message_seek::VideoMessageSeek;
use tg_ui::tg_style::st;

/// `st::historyVideoMessageMuteSize`.
const MUTE_SIZE: f32 = 25.0;
/// `st::msgDateImgDelta`.
const DATE_IMG_DELTA: f32 = 4.0;
/// `st::msgDateImgPadding`.
const DATE_IMG_PADDING: Vec2 = Vec2::new(8.0, 2.0);
/// `st::msgFileLayout.thumbSize`: the centre play / download circle.
const CENTER_BUTTON_SIZE: f32 = 44.0;
/// `kUseNonBlurredThreshold` (history_view_gif.cpp): smaller thumbnails are shown blurred.
const NON_BLURRED_THRESHOLD: u32 = 240;
/// Largest clip / audio clock difference tolerated before the clip is re-seeked to the audio.
const RESYNC_DRIFT_MS: i64 = 300;

/// Playback phase of one round video message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundVideoPhase {
    /// The video is still downloading: thumbnail plus the radial loading indicator.
    NotLoaded,
    /// Autoplay is off: first frame with the play button in the centre.
    Static,
    /// Muted looping autoplay with the mute badge (`displayMute`).
    SilentLoop,
    /// Playing with sound and the progress ring (`activeRoundStreamed`).
    SoundPlaying,
    /// Sound playback paused: frozen frame with the seek ring shown.
    SoundPaused,
}

impl HistoryMediaRuntime {
    /// Returns `true` once the round video's bytes are available (mock rounds without a
    /// descriptor count as loaded and play the synthesized track).
    fn round_loaded(&self, msg_id: MsgId) -> bool {
        self.descriptors
            .get(&msg_id)
            .is_none_or(|d| d.video_data.is_some() || d.audio_bytes.is_some())
    }

    /// Returns `true` if a `tg-audio` track is registered for `msg_id` (tests).
    #[cfg(test)]
    pub(crate) fn audio_registered(&self, msg_id: MsgId) -> bool {
        self.registered_audio.contains_key(&msg_id)
    }

    /// Returns the current [`RoundVideoPhase`] of `msg_id`.
    #[must_use]
    pub fn round_video_phase(&self, msg_id: MsgId) -> RoundVideoPhase {
        if self.unmuted_round_msg == Some(msg_id) {
            let (_, playing, _, _, _) = self.audio_item_state(msg_id, AudioType::Voice);
            return if playing {
                RoundVideoPhase::SoundPlaying
            } else {
                RoundVideoPhase::SoundPaused
            };
        }
        if !self.round_loaded(msg_id) {
            RoundVideoPhase::NotLoaded
        } else if self.policy.allows_inline(InlinePlaybackKind::RoundVideo) {
            RoundVideoPhase::SilentLoop
        } else {
            RoundVideoPhase::Static
        }
    }

    /// Handles a click on the circle: the first click restarts the video from the start with
    /// sound, later clicks pause / resume it. Ignored until the video is downloaded.
    pub fn click_round_video(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        sender_name: &str,
        duration_secs: u32,
        now_ms: i64,
    ) {
        if !self.round_loaded(msg_id) {
            tracing::debug!(msg = msg_id.0, "round video click ignored: not loaded");
            return;
        }
        let item = media_item_id_for_msg(msg_id);
        if self.unmuted_round_msg == Some(msg_id) {
            tracing::debug!(msg = msg_id.0, "round video click: toggle pause");
            let _ = self
                .audio_player_bar
                .player_mut()
                .play_pause(AudioType::Voice);
            let (_, playing, _, _, _) = self.audio_item_state(msg_id, AudioType::Voice);
            self.inline.set_item_paused(item, !playing, now_ms);
            return;
        }
        let audio_id =
            self.ensure_voice_registered(peer_id, msg_id, sender_name, duration_secs, true);
        tracing::debug!(msg = msg_id.0, "round video click: start with sound");
        self.unmuted_round_msg = Some(msg_id);
        self.round_sound_seen_active = false;
        self.round_seek_positions.remove(&msg_id);
        self.inline.seek_item(item, 0, now_ms);
        self.inline.set_item_paused(item, false, now_ms);
        let ctx = PlaylistContext {
            peer_id: AudioPeerId(peer_id.0),
            ..PlaylistContext::default()
        };
        let _ = self.audio_player_bar.play_item(audio_id, Some(ctx));
        // `play_item` resumes from the saved per-document position, which is the end of a
        // round that already played through; a round always restarts from the beginning.
        let _ = self
            .audio_player_bar
            .player_mut()
            .finish_seeking(AudioType::Voice, 0.0);
    }

    /// Keeps the sound-on round video's clip in step with its audio track: pauses it with the
    /// audio, re-seeks it when the clocks drift apart, and drops back to the muted loop once
    /// the audio has finished or was stopped elsewhere (the audio bar's close button).
    pub fn sync_round_video(&mut self, msg_id: MsgId, duration_secs: u32, now_ms: i64) {
        if self.unmuted_round_msg != Some(msg_id) {
            return;
        }
        let item = media_item_id_for_msg(msg_id);
        let (active, playing, progress, _, _) = self.audio_item_state(msg_id, AudioType::Voice);
        if !active {
            if self.round_sound_seen_active {
                tracing::debug!(
                    msg = msg_id.0,
                    "round video sound ended: back to muted loop"
                );
                self.unmuted_round_msg = None;
                self.round_sound_seen_active = false;
                self.round_seek_positions.remove(&msg_id);
                self.inline.seek_item(item, 0, now_ms);
                self.inline.set_item_paused(item, false, now_ms);
            }
            return;
        }
        self.round_sound_seen_active = true;
        self.inline.set_item_paused(item, !playing, now_ms);
        if playing && let Some(clip_ms) = self.inline.item_position_ms(item) {
            let audio_ms = (f64::from(progress) * f64::from(duration_secs) * 1000.0) as i64;
            if (clip_ms - audio_ms).abs() > RESYNC_DRIFT_MS {
                self.inline.seek_item(item, audio_ms, now_ms);
            }
        }
    }

    /// Decodes the round video's current frame. Unlike [`Self::inline_clip_frame`], a round
    /// playing with sound keeps advancing even when muted autoplay is switched off.
    fn round_clip_frame(
        &mut self,
        ctx: &egui::Context,
        msg_id: MsgId,
        target_px: u32,
        force_play: bool,
        now_ms: i64,
    ) -> Option<InlineFrameState> {
        self.mark_visible(msg_id);
        let data = self.descriptors.get(&msg_id)?.video_data.clone()?;
        let allowed = force_play || self.policy.allows_inline(InlinePlaybackKind::RoundVideo);
        self.inline.set_autoplay_round_videos(allowed);
        let source = InlineMediaSource {
            id: media_item_id_for_msg(msg_id),
            kind: InlinePlaybackKind::RoundVideo,
            data,
            target_size: Some((target_px, target_px)),
        };
        Some(self.inline.frame_for(ctx, &source, now_ms))
    }

    /// Circle-masked document thumbnail shown until the first video frame is decoded
    /// (`Gif::prepareThumbCache`: small thumbnails are upscaled blurred).
    fn round_thumb_texture(
        &mut self,
        ctx: &egui::Context,
        msg_id: MsgId,
        side_px: u32,
    ) -> Option<TextureHandle> {
        self.pump_decoded(ctx);
        if let Some(cached) = self.round_thumbs.get(&msg_id) {
            return cached.clone();
        }
        let revision = self.media_revision(msg_id);
        if self.round_thumbs_pending.get(&msg_id) == Some(&revision) {
            // Being prepared: the round paints its placeholder meanwhile.
            return None;
        }
        let bytes = match self.descriptors.get(&msg_id)?.photo_load.as_ref()? {
            PhotoLoadState::Ready { bytes } => std::sync::Arc::clone(bytes),
            PhotoLoadState::Loading { .. }
            | PhotoLoadState::Pending
            | PhotoLoadState::NotLoaded => return None,
        };
        self.round_thumbs_pending.insert(msg_id, revision);
        self.run_decode(
            ctx,
            super::decode_pool::DecodeJob::RoundThumb {
                msg_id,
                revision,
                bytes,
                side_px,
            },
        );
        self.round_thumbs.get(&msg_id).cloned().flatten()
    }
}

/// Decodes a thumbnail, centre-crops it square, scales it to `side_px` (blurring small ones)
/// and applies the round video circle mask.
pub(super) fn round_thumb_rgba(bytes: &[u8], side_px: u32) -> Option<(u32, Vec<u8>)> {
    let (w, h, raw) = decode_inline_photo_rgba(bytes)?;
    let image = image::RgbaImage::from_raw(w, h, raw)?;
    let crop = w.min(h);
    let square =
        image::imageops::crop_imm(&image, (w - crop) / 2, (h - crop) / 2, crop, crop).to_image();
    let side = side_px.max(16);
    let mut scaled =
        image::imageops::resize(&square, side, side, image::imageops::FilterType::Triangle);
    if crop < NON_BLURRED_THRESHOLD {
        scaled = image::imageops::blur(&scaled, side as f32 / 48.0);
    }
    let mut rgba = scaled.into_raw();
    tg_video::apply_circle_mask_rgba(&mut rgba, side, side);
    Some((side, rgba))
}

/// Remaining seconds shown while a round plays with sound (`Gif::updateStatusText`:
/// `(length - position) / frequency + 1`), never above the full duration.
#[must_use]
pub fn round_remaining_secs(duration_secs: u32, progress: f32) -> u32 {
    let remaining = (1.0 - f64::from(progress.clamp(0.0, 1.0))) * f64::from(duration_secs);
    ((remaining.floor() as u32) + 1).min(duration_secs.max(1))
}

/// Paints a round video message into `rect` (a bare circle of `rect`'s smaller side at its
/// top-left, `Gif::draw` with `isRound`) and handles click / seek input.
#[allow(clippy::too_many_arguments)]
pub fn paint_round_video_inline(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    sender_name: &str,
    rv: &RoundVideoAttachment,
    rect: Rect,
    colors: &HistoryColors,
    runtime: &mut HistoryMediaRuntime,
) {
    let ctx = ui.ctx().clone();
    let now_secs = ctx.input(|i| i.time);
    let now_ms = (now_secs * 1000.0).max(0.0) as i64;
    let side = rect.width().min(rect.height()).max(1.0);
    let circle = Rect::from_min_size(rect.min, Vec2::splat(side));
    let target_px = ((side * ctx.pixels_per_point()).round() as u32).clamp(64, 720);

    runtime.sync_round_video(msg_id, rv.duration_secs, now_ms);
    let phase = runtime.round_video_phase(msg_id);
    let sound = matches!(
        phase,
        RoundVideoPhase::SoundPlaying | RoundVideoPhase::SoundPaused
    );
    let (_, _, audio_progress, _, _) = runtime.audio_item_state(msg_id, AudioType::Voice);
    let clip = runtime.round_clip_frame(&ctx, msg_id, target_px, sound, now_ms);
    let has_descriptor = runtime.descriptor(msg_id).is_some();

    // 1. Video frame, else the blurred thumbnail, else a plain plate.
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let frame = clip
        .as_ref()
        .and_then(|c| c.texture.clone())
        .or_else(|| runtime.round_thumb_texture(&ctx, msg_id, target_px));
    if let Some(texture) = frame {
        ui.painter().image(texture.id(), circle, uv, Color32::WHITE);
    } else if has_descriptor {
        ui.painter()
            .circle_filled(circle.center(), side / 2.0, colors.service_bg);
    } else {
        // Mock rounds carry no media: keep their procedural colours.
        let [r, g, b] = rv.accent_rgb.0;
        ui.painter()
            .circle_filled(circle.center(), side / 2.0, Color32::from_rgb(r, g, b));
    }

    // 2. Seek / progress ring (`Gif::paintRoundPlaybackProgress`): only with sound.
    let resp = ui.interact(
        circle,
        Id::unique(("tg_round_vid", msg_id.0)),
        Sense::click_and_drag(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::slider(
            true,
            f64::from(if sound { audio_progress } else { 0.0 }),
            format!(
                "Round video message ({})",
                format_duration(rv.duration_secs)
            ),
        )
    });
    let pointer = ctx.pointer_interact_pos();
    let in_band = pointer.is_some_and(|pt| VideoMessageSeek::grab_point(circle, pt));
    let mut seeking = runtime
        .round_seeks
        .get(&msg_id)
        .is_some_and(VideoMessageSeek::is_grabbed);
    let mut pending_seek = None;
    if sound {
        if resp.drag_started()
            && let Some(pt) = pointer
            && VideoMessageSeek::grab_point(circle, pt)
        {
            let p = VideoMessageSeek::progress_from_point(circle, pt);
            let seek = runtime.round_seek_mut(msg_id);
            seek.set_grabbed(true, now_secs);
            seek.set_dragged_progress(p, now_secs);
            seeking = true;
        } else if resp.dragged()
            && seeking
            && let Some(pt) = pointer
        {
            let p = VideoMessageSeek::progress_from_point(circle, pt);
            runtime
                .round_seek_mut(msg_id)
                .set_dragged_progress(p, now_secs);
        } else if resp.drag_stopped() && seeking {
            let p = pointer.map_or_else(
                || runtime.round_seek_mut(msg_id).progress(),
                |pt| VideoMessageSeek::progress_from_point(circle, pt),
            );
            let seek = runtime.round_seek_mut(msg_id);
            seek.set_dragged_progress(p, now_secs);
            seek.set_grabbed(false, now_secs);
            seeking = false;
            pending_seek = Some(p);
        }
        if resp.has_focus() {
            let delta = ctx.input(|i| {
                if i.key_pressed(egui::Key::ArrowRight) {
                    Some(0.05)
                } else if i.key_pressed(egui::Key::ArrowLeft) {
                    Some(-0.05)
                } else {
                    None
                }
            });
            if let Some(d) = delta {
                pending_seek = Some((f64::from(audio_progress) + d).clamp(0.0, 1.0));
            }
        }
    }
    if let Some(p) = pending_seek {
        runtime.seek_round_video(peer_id, msg_id, sender_name, rv.duration_secs, p);
    }
    let seek_progress = runtime.round_seek_mut(msg_id).progress();
    let reduced_motion = runtime.policy.power_saving_enabled();
    let ring_repaint = runtime.round_seek_mut(msg_id).paint(
        ui.painter(),
        circle,
        seeking || phase == RoundVideoPhase::SoundPaused,
        seeking,
        if sound {
            f64::from(audio_progress)
        } else {
            -1.0
        },
        false,
        now_secs,
        reduced_motion,
    );
    if ring_repaint || phase == RoundVideoPhase::SoundPlaying {
        ctx.request_repaint();
    }

    // 3. Centre button: radial loader while downloading, play when autoplay is off.
    let center_rect = Rect::from_center_size(circle.center(), Vec2::splat(CENTER_BUTTON_SIZE));
    match phase {
        RoundVideoPhase::NotLoaded => {
            ui.painter().circle_filled(
                center_rect.center(),
                CENTER_BUTTON_SIZE / 2.0,
                colors.date_img_bg,
            );
            runtime.spinner.start(now_secs);
            runtime
                .spinner
                .paint(&ctx, ui.painter(), center_rect.shrink(3.0));
        }
        RoundVideoPhase::Static => {
            ui.painter().circle_filled(
                center_rect.center(),
                CENTER_BUTTON_SIZE / 2.0,
                colors.date_img_bg,
            );
            tg_app_core::ui_icons::paint_style_tinted(
                ui.painter(),
                center_rect,
                st::history_file_thumb_play,
                Color32::WHITE,
            );
        }
        _ => {}
    }

    // 4. Mute badge at the top centre while looping muted (`displayMute`).
    if phase == RoundVideoPhase::SilentLoop && clip.as_ref().is_some_and(|c| c.is_playing) {
        let mute = Rect::from_min_size(
            Pos2::new(
                circle.center().x - MUTE_SIZE / 2.0,
                circle.top() + DATE_IMG_DELTA,
            ),
            Vec2::splat(MUTE_SIZE),
        );
        ui.painter()
            .circle_filled(mute.center(), MUTE_SIZE / 2.0, colors.date_img_bg);
        tg_app_core::ui_icons::paint_style_tinted(
            ui.painter(),
            mute,
            st::history_video_message_mute,
            Color32::WHITE,
        );
    }

    // 5. Duration / remaining-time pill in the bottom-left corner.
    let status_secs = if seeking {
        1 + ((1.0 - seek_progress) * f64::from(rv.duration_secs)).round() as u32
    } else if sound {
        round_remaining_secs(rv.duration_secs, audio_progress)
    } else {
        rv.duration_secs
    };
    let galley = ui.painter().layout_no_wrap(
        format_duration(status_secs),
        FontId::proportional(13.0),
        colors.service_fg,
    );
    let pill_size = galley.size() + 2.0 * DATE_IMG_PADDING;
    let pill = Rect::from_min_size(
        Pos2::new(
            circle.left() + DATE_IMG_DELTA,
            circle.bottom() - DATE_IMG_DELTA - pill_size.y,
        ),
        pill_size,
    );
    ui.painter().rect_filled(
        pill,
        CornerRadius::same((pill_size.y / 2.0).round() as u8),
        colors.service_bg,
    );
    ui.painter()
        .galley(pill.min + DATE_IMG_PADDING, galley, colors.service_fg);

    // 6. Spoiler veil, then click handling.
    let spoiler_hidden = runtime.paint_spoiler_overlay(
        &ctx,
        ui.painter(),
        msg_id,
        circle,
        CornerRadius::same((side / 2.0).round().min(255.0) as u8),
        now_secs,
    );
    if resp.hovered() {
        ctx.set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() && !seeking {
        if spoiler_hidden {
            runtime.reveal_spoiler(msg_id, now_secs);
        } else if sound
            && in_band
            && let Some(pt) = pointer
        {
            let p = VideoMessageSeek::progress_from_point(circle, pt);
            runtime.seek_round_video(peer_id, msg_id, sender_name, rv.duration_secs, p);
        } else {
            runtime.click_round_video(peer_id, msg_id, sender_name, rv.duration_secs, now_ms);
        }
    }
}
