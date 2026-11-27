//! Media viewer overlay, video playback controls, Picture-in-Picture, top audio player bar,
//! round-video floating mini-player, and inline GIF/round-video playback helpers.
//!
//! Port origin:
//! - `Telegram/SourceFiles/media/view/media_view_overlay_widget.cpp`
//! - `Telegram/SourceFiles/media/view/media_view_playback_controls.cpp`
//! - `Telegram/SourceFiles/media/view/media_view_pip.cpp`
//! - `Telegram/SourceFiles/media/player/media_player_widget.cpp`
//! - `Telegram/SourceFiles/media/player/media_player_float.cpp`

pub use tg_app_core::media::backend;
pub mod controls;
pub mod group_thumbs;
pub mod inline;
pub use tg_app_core::core_types::listen_tracker;
pub mod mock;
pub mod ocr_selection;
pub mod overlay;
pub mod pip;
pub mod player_bar;
pub mod round_float;
pub mod round_video_capture;
pub mod strings;
pub mod style;
#[cfg(test)]
mod tests_inline_pacing;
#[cfg(test)]
mod tests_seca;
#[cfg(test)]
mod tests_w8_copy;
#[cfg(test)]
mod tests_wp14;
pub mod video_playback;
pub mod video_sound;
pub mod video_stream;
pub mod video_volume;
pub mod zoom;

pub use backend::{AudioTrackMetadata, MediaBackend, MediaItem, MediaItemId, MediaItemKind};
pub use controls::{
    PlaybackControlsAction, PlaybackControlsState, SPEED_PRESETS, TIMESTAMP_EPS, TimestampChapter,
};
pub use inline::{
    DEFAULT_MAX_ACTIVE_INLINE_CLIPS, InlineDocumentData, InlineFrameState, InlineMediaSource,
    InlinePlayback, InlinePlaybackKind,
};
pub use mock::{
    MOCK_CHAT_PEER_ID, MOCK_GIF_ID, MOCK_PHOTO_AURORA_ID, MOCK_PHOTO_LANDSCAPE_ID,
    MOCK_PHOTO_PORTRAIT_ID, MOCK_ROUND_VIDEO_ID, MOCK_VIDEO_ID, MockActionLog, MockMediaBackend,
    MockVideoFile, build_caption_with_matches, generate_test_mp4,
};
pub use ocr_selection::{OcrOverlayState, OcrPosition, OcrSelection, OcrSpan};
pub use overlay::{
    GalleryNavigator, MediaOverlayWidget, OverlayEvent, SEEK_STEP_LONG_MS, SEEK_STEP_SHORT_MS,
};
pub use pip::{
    PIP_BORDER_SKIP, PIP_BORDER_SNAP_AREA, PIP_DEFAULT_SIZE, PIP_MINIMAL_SIZE, PipAttachments,
    PipEdge, PipEvent, PipRect, PipResumeState, PipWindow, clamp_to_edges, constrained_rect,
    default_pip_rect, max_allowed_size_for_screen, transformed_rect,
};
pub use player_bar::{AudioPlayerBar, AudioPlayerBarEvent};
pub use round_float::{
    FLOAT_WIDGET_OUTER_SIZE, RoundFloatEvent, RoundVideoFloat, single_section_layout,
    single_section_layout_with_top_inset,
};
pub use style::{VolumeIconLevel, format_duration_secs, format_remaining_secs};
pub use video_sound::VideoSoundOutput;
pub use video_stream::{
    MAX_TOP_PAID_DONORS_SHOWN, PlaybackSponsoredEvent, PlaybackSponsoredOverlay,
    SPONSORED_DURATION_MIN_MS, SPONSORED_START_DELAY_MIN_MS, SponsoredForVideo,
    SponsoredForVideoState, ThemeFilePreview, ThemePreviewAction, VideoStreamAction,
    VideoStreamComment, VideoStreamDescriptor, VideoStreamDonor, VideoStreamQuality,
    VideoStreamStatus, VideoStreamViewer, is_theme_file, top_donor_places, top_video_stream_donors,
};
pub use video_volume::VideoVolumeSync;
pub use zoom::{
    MAX_ZOOM_LEVEL, PINCH_ZOOM_STEP, ZOOM_TO_SCREEN_LEVEL, ZoomState, count_zoom_for,
    flip_size_by_rotation, normalize_rotation, zoom_value_to_scale,
};

/// Runs an automated `eframe` session (must be called on the main thread) that renders and saves
/// Day and Night screenshots (`media_photo.png`, `media_video.png`, `media_player_float.png`,
/// `media_photo_night.png`, `media_video_night.png`, and `media_player_float_night.png`) into
/// `docs_dir`.
pub fn run_screenshot_capture(docs_dir: &std::path::Path) -> anyhow::Result<()> {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tg_audio::{FullMsgId, MsgId, PeerId};
    use tg_text::EntityType;
    use tg_ui::fonts::ensure_tdesktop_fonts;
    use tg_ui::tg_theme::EmbeddedType;
    use tg_ui::theme::ThemeManager;
    use tg_video::{
        FfmpegStreamDecoder, OutputPixelFormat, PlaybackMode, PlaybackOptions, SliceConfig,
        StreamingPlayer,
    };

    use tg_app_core::stand_in::theme::{Palette, ThemeMode};

    std::fs::create_dir_all(docs_dir)?;
    let tmp_dir = std::env::temp_dir().join("rustgram_media_screenshots");
    let mp4_path = tmp_dir.join("viewer_demo.mp4");
    generate_test_mp4(&mp4_path, 960, 540, 4, true)?;
    let round_mp4_path = tmp_dir.join("round_demo.mp4");
    generate_test_mp4(&round_mp4_path, 256, 256, 3, false)?;

    let backend = Arc::new(MockMediaBackend::new());
    let video_caption = build_caption_with_matches(
        "Telemetry Test Clip (960x540 stream) — 0:00 Intro · 0:01 Color Sweep #streaming",
        &[
            ("Telemetry Test Clip", EntityType::Bold),
            ("#streaming", EntityType::Hashtag),
        ],
    );
    backend.register_video_file(MockVideoFile {
        id: MOCK_VIDEO_ID,
        kind: MediaItemKind::Video,
        path: mp4_path.clone(),
        width: 960,
        height: 540,
        duration_ms: 4_000,
        sender_name: "Broadcast Studio".to_owned(),
        caption: video_caption,
    })?;

    let round_bytes = std::fs::read(&round_mp4_path)?;
    let outputs = [
        docs_dir.join("media_photo.png"),
        docs_dir.join("media_video.png"),
        docs_dir.join("media_player_float.png"),
        docs_dir.join("media_photo_night.png"),
        docs_dir.join("media_video_night.png"),
        docs_dir.join("media_player_float_night.png"),
    ];

    struct CaptureApp {
        backend: Arc<MockMediaBackend>,
        overlay: MediaOverlayWidget,
        player_bar: AudioPlayerBar,
        pip: Option<PipWindow>,
        round_float: RoundVideoFloat,
        stage: usize,
        stage_frames: u32,
        stage_requested: bool,
        completed_stages: Arc<AtomicUsize>,
        outputs: [PathBuf; 6],
        round_bytes: Vec<u8>,
    }

    impl CaptureApp {
        fn configure_stage(&mut self, stage: usize) {
            self.stage = stage;
            self.stage_frames = 0;
            self.stage_requested = false;
            match stage % 3 {
                0 => {
                    self.pip = None;
                    self.round_float.detach();
                    let _ = self.overlay.open_item(
                        self.backend.as_ref(),
                        MOCK_CHAT_PEER_ID,
                        MOCK_PHOTO_LANDSCAPE_ID,
                        0,
                    );
                }
                1 => {
                    self.pip = None;
                    self.round_float.detach();
                    let _ = self.overlay.open_item(
                        self.backend.as_ref(),
                        MOCK_CHAT_PEER_ID,
                        MOCK_VIDEO_ID,
                        0,
                    );
                    self.overlay.set_video_timestamps(vec![
                        TimestampChapter {
                            position: 0.0,
                            label: "0:00 Intro".to_owned(),
                        },
                        TimestampChapter {
                            position: 0.35,
                            label: "0:01 Color Sweep".to_owned(),
                        },
                        TimestampChapter {
                            position: 0.72,
                            label: "0:03 Resolution Check".to_owned(),
                        },
                    ]);
                    self.overlay.seek_video(1_650, 1_750);
                    self.overlay.step_video(1_750);
                    self.overlay.controls_mut().set_volume(0.72);
                    self.overlay.controls_mut().set_speed(1.25);
                    self.overlay
                        .controls_mut()
                        .update_playback(true, 1_650, 4_000, 0.88);
                }
                2 => {
                    self.overlay.close();
                    if let (Some(item), Ok(loader)) = (
                        self.backend.get_item(MOCK_VIDEO_ID),
                        self.backend.open_range_loader(MOCK_VIDEO_ID),
                    ) {
                        let opts = PlaybackOptions {
                            mode: PlaybackMode::Both,
                            position_ms: 1_200,
                            output_format: OutputPixelFormat::Rgba,
                            ..PlaybackOptions::default()
                        };
                        if let Ok(dec) = FfmpegStreamDecoder::from_range_loader(
                            loader,
                            None,
                            SliceConfig::default(),
                            opts.clone(),
                        )
                        .and_then(video_playback::viewer_decoder)
                            && let Ok(mut player) = StreamingPlayer::new(dec, opts)
                        {
                            player.start(1_200);
                            let _ = player.step_video(1_300);
                            player.pause(1_300);
                            self.pip = Some(PipWindow::new(
                                item,
                                player,
                                PipRect::new(0, 0, 1200, 760),
                                0.8,
                            ));
                        }
                    }
                    self.round_float.set_layout(
                        1200,
                        760,
                        single_section_layout_with_top_inset(
                            1200,
                            760,
                            style::MEDIA_PLAYER_HEIGHT as i32 + 6,
                        ),
                    );
                    let _ = self.round_float.open_round_video_bytes(
                        FullMsgId::new(PeerId(MOCK_CHAT_PEER_ID), MsgId(3001)),
                        self.round_bytes.clone(),
                        false,
                    );
                }
                _ => {}
            }
        }
    }

    impl eframe::App for CaptureApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            let is_night = self.stage >= 3;
            let mode = if is_night {
                ThemeMode::Night
            } else {
                ThemeMode::Day
            };
            let theme = ThemeManager::with_embedded(if is_night {
                EmbeddedType::Night
            } else {
                EmbeddedType::Default
            });
            theme.apply_to_egui(&ctx);
            ensure_tdesktop_fonts(&ctx);
            let palette = Palette::for_mode(mode);
            let now_ms = match self.stage % 3 {
                1 => 1_750,
                2 => 1_300,
                _ => 100,
            };

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(palette.chat_canvas_bg))
                .show(ui, |ui| {
                    let vp = ctx.input(|i| i.viewport_rect());
                    if self.stage % 3 == 2 {
                        let _ = self
                            .player_bar
                            .show(ui, self.backend.as_ref(), &palette, now_ms);
                        let painter = ui.painter();
                        painter.text(
                            egui::Pos2::new(vp.center().x, vp.min.y + 84.0),
                            egui::Align2::CENTER_CENTER,
                            strings::CAPTURE_STAGE_TITLE,
                            egui::FontId::proportional(17.0),
                            palette.window_bold_fg,
                        );
                    }
                    let _ = self.overlay.show(&ctx, vp, self.backend.as_ref(), now_ms);
                    if let Some(pip) = &mut self.pip {
                        let _ = pip.show(&ctx, vp, now_ms);
                    }
                    let _ = self.round_float.show(&ctx, vp, now_ms);
                });

            let completed = self.completed_stages.load(Ordering::Acquire);
            if completed >= self.outputs.len() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if completed > self.stage {
                self.configure_stage(completed);
                ctx.request_repaint();
                return;
            }

            self.stage_frames += 1;
            if !self.stage_requested && self.stage_frames >= 8 {
                self.stage_requested = true;
                let out = self.outputs[self.stage].clone();
                let done = Arc::clone(&self.completed_stages);
                let cb_ctx = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::ScreenshotCallback::new(move |img| {
                        let [w, h] = img.size;
                        let mut raw = Vec::with_capacity(w * h * 4);
                        for px in &img.pixels {
                            let [r, g, b, a] = px.to_srgba_unmultiplied();
                            raw.extend_from_slice(&[r, g, b, a]);
                        }
                        if let Some(rgba) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                            let _ = rgba.save(&out);
                        }
                        done.fetch_add(1, Ordering::AcqRel);
                        cb_ctx.request_repaint();
                    }),
                ));
            } else {
                ctx.request_repaint();
            }
        }
    }

    #[cfg(all(test, target_os = "windows"))]
    let event_loop_builder: Option<eframe::EventLoopBuilderHook> = Some(Box::new(|builder| {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }));
    #[cfg(not(all(test, target_os = "windows")))]
    let event_loop_builder: Option<eframe::EventLoopBuilderHook> = None;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(tg_lang::tr::lng_mediaview_title())
            .with_inner_size([1200.0, 760.0])
            .with_decorations(false),
        event_loop_builder,
        ..Default::default()
    };

    let backend_clone = Arc::clone(&backend);
    let run_res = eframe::run_native(
        "Rustgram Media Capture",
        options,
        Box::new(move |cc| {
            ThemeManager::with_embedded(EmbeddedType::Default).apply_to_egui(&cc.egui_ctx);
            ensure_tdesktop_fonts(&cc.egui_ctx);
            let mut player_bar = AudioPlayerBar::new(backend_clone.as_ref(), false);
            let _ = player_bar.play_item(backend_clone.sample_song_ids()[0], None);
            player_bar.advance_software_ms(1_400);
            let mut app = CaptureApp {
                backend: backend_clone,
                overlay: MediaOverlayWidget::new(),
                player_bar,
                pip: None,
                round_float: RoundVideoFloat::new(1200, 760),
                stage: 0,
                stage_frames: 0,
                stage_requested: false,
                completed_stages: Arc::new(AtomicUsize::new(0)),
                outputs,
                round_bytes,
            };
            app.configure_stage(0);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe screenshot capture failed: {e}"));

    let _ = std::fs::remove_file(&mp4_path);
    let _ = std::fs::remove_file(&round_mp4_path);
    run_res
}

#[cfg(test)]
mod tests {
    use tg_audio::{AudioType, FullMsgId, MsgId, OrderMode, PeerId, RepeatMode};
    use tg_text::{EntityInText, EntityType, TextWithEntities};

    use super::*;
    use crate::media::overlay::build_caption_layout_job;

    // ── Zoom math tests ──────────────────────────────────────────────────────

    #[test]
    fn zoom_count_and_scale_factor_match_tdesktop_formulas() {
        // Larger image fitted down to half size -> ratio = 0.5 -> z = 1 - 1/0.5 = -1.0 -> scale = 0.5.
        let z_half = count_zoom_for(2000.0, 1000.0, 1000.0, 800.0);
        assert!((z_half - (-1.0)).abs() < 1e-6);
        assert!((zoom_value_to_scale(z_half) - 0.5).abs() < 1e-6);

        // Smaller image fitted up to 2x -> ratio = 2.0 -> z = 2.0 - 1.0 = +1.0 -> scale = 2.0.
        let z_double = count_zoom_for(400.0, 300.0, 800.0, 600.0);
        assert!((z_double - 1.0).abs() < 1e-6);
        assert!((zoom_value_to_scale(z_double) - 2.0).abs() < 1e-6);

        // Exact fit -> ratio = 1.0 -> z = 0.0 -> scale = 1.0.
        let z_exact = count_zoom_for(800.0, 600.0, 800.0, 600.0);
        assert!(z_exact.abs() < 1e-6);
        assert!((zoom_value_to_scale(z_exact) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zoom_in_out_reset_and_max_bounds() {
        let mut z = ZoomState::new(1600.0, 900.0, 800.0, 600.0);
        // Oversized photo opens in fit-to-screen mode (`ZOOM_TO_SCREEN_LEVEL`).
        assert_eq!(z.zoom, ZOOM_TO_SCREEN_LEVEL);
        assert!(z.w <= z.viewport_w + 1.0);
        assert!(z.h <= z.available_height + 1.0);

        // Reset toggles from fit-to-screen to 100% (`zoom == 0`).
        z.zoom_reset();
        assert_eq!(z.zoom, 0);
        assert_eq!(z.zoom_percent(), 100);
        assert!(z.can_drag());

        // Reset again returns to fit-to-screen.
        z.zoom_reset();
        assert_ne!(z.zoom, 0);

        // Step zoom_in up to MAX_ZOOM_LEVEL (7 -> 800%).
        for _ in 0..16 {
            z.zoom_in(None);
        }
        assert_eq!(z.zoom, MAX_ZOOM_LEVEL);
        assert_eq!(z.zoom_percent(), 800);

        // Step zoom_out down to -MAX_ZOOM_LEVEL (-7 -> 1/8 = 12.5% -> 13%).
        for _ in 0..24 {
            z.zoom_out(None);
        }
        assert_eq!(z.zoom, -MAX_ZOOM_LEVEL);
        assert!((z.scale_factor() - 0.125).abs() < 1e-6);
    }

    #[test]
    fn zoom_anchored_wheel_and_pan_snap_xy() {
        let mut z = ZoomState::new(1200.0, 800.0, 800.0, 600.0);
        z.set_zoom_level(0, true, None);
        assert_eq!(z.zoom, 0);

        // Zoom in anchored near top-left quarter.
        let anchor = (200.0, 150.0);
        z.zoom_in(Some(anchor));
        assert_eq!(z.zoom, 1);
        assert_eq!(z.w, 2400.0);
        assert_eq!(z.h, 1600.0);

        // Extreme drag is clamped inside viewport edges by snap_xy().
        z.drag_by(10_000.0, 10_000.0);
        assert!(z.x <= 0.0 + 1e-3);
        assert!(z.y <= z.min_used_top + 1e-3);

        z.drag_by(-10_000.0, -10_000.0);
        assert!(z.x >= z.viewport_w - z.w - 1e-3);
        assert!(z.y >= z.viewport_h - z.h - 1e-3);
    }

    #[test]
    fn zoom_rotation_transposes_dimensions() {
        let mut z = ZoomState::new(1200.0, 600.0, 800.0, 600.0);
        assert_eq!((z.width, z.height), (1200.0, 600.0));
        z.rotate_cw();
        assert_eq!(z.rotation, 90);
        assert_eq!((z.width, z.height), (600.0, 1200.0));
        z.rotate_cw();
        assert_eq!(z.rotation, 180);
        assert_eq!((z.width, z.height), (1200.0, 600.0));
        z.rotate_ccw();
        assert_eq!(z.rotation, 90);
        assert_eq!(normalize_rotation(-90), 270);
        assert_eq!(flip_size_by_rotation(100.0, 50.0, 270), (50.0, 100.0));
    }

    // ── Gallery navigation tests ─────────────────────────────────────────────

    #[test]
    fn gallery_navigator_bounds_counter_and_delete() {
        let backend = MockMediaBackend::new();
        let siblings = backend.siblings_in_chat(MOCK_CHAT_PEER_ID, MOCK_PHOTO_LANDSCAPE_ID);
        assert_eq!(siblings.len(), 3);

        let mut nav = GalleryNavigator::new(siblings, MOCK_PHOTO_PORTRAIT_ID);
        assert_eq!(nav.current_index(), 1);
        assert_eq!(
            nav.counter_text().as_deref(),
            Some(tg_lang::tr::lng_mediaview_n_of_amount("2", "3").as_str())
        );
        assert!(nav.can_prev());
        assert!(nav.can_next());

        assert!(nav.move_by(-1));
        assert_eq!(nav.current().map(|i| i.id), Some(MOCK_PHOTO_LANDSCAPE_ID));
        assert!(!nav.can_prev());
        assert!(!nav.move_by(-1));

        assert!(nav.move_by(1));
        assert!(nav.move_by(1));
        assert_eq!(nav.current().map(|i| i.id), Some(MOCK_PHOTO_AURORA_ID));
        assert!(!nav.can_next());
        assert!(!nav.move_by(1));

        // Deleting last item clamps index back to new last item.
        let removed = nav.remove_current();
        assert_eq!(removed.map(|i| i.id), Some(MOCK_PHOTO_AURORA_ID));
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.current_index(), 1);
        assert_eq!(
            nav.counter_text().as_deref(),
            Some(tg_lang::tr::lng_mediaview_n_of_amount("2", "2").as_str())
        );
    }

    // ── PlaybackControls state tests ─────────────────────────────────────────

    #[test]
    fn playback_controls_time_volume_speed_chapters_and_fade() {
        let mut ctrl = PlaybackControlsState::default();
        ctrl.update_playback(true, 65_000, 185_000, 0.75);
        assert!(ctrl.show_pause);
        assert_eq!(
            ctrl.time_texts(),
            ("1:05".to_owned(), "\u{2212}2:00".to_owned())
        );
        assert!((ctrl.displayed_progress() - (65.0 / 185.0)).abs() < 1e-6);

        // Live scrub overrides displayed time texts.
        ctrl.seek_position_ms = Some(120_000);
        assert_eq!(
            ctrl.time_texts(),
            ("2:00".to_owned(), "\u{2212}1:05".to_owned())
        );
        ctrl.seek_position_ms = None;

        // Volume toggle remembers last positive volume & icon thresholds.
        ctrl.set_volume(0.35);
        assert_eq!(ctrl.volume_icon_level(), VolumeIconLevel::Small);
        assert_eq!(ctrl.toggle_volume(), 0.0);
        assert_eq!(ctrl.volume_icon_level(), VolumeIconLevel::Off);
        assert!((ctrl.toggle_volume() - 0.35).abs() < 1e-6);
        ctrl.set_volume(0.8);
        assert_eq!(ctrl.volume_icon_level(), VolumeIconLevel::Full);

        // Speed clamping & formatting.
        ctrl.set_speed(5.0);
        assert_eq!(ctrl.speed, tg_video::SPEED_MAX);
        assert_eq!(ctrl.speed_label(), "2.5x");
        ctrl.set_speed(1.0);
        assert_eq!(ctrl.speed_label(), "1x");

        // Chapter navigation (`next_timestamp` / `prev_timestamp`).
        ctrl.timestamps = vec![
            TimestampChapter {
                position: 0.0,
                label: "Intro".to_owned(),
            },
            TimestampChapter {
                position: 0.35,
                label: "Trailhead".to_owned(),
            },
            TimestampChapter {
                position: 0.75,
                label: "Summit".to_owned(),
            },
        ];
        assert_eq!(
            ctrl.next_timestamp(0.1).map(|c| c.label.as_str()),
            Some("Trailhead")
        );
        assert_eq!(
            ctrl.prev_timestamp(0.5).map(|c| c.label.as_str()),
            Some("Trailhead")
        );
        assert_eq!(ctrl.current_chapter_label(), Some("Trailhead"));

        // Download progress badge.
        ctrl.set_loading_progress(450, 1000);
        assert_eq!(ctrl.download_percent_text().as_deref(), Some("45%"));
        ctrl.set_loading_progress(1000, 1000);
        assert_eq!(ctrl.download_percent_text(), None);

        // Auto-hide fade when playing and idle past WAIT_HIDE_MS + HIDE_DURATION_MS.
        ctrl.note_activity(1_000);
        ctrl.step_fade(1_500, false);
        assert!((ctrl.opacity - 1.0).abs() < 1e-6);
        ctrl.step_fade(3_000, false);
        assert!(ctrl.opacity < 0.01);
        ctrl.step_fade(3_100, true);
        assert!(ctrl.opacity > 0.0);
    }

    // ── PiP geometry math tests ──────────────────────────────────────────────

    #[test]
    fn pip_edge_snap_and_aspect_ratio_constrained_resize() {
        let screen = PipRect::new(0, 0, 1920, 1080);
        let def = default_pip_rect(screen, 16, 9);
        assert_eq!(def.w, PIP_DEFAULT_SIZE);
        assert_eq!(def.h, 180);

        // Within PIP_BORDER_SNAP_AREA of right/bottom margin -> snaps cleanly to margin.
        let near_right = PipRect::new(1920 - PIP_BORDER_SKIP - def.w - 8, def.y, def.w, def.h);
        let (sx, sy) = clamp_to_edges(screen, near_right);
        assert_eq!(sx, 1920 - PIP_BORDER_SKIP - def.w);
        assert_eq!(sy, def.y);

        // Corner resize preserves 16:9 aspect ratio within [min, max].
        let min_sz = (PIP_MINIMAL_SIZE, PIP_MINIMAL_SIZE);
        let max_sz = max_allowed_size_for_screen(screen.w, screen.h);
        let raw = transformed_rect(def, min_sz, max_sz, 160, 40, PipEdge::BottomRight);
        let fixed = constrained_rect(
            raw,
            min_sz,
            max_sz,
            (16, 9),
            PipEdge::BottomRight,
            PipAttachments::default(),
        );
        let ratio = fixed.w as f64 / fixed.h as f64;
        assert!((ratio - (16.0 / 9.0)).abs() < 0.05);
    }

    // ── MockBackend, AudioPlayerBar, Caption entities, & FFmpeg integration ──

    #[test]
    fn mock_backend_save_copy_forward_delete_and_audio_player_bar() {
        let backend = MockMediaBackend::new();
        let tmp_dir = std::env::temp_dir().join("rustgram_media_unit_test");
        let saved = backend
            .save_to_disk(MOCK_PHOTO_LANDSCAPE_ID, &tmp_dir.join("saved.png"))
            .expect("save photo");
        assert!(saved.exists());
        let _ = std::fs::remove_file(&saved);

        backend.copy_item(MOCK_PHOTO_LANDSCAPE_ID).expect("copy");
        backend
            .forward_item(MOCK_PHOTO_LANDSCAPE_ID)
            .expect("forward");
        backend.show_in_chat(MOCK_CHAT_PEER_ID, 1001);
        assert!(backend.action_log().len() >= 4);

        // Caption entities layout job preserves text and builds spans.
        let caption = TextWithEntities {
            text: "Hello world #tag".to_owned(),
            entities: vec![
                EntityInText::new(EntityType::Bold, 0, 5),
                EntityInText::new(EntityType::Hashtag, 12, 4),
            ],
        };
        let job = build_caption_layout_job(&caption, 400.0, &std::collections::HashSet::new());
        assert_eq!(job.text, "Hello world #tag");
        assert!(job.sections.len() >= 3);

        // AudioPlayerBar plays synthesized WAV via tg-audio PlayerInstance & MixerCore.
        let mut bar = AudioPlayerBar::new(&backend, false);
        let song_id = backend.sample_song_ids()[0];
        bar.play_item(song_id, None).expect("play song");
        assert!(bar.is_visible());
        assert_eq!(bar.active_type(), AudioType::Song);
        bar.advance_software_ms(120);
        assert!(bar.current_state().position_ms() > 0);
        assert_eq!(bar.cycle_repeat_mode(), RepeatMode::One);
        assert_eq!(bar.cycle_order_mode(), OrderMode::Reverse);
        assert_eq!(bar.toggle_volume(), 0.0);
        assert!((bar.toggle_volume() - 1.0).abs() < 1e-6);
        assert!(bar.stop_and_close());
        assert!(!bar.is_visible());
    }

    #[test]
    fn ffmpeg_video_streaming_inline_and_round_float_pipeline() {
        let tmp = std::env::temp_dir().join("rustgram_media_ffmpeg_test");
        let mp4_path = tmp.join("sample_test.mp4");
        generate_test_mp4(&mp4_path, 320, 240, 2, true).expect("generate test mp4");

        let backend = MockMediaBackend::new();
        backend
            .register_video_file(MockVideoFile {
                id: MOCK_VIDEO_ID,
                kind: MediaItemKind::Video,
                path: mp4_path.clone(),
                width: 320,
                height: 240,
                duration_ms: 2_000,
                sender_name: "Studio Feed".to_owned(),
                caption: TextWithEntities::simple("Generated test video"),
            })
            .expect("register video");

        // MediaOverlayWidget opens video with tg-video StreamingPlayer and seeks cleanly.
        let mut overlay = MediaOverlayWidget::new();
        overlay
            .open_item(&backend, MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, 0)
            .expect("open video in overlay");
        assert!(overlay.is_open());
        overlay.step_video(500);
        assert!(overlay.controls().duration_ms > 0);
        overlay.seek_video(1_000, 1_000);
        assert!(overlay.controls().position_ms >= 900);

        // RoundVideoFloat opens circular clip and uses tg-audio FloatController.
        let mp4_bytes = std::fs::read(&mp4_path).expect("read mp4");
        let mut float_player = RoundVideoFloat::new(1280, 720);
        float_player.set_layout(1280, 720, single_section_layout(1280, 720));
        let msg = FullMsgId::new(PeerId(MOCK_CHAT_PEER_ID), MsgId(3001));
        float_player
            .open_round_video_bytes(msg, mp4_bytes.clone(), false)
            .expect("open round video");
        assert!(float_player.is_visible());
        float_player.set_history_item_visible(true);
        assert!(!float_player.is_visible());
        float_player.set_history_item_visible(false);
        assert!(float_player.is_visible());

        // InlinePlayback decodes GIF/round video frame into an egui TextureHandle.
        let ctx = egui::Context::default();
        let mut inline = InlinePlayback::new();
        let source = InlineMediaSource {
            id: MOCK_ROUND_VIDEO_ID,
            kind: InlinePlaybackKind::RoundVideo,
            data: InlineDocumentData::Bytes(mp4_bytes),
            target_size: Some((128, 128)),
        };
        let state = inline.frame_for(&ctx, &source, 100);
        assert!(state.error.is_none(), "inline error: {:?}", state.error);
        assert!(state.texture.is_some());
        assert_eq!(state.size, egui::Vec2::new(128.0, 128.0));
        assert!(state.is_playing);

        let _ = std::fs::remove_file(&mp4_path);
    }

    #[test]
    #[ignore = "Requires desktop display; run with --ignored --test-threads=1"]
    fn capture_media_screenshots() {
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        run_screenshot_capture(&docs_dir).expect("capture media screenshots");
    }
}
