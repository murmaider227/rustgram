//! Floating video playback controls bar for the fullscreen media viewer.
//!
//! Port origin: `Telegram/SourceFiles/media/view/media_view_playback_controls.h`,
//! `Telegram/SourceFiles/media/view/media_view_playback_controls.cpp`, and
//! `Telegram/SourceFiles/media/view/media_view_playback_progress.cpp`.

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::tg_style::st;
use tg_video::{SPEED_MAX, SPEED_MIN};

use crate::media::style::{
    MEDIAVIEW_CONTROLLER_HEIGHT, MEDIAVIEW_CONTROLLER_RADIUS, MEDIAVIEW_CONTROLLER_WIDTH,
    MEDIAVIEW_HIDE_DURATION_MS, MEDIAVIEW_MENU_BG, MEDIAVIEW_MENU_BG_OVER, MEDIAVIEW_MENU_FG,
    MEDIAVIEW_PLAYBACK_ACTIVE, MEDIAVIEW_PLAYBACK_ACTIVE_OVER, MEDIAVIEW_PLAYBACK_BUFFERED,
    MEDIAVIEW_PLAYBACK_ICON_FG, MEDIAVIEW_PLAYBACK_ICON_FG_OVER, MEDIAVIEW_PLAYBACK_INACTIVE,
    MEDIAVIEW_PLAYBACK_PROGRESS_FG, MEDIAVIEW_SAVE_MSG_BG, MEDIAVIEW_SHOW_DURATION_MS,
    MEDIAVIEW_TEXT_LINK_FG, MEDIAVIEW_WAIT_HIDE_MS, MediaColors, VolumeIconLevel,
    format_duration_secs, format_remaining_secs, paint_centered_style_icon_alpha, with_opacity,
};

/// Minimum progress delta used by chapter navigation (`kEps = 0.005` in `media_view_playback_controls.cpp:29`).
pub const TIMESTAMP_EPS: f64 = 0.005;

/// Standard video playback speed presets offered in the speed popup menu.
pub const SPEED_PRESETS: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5];

/// Video chapter timestamp marker shown as a gap on the playback progress bar.
#[derive(Debug, Clone, PartialEq)]
pub struct TimestampChapter {
    /// Normalized position in `[0.0, 1.0]`.
    pub position: f64,
    /// Chapter title label.
    pub label: String,
}

/// Actions emitted by [`PlaybackControlsState::show`] when the user interacts with controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackControlsAction {
    /// Start or resume video playback.
    Play,
    /// Pause video playback.
    Pause,
    /// Continuous scrubbing update while dragging the timeline slider (position in ms).
    SeekProgress(i64),
    /// Final seek commit on pointer release (position in ms).
    SeekFinished(i64),
    /// Volume slider adjusted (`0.0..=1.0`).
    VolumeChanged(f64),
    /// Volume mute button toggled.
    VolumeToggled,
    /// Playback speed selected (`SPEED_MIN..=SPEED_MAX`).
    SpeedChanged(f64),
    /// Fullscreen video mode toggled.
    ToggleFullScreen,
    /// Switch active video into Picture-in-Picture floating window.
    ToPictureInPicture,
}

/// State model for the video playback controls overlay bar (`Media::View::PlaybackControls`).
#[derive(Debug, Clone)]
pub struct PlaybackControlsState {
    /// Whether the video is currently playing (or seeking while playing).
    pub show_pause: bool,
    /// Current playback position in milliseconds.
    pub position_ms: i64,
    /// Total duration in milliseconds.
    pub duration_ms: i64,
    /// Buffered/preloaded fraction in `[0.0, 1.0]`.
    pub buffered_fraction: f64,
    /// Active drag-scrub position in milliseconds (`None` when not dragging).
    pub seek_position_ms: Option<i64>,
    /// Current volume in `[0.0, 1.0]`.
    pub volume: f64,
    /// Last non-zero volume restored when unmuting.
    pub last_positive_volume: f64,
    /// Playback speed multiplier (`SPEED_MIN..=SPEED_MAX`).
    pub speed: f64,
    /// Whether the speed dropdown popup is open.
    pub speed_menu_open: bool,
    /// Whether fullscreen video mode is active.
    pub in_fullscreen: bool,
    /// Downloaded bytes count (for progressive loading percentage badge).
    pub loading_ready: u64,
    /// Total file size in bytes.
    pub loading_total: u64,
    /// Optional video chapter timestamps.
    pub timestamps: Vec<TimestampChapter>,
    /// Current opacity in `[0.0, 1.0]` for auto-hide fade.
    pub opacity: f32,
    /// Last activity timestamp in milliseconds for idle auto-hide.
    pub last_activity_ms: i64,
    /// Whether the pointer is currently hovering or dragging inside the controls bar.
    pub pointer_active: bool,
}

impl Default for PlaybackControlsState {
    fn default() -> Self {
        Self {
            show_pause: false,
            position_ms: 0,
            duration_ms: 0,
            buffered_fraction: 1.0,
            seek_position_ms: None,
            volume: 1.0,
            last_positive_volume: 1.0,
            speed: 1.0,
            speed_menu_open: false,
            in_fullscreen: false,
            loading_ready: 0,
            loading_total: 0,
            timestamps: Vec::new(),
            opacity: 1.0,
            last_activity_ms: 0,
            pointer_active: false,
        }
    }
}

impl PlaybackControlsState {
    /// Updates playback position, duration, playing status, and buffered fraction.
    pub fn update_playback(
        &mut self,
        playing: bool,
        position_ms: i64,
        duration_ms: i64,
        buffered_fraction: f64,
    ) {
        self.show_pause = playing || self.seek_position_ms.is_some();
        self.duration_ms = duration_ms.max(0);
        self.position_ms = position_ms.clamp(0, self.duration_ms.max(position_ms.max(0)));
        self.buffered_fraction = buffered_fraction.clamp(0.0, 1.0);
    }

    /// Updates download progress indicator (`setLoadingProgress`).
    pub fn set_loading_progress(&mut self, ready: u64, total: u64) {
        self.loading_ready = ready;
        self.loading_total = total;
    }

    /// Returns the download percentage text when partially downloaded (e.g. `Some("64%")`).
    #[must_use]
    pub fn download_percent_text(&self) -> Option<String> {
        if self.loading_total == 0
            || self.loading_ready == 0
            || self.loading_ready >= self.loading_total
        {
            return None;
        }
        let pct = ((self.loading_ready as f64 / self.loading_total as f64) * 100.0).round() as u32;
        Some(format!("{}%", pct.min(99)))
    }

    /// Sets volume clamped to `[0.0, 1.0]`, remembering the last positive value for unmute.
    pub fn set_volume(&mut self, volume: f64) {
        let clamped = volume.clamp(0.0, 1.0);
        if clamped > 0.0 {
            self.last_positive_volume = clamped;
        }
        self.volume = clamped;
    }

    /// Toggles mute, restoring `last_positive_volume` when unmuting.
    pub fn toggle_volume(&mut self) -> f64 {
        if self.volume > 0.0 {
            self.last_positive_volume = self.volume;
            self.volume = 0.0;
        } else {
            self.volume = self.last_positive_volume.clamp(0.1, 1.0);
        }
        self.volume
    }

    /// Sets playback speed clamped to `[SPEED_MIN, SPEED_MAX]`.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(SPEED_MIN, SPEED_MAX);
    }

    /// Returns the current speaker icon level.
    #[must_use]
    pub fn volume_icon_level(&self) -> VolumeIconLevel {
        VolumeIconLevel::from_volume(self.volume)
    }

    /// Returns the displayed playback progress in `[0.0, 1.0]` (using live scrub position if dragging).
    #[must_use]
    pub fn displayed_progress(&self) -> f64 {
        if self.duration_ms <= 0 {
            return 0.0;
        }
        let pos = self.seek_position_ms.unwrap_or(self.position_ms);
        (pos as f64 / self.duration_ms as f64).clamp(0.0, 1.0)
    }

    /// Returns `(elapsed_text, remaining_text)` matching `PlaybackControls::refreshTimeTexts`.
    #[must_use]
    pub fn time_texts(&self) -> (String, String) {
        let pos_ms = self.seek_position_ms.unwrap_or(self.position_ms).max(0);
        let dur_ms = self.duration_ms.max(0);
        let already_secs = pos_ms / 1000;
        let total_secs = dur_ms / 1000;
        let left_secs = (total_secs - already_secs).max(0);
        (
            format_duration_secs(already_secs),
            format_remaining_secs(left_secs),
        )
    }

    /// Formats the compact speed badge label (e.g. `"1x"`, `"1.5x"`, `"2x"`).
    #[must_use]
    pub fn speed_label(&self) -> String {
        let rounded = (self.speed * 100.0).round() / 100.0;
        if (rounded - rounded.round()).abs() < 0.01 {
            format!("{}x", rounded.round() as i32)
        } else if ((rounded * 10.0) - (rounded * 10.0).round()).abs() < 0.01 {
            format!("{rounded:.1}x")
        } else {
            format!("{rounded:.2}x")
        }
    }

    /// Returns the next chapter strictly after `progress + TIMESTAMP_EPS` (`PlaybackControls::nextTimestamp`).
    #[must_use]
    pub fn next_timestamp(&self, progress: f64) -> Option<&TimestampChapter> {
        self.timestamps
            .iter()
            .find(|ts| ts.position > progress + TIMESTAMP_EPS)
    }

    /// Returns the previous chapter strictly before `progress - TIMESTAMP_EPS` (`PlaybackControls::prevTimestamp`).
    #[must_use]
    pub fn prev_timestamp(&self, progress: f64) -> Option<&TimestampChapter> {
        self.timestamps
            .iter()
            .rev()
            .find(|ts| ts.position < progress - TIMESTAMP_EPS)
    }

    /// Returns the chapter label active at the current displayed progress, if any.
    #[must_use]
    pub fn current_chapter_label(&self) -> Option<&str> {
        let progress = self.displayed_progress();
        self.timestamps
            .iter()
            .rev()
            .find(|ts| ts.position <= progress && !ts.label.is_empty())
            .map(|ts| ts.label.as_str())
    }

    /// Marks user activity at `now_ms`, resetting the idle auto-hide timer.
    pub fn note_activity(&mut self, now_ms: i64) {
        self.last_activity_ms = now_ms;
    }

    /// Updates auto-hide opacity according to `now_ms` (`mediaviewWaitHide` / `HideDuration` / `ShowDuration`).
    pub fn step_fade(&mut self, now_ms: i64, keep_visible: bool) {
        let hold_visible = keep_visible
            || self.pointer_active
            || self.speed_menu_open
            || self.seek_position_ms.is_some()
            || !self.show_pause;
        if hold_visible {
            let step = 16.0 / MEDIAVIEW_SHOW_DURATION_MS as f32;
            self.opacity = (self.opacity + step).min(1.0);
            self.last_activity_ms = now_ms;
            return;
        }
        let idle_ms = (now_ms - self.last_activity_ms).max(0) as u64;
        if idle_ms <= MEDIAVIEW_WAIT_HIDE_MS {
            self.opacity = 1.0;
        } else {
            let fade_elapsed = (idle_ms - MEDIAVIEW_WAIT_HIDE_MS) as f32;
            let t = (fade_elapsed / MEDIAVIEW_HIDE_DURATION_MS as f32).clamp(0.0, 1.0);
            self.opacity = 1.0 - t;
        }
    }

    /// Computes the centered controls pill rect inside `viewport_rect` above the bottom footer.
    #[must_use]
    pub fn layout_rect(&self, viewport_rect: Rect, bottom_margin: f32) -> Rect {
        let w = MEDIAVIEW_CONTROLLER_WIDTH.min((viewport_rect.width() - 32.0).max(300.0));
        let h = if self.current_chapter_label().is_some() {
            MEDIAVIEW_CONTROLLER_HEIGHT + 14.0
        } else {
            MEDIAVIEW_CONTROLLER_HEIGHT
        };
        let x = viewport_rect.center().x - w * 0.5;
        let y = (viewport_rect.max.y - bottom_margin - h).max(viewport_rect.min.y + 8.0);
        Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
    }

    /// Renders the floating dark playback controls pill and returns any emitted actions.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, id_salt: Id) -> Vec<PlaybackControlsAction> {
        let mut actions = Vec::new();
        if self.opacity <= 0.01 {
            self.pointer_active = false;
            return actions;
        }
        let ctx = ui.ctx().clone();
        ensure_tdesktop_fonts(&ctx);
        let colors = MediaColors::from_ctx(&ctx);
        let save_msg_bg = colors.color_or("mediaviewSaveMsgBg", MEDIAVIEW_SAVE_MSG_BG);
        let playback_icon_fg =
            colors.color_or("mediaviewPlaybackIconFg", MEDIAVIEW_PLAYBACK_ICON_FG);
        let playback_icon_over = colors.color_or(
            "mediaviewPlaybackIconFgOver",
            MEDIAVIEW_PLAYBACK_ICON_FG_OVER,
        );
        let playback_progress_fg = colors.color_or(
            "mediaviewPlaybackProgressFg",
            MEDIAVIEW_PLAYBACK_PROGRESS_FG,
        );
        let text_link_fg = colors.color_or("mediaviewTextLinkFg", MEDIAVIEW_TEXT_LINK_FG);
        let menu_bg = colors.color_or("mediaviewMenuBg", MEDIAVIEW_MENU_BG);
        let menu_bg_over = colors.color_or("mediaviewMenuBgOver", MEDIAVIEW_MENU_BG_OVER);
        let menu_fg = colors.color_or("mediaviewMenuFg", MEDIAVIEW_MENU_FG);
        let ripple_bg = colors.color_or(
            "mediaviewPlaybackIconRipple",
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22),
        );

        let alpha = self.opacity;
        let painter = ui.painter();

        // Background pill (`mediaviewSaveMsgBg`, radius `st::mediaviewControllerRadius`).
        painter.rect_filled(
            rect,
            CornerRadius::same(MEDIAVIEW_CONTROLLER_RADIUS as u8),
            with_opacity(save_msg_bg, alpha),
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(MEDIAVIEW_CONTROLLER_RADIUS as u8),
            Stroke::new(1.0, with_opacity(ripple_bg, alpha)),
            egui::StrokeKind::Inside,
        );

        let bar_resp = ui.interact(rect, id_salt.with("bg"), Sense::hover());
        let mut active_now = bar_resp.hovered();

        // ── Top row: Volume (left), Play/Pause (center), Fullscreen + PiP + Speed (right) ──
        let vol_btn_w = st::mediaview_volume_toggle.width.0 as f32;
        let vol_btn_h = st::mediaview_volume_toggle.height.0 as f32;
        let vol_btn_rect = Rect::from_min_size(
            Pos2::new(rect.min.x + 8.0, rect.min.y + 6.0),
            Vec2::new(vol_btn_w, vol_btn_h),
        );
        let vol_btn_resp = ui
            .interact(vol_btn_rect, id_salt.with("vol_btn"), Sense::click())
            .on_hover_text(tg_lang::tr::lng_ringtones_box_volume());
        active_now |= vol_btn_resp.hovered();
        let vol_icon = match (self.volume_icon_level(), vol_btn_resp.hovered()) {
            (VolumeIconLevel::Off, true) => &st::mediaview_volume_icon0_over,
            (VolumeIconLevel::Off, false) => &st::mediaview_volume_icon0,
            (VolumeIconLevel::Small, true) => &st::mediaview_volume_icon1_over,
            (VolumeIconLevel::Small, false) => &st::mediaview_volume_icon1,
            (VolumeIconLevel::Full, true) => &st::mediaview_volume_icon2_over,
            (VolumeIconLevel::Full, false) => &st::mediaview_volume_icon2,
        };
        paint_centered_style_icon_alpha(&ctx, painter, vol_icon, vol_btn_rect, &colors, alpha);
        if vol_btn_resp.clicked() {
            self.toggle_volume();
            actions.push(PlaybackControlsAction::VolumeToggled);
        }

        // Volume slider (`st::mediaviewVolumeWidth = 75px`).
        let vol_slider_rect = Rect::from_min_size(
            Pos2::new(vol_btn_rect.max.x + 4.0, vol_btn_rect.center().y - 8.0),
            Vec2::new(st::mediaview_volume_width.0 as f32, 16.0),
        );
        let vol_slider_resp = ui.interact(
            vol_slider_rect,
            id_salt.with("vol_slider"),
            Sense::click_and_drag(),
        );
        active_now |= vol_slider_resp.hovered() || vol_slider_resp.dragged();
        if (vol_slider_resp.clicked() || vol_slider_resp.dragged())
            && let Some(pos) = vol_slider_resp.interact_pointer_pos()
        {
            let frac = ((pos.x - vol_slider_rect.min.x) / vol_slider_rect.width()).clamp(0.0, 1.0);
            self.set_volume(f64::from(frac));
            actions.push(PlaybackControlsAction::VolumeChanged(self.volume));
        }
        paint_slider(
            painter,
            vol_slider_rect,
            self.volume as f32,
            1.0,
            vol_slider_resp.hovered() || vol_slider_resp.dragged(),
            alpha,
            &[],
            &colors,
        );

        // Center Play / Pause button (`st::mediaviewPlayButton: 40x40`).
        let play_rect = Rect::from_center_size(
            Pos2::new(rect.center().x, rect.min.y + 22.0),
            Vec2::new(
                st::mediaview_play_button.width.0 as f32,
                st::mediaview_play_button.height.0 as f32,
            ),
        );
        let play_tip = if self.show_pause {
            tg_lang::tr::lng_shortcuts_media_pause()
        } else {
            tg_lang::tr::lng_shortcuts_media_play()
        };
        let play_resp = ui
            .interact(play_rect, id_salt.with("play"), Sense::click())
            .on_hover_text(play_tip);
        active_now |= play_resp.hovered();
        if play_resp.hovered() {
            painter.circle_filled(play_rect.center(), 18.0, with_opacity(ripple_bg, alpha));
        }
        let play_icon = match (self.show_pause, play_resp.hovered()) {
            (true, true) => &st::mediaview_pause_icon_over,
            (true, false) => &st::mediaview_pause_icon,
            (false, true) => &st::mediaview_play_button.icon_over,
            (false, false) => &st::mediaview_play_button.icon,
        };
        paint_centered_style_icon_alpha(&ctx, painter, play_icon, play_rect, &colors, alpha);
        if play_resp.clicked() {
            if self.show_pause {
                actions.push(PlaybackControlsAction::Pause);
            } else {
                actions.push(PlaybackControlsAction::Play);
            }
        }

        // Right-side buttons: Speed, PiP, Fullscreen (right-to-left matching `resizeEvent:497-505`).
        let speed_str = self.speed_label();
        let speed_rect = Rect::from_min_size(
            Pos2::new(rect.max.x - 8.0 - 38.0, rect.min.y + 6.0),
            Vec2::new(38.0, 32.0),
        );
        let speed_resp = ui
            .interact(speed_rect, id_salt.with("speed"), Sense::click())
            .on_hover_text(tg_lang::tr::lng_mediaview_playback_speed(&speed_str));
        active_now |= speed_resp.hovered();
        let speed_active = self.speed_menu_open || (self.speed - 1.0).abs() > 0.05;
        let speed_fg = with_opacity(
            if speed_active {
                text_link_fg
            } else if speed_resp.hovered() {
                playback_icon_over
            } else {
                playback_icon_fg
            },
            alpha,
        );
        painter.rect_stroke(
            speed_rect.shrink2(Vec2::new(2.0, 5.0)),
            CornerRadius::same(5),
            Stroke::new(1.2, with_opacity(speed_fg, 0.7)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            speed_rect.center(),
            Align2::CENTER_CENTER,
            speed_str,
            font_id_from_style(st::mediaview_play_progress_label.font),
            speed_fg,
        );
        if speed_resp.clicked() {
            self.speed_menu_open = !self.speed_menu_open;
        }

        let pip_w = st::mediaview_pip_button.width.0 as f32;
        let pip_h = st::mediaview_pip_button.height.0 as f32;
        let pip_rect = Rect::from_min_size(
            Pos2::new(speed_rect.min.x - 4.0 - pip_w, rect.min.y + 6.0),
            Vec2::new(pip_w, pip_h),
        );
        let pip_resp = ui.interact(pip_rect, id_salt.with("pip"), Sense::click());
        active_now |= pip_resp.hovered();
        let pip_icon = if pip_resp.hovered() {
            &st::mediaview_pip_button.icon_over
        } else {
            &st::mediaview_pip_button.icon
        };
        paint_centered_style_icon_alpha(&ctx, painter, pip_icon, pip_rect, &colors, alpha);
        if pip_resp.clicked() {
            actions.push(PlaybackControlsAction::ToPictureInPicture);
        }

        let fs_w = st::mediaview_full_screen_button.width.0 as f32;
        let fs_h = st::mediaview_full_screen_button.height.0 as f32;
        let fs_rect = Rect::from_min_size(
            Pos2::new(pip_rect.min.x - 4.0 - fs_w, rect.min.y + 6.0),
            Vec2::new(fs_w, fs_h),
        );
        let fs_resp = ui
            .interact(fs_rect, id_salt.with("fs"), Sense::click())
            .on_hover_text(tg_lang::tr::lng_shortcuts_media_fullscreen());
        active_now |= fs_resp.hovered();
        let fs_icon = match (self.in_fullscreen, fs_resp.hovered()) {
            (true, true) => &st::mediaview_full_screen_out_icon_over,
            (true, false) => &st::mediaview_full_screen_out_icon,
            (false, true) => &st::mediaview_full_screen_button.icon_over,
            (false, false) => &st::mediaview_full_screen_button.icon,
        };
        paint_centered_style_icon_alpha(&ctx, painter, fs_icon, fs_rect, &colors, alpha);
        if fs_resp.clicked() {
            actions.push(PlaybackControlsAction::ToggleFullScreen);
        }

        // Optional download percentage label between Play and Fullscreen buttons.
        if let Some(dl_text) = self.download_percent_text() {
            let mid_x = (play_rect.max.x + fs_rect.min.x) * 0.5;
            painter.text(
                Pos2::new(mid_x, play_rect.center().y),
                Align2::CENTER_CENTER,
                dl_text,
                font_id_from_style(st::mediaview_play_progress_label.font),
                with_opacity(playback_progress_fg, alpha),
            );
        }

        // ── Bottom row: Elapsed time | Seek slider with chapters | Remaining time ──
        let (elapsed_text, remaining_text) = self.time_texts();
        let time_y = rect.min.y + 53.0;
        let time_font = font_id_from_style(st::mediaview_play_progress_label.font);
        let time_color = with_opacity(playback_progress_fg, alpha);

        painter.text(
            Pos2::new(rect.min.x + 14.0, time_y),
            Align2::LEFT_CENTER,
            &elapsed_text,
            time_font.clone(),
            time_color,
        );
        painter.text(
            Pos2::new(rect.max.x - 14.0, time_y),
            Align2::RIGHT_CENTER,
            &remaining_text,
            time_font,
            time_color,
        );

        let slider_left = rect.min.x + 62.0;
        let slider_right = rect.max.x - 66.0;
        let slider_rect = Rect::from_min_max(
            Pos2::new(slider_left, time_y - 9.0),
            Pos2::new(slider_right.max(slider_left + 40.0), time_y + 9.0),
        );
        let slider_resp = ui.interact(
            slider_rect,
            id_salt.with("seek_slider"),
            Sense::click_and_drag(),
        );
        active_now |= slider_resp.hovered() || slider_resp.dragged();

        if self.duration_ms > 0 {
            if slider_resp.dragged()
                && let Some(pos) = slider_resp.interact_pointer_pos()
            {
                let frac = ((pos.x - slider_rect.min.x) / slider_rect.width()).clamp(0.0, 1.0);
                let target_ms = (f64::from(frac) * self.duration_ms as f64).round() as i64;
                self.seek_position_ms = Some(target_ms);
                actions.push(PlaybackControlsAction::SeekProgress(target_ms));
            } else if slider_resp.drag_stopped() {
                if let Some(target_ms) = self.seek_position_ms.take() {
                    actions.push(PlaybackControlsAction::SeekFinished(target_ms));
                }
            } else if slider_resp.clicked()
                && let Some(pos) = slider_resp.interact_pointer_pos()
            {
                let frac = ((pos.x - slider_rect.min.x) / slider_rect.width()).clamp(0.0, 1.0);
                let target_ms = (f64::from(frac) * self.duration_ms as f64).round() as i64;
                self.seek_position_ms = None;
                actions.push(PlaybackControlsAction::SeekFinished(target_ms));
            }
        }

        let chapter_positions: Vec<f32> = self
            .timestamps
            .iter()
            .filter(|t| t.position > 0.0 && t.position < 1.0)
            .map(|t| t.position as f32)
            .collect();
        paint_slider(
            painter,
            slider_rect,
            self.displayed_progress() as f32,
            self.buffered_fraction as f32,
            slider_resp.hovered() || slider_resp.dragged(),
            alpha,
            &chapter_positions,
            &colors,
        );

        // Optional chapter label below slider (`st::mediaviewTimestampLabel`).
        if let Some(label) = self.current_chapter_label() {
            painter.text(
                Pos2::new(slider_rect.center().x, rect.max.y - 10.0),
                Align2::CENTER_CENTER,
                label,
                font_id_from_style(st::mediaview_timestamp_label.font),
                with_opacity(playback_progress_fg, alpha),
            );
        }

        // Speed selection popup above the speed button.
        if self.speed_menu_open {
            let menu_w = 88.0;
            let row_h = 26.0;
            let menu_h = SPEED_PRESETS.len() as f32 * row_h + 10.0;
            let menu_rect = Rect::from_min_size(
                Pos2::new(speed_rect.max.x - menu_w, rect.min.y - menu_h - 6.0),
                Vec2::new(menu_w, menu_h),
            );
            painter.rect_filled(menu_rect, CornerRadius::same(6), menu_bg);
            let menu_resp = ui.interact(menu_rect, id_salt.with("speed_menu"), Sense::hover());
            active_now |= menu_resp.hovered();

            for (idx, &preset) in SPEED_PRESETS.iter().enumerate() {
                let row_rect = Rect::from_min_size(
                    Pos2::new(
                        menu_rect.min.x + 4.0,
                        menu_rect.min.y + 5.0 + idx as f32 * row_h,
                    ),
                    Vec2::new(menu_w - 8.0, row_h),
                );
                let row_resp =
                    ui.interact(row_rect, id_salt.with(("speed_opt", idx)), Sense::click());
                if row_resp.hovered() {
                    painter.rect_filled(row_rect, CornerRadius::same(4), menu_bg_over);
                }
                let selected = (self.speed - preset).abs() < 0.05;
                let text_col = if selected { text_link_fg } else { menu_fg };
                let label = if (preset - preset.round()).abs() < 0.01 {
                    format!("{}x", preset.round() as i32)
                } else {
                    format!("{preset}x")
                };
                painter.text(
                    Pos2::new(row_rect.min.x + 12.0, row_rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    font_id_from_style(st::mediaview_font),
                    text_col,
                );
                if selected {
                    painter.circle_filled(
                        Pos2::new(row_rect.max.x - 10.0, row_rect.center().y),
                        3.0,
                        text_link_fg,
                    );
                }
                if row_resp.clicked() {
                    self.set_speed(preset);
                    self.speed_menu_open = false;
                    actions.push(PlaybackControlsAction::SpeedChanged(self.speed));
                }
            }
        }

        self.pointer_active = active_now;
        actions
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_slider(
    painter: &egui::Painter,
    rect: Rect,
    value: f32,
    buffered: f32,
    hovered: bool,
    alpha: f32,
    chapter_gaps: &[f32],
    colors: &MediaColors<'_>,
) {
    let inactive = colors.color_or("mediaviewPlaybackInactive", MEDIAVIEW_PLAYBACK_INACTIVE);
    let buffered_col =
        colors.color_or("mediaviewPlaybackInactiveOver", MEDIAVIEW_PLAYBACK_BUFFERED);
    let active = colors.color_or("mediaviewPlaybackActive", MEDIAVIEW_PLAYBACK_ACTIVE);
    let active_over = colors.color_or(
        "mediaviewPlaybackActiveOver",
        MEDIAVIEW_PLAYBACK_ACTIVE_OVER,
    );
    let gap_bg = colors.color_or("mediaviewSaveMsgBg", MEDIAVIEW_SAVE_MSG_BG);

    let track_h = if hovered { 4.0 } else { 3.0 };
    let cy = rect.center().y;
    let track = Rect::from_min_max(
        Pos2::new(rect.min.x, cy - track_h * 0.5),
        Pos2::new(rect.max.x, cy + track_h * 0.5),
    );
    painter.rect_filled(track, CornerRadius::same(2), with_opacity(inactive, alpha));

    let buf_clamped = buffered.clamp(0.0, 1.0);
    if buf_clamped > 0.0 {
        let buf_rect = Rect::from_min_max(
            track.min,
            Pos2::new(track.min.x + track.width() * buf_clamped, track.max.y),
        );
        painter.rect_filled(
            buf_rect,
            CornerRadius::same(2),
            with_opacity(buffered_col, alpha),
        );
    }

    let val_clamped = value.clamp(0.0, 1.0);
    let knob_x = track.min.x + track.width() * val_clamped;
    if val_clamped > 0.0 {
        let active_rect = Rect::from_min_max(track.min, Pos2::new(knob_x, track.max.y));
        painter.rect_filled(
            active_rect,
            CornerRadius::same(2),
            with_opacity(if hovered { active_over } else { active }, alpha),
        );
    }

    // Chapter timestamp gaps (`mediaviewPlaybackTimestamp`).
    for &pos in chapter_gaps {
        let gx = track.min.x + track.width() * pos.clamp(0.0, 1.0);
        let gap_rect = Rect::from_center_size(Pos2::new(gx, cy), Vec2::new(2.0, track_h + 2.0));
        painter.rect_filled(gap_rect, CornerRadius::ZERO, with_opacity(gap_bg, alpha));
    }

    // Seek handle knob (`seekSize: 12x12`).
    let knob_r = if hovered { 6.0 } else { 4.5 };
    painter.circle_filled(
        Pos2::new(knob_x, cy),
        knob_r,
        with_opacity(if hovered { active_over } else { active }, alpha),
    );
}
