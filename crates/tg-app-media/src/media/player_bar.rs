//! Top audio player bar for voice messages and music tracks backed by `tg-audio::PlayerInstance`.
//!
//! Port origin: `Telegram/SourceFiles/media/player/media_player_widget.h`,
//! `Telegram/SourceFiles/media/player/media_player_widget.cpp`, and `media_player.style`.

use std::sync::{Arc, Mutex};

use egui::{Align2, Area, CornerRadius, Id, Order, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_audio::{
    AudioMsgId, AudioOutputEngine, AudioType, DEFAULT_FREQUENCY, DeviceManager, FullMsgId,
    InMemoryPositionStore, MixerCore, OrderMode, PlayerEvent, PlayerInstance, PlaylistContext,
    RepeatMode, SPEED_MAX, SPEED_MIN, TrackState,
};
use tg_platform::smtc::{
    SMTC_SKIP_MS, SmtcCommand, SmtcControlsState, SmtcMediaType, SmtcMetadata, SmtcStateTracker,
    SystemMediaControls, map_playback_state,
};
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::tg_style::st;

use crate::media::backend::MediaBackend;
use crate::media::listen_tracker::{ListenReport, MusicListenTracker};
use crate::media::strings;
use crate::media::style::{
    MEDIA_PLAYER_ACTIVE_FG, MEDIA_PLAYER_HEIGHT, MEDIA_PLAYER_INACTIVE_FG, MEDIA_PLAYER_WIDE_WIDTH,
    MediaColors, VolumeIconLevel, format_duration_secs, paint_centered_style_icon,
    paint_centered_style_icon_tinted,
};
use tg_app_core::stand_in::theme::Palette;

/// Events emitted by [`AudioPlayerBar::show`] to the host shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlayerBarEvent {
    /// User closed the top audio player bar.
    Closed,
    /// User clicked the voice/audio label to jump to its message in chat.
    ShowItem(FullMsgId),
    /// User clicked the song title area to toggle the playlist panel.
    TogglePlaylist,
}

/// Top audio player bar widget (`Media::Player::Widget`) wrapping [`PlayerInstance`].
pub struct AudioPlayerBar {
    mixer: Arc<Mutex<MixerCore>>,
    player: PlayerInstance,
    active_type: AudioType,
    voice_is_active: bool,
    visible: bool,
    volume: f64,
    last_positive_volume: f64,
    volume_popup_open: bool,
    speed_popup_open: bool,
    seek_progress: Option<f64>,
    software_clock_ms: Option<i64>,
    has_hardware_output: bool,
    smtc: Option<SystemMediaControls>,
    smtc_tracker: SmtcStateTracker,
    smtc_last_order_mode: OrderMode,
    smtc_raise_requested: bool,
    listen_tracker: MusicListenTracker,
    pending_listen_reports: Vec<ListenReport>,
}

impl AudioPlayerBar {
    /// Creates a new [`AudioPlayerBar`] wired to `backend`'s audio source and playlist provider.
    ///
    /// Pass `enable_hardware_output = false` in tests/headless mode so playback advances
    /// deterministically via [`Self::advance_software_ms`] without initializing Windows SMTC.
    #[must_use]
    pub fn new(backend: &dyn MediaBackend, enable_hardware_output: bool) -> Self {
        let mixer = Arc::new(Mutex::new(MixerCore::new()));
        let output = if enable_hardware_output {
            let devices = Arc::new(DeviceManager::new());
            Some(Arc::new(AudioOutputEngine::new(
                devices,
                Arc::clone(&mixer),
            )))
        } else {
            None
        };
        let has_hardware_output = output.is_some();
        let smtc = if enable_hardware_output {
            SystemMediaControls::try_new()
        } else {
            None
        };
        let positions = Arc::new(InMemoryPositionStore::new());
        let player = PlayerInstance::new(
            Arc::clone(&mixer),
            output,
            backend.audio_data_source(),
            positions,
            backend.playlist_provider(),
        );
        Self {
            mixer,
            player,
            active_type: AudioType::Song,
            voice_is_active: false,
            visible: false,
            volume: 1.0,
            last_positive_volume: 1.0,
            volume_popup_open: false,
            speed_popup_open: false,
            seek_progress: None,
            software_clock_ms: None,
            has_hardware_output,
            smtc,
            smtc_tracker: SmtcStateTracker::new(),
            smtc_last_order_mode: OrderMode::Default,
            smtc_raise_requested: false,
            listen_tracker: MusicListenTracker::new(),
            pending_listen_reports: Vec::new(),
        }
    }

    /// Attaches an explicit [`SystemMediaControls`] handle (e.g. for custom wiring).
    pub fn attach_smtc(&mut self, smtc: SystemMediaControls) {
        self.smtc = Some(smtc);
        self.smtc_tracker.reset();
    }

    /// Returns `true` once if the user clicked the Windows SMTC flyout to raise the window.
    pub fn take_raise_request(&mut self) -> bool {
        std::mem::take(&mut self.smtc_raise_requested)
    }

    /// Feeds current music playback state into [`MusicListenTracker`] and drains completed reports
    /// ready to send via `messages.reportMusicListen`.
    pub fn poll_listen_reports(&mut self, now_ms: i64) -> Vec<ListenReport> {
        let track_id = self.player.current(AudioType::Song);
        let state = self.player.get_state(AudioType::Song);
        if let Some(report) = self
            .listen_tracker
            .update(now_ms, track_id, AudioType::Song, &state)
        {
            self.pending_listen_reports.push(report);
        }
        if let Some(report) = self.listen_tracker.poll_timeout(now_ms) {
            self.pending_listen_reports.push(report);
        }
        std::mem::take(&mut self.pending_listen_reports)
    }

    /// Builds the current Windows SMTC control state and track metadata snapshot from
    /// [`PlayerInstance`] and `backend` (port of `SystemMediaControlsManager`).
    #[must_use]
    pub fn build_smtc_snapshot(
        &self,
        backend: &dyn MediaBackend,
    ) -> (SmtcControlsState, Option<SmtcMetadata>) {
        let state = self.current_state();
        let current_id = self.player.current(self.active_type);
        let active = self.visible && current_id.is_valid() && !state.state.is_stopped_or_stopping();
        if !active {
            return (SmtcControlsState::disabled(), None);
        }

        let status = map_playback_state(
            state.state.is_stopped_or_stopping(),
            state.state.is_paused_or_pausing(),
            false,
        );
        let controls = SmtcControlsState::active(
            status,
            self.player.previous_available(self.active_type),
            self.player.next_available(self.active_type),
        );
        let (title, artist) = backend.audio_track_metadata(current_id).map_or_else(
            || (strings::AUDIO_TRACK_FALLBACK.to_owned(), String::new()),
            |m| (m.title, m.subtitle),
        );
        let thumbnail_bytes = backend.audio_track_cover(current_id);
        let metadata = SmtcMetadata {
            title,
            artist,
            album_title: String::new(),
            media_type: SmtcMediaType::Music,
            thumbnail_bytes,
        };
        (controls, Some(metadata))
    }

    /// Applies an [`SmtcCommand`] from OS media keys or the Windows media flyout to the player
    /// (port of `SystemMediaControlsManager` command handler in `system_media_controls_manager.cpp:447-500`).
    ///
    /// Returns `true` when the command mutated player state or requested a window raise.
    pub fn apply_smtc_command(&mut self, cmd: SmtcCommand) -> bool {
        let slot = self.active_type;
        match cmd {
            SmtcCommand::PlayPause => {
                let _ = self.player.play_pause(slot);
                true
            }
            SmtcCommand::Play => {
                let _ = self.player.play(slot);
                true
            }
            SmtcCommand::Pause => {
                self.player.pause(slot);
                true
            }
            SmtcCommand::Next => {
                let _ = self.player.next(slot);
                true
            }
            SmtcCommand::Previous => {
                let _ = self.player.previous(slot);
                true
            }
            SmtcCommand::Stop => {
                self.player.stop(slot, false);
                true
            }
            SmtcCommand::Quit => {
                self.stop_and_close();
                true
            }
            SmtcCommand::Raise => {
                self.smtc_raise_requested = true;
                true
            }
            SmtcCommand::LoopNone => {
                self.player.set_repeat_mode(RepeatMode::None);
                true
            }
            SmtcCommand::LoopTrack => {
                self.player.set_repeat_mode(RepeatMode::One);
                true
            }
            SmtcCommand::LoopPlaylist => {
                self.player.set_repeat_mode(RepeatMode::All);
                true
            }
            SmtcCommand::Shuffle => {
                let cur = self.player.order_mode();
                let next = if cur == OrderMode::Shuffle {
                    self.smtc_last_order_mode
                } else {
                    self.smtc_last_order_mode = cur;
                    OrderMode::Shuffle
                };
                self.player.set_order_mode(next);
                true
            }
            SmtcCommand::SkipForward | SmtcCommand::SkipBackward => {
                let state = self.current_state();
                let len_ms = state.length_ms();
                if len_ms > 0 {
                    let delta = if cmd == SmtcCommand::SkipForward {
                        SMTC_SKIP_MS
                    } else {
                        -SMTC_SKIP_MS
                    };
                    let target = (state.position_ms() + delta).clamp(0, len_ms);
                    let progress = target as f64 / len_ms as f64;
                    let _ = self.player.finish_seeking(slot, progress);
                    true
                } else {
                    false
                }
            }
            SmtcCommand::None => false,
        }
    }

    /// Drains any pending OS SMTC commands, applies them to the player, and synchronizes
    /// incremental state/metadata/cover diffs back to Windows SMTC.
    pub fn poll_smtc(&mut self, backend: &dyn MediaBackend) -> Vec<SmtcCommand> {
        let cmds = self
            .smtc
            .as_ref()
            .map(SystemMediaControls::drain_commands)
            .unwrap_or_default();
        for &cmd in &cmds {
            self.apply_smtc_command(cmd);
        }
        self.sync_smtc(backend);
        cmds
    }

    /// Synchronizes the active track's playback status, buttons, metadata, and cover art to SMTC.
    pub fn sync_smtc(&mut self, backend: &dyn MediaBackend) {
        let (controls, metadata) = self.build_smtc_snapshot(backend);
        let diff = self.smtc_tracker.compute_diff(controls, metadata.as_ref());
        if let Some(smtc) = &self.smtc {
            smtc.apply_diff(diff, metadata.as_ref());
        }
    }

    /// Starts playing `audio` in optional `context` and shows the top player bar.
    pub fn play_item(
        &mut self,
        audio: AudioMsgId,
        context: Option<PlaylistContext>,
    ) -> anyhow::Result<()> {
        self.player
            .play_item(audio, context)
            .map_err(|e| anyhow::anyhow!("audio play failed: {e}"))?;
        if audio.audio_type == AudioType::Voice {
            self.voice_is_active = true;
            self.active_type = AudioType::Voice;
        } else {
            self.active_type = AudioType::Song;
        }
        self.visible = true;
        Ok(())
    }

    /// Returns `true` when the audio player bar should be displayed.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.visible
    }

    /// Returns the currently displayed audio slot (`Voice` or `Song`).
    #[must_use]
    pub const fn active_type(&self) -> AudioType {
        self.active_type
    }

    /// Returns a shared reference to the underlying [`PlayerInstance`].
    #[must_use]
    pub const fn player(&self) -> &PlayerInstance {
        &self.player
    }

    /// Returns a mutable reference to the underlying [`PlayerInstance`].
    pub fn player_mut(&mut self) -> &mut PlayerInstance {
        &mut self.player
    }

    /// Returns the current [`TrackState`] for the active audio slot.
    #[must_use]
    pub fn current_state(&self) -> TrackState {
        self.player.get_state(self.active_type)
    }

    /// Returns the current song/voice volume (`0.0..=1.0`).
    #[must_use]
    pub const fn volume(&self) -> f64 {
        self.volume
    }

    /// Sets song/voice volume (`0.0..=1.0`) and updates [`MixerCore`].
    pub fn set_volume(&mut self, volume: f64) {
        let clamped = volume.clamp(0.0, 1.0);
        if clamped > 0.0 {
            self.last_positive_volume = clamped;
        }
        self.volume = clamped;
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.set_song_volume(clamped);
        }
    }

    /// Toggles mute while remembering the last positive volume (`media_player_widget.cpp:131-138`).
    pub fn toggle_volume(&mut self) -> f64 {
        let next = if self.volume > 0.0 {
            0.0
        } else {
            self.last_positive_volume.clamp(0.1, 1.0)
        };
        self.set_volume(next);
        next
    }

    /// Cycles music repeat mode `None -> One -> All -> None` (`media_player_widget.cpp:149-160`).
    pub fn cycle_repeat_mode(&mut self) -> RepeatMode {
        let next = match self.player.repeat_mode() {
            RepeatMode::None => RepeatMode::One,
            RepeatMode::One => RepeatMode::All,
            RepeatMode::All => RepeatMode::None,
        };
        self.player.set_repeat_mode(next);
        next
    }

    /// Cycles music order mode `Default -> Reverse -> Shuffle -> Default`.
    pub fn cycle_order_mode(&mut self) -> OrderMode {
        let next = match self.player.order_mode() {
            OrderMode::Default => OrderMode::Reverse,
            OrderMode::Reverse => OrderMode::Shuffle,
            OrderMode::Shuffle => OrderMode::Default,
        };
        self.player.set_order_mode(next);
        next
    }

    /// Stops playback and hides the bar (or falls back from Voice to active Song, matching `Widget::stopAndClose`).
    pub fn stop_and_close(&mut self) -> bool {
        self.voice_is_active = false;
        self.volume_popup_open = false;
        self.speed_popup_open = false;
        if self.active_type == AudioType::Voice {
            let song_id = self.player.current(AudioType::Song);
            let song_state = self.player.get_state(AudioType::Song);
            if song_id.is_valid()
                && song_id == song_state.id
                && !song_state.state.is_stopped_or_stopping()
            {
                self.player.stop(AudioType::Voice, false);
                self.active_type = AudioType::Song;
                return false;
            }
        }
        self.player.stop_and_close();
        self.visible = false;
        let diff = self
            .smtc_tracker
            .compute_diff(SmtcControlsState::disabled(), None);
        if let Some(smtc) = &self.smtc {
            smtc.apply_diff(diff, None);
        }
        true
    }

    /// Advances the software mixer by `delta_ms` when running without hardware audio output.
    pub fn advance_software_ms(&mut self, delta_ms: i64) {
        let mut remaining = delta_ms.max(0);
        while remaining > 0 {
            let step = remaining.min(100);
            remaining -= step;
            let frames = (((step as u64) * u64::from(DEFAULT_FREQUENCY)) / 1000) as usize;
            if frames == 0 {
                break;
            }
            let mut buf = vec![0.0_f32; frames * 2];
            let mixer_events = self
                .mixer
                .lock()
                .map(|mut m| m.render_stereo_48k(&mut buf))
                .unwrap_or_default();
            if !mixer_events.is_empty() {
                let _ = self.player.handle_mixer_events(&mixer_events);
            }
        }
        self.sync_active_type();
    }

    fn sync_active_type(&mut self) {
        for ev in self.player.take_events() {
            if matches!(
                ev,
                PlayerEvent::TracksFinished(AudioType::Voice)
                    | PlayerEvent::Stopped(AudioType::Voice)
            ) {
                self.voice_is_active = false;
                let song = self.player.current(AudioType::Song);
                let song_state = self.player.get_state(AudioType::Song);
                if song.is_valid()
                    && song == song_state.id
                    && !song_state.state.is_stopped_or_stopping()
                {
                    self.active_type = AudioType::Song;
                }
            }
        }

        let voice_id = self.player.current(AudioType::Voice);
        let voice_state = self.player.get_state(AudioType::Voice);
        let voice_running = voice_id.is_valid()
            && voice_id == voice_state.id
            && !voice_state.state.is_stopped_or_stopping();
        if voice_running {
            self.voice_is_active = true;
            self.active_type = AudioType::Voice;
        } else if !self.voice_is_active {
            let song_id = self.player.current(AudioType::Song);
            let song_state = self.player.get_state(AudioType::Song);
            if song_id.is_valid()
                && song_id == song_state.id
                && !song_state.state.is_stopped_or_stopping()
            {
                self.active_type = AudioType::Song;
            }
        }
    }

    /// Renders the top audio player bar into `ui` (full width × `MEDIA_PLAYER_HEIGHT`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn MediaBackend,
        palette: &Palette,
        now_ms: i64,
    ) -> Option<AudioPlayerBarEvent> {
        if !self.visible {
            return None;
        }

        // Handle OS SMTC button presses (Play/Pause/Next/Prev/Stop) before painting.
        let _ = self.poll_smtc(backend);
        if !self.visible {
            return Some(AudioPlayerBarEvent::Closed);
        }

        if !self.has_hardware_output {
            let prev = self.software_clock_ms.replace(now_ms).unwrap_or(now_ms);
            let dt = (now_ms - prev).clamp(0, 100);
            self.advance_software_ms(dt);
        } else {
            self.sync_active_type();
        }

        let state = self.current_state();
        let current_id = self.player.current(self.active_type);
        let show_pause = state.state.show_pause_icon() || self.player.is_seeking(self.active_type);
        if show_pause {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }

        let ctx = ui.ctx().clone();
        ensure_tdesktop_fonts(&ctx);
        let colors = MediaColors::from_palette(palette);
        let player_bg = colors.color_or("mediaPlayerBg", palette.window_bg);
        let active_fg = colors.color_or("mediaPlayerActiveFg", MEDIA_PLAYER_ACTIVE_FG);
        let inactive_fg = colors.color_or("mediaPlayerInactiveFg", MEDIA_PLAYER_INACTIVE_FG);
        let hover_bg = colors.color_or("windowBgOver", palette.window_bg_over);
        let bold_fg = colors.color_or("windowBoldFg", palette.window_bold_fg);
        let sub_fg = colors.color_or("windowSubTextFg", palette.window_sub_text_fg);
        let divider_fg = colors.color_or("menuSeparatorFg", palette.divider);

        let bar_w = ui.available_width().max(240.0);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(bar_w, MEDIA_PLAYER_HEIGHT), Sense::hover());
        let painter = ui.painter();
        let mut event = None;

        // Background + bottom divider.
        painter.rect_filled(rect, CornerRadius::ZERO, player_bg);
        painter.line_segment(
            [
                Pos2::new(rect.min.x, rect.max.y),
                Pos2::new(rect.max.x, rect.max.y),
            ],
            Stroke::new(1.0, divider_fg),
        );

        let prev_avail = self.player.previous_available(self.active_type);
        let next_avail = self.player.next_available(self.active_type);
        let has_prev_next = prev_avail || next_avail;

        // ── Left transport controls: [Prev] Play/Pause [Next] ──
        let mut left_x = rect.min.x + st::media_player_play_left.0 as f32;
        let btn_y = rect.min.y + 3.0;
        let btn_h = st::media_player_play_button.size.height.0 as f32 - 2.0;

        if has_prev_next {
            let prev_w = st::media_player_previous_button.width.0 as f32;
            let prev_rect = Rect::from_min_size(Pos2::new(left_x, btn_y), Vec2::new(prev_w, btn_h));
            let prev_resp = ui
                .interact(prev_rect, Id::unique("mp_prev"), Sense::click())
                .on_hover_text(tg_lang::tr::lng_shortcuts_media_previous());
            let prev_icon = if prev_avail {
                &st::media_player_previous_button.icon
            } else {
                &st::media_player_previous_disabled_icon
            };
            paint_centered_style_icon(&ctx, painter, prev_icon, prev_rect, &colors);
            if prev_avail && prev_resp.clicked() {
                let _ = self.player.previous(self.active_type);
            }
            left_x += prev_w + 2.0;
        }

        let play_rect = Rect::from_min_size(Pos2::new(left_x, btn_y), Vec2::new(28.0, btn_h));
        let play_tip = if show_pause {
            tg_lang::tr::lng_shortcuts_media_pause()
        } else {
            tg_lang::tr::lng_shortcuts_media_play()
        };
        let play_resp = ui
            .interact(play_rect, Id::unique("mp_play"), Sense::click())
            .on_hover_text(play_tip);
        if play_resp.hovered() {
            painter.circle_filled(play_rect.center(), 13.0, hover_bg);
        }
        let play_icon = if show_pause {
            &st::pip_pause_icon
        } else {
            &st::pip_play_icon
        };
        paint_centered_style_icon_tinted(&ctx, painter, play_icon, play_rect, &colors, active_fg);
        if play_resp.clicked() {
            let _ = self.player.play_pause(self.active_type);
        }
        left_x += 30.0;

        if has_prev_next {
            let next_w = st::media_player_next_button.width.0 as f32;
            let next_rect = Rect::from_min_size(Pos2::new(left_x, btn_y), Vec2::new(next_w, btn_h));
            let next_resp = ui
                .interact(next_rect, Id::unique("mp_next"), Sense::click())
                .on_hover_text(tg_lang::tr::lng_shortcuts_media_next());
            let next_icon = if next_avail {
                &st::media_player_next_button.icon
            } else {
                &st::media_player_next_disabled_icon
            };
            paint_centered_style_icon(&ctx, painter, next_icon, next_rect, &colors);
            if next_avail && next_resp.clicked() {
                let _ = self.player.next(self.active_type);
            }
            left_x += next_w + 4.0;
        }

        // ── Right controls (right-to-left): Close, Speed, Repeat/Order (Song), Volume, Time ──
        let mut right_x = rect.max.x - 6.0;

        // Close button (`st::mediaPlayerClose`).
        let close_rect =
            Rect::from_min_size(Pos2::new(right_x - 30.0, btn_y), Vec2::new(30.0, btn_h));
        let close_resp = ui
            .interact(close_rect, Id::unique("mp_close"), Sense::click())
            .on_hover_text(tg_lang::tr::lng_sr_player_close());
        if close_resp.hovered() {
            painter.circle_filled(close_rect.center(), 12.0, hover_bg);
        }
        let close_icon = if close_resp.hovered() {
            &st::media_player_close.icon_over
        } else {
            &st::media_player_close.icon
        };
        paint_centered_style_icon(&ctx, painter, close_icon, close_rect, &colors);
        if close_resp.clicked() && self.stop_and_close() {
            event = Some(AudioPlayerBarEvent::Closed);
        }
        right_x = close_rect.min.x - 2.0;

        let narrow = bar_w < MEDIA_PLAYER_WIDE_WIDTH;
        let show_right = !narrow || ui.rect_contains_pointer(rect);

        if show_right {
            // Speed toggle (when changeable_playback_speed or voice).
            if current_id.changeable_playback_speed || self.active_type == AudioType::Voice {
                let speed = self.player.lookup_playback_speed(&current_id);
                let speed_label = format!("{speed:.1}x").replace(".0x", "x");
                let speed_rect =
                    Rect::from_min_size(Pos2::new(right_x - 34.0, btn_y), Vec2::new(34.0, btn_h));
                let speed_resp = ui
                    .interact(speed_rect, Id::unique("mp_speed"), Sense::click())
                    .on_hover_text(tg_lang::tr::lng_mediaview_playback_speed(&speed_label));
                let speed_active = (speed - 1.0).abs() > 0.05 || self.speed_popup_open;
                let speed_col = if speed_active { active_fg } else { sub_fg };
                painter.text(
                    speed_rect.center(),
                    Align2::CENTER_CENTER,
                    speed_label,
                    font_id_from_style(st::media_player_time.font),
                    speed_col,
                );
                if speed_resp.clicked() {
                    if self.active_type == AudioType::Voice {
                        self.player.toggle_voice_fast_speed();
                    } else {
                        self.speed_popup_open = !self.speed_popup_open;
                        self.volume_popup_open = false;
                    }
                }
                if self.speed_popup_open {
                    self.paint_speed_popup(&ctx, speed_rect, &colors);
                }
                right_x = speed_rect.min.x - 2.0;
            }

            if self.active_type == AudioType::Song {
                // Repeat toggle (`st::mediaPlayerRepeat*`).
                let rep_w = st::media_player_repeat_button.width.0 as f32;
                let rep_rect =
                    Rect::from_min_size(Pos2::new(right_x - rep_w, btn_y), Vec2::new(rep_w, btn_h));
                let rep_resp = ui
                    .interact(rep_rect, Id::unique("mp_repeat"), Sense::click())
                    .on_hover_text(tg_lang::tr::lng_schedule_repeat_label());
                if rep_resp.hovered() {
                    painter.circle_filled(rep_rect.center(), 12.0, hover_bg);
                }
                let rep_icon = match (self.player.repeat_mode(), rep_resp.hovered()) {
                    (RepeatMode::None, true) => &st::media_player_repeat_disabled_icon_over,
                    (RepeatMode::None, false) => &st::media_player_repeat_disabled_icon,
                    (RepeatMode::One, _) => &st::media_player_repeat_one_icon,
                    (RepeatMode::All, _) => &st::media_player_repeat_button.icon,
                };
                paint_centered_style_icon(&ctx, painter, rep_icon, rep_rect, &colors);
                if rep_resp.clicked() {
                    self.cycle_repeat_mode();
                }
                right_x = rep_rect.min.x - 2.0;

                // Order toggle (`st::mediaPlayerReverse*` / `st::mediaPlayerShuffleIcon`).
                let ord_w = st::media_player_order_button.width.0 as f32;
                let ord_rect =
                    Rect::from_min_size(Pos2::new(right_x - ord_w, btn_y), Vec2::new(ord_w, btn_h));
                let ord_tip = match self.player.order_mode() {
                    OrderMode::Default => tg_lang::tr::lng_sr_playback_order(),
                    OrderMode::Reverse => tg_lang::tr::lng_audio_player_reverse(),
                    OrderMode::Shuffle => tg_lang::tr::lng_audio_player_shuffle(),
                };
                let ord_resp = ui
                    .interact(ord_rect, Id::unique("mp_order"), Sense::click())
                    .on_hover_text(ord_tip);
                if ord_resp.hovered() {
                    painter.circle_filled(ord_rect.center(), 12.0, hover_bg);
                }
                let ord_icon = match (self.player.order_mode(), ord_resp.hovered()) {
                    (OrderMode::Default, true) => &st::media_player_reverse_disabled_icon_over,
                    (OrderMode::Default, false) => &st::media_player_reverse_disabled_icon,
                    (OrderMode::Reverse, _) => &st::media_player_reverse_icon,
                    (OrderMode::Shuffle, _) => &st::media_player_shuffle_icon,
                };
                paint_centered_style_icon(&ctx, painter, ord_icon, ord_rect, &colors);
                if ord_resp.clicked() {
                    self.cycle_order_mode();
                }
                right_x = ord_rect.min.x - 2.0;
            }

            // Volume toggle + vertical slider dropdown (`st::mediaPlayerVolume*`).
            let vol_w = st::media_player_volume_toggle.width.0 as f32;
            let vol_rect =
                Rect::from_min_size(Pos2::new(right_x - vol_w, btn_y), Vec2::new(vol_w, btn_h));
            let vol_resp = ui
                .interact(vol_rect, Id::unique("mp_vol"), Sense::click())
                .on_hover_text(tg_lang::tr::lng_ringtones_box_volume());
            if vol_resp.hovered() {
                painter.circle_filled(vol_rect.center(), 12.0, hover_bg);
            }
            let vol_level = if self.volume <= 0.0 {
                VolumeIconLevel::Off
            } else if self.volume < 0.66 {
                VolumeIconLevel::Small
            } else {
                VolumeIconLevel::Full
            };
            let vol_icon = match vol_level {
                VolumeIconLevel::Off => &st::media_player_volume_icon0,
                VolumeIconLevel::Small => &st::media_player_volume_icon1,
                VolumeIconLevel::Full => &st::media_player_volume_toggle.icon,
            };
            paint_centered_style_icon(&ctx, painter, vol_icon, vol_rect, &colors);
            if vol_resp.clicked() {
                self.volume_popup_open = !self.volume_popup_open;
                self.speed_popup_open = false;
            }
            if vol_resp.secondary_clicked() {
                self.toggle_volume();
            }
            if self.volume_popup_open {
                self.paint_volume_popup(&ctx, vol_rect, &colors);
            }
            right_x = vol_rect.min.x - 6.0;

            // Elapsed time text (`st::mediaPlayerTime`).
            let dur_ms = state.length_ms().max(current_id.duration_ms).max(0);
            let pos_ms = if let Some(frac) = self.seek_progress {
                (frac * dur_ms as f64).round() as i64
            } else {
                state.position_ms()
            };
            let time_text = format_duration_secs(pos_ms / 1000);
            painter.text(
                Pos2::new(right_x, rect.center().y - 1.0),
                Align2::RIGHT_CENTER,
                time_text,
                font_id_from_style(st::media_player_time.font),
                sub_fg,
            );
            right_x -= 42.0;
        }

        // ── Center track title + subtitle (`st::mediaPlayerName`) ──
        let label_rect = Rect::from_min_max(
            Pos2::new(left_x + 4.0, rect.min.y),
            Pos2::new(right_x.max(left_x + 40.0), rect.max.y - 6.0),
        );
        let label_resp = ui.interact(label_rect, Id::unique("mp_title"), Sense::click());
        let (title, subtitle) = backend.audio_track_metadata(current_id).map_or_else(
            || (strings::AUDIO_TRACK_FALLBACK.to_owned(), String::new()),
            |m| (m.title, m.subtitle),
        );
        let display_line = if subtitle.is_empty() {
            title
        } else {
            format!("{title} — {subtitle}")
        };
        painter.with_clip_rect(label_rect).text(
            Pos2::new(label_rect.min.x, label_rect.center().y),
            Align2::LEFT_CENTER,
            display_line,
            font_id_from_style(st::media_player_name.style.font),
            bold_fg,
        );
        if label_resp.clicked() {
            if self.active_type == AudioType::Voice {
                event = Some(AudioPlayerBarEvent::ShowItem(current_id.context_id));
            } else {
                event = Some(AudioPlayerBarEvent::TogglePlaylist);
            }
        }

        // ── Bottom edge progress scrubber (`mediaPlayerPlayback`: 2px resting, 6px hit/hover) ──
        let slider_hit = Rect::from_min_max(
            Pos2::new(rect.min.x, rect.max.y - 6.0),
            Pos2::new(rect.max.x, rect.max.y),
        );
        let can_seek = self.active_type == AudioType::Song;
        let slider_resp = ui.interact(slider_hit, Id::unique("mp_seek"), Sense::click_and_drag());
        let progress = if let Some(frac) = self.seek_progress {
            frac as f32
        } else {
            state.progress() as f32
        };

        if can_seek {
            if slider_resp.drag_started() {
                self.player.start_seeking(self.active_type);
            }
            if slider_resp.dragged()
                && let Some(pos) = slider_resp.interact_pointer_pos()
            {
                let frac =
                    f64::from(((pos.x - slider_hit.min.x) / slider_hit.width()).clamp(0.0, 1.0));
                self.seek_progress = Some(frac);
                let _ = self.player.update_seeking(self.active_type, frac);
            } else if slider_resp.drag_stopped() {
                if let Some(frac) = self.seek_progress.take() {
                    let _ = self.player.finish_seeking(self.active_type, frac);
                }
            } else if slider_resp.clicked()
                && let Some(pos) = slider_resp.interact_pointer_pos()
            {
                let frac =
                    f64::from(((pos.x - slider_hit.min.x) / slider_hit.width()).clamp(0.0, 1.0));
                let _ = self.player.finish_seeking(self.active_type, frac);
            }
        }

        let bar_h = if can_seek && (slider_resp.hovered() || slider_resp.dragged()) {
            4.0
        } else {
            2.0
        };
        let track = Rect::from_min_max(
            Pos2::new(rect.min.x, rect.max.y - bar_h),
            Pos2::new(rect.max.x, rect.max.y),
        );
        painter.rect_filled(track, CornerRadius::ZERO, inactive_fg);
        let filled = Rect::from_min_max(
            track.min,
            Pos2::new(
                track.min.x + track.width() * progress.clamp(0.0, 1.0),
                track.max.y,
            ),
        );
        painter.rect_filled(filled, CornerRadius::ZERO, active_fg);

        // Push any button/track transitions triggered during this frame to Windows SMTC.
        self.sync_smtc(backend);

        event
    }

    fn paint_volume_popup(&mut self, ctx: &egui::Context, anchor: Rect, colors: &MediaColors<'_>) {
        let popup_bg = colors.color_or("menuBg", egui::Color32::WHITE);
        let divider = colors.color_or("menuSeparatorFg", egui::Color32::GRAY);
        let active_fg = colors.color_or("mediaPlayerActiveFg", MEDIA_PLAYER_ACTIVE_FG);
        let inactive_fg = colors.color_or("mediaPlayerInactiveFg", MEDIA_PLAYER_INACTIVE_FG);
        let popup_pos = Pos2::new(anchor.center().x - 16.0, anchor.max.y + 4.0);
        Area::new(Id::unique("mp_vol_dropdown"))
            .order(Order::Foreground)
            .fixed_pos(popup_pos)
            .show(ctx, |ui| {
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(32.0, 108.0), Sense::click_and_drag());
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(6), popup_bg);
                painter.rect_stroke(
                    rect,
                    CornerRadius::same(6),
                    Stroke::new(1.0, divider),
                    egui::StrokeKind::Inside,
                );
                let track = Rect::from_min_max(
                    Pos2::new(rect.center().x - 2.0, rect.min.y + 12.0),
                    Pos2::new(rect.center().x + 2.0, rect.max.y - 12.0),
                );
                if (resp.clicked() || resp.dragged())
                    && let Some(ptr) = resp.interact_pointer_pos()
                {
                    let frac = 1.0 - ((ptr.y - track.min.y) / track.height()).clamp(0.0, 1.0);
                    self.set_volume(f64::from(frac));
                }
                painter.rect_filled(track, CornerRadius::same(2), inactive_fg);
                let fill_top = track.max.y - track.height() * self.volume as f32;
                let filled = Rect::from_min_max(Pos2::new(track.min.x, fill_top), track.max);
                painter.rect_filled(filled, CornerRadius::same(2), active_fg);
                painter.circle_filled(Pos2::new(track.center().x, fill_top), 5.0, active_fg);
            });
    }

    fn paint_speed_popup(&mut self, ctx: &egui::Context, anchor: Rect, colors: &MediaColors<'_>) {
        let popup_bg = colors.color_or("menuBg", egui::Color32::WHITE);
        let popup_over = colors.color_or("menuBgOver", egui::Color32::LIGHT_GRAY);
        let popup_fg = colors.color_or("menuFg", egui::Color32::BLACK);
        let divider = colors.color_or("menuSeparatorFg", egui::Color32::GRAY);
        let presets = [0.5_f64, 1.0, 1.5, 1.7, 2.0, 2.5];
        let popup_pos = Pos2::new(anchor.max.x - 74.0, anchor.max.y + 4.0);
        Area::new(Id::unique("mp_speed_dropdown"))
            .order(Order::Foreground)
            .fixed_pos(popup_pos)
            .show(ctx, |ui| {
                let h = presets.len() as f32 * 26.0 + 8.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::new(74.0, h), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(6), popup_bg);
                painter.rect_stroke(
                    rect,
                    CornerRadius::same(6),
                    Stroke::new(1.0, divider),
                    egui::StrokeKind::Inside,
                );
                for (i, &spd) in presets.iter().enumerate() {
                    let row = Rect::from_min_size(
                        Pos2::new(rect.min.x + 4.0, rect.min.y + 4.0 + i as f32 * 26.0),
                        Vec2::new(66.0, 26.0),
                    );
                    let resp = ui.interact(row, Id::unique(("mp_spd_opt", i)), Sense::click());
                    if resp.hovered() {
                        painter.rect_filled(row, CornerRadius::same(4), popup_over);
                    }
                    painter.text(
                        row.center(),
                        Align2::CENTER_CENTER,
                        format!("{spd}x"),
                        font_id_from_style(st::media_player_time.font),
                        popup_fg,
                    );
                    if resp.clicked() {
                        let clamped = spd.clamp(SPEED_MIN, SPEED_MAX);
                        self.player.set_playback_speed(clamped);
                        self.speed_popup_open = false;
                    }
                }
            });
    }
}
