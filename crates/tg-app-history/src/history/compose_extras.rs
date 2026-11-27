//! Compose extras and inline overlays: voice recording (hold/lock + live waveform via `tg-audio`),
//! round video recording, spellcheck underline + suggestions (`tg-platform` `ISpellChecker`),
//! inline bot query results panel hook, link preview editor, send-as peer chooser, slow mode timer,
//! live message effects picker (`messages.getAvailableEffects` + `tg-lottie` playback), and full
//! emoji + `tg-lottie` animated reactions picker.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/controls/history_view_voice_record_bar.cpp`
//! - `Telegram/SourceFiles/history/view/controls/history_view_webpage_processor.cpp`
//! - `Telegram/SourceFiles/inline_bots/inline_results_widget.cpp`
//! - `Telegram/SourceFiles/chat_helpers/spellchecker_common.cpp`
//! - `Telegram/SourceFiles/history/view/reactions/history_view_reactions_selector.cpp`
//! - `Telegram/SourceFiles/menu/menu_send.cpp`

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui::{
    Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect,
    ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, TextureHandle, TextureOptions, Ui,
    Vec2, WidgetInfo,
};
use tg_audio::capture::{AudioRecorder, CaptureConfig, CaptureUpdate, VoiceCapturePipeline};
use tg_audio::device::DeviceManager;
use tg_audio::waveform::encode_5bit;
use tg_chat::emoji::DEFAULT_RECENT_EMOJI;
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_lottie::{Composition, Size as LottieSize, render_composition};
use tg_platform::spellcheck::{DEFAULT_MAX_SUGGESTIONS, MisspelledRange, SpellChecker};
use tg_text::emoji::{self, Section as EmojiSection};
use tg_text::{EntityType, TextParseFlags, parse_entities};
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;

use crate::history::backend::{
    AvailableMessageEffect, AvailableReactionItem, HistoryBackend, HistoryMessage, VoiceAttachment,
};
use crate::history::sections_model::{
    InlineBotResult, InlineBotResultKind, LinkPreviewData, MESSAGE_EFFECTS, RoundVideoAttachment,
    SendAsPeer, SlowModeState, animated_effect_lottie_json, animated_reaction_lottie_json,
    effect_name_and_accent, parse_inline_bot_query,
};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_core::ui_icons::{
    ArrowDir, paint_lock, paint_select_check, paint_star, paint_triangle, selectable_icon_chip,
};

// ---------------------------------------------------------------------------
// 1. Voice Recording UI (hold / slide-to-cancel / lock + live waveform via `tg-audio`)
// ---------------------------------------------------------------------------

/// Slide-left distance in logical pixels required to cancel a held recording.
pub const VOICE_SLIDE_CANCEL_THRESHOLD_PX: f32 = 96.0;
/// Slide-up distance in logical pixels required to lock hands-free recording.
pub const VOICE_SLIDE_LOCK_THRESHOLD_PX: f32 = 56.0;

/// Toggle between Voice Message and Round Video recording on the right compose button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordButtonMode {
    /// Voice note (microphone icon).
    #[default]
    Voice,
    /// Round video note placeholder (video camera icon).
    RoundVideo,
}

/// Active phase of [`VoiceRecorderController`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VoiceRecordPhase {
    /// Not recording.
    Idle,
    /// User is holding the mic button; sliding left cancels, sliding up locks.
    Holding {
        /// Start timestamp in seconds.
        started_at: f64,
        /// Drag offset X (negative = dragging left toward Cancel).
        drag_dx: f32,
        /// Drag offset Y (negative = dragging up toward Lock).
        drag_dy: f32,
    },
    /// Recording is locked hands-free (with Pause/Resume, Discard, Send controls).
    Locked {
        /// Effective start timestamp (adjusted for pauses).
        started_at: f64,
        /// True when paused.
        paused: bool,
        /// Elapsed seconds frozen while paused.
        paused_elapsed_secs: f64,
    },
}

/// Live amplitude bar feed populated by `tg_audio::capture::AudioRecorder` callback.
#[derive(Debug, Default)]
struct LiveLevelFeed {
    bars: Vec<u8>,
    last_sample_count: i64,
    hardware_active: bool,
}

/// A finished voice recording: the bubble data plus the encoded file to upload
/// (tdesktop's `VoiceToSend { bytes, waveform, duration }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedVoice {
    /// Duration, 5-bit waveform and size for `documentAttributeAudio` and the pending bubble.
    pub attachment: VoiceAttachment,
    /// Telegram-compatible OGG/Opus file bytes (`audio/ogg`).
    pub ogg: Vec<u8>,
}

/// Longest synthetic PCM span fed in one fallback step, in seconds: a stalled frame must not
/// turn into a long silent gap or a huge allocation.
const FALLBACK_MAX_STEP_SECS: f64 = 1.0;

/// Whether voice recording may open the real microphone (`cpal`). Never in unit tests, and
/// never when `RUSTGRAM_HEADLESS_AUDIO` / `TGFORK_HEADLESS_AUDIO` is set (agent and CI runs):
/// then the synthetic capture source below records instead.
#[must_use]
pub fn hardware_voice_capture_enabled() -> bool {
    crate::history::inline_media::default_hardware_audio_enabled()
}

/// The microphone could not start a recording: no capture device, or opening it failed.
///
/// tdesktop's `VoiceRecordBar::startRecording` stops when `Media::Capture::instance()->available()`
/// is false or the capture reports an error; nothing is recorded or sent. The compose bar shows
/// `lng_record_audio_problem` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceCaptureUnavailable;

impl fmt::Display for VoiceCaptureUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no usable microphone")
    }
}

impl std::error::Error for VoiceCaptureUnavailable {}

/// Opens the microphone and streams its levels into the feed, or `None` when it cannot start.
type MicrophoneOpener = fn(&Arc<Mutex<LiveLevelFeed>>) -> Option<AudioRecorder>;

/// Where a recording takes its sound from, fixed when the controller is created.
#[derive(Clone, Copy)]
enum CaptureSource {
    /// The real microphone; a recording does not start without one.
    Microphone(MicrophoneOpener),
    /// Generated speech-like PCM: headless agent/CI runs and unit tests only.
    Synthetic,
}

/// Voice recorder controller wrapping `tg-audio`'s [`AudioRecorder`] and [`VoiceCapturePipeline`].
///
/// Records from the microphone via `cpal` when hardware capture is allowed
/// ([`hardware_voice_capture_enabled`]); without a usable microphone a recording does not
/// start ([`VoiceCaptureUnavailable`]). Headless runs and unit tests feed synthetic PCM into
/// `VoiceCapturePipeline` instead, so the real Opus + 5-bit waveform pipeline still produces
/// a sendable [`RecordedVoice`] there.
pub struct VoiceRecorderController {
    phase: VoiceRecordPhase,
    source: CaptureSource,
    recorder: Option<AudioRecorder>,
    fallback_pipeline: Option<VoiceCapturePipeline>,
    feed: Arc<Mutex<LiveLevelFeed>>,
    last_fallback_tick_secs: f64,
}

impl fmt::Debug for VoiceRecorderController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VoiceRecorderController")
            .field("phase", &self.phase)
            .finish()
    }
}

impl Default for VoiceRecorderController {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceRecorderController {
    /// Creates an idle voice recorder controller: microphone capture in a normal run, the
    /// synthetic source in headless runs and unit tests.
    #[must_use]
    pub fn new() -> Self {
        let source = if hardware_voice_capture_enabled() {
            CaptureSource::Microphone(open_microphone)
        } else {
            CaptureSource::Synthetic
        };
        Self::with_source(source)
    }

    /// A controller whose microphone never opens, like a machine without a capture device.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn without_microphone() -> Self {
        Self::with_source(CaptureSource::Microphone(|_| None))
    }

    fn with_source(source: CaptureSource) -> Self {
        Self {
            phase: VoiceRecordPhase::Idle,
            source,
            recorder: None,
            fallback_pipeline: None,
            feed: Arc::new(Mutex::new(LiveLevelFeed::default())),
            last_fallback_tick_secs: 0.0,
        }
    }

    /// Returns current recording phase.
    #[must_use]
    pub const fn phase(&self) -> VoiceRecordPhase {
        self.phase
    }

    /// Returns `true` while holding or locked recording is active.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        !matches!(self.phase, VoiceRecordPhase::Idle)
    }

    /// Returns `true` when recording is locked in hands-free mode.
    #[must_use]
    pub const fn is_locked(&self) -> bool {
        matches!(self.phase, VoiceRecordPhase::Locked { .. })
    }

    /// Returns `true` when locked and paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        matches!(self.phase, VoiceRecordPhase::Locked { paused: true, .. })
    }

    /// Returns elapsed recording duration in seconds at `now_secs`.
    #[must_use]
    pub fn elapsed_secs(&self, now_secs: f64) -> f64 {
        match self.phase {
            VoiceRecordPhase::Idle => 0.0,
            VoiceRecordPhase::Holding { started_at, .. } => (now_secs - started_at).max(0.0),
            VoiceRecordPhase::Locked {
                started_at,
                paused,
                paused_elapsed_secs,
            } => {
                if paused {
                    paused_elapsed_secs
                } else {
                    (now_secs - started_at).max(0.0)
                }
            }
        }
    }

    /// Starts hold-to-record at `now_secs`, initializing `tg-audio` capture.
    ///
    /// # Errors
    /// [`VoiceCaptureUnavailable`] when the microphone cannot start; the controller stays idle.
    pub fn start_hold(&mut self, now_secs: f64) -> Result<(), VoiceCaptureUnavailable> {
        self.begin_capture(now_secs)?;
        self.phase = VoiceRecordPhase::Holding {
            started_at: now_secs,
            drag_dx: 0.0,
            drag_dy: 0.0,
        };
        Ok(())
    }

    /// Starts directly in locked mode (e.g. click-to-record convenience).
    ///
    /// # Errors
    /// [`VoiceCaptureUnavailable`] when the microphone cannot start; the controller stays idle.
    pub fn start_locked(&mut self, now_secs: f64) -> Result<(), VoiceCaptureUnavailable> {
        self.begin_capture(now_secs)?;
        self.phase = VoiceRecordPhase::Locked {
            started_at: now_secs,
            paused: false,
            paused_elapsed_secs: 0.0,
        };
        Ok(())
    }

    fn begin_capture(&mut self, now_secs: f64) -> Result<(), VoiceCaptureUnavailable> {
        self.cancel();
        if let Ok(mut feed) = self.feed.lock() {
            feed.bars.clear();
            feed.last_sample_count = 0;
            feed.hardware_active = false;
        }
        self.last_fallback_tick_secs = now_secs;

        match self.source {
            CaptureSource::Microphone(open) => {
                self.recorder = Some(open(&self.feed).ok_or(VoiceCaptureUnavailable)?);
            }
            CaptureSource::Synthetic => {
                // Headless source feeding the real `tg-audio` Opus + waveform pipeline.
                self.fallback_pipeline = Some(
                    VoiceCapturePipeline::telegram_default()
                        .map_err(|_| VoiceCaptureUnavailable)?,
                );
            }
        }
        Ok(())
    }

    /// Updates pointer drag delta during hold-to-record.
    /// Returns `Some(true)` if auto-cancelled by sliding left, `Some(false)` if auto-locked by sliding up.
    pub fn update_hold_drag(&mut self, delta: Vec2, now_secs: f64) -> Option<bool> {
        let VoiceRecordPhase::Holding {
            started_at,
            ref mut drag_dx,
            ref mut drag_dy,
        } = self.phase
        else {
            return None;
        };
        *drag_dx += delta.x;
        *drag_dy += delta.y;

        if -*drag_dx >= VOICE_SLIDE_CANCEL_THRESHOLD_PX {
            self.cancel();
            return Some(true);
        }
        if -*drag_dy >= VOICE_SLIDE_LOCK_THRESHOLD_PX {
            self.phase = VoiceRecordPhase::Locked {
                started_at,
                paused: false,
                paused_elapsed_secs: (now_secs - started_at).max(0.0),
            };
            return Some(false);
        }
        None
    }

    /// Transitions a held recording into locked hands-free mode.
    pub fn lock(&mut self, now_secs: f64) {
        if let VoiceRecordPhase::Holding { started_at, .. } = self.phase {
            self.phase = VoiceRecordPhase::Locked {
                started_at,
                paused: false,
                paused_elapsed_secs: (now_secs - started_at).max(0.0),
            };
        }
    }

    /// Toggles pause/resume while in locked mode.
    pub fn toggle_pause(&mut self, now_secs: f64) {
        if self.recorder.is_none() && self.is_locked() && !self.is_paused() {
            // Record the synthetic source up to the pause.
            self.feed_fallback_until(now_secs);
        }
        if let VoiceRecordPhase::Locked {
            ref mut started_at,
            ref mut paused,
            ref mut paused_elapsed_secs,
        } = self.phase
        {
            if *paused {
                *started_at = now_secs - *paused_elapsed_secs;
                *paused = false;
                // The synthetic source must not fill the paused span on resume.
                self.last_fallback_tick_secs = now_secs;
                if let Some(ref rec) = self.recorder {
                    let _ = rec.pause(false);
                }
                if let Some(ref mut pipe) = self.fallback_pipeline {
                    let _ = pipe.set_paused(false);
                }
            } else {
                *paused_elapsed_secs = (now_secs - *started_at).max(0.0);
                *paused = true;
                if let Some(ref rec) = self.recorder {
                    let _ = rec.pause(true);
                }
                if let Some(ref mut pipe) = self.fallback_pipeline {
                    let _ = pipe.set_paused(true);
                }
            }
        }
    }

    /// Advances fallback waveform metering when no hardware mic is streaming.
    pub fn tick(&mut self, now_secs: f64) {
        if !self.is_active() || self.is_paused() {
            return;
        }
        let hw_active = self.feed.lock().is_ok_and(|g| g.hardware_active);
        if hw_active {
            return;
        }
        if now_secs - self.last_fallback_tick_secs < 0.08 {
            return;
        }
        self.feed_fallback_until(now_secs);
    }

    /// Feeds the synthetic source with PCM for the wall time since the last step, so the
    /// recorded duration follows the clock even when frames are sparse (an idle window
    /// repaints only a few times a second).
    fn feed_fallback_until(&mut self, now_secs: f64) {
        let span = (now_secs - self.last_fallback_tick_secs).clamp(0.0, FALLBACK_MAX_STEP_SECS);
        self.last_fallback_tick_secs = now_secs;
        let sample_count = (span * 48_000.0).round() as usize;
        if sample_count == 0 {
            return;
        }

        // A synthetic speech-like tone burst into the real `VoiceCapturePipeline`.
        let t0 = now_secs * 6.0;
        let amp = ((t0.sin() * 0.5 + 0.5) * 0.65 + ((t0 * 2.7).cos() * 0.5 + 0.5) * 0.35) * 22000.0;
        let mut pcm = Vec::with_capacity(sample_count);
        for i in 0..sample_count {
            let s = (((i as f64) * 0.055 + t0).sin() * amp).round() as i16;
            pcm.push(s);
        }
        let bar = if let Some(ref mut pipe) = self.fallback_pipeline {
            if let Ok(Some(upd)) = pipe.push_mono_i16(&pcm) {
                ((u32::from(upd.level) * 31) / 32768).clamp(3, 31) as u8
            } else {
                ((amp / 32768.0) * 31.0).round().clamp(3.0, 31.0) as u8
            }
        } else {
            ((amp / 32768.0) * 31.0).round().clamp(3.0, 31.0) as u8
        };

        if let Ok(mut guard) = self.feed.lock() {
            guard.bars.push(bar);
            if guard.bars.len() > 64 {
                guard.bars.remove(0);
            }
        }
    }

    /// Returns a snapshot of live waveform bars (`0..=31`) for rendering while recording.
    #[must_use]
    pub fn live_bars(&self) -> Vec<u8> {
        self.feed
            .lock()
            .map_or_else(|_| Vec::new(), |g| g.bars.clone())
    }

    /// Cancels and discards the active recording.
    pub fn cancel(&mut self) {
        if let Some(rec) = self.recorder.take() {
            rec.cancel();
        }
        self.fallback_pipeline = None;
        self.phase = VoiceRecordPhase::Idle;
    }

    /// Stops recording and finalizes the OGG/Opus file with its 5-bit packed waveform.
    ///
    /// Returns `None` when nothing is recorded or the recording is shorter than
    /// `tg_audio`'s minimum (one second): tdesktop's `VoiceRecordBar::stopRecording`
    /// discards an empty result instead of sending it.
    pub fn finish(&mut self, now_secs: f64) -> Option<RecordedVoice> {
        if !self.is_active() {
            return None;
        }
        if self.recorder.is_none() && !self.is_paused() {
            self.feed_fallback_until(now_secs);
        }

        let capture_result = if let Some(rec) = self.recorder.take() {
            rec.stop().ok()
        } else if let Some(pipe) = self.fallback_pipeline.take() {
            pipe.finish().ok()
        } else {
            None
        };
        self.phase = VoiceRecordPhase::Idle;

        let result = capture_result.filter(|r| !r.bytes.is_empty())?;
        // `FileLoadTask::process`: `seconds = duration / 1000` (whole seconds, rounded down).
        let duration_secs = u32::try_from(result.duration_ms.max(0) / 1000).unwrap_or(u32::MAX);
        let waveform_5bit = if result.waveform_5bit.is_empty() {
            encode_5bit(&[0; 100])
        } else {
            result.waveform_5bit
        };
        Some(RecordedVoice {
            attachment: VoiceAttachment {
                duration_secs,
                waveform_5bit,
                size_bytes: result.bytes.len() as u64,
                is_unread: false,
                transcription: None,
            },
            ogg: result.bytes,
        })
    }

    /// Returns `true` while the real microphone stream is recording (never in headless runs).
    #[must_use]
    pub const fn uses_hardware_capture(&self) -> bool {
        self.recorder.is_some()
    }
}

/// Opens the default microphone (`cpal`) and streams its levels into `feed`, or `None` when
/// there is no capture device or it fails to start.
fn open_microphone(feed: &Arc<Mutex<LiveLevelFeed>>) -> Option<AudioRecorder> {
    let rec = AudioRecorder::new(Arc::new(DeviceManager::new()), CaptureConfig::default());
    let feed_cb = Arc::clone(feed);
    let cb = Arc::new(move |upd: CaptureUpdate| {
        let level_bar = ((u32::from(upd.level) * 31) / 32768).clamp(2, 31) as u8;
        if let Ok(mut guard) = feed_cb.lock() {
            guard.hardware_active = true;
            guard.last_sample_count = upd.samples;
            guard.bars.push(level_bar);
            if guard.bars.len() > 64 {
                guard.bars.remove(0);
            }
        }
    });
    (rec.available() && rec.start(Some(cb)).is_ok()).then_some(rec)
}

/// Action emitted by [`paint_voice_recording_bar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceBarAction {
    /// Cancel and discard recording.
    Cancel,
    /// Toggle pause/resume in locked mode.
    TogglePause,
    /// Lock recording hands-free.
    Lock,
    /// Finish and send recorded voice note.
    Send,
}

/// Paints the live voice recording bar (pulsing dot, elapsed timer, live waveform bars,
/// slide-to-cancel / lock controls) inside `field_rect`.
pub fn paint_voice_recording_bar(
    ui: &mut Ui,
    field_rect: Rect,
    controller: &VoiceRecorderController,
    now_secs: f64,
    colors: &HistoryColors,
) -> Option<VoiceBarAction> {
    let mut action = None;
    let elapsed = controller.elapsed_secs(now_secs);
    let secs = elapsed.floor() as u32;
    let tenths = (elapsed.fract() * 10.0).floor() as u32;

    // Pulsing red recording dot.
    let pulse = ((now_secs * 4.0).sin() * 0.5 + 0.5) as f32;
    let dot_r = 4.5 + pulse * 1.5;
    let dot_col = if controller.is_paused() {
        colors.sub_text_fg
    } else {
        colors.attention_fg
    };
    ui.painter().circle_filled(
        Pos2::new(field_rect.left() + 10.0, field_rect.center().y),
        dot_r,
        dot_col,
    );

    // Timer text (`0:04.2`)
    let timer_str = format!("{}:{:02}.{tenths}", secs / 60, secs % 60);
    ui.painter().text(
        Pos2::new(field_rect.left() + 24.0, field_rect.center().y),
        Align2::LEFT_CENTER,
        timer_str,
        FontId::monospace(13.0),
        colors.text_fg,
    );

    // Right controls (Cancel / Pause / Lock)
    let right_controls_w = if controller.is_locked() { 132.0 } else { 150.0 };
    let wave_left = field_rect.left() + 86.0;
    let wave_right = (field_rect.right() - right_controls_w).max(wave_left + 40.0);
    let wave_rect = Rect::from_min_max(
        Pos2::new(wave_left, field_rect.top() + 4.0),
        Pos2::new(wave_right, field_rect.bottom() - 4.0),
    );

    // Live scrolling waveform bars from `tg-audio`.
    let bars = controller.live_bars();
    let max_bars = ((wave_rect.width() / 4.5).floor() as usize).clamp(8, 48);
    let start_idx = bars.len().saturating_sub(max_bars);
    let visible = &bars[start_idx..];
    for (i, &lvl) in visible.iter().enumerate() {
        let bx = wave_rect.left() + i as f32 * 4.5;
        if bx + 2.5 > wave_rect.right() {
            break;
        }
        let h = ((f32::from(lvl) / 31.0) * (wave_rect.height() - 4.0)).max(3.0);
        let bar_rect = Rect::from_center_size(
            Pos2::new(bx + 1.25, wave_rect.center().y),
            Vec2::new(2.5, h),
        );
        ui.painter()
            .rect_filled(bar_rect, CornerRadius::same(1), colors.accent);
    }

    if controller.is_locked() {
        let pause_rect = Rect::from_min_size(
            Pos2::new(field_rect.right() - 124.0, field_rect.center().y - 12.0),
            Vec2::new(56.0, 24.0),
        );
        let pause_resp = ui.interact(pause_rect, Id::unique("tg_voice_pause"), Sense::click());
        if pause_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(pause_rect, CornerRadius::same(6), colors.bar_bg_over);
        }
        ui.painter().text(
            pause_rect.center(),
            Align2::CENTER_CENTER,
            if controller.is_paused() {
                "Resume"
            } else {
                "Pause"
            },
            FontId::proportional(12.0),
            colors.active_text_fg,
        );
        if pause_resp.clicked() {
            action = Some(VoiceBarAction::TogglePause);
        }

        let cancel_rect = Rect::from_min_size(
            Pos2::new(field_rect.right() - 62.0, field_rect.center().y - 12.0),
            Vec2::new(58.0, 24.0),
        );
        let cancel_resp = ui.interact(cancel_rect, Id::unique("tg_voice_cancel"), Sense::click());
        if cancel_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(cancel_rect, CornerRadius::same(6), colors.bar_bg_over);
        }
        ui.painter().text(
            cancel_rect.center(),
            Align2::CENTER_CENTER,
            "Discard",
            FontId::proportional(12.0),
            colors.attention_fg,
        );
        if cancel_resp.clicked() {
            action = Some(VoiceBarAction::Cancel);
        }
    } else {
        // Holding mode: show "< Slide to cancel" + clickable "Lock" button.
        let lock_rect = Rect::from_min_size(
            Pos2::new(field_rect.right() - 56.0, field_rect.center().y - 12.0),
            Vec2::new(52.0, 24.0),
        );
        let lock_resp = ui.interact(lock_rect, Id::unique("tg_voice_lock"), Sense::click());
        lock_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Lock recording"));
        if lock_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(lock_rect, CornerRadius::same(6), colors.bar_bg_over);
        }
        paint_lock(
            ui.painter(),
            Pos2::new(lock_rect.left() + 11.0, lock_rect.center().y),
            12.0,
            colors.active_text_fg,
        );
        ui.painter().text(
            Pos2::new(lock_rect.left() + 21.0, lock_rect.center().y),
            Align2::LEFT_CENTER,
            "Lock",
            FontId::proportional(11.5),
            colors.active_text_fg,
        );
        if lock_resp.clicked() {
            action = Some(VoiceBarAction::Lock);
        }

        let slide_galley = ui.painter().layout_no_wrap(
            "Slide to cancel".to_owned(),
            FontId::proportional(11.5),
            colors.sub_text_fg,
        );
        let slide_right = lock_rect.left() - 6.0;
        let slide_text_x = slide_right - slide_galley.size().x;
        paint_triangle(
            ui.painter(),
            Pos2::new(slide_text_x - 7.0, field_rect.center().y),
            8.0,
            ArrowDir::Left,
            colors.sub_text_fg,
        );
        ui.painter().galley(
            Pos2::new(
                slide_text_x,
                field_rect.center().y - slide_galley.size().y * 0.5,
            ),
            slide_galley,
            colors.sub_text_fg,
        );
    }

    ui.ctx().request_repaint();
    action
}

// ---------------------------------------------------------------------------
// 2. Round Video Recording (real webcam via `tg-voip::video_mf` + `tg-video`, headless-safe)
// ---------------------------------------------------------------------------

static ACTIVE_ROUND_VIDEO_TEMP_DIR: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

/// Configures the active account's temporary directory for round video captures
/// (e.g. `<tdata>/temp_<dataName>/round_videos`) so captures never land in `%TEMP%`.
pub fn set_active_round_video_temp_dir(dir: Option<std::path::PathBuf>) {
    if let Ok(mut guard) = ACTIVE_ROUND_VIDEO_TEMP_DIR.lock() {
        *guard = dir;
    }
}

/// Returns the currently configured account round-video temp directory, if any.
#[must_use]
pub fn active_round_video_temp_dir() -> Option<std::path::PathBuf> {
    ACTIVE_ROUND_VIDEO_TEMP_DIR
        .lock()
        .ok()
        .and_then(|g| g.clone())
}

/// Removes stale temporary round video files inside `dir` (skipping any file currently held open).
pub fn cleanup_round_video_temp_dir(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::remove_file(&path);
        } else if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Stateful circular round-video recorder (`ui/controls/round_video_recorder.cpp`).
///
/// Opens a real webcam through `tg-voip::video_mf` when available, renders live circular
/// preview frames, and exports a square 384×384 MP4 via `tg-video` on finish. Safely skips
/// hardware camera initialization in headless mode or when no webcam is present.
#[derive(Default)]
pub struct RoundVideoRecorderState {
    /// True while recording a round video message.
    pub active: bool,
    /// True when locked hands-free.
    pub locked: bool,
    /// Start timestamp in seconds.
    pub started_at: f64,
    /// Active real webcam capture session (when a camera is present and non-headless).
    pub webcam: Option<tg_app_media::media::round_video_capture::RoundVideoWebcamCapture>,
    /// Cached egui texture for the live webcam square preview frame.
    preview_tex: Option<TextureHandle>,
    /// Path to the most recently captured square MP4 file (if exported).
    pub last_captured_mp4: Option<String>,
    /// Optional explicit output directory overriding the active account temp directory.
    output_dir: Option<std::path::PathBuf>,
}

impl Drop for RoundVideoRecorderState {
    fn drop(&mut self) {
        self.cleanup_captured_files();
    }
}

impl std::fmt::Debug for RoundVideoRecorderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundVideoRecorderState")
            .field("active", &self.active)
            .field("locked", &self.locked)
            .field("started_at", &self.started_at)
            .field("has_webcam", &self.webcam.is_some())
            .field("has_preview_tex", &self.preview_tex.is_some())
            .field("last_captured_mp4", &self.last_captured_mp4)
            .finish()
    }
}

impl RoundVideoRecorderState {
    /// Configures an explicit per-account output directory for exported round video MP4s.
    pub fn set_output_dir(&mut self, dir: std::path::PathBuf) {
        self.output_dir = Some(dir);
    }

    /// Resolves the output directory for round video captures: explicit override, then the active
    /// account temp directory. `None` when neither is known: a capture is never written to
    /// `%TEMP%`.
    #[must_use]
    pub fn resolve_output_dir(&self) -> Option<std::path::PathBuf> {
        if let Some(ref dir) = self.output_dir {
            return Some(dir.clone());
        }
        active_round_video_temp_dir()
    }

    /// Deletes the most recently captured temporary MP4 file if present (skipping if busy).
    pub fn cleanup_captured_files(&mut self) {
        if let Some(path) = self.last_captured_mp4.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Starts round video recording at `now_secs`, opening a webcam if available (headless-safe).
    pub fn start(&mut self, now_secs: f64, locked: bool) {
        self.cancel();
        self.cleanup_captured_files();
        self.active = true;
        self.locked = locked;
        self.started_at = now_secs;
        self.webcam =
            tg_app_media::media::round_video_capture::RoundVideoWebcamCapture::try_start_default()
                .ok()
                .flatten();
    }

    /// Cancels round video recording and shuts down any active camera stream.
    pub fn cancel(&mut self) {
        if let Some(mut cam) = self.webcam.take() {
            cam.cancel();
        }
        self.preview_tex = None;
        self.active = false;
        self.locked = false;
    }

    /// Finishes round video recording, exports a square MP4 when webcam/FFmpeg is available,
    /// and returns a [`RoundVideoAttachment`].
    pub fn finish(&mut self, now_secs: f64) -> Option<RoundVideoAttachment> {
        if !self.active {
            return None;
        }
        let duration_secs = ((now_secs - self.started_at).round() as u32).clamp(1, 60);
        if let Some(mut cam) = self.webcam.take() {
            match self.resolve_output_dir() {
                Some(out_dir) => {
                    let _ = std::fs::create_dir_all(&out_dir);
                    let captured = cam.finish(&out_dir, Some(duration_secs));
                    self.last_captured_mp4 = captured
                        .mp4_path
                        .and_then(|p| p.to_str().map(str::to_owned));
                }
                // No account folder: drop the capture instead of exporting it to `%TEMP%`.
                None => cam.cancel(),
            }
        }
        self.preview_tex = None;
        self.active = false;
        self.locked = false;
        Some(RoundVideoAttachment {
            duration_secs,
            diameter_px: 220,
            accent_rgb: ([0x38, 0x62, 0x8F], [0x67, 0xB2, 0xE8]),
        })
    }

    /// Paints the floating circular camera preview above `compose_rect`.
    /// Returns `Some(true)` to send, `Some(false)` to cancel, `None` while continuing.
    pub fn show_overlay(
        &mut self,
        ui: &mut Ui,
        compose_rect: Rect,
        now_secs: f64,
        colors: &HistoryColors,
    ) -> Option<bool> {
        if !self.active {
            return None;
        }
        if let Some(ref cam) = self.webcam
            && let Some(frame) = cam.latest_preview()
        {
            let img = ColorImage::from_rgba_unmultiplied(
                [frame.side as usize, frame.side as usize],
                &frame.rgba,
            );
            self.preview_tex = Some(ui.ctx().load_texture(
                "tg_round_video_webcam_live",
                img,
                TextureOptions::LINEAR,
            ));
        }
        let has_live_webcam = self.webcam.is_some();
        let mut outcome = None;
        let elapsed = (now_secs - self.started_at).clamp(0.0, 60.0);
        let secs = elapsed.floor() as u32;

        let diameter = 180.0;
        let center = Pos2::new(
            compose_rect.center().x,
            compose_rect.top() - diameter * 0.5 - 24.0,
        );
        let card_rect = Rect::from_center_size(center, Vec2::new(diameter + 36.0, diameter + 54.0));

        egui::Area::new(Id::unique("tg_round_video_rec_overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(card_rect.min)
            .show(ui.ctx(), |ui| {
                let (r, _) = ui.allocate_exact_size(card_rect.size(), Sense::hover());
                ui.painter()
                    .rect_filled(r, CornerRadius::same(16), colors.bar_bg);
                ui.painter().rect_stroke(
                    r,
                    CornerRadius::same(16),
                    Stroke::new(1.0, colors.divider),
                    StrokeKind::Inside,
                );

                let circle_c = Pos2::new(r.center().x, r.top() + diameter * 0.5 + 12.0);
                let radius = diameter * 0.5 - 4.0;
                // Live webcam texture or headless fallback circle
                if let Some(ref tex) = self.preview_tex {
                    let img_rect = Rect::from_center_size(circle_c, Vec2::splat(radius * 2.0));
                    ui.painter().image(
                        tex.id(),
                        img_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().circle_filled(
                        circle_c,
                        radius,
                        Color32::from_rgb(0x28, 0x3D, 0x54),
                    );
                    ui.painter().circle_filled(
                        circle_c - Vec2::new(0.0, 12.0),
                        24.0,
                        Color32::from_rgba_unmultiplied(255, 255, 255, 42),
                    );
                    ui.painter().text(
                        circle_c + Vec2::new(0.0, 26.0),
                        Align2::CENTER_CENTER,
                        if has_live_webcam {
                            "Starting camera..."
                        } else {
                            "No webcam detected\n(headless round video)"
                        },
                        FontId::proportional(11.0),
                        Color32::from_rgba_unmultiplied(230, 240, 250, 210),
                    );
                }
                // Progress ring
                ui.painter()
                    .circle_stroke(circle_c, radius + 2.0, Stroke::new(3.0, colors.accent));

                // Timer pill
                let badge_rect = Rect::from_center_size(
                    Pos2::new(circle_c.x, circle_c.y - radius + 16.0),
                    Vec2::new(66.0, 20.0),
                );
                ui.painter().rect_filled(
                    badge_rect,
                    CornerRadius::same(10),
                    Color32::from_rgba_unmultiplied(210, 42, 42, 220),
                );
                ui.painter().circle_filled(
                    Pos2::new(badge_rect.left() + 12.0, badge_rect.center().y),
                    3.2,
                    Color32::WHITE,
                );
                ui.painter().text(
                    Pos2::new(badge_rect.center().x + 4.0, badge_rect.center().y),
                    Align2::CENTER_CENTER,
                    format!("{}:{:02}", secs / 60, secs % 60),
                    FontId::monospace(11.0),
                    Color32::WHITE,
                );

                // Cancel / Send buttons
                let btn_y = r.bottom() - 20.0;
                let cancel_r = Rect::from_center_size(
                    Pos2::new(r.center().x - 48.0, btn_y),
                    Vec2::new(72.0, 26.0),
                );
                let send_r = Rect::from_center_size(
                    Pos2::new(r.center().x + 48.0, btn_y),
                    Vec2::new(72.0, 26.0),
                );
                let c_resp = ui.interact(cancel_r, Id::unique("tg_rv_cancel"), Sense::click());
                ui.painter()
                    .rect_filled(cancel_r, CornerRadius::same(6), colors.bar_bg_over);
                ui.painter().text(
                    cancel_r.center(),
                    Align2::CENTER_CENTER,
                    "Cancel",
                    FontId::proportional(12.0),
                    colors.attention_fg,
                );
                if c_resp.clicked() {
                    outcome = Some(false);
                }

                let s_resp = ui.interact(send_r, Id::unique("tg_rv_send"), Sense::click());
                ui.painter()
                    .rect_filled(send_r, CornerRadius::same(6), colors.accent);
                ui.painter().text(
                    send_r.center(),
                    Align2::CENTER_CENTER,
                    "Send",
                    FontId::proportional(12.0),
                    Color32::WHITE,
                );
                if s_resp.clicked() {
                    outcome = Some(true);
                }
            });

        ui.ctx().request_repaint();
        outcome
    }
}

// ---------------------------------------------------------------------------
// 3. Spellcheck Underline & Context Menu (`tg-platform` `ISpellChecker`)
// ---------------------------------------------------------------------------

/// Stateful compose spellchecker wrapping `tg_platform::spellcheck::SpellChecker`.
pub struct ComposeSpellchecker {
    engine: Option<SpellChecker>,
    enabled: bool,
    last_text: String,
    cached_errors: Vec<MisspelledRange>,
    ignored_words: HashSet<String>,
    /// Currently open suggestion menu for `(range, word, suggestions, screen_pos)`.
    active_menu: Option<(MisspelledRange, String, Vec<String>, Pos2)>,
}

impl fmt::Debug for ComposeSpellchecker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ComposeSpellchecker")
            .field("enabled", &self.enabled)
            .field("cached_errors", &self.cached_errors.len())
            .finish()
    }
}

impl Default for ComposeSpellchecker {
    fn default() -> Self {
        Self::new()
    }
}

impl ComposeSpellchecker {
    /// Initializes the native OS spellchecker (`ISpellChecker` on Windows).
    #[must_use]
    pub fn new() -> Self {
        let engine = SpellChecker::new().ok();
        Self {
            engine,
            enabled: true,
            last_text: String::new(),
            cached_errors: Vec::new(),
            ignored_words: HashSet::new(),
            active_menu: None,
        }
    }

    /// Returns whether spellchecking is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enables or disables compose spellchecking.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.cached_errors.clear();
            self.active_menu = None;
        }
    }

    /// Checks `text` (using cached results when unchanged) and returns misspelled ranges.
    pub fn check(&mut self, text: &str) -> &[MisspelledRange] {
        if !self.enabled || text.trim().is_empty() {
            self.last_text.clear();
            self.cached_errors.clear();
            return &self.cached_errors;
        }
        if self.last_text == text {
            return &self.cached_errors;
        }
        self.last_text = text.to_owned();
        self.cached_errors.clear();

        if let Some(ref engine) = self.engine {
            let raw = engine.check_text(text);
            for r in raw {
                let end = r.byte_start.saturating_add(r.byte_len);
                if end > text.len()
                    || !text.is_char_boundary(r.byte_start)
                    || !text.is_char_boundary(end)
                {
                    continue;
                }
                let word = &text[r.byte_start..end];
                if should_skip_spellcheck_token(word)
                    || self.ignored_words.contains(&word.to_lowercase())
                {
                    continue;
                }
                self.cached_errors.push(r);
            }
        }
        &self.cached_errors
    }

    /// Returns cached misspelled ranges from the last check.
    #[must_use]
    pub fn errors(&self) -> &[MisspelledRange] {
        &self.cached_errors
    }

    /// Queries up to `DEFAULT_MAX_SUGGESTIONS` replacement suggestions for `word`.
    #[must_use]
    pub fn suggestions(&self, word: &str) -> Vec<String> {
        self.engine
            .as_ref()
            .map(|e| e.suggestions(word, DEFAULT_MAX_SUGGESTIONS))
            .unwrap_or_default()
    }

    /// Adds `word` to the user dictionary via `ISpellChecker::Add` and clears cached errors for it.
    pub fn add_to_dictionary(&mut self, word: &str) {
        if let Some(ref engine) = self.engine {
            let _ = engine.add_word(word);
        }
        self.ignored_words.insert(word.to_lowercase());
        self.last_text.clear();
    }

    /// Ignores `word` for the current session.
    pub fn ignore_word(&mut self, word: &str) {
        self.ignored_words.insert(word.to_lowercase());
        self.last_text.clear();
    }

    /// Replaces `range` in `text` with `replacement`. Returns `true` if replaced.
    pub fn replace_range(text: &mut String, range: MisspelledRange, replacement: &str) -> bool {
        let end = range.byte_start.saturating_add(range.byte_len);
        if end <= text.len()
            && text.is_char_boundary(range.byte_start)
            && text.is_char_boundary(end)
        {
            text.replace_range(range.byte_start..end, replacement);
            return true;
        }
        false
    }

    /// Opens the suggestion context menu for the misspelled range covering `byte_offset` (or the first error).
    pub fn open_menu_for_text(
        &mut self,
        text: &str,
        screen_pos: Pos2,
        target_range: Option<MisspelledRange>,
    ) {
        let chosen = target_range.or_else(|| self.cached_errors.first().copied());
        let Some(range) = chosen else {
            return;
        };
        let end = range.byte_start.saturating_add(range.byte_len);
        if end > text.len() {
            return;
        }
        let word = text[range.byte_start..end].to_owned();
        let suggs = self.suggestions(&word);
        self.active_menu = Some((range, word, suggs, screen_pos));
    }

    /// Paints red wavy underlines over `field_rect` for all misspelled spans and renders the
    /// suggestion context menu if open. Returns `true` if `text` was mutated by a suggestion.
    pub fn paint_underlines_and_menu(
        &mut self,
        ui: &mut Ui,
        field_rect: Rect,
        text: &mut String,
        colors: &HistoryColors,
    ) -> bool {
        self.check(text);
        if self.cached_errors.is_empty() && self.active_menu.is_none() {
            return false;
        }

        // Paint wavy red underlines aligned to the compose TextEdit galley layout.
        if !self.cached_errors.is_empty() {
            let galley = ui.painter().layout(
                text.clone(),
                FontId::proportional(13.5),
                colors.text_fg,
                field_rect.width().max(60.0),
            );
            let origin = field_rect.min + Vec2::new(4.0, 2.0);
            let clipped = ui.painter().with_clip_rect(field_rect);

            for err in &self.cached_errors {
                let end = err.byte_start.saturating_add(err.byte_len);
                if end > text.len() {
                    continue;
                }
                let start_char = text[..err.byte_start].chars().count();
                let end_char = start_char + text[err.byte_start..end].chars().count();
                for sr in crate::history::text_bridge::char_range_rects(
                    &galley, origin, start_char, end_char,
                ) {
                    paint_wavy_underline(
                        &clipped,
                        sr.left(),
                        sr.right(),
                        sr.bottom() - 1.0,
                        colors.attention_fg,
                    );
                }
            }
        }

        // Context menu with spelling suggestions.
        let mut modified = false;
        if let Some((range, word, suggs, pos)) = self.active_menu.clone() {
            let mut close = false;
            egui::Area::new(Id::unique("tg_spellcheck_ctx_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE
                        .fill(colors.bar_bg)
                        .stroke(Stroke::new(1.0, colors.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            ui.set_min_width(170.0);
                            ui.label(
                                egui::RichText::new(format!("Spelling: \"{word}\""))
                                    .size(11.5)
                                    .color(colors.sub_text_fg),
                            );
                            ui.separator();
                            if suggs.is_empty() {
                                ui.label(
                                    egui::RichText::new("No suggestions")
                                        .size(12.0)
                                        .color(colors.sub_text_fg),
                                );
                            } else {
                                for s in &suggs {
                                    if ui
                                        .selectable_label(false, egui::RichText::new(s).size(13.0))
                                        .clicked()
                                    {
                                        if Self::replace_range(text, range, s) {
                                            modified = true;
                                        }
                                        close = true;
                                    }
                                }
                            }
                            ui.separator();
                            if ui
                                .selectable_label(
                                    false,
                                    egui::RichText::new("Add to Dictionary").size(12.5),
                                )
                                .clicked()
                            {
                                self.add_to_dictionary(&word);
                                close = true;
                            }
                            if ui
                                .selectable_label(false, egui::RichText::new("Ignore").size(12.5))
                                .clicked()
                            {
                                self.ignore_word(&word);
                                close = true;
                            }
                            if ui
                                .selectable_label(false, egui::RichText::new("Close").size(12.0))
                                .clicked()
                            {
                                close = true;
                            }
                        });
                });
            if close || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                self.active_menu = None;
            }
        }
        modified
    }
}

fn should_skip_spellcheck_token(word: &str) -> bool {
    word.starts_with('@')
        || word.starts_with('#')
        || word.starts_with('/')
        || word.contains("://")
        || word.chars().all(|c| c.is_ascii_digit())
}

fn paint_wavy_underline(painter: &Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    if x1 <= x0 + 2.0 {
        return;
    }
    let mut pts = Vec::new();
    let step = 3.0;
    let mut x = x0;
    let mut up = false;
    while x <= x1 {
        pts.push(Pos2::new(x, y + if up { -1.2 } else { 1.2 }));
        up = !up;
        x += step;
    }
    if pts.len() >= 2 {
        painter.add(Shape::line(pts, Stroke::new(1.2, color)));
    }
}

// ---------------------------------------------------------------------------
// 4. Link Preview Editing (above compose field, with remove/disable)
// ---------------------------------------------------------------------------

/// Height of the editable link preview strip above the compose input.
pub const LINK_PREVIEW_BAR_HEIGHT: f32 = 48.0;

/// Debounce delay in milliseconds before invoking `messages.getWebPagePreview` while typing a URL.
pub const LINK_PREVIEW_DEBOUNCE_MS: u64 = 400;

/// Stateful link preview detector and editor above the compose input.
#[derive(Debug, Clone, Default)]
pub struct LinkPreviewEditorState {
    /// Currently resolved preview, if any.
    pub preview: Option<LinkPreviewData>,
    /// URL the user explicitly disabled via `×`.
    pub disabled_url: Option<String>,
    /// Last URL resolved via backend.
    last_url: Option<String>,
    /// Pending URL awaiting debounce deadline `(url, ready_at_ms)`.
    pending_debounce: Option<(String, u64)>,
}

impl LinkPreviewEditorState {
    /// Resets state (e.g. after sending or switching peers).
    pub fn clear(&mut self) {
        self.preview = None;
        self.disabled_url = None;
        self.last_url = None;
        self.pending_debounce = None;
    }

    /// Returns `true` if a link preview bar should be displayed above the compose field.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.preview.is_some()
    }

    /// Returns `true` if the user explicitly disabled link previews for the current text.
    #[must_use]
    pub const fn is_disabled(&self) -> bool {
        self.disabled_url.is_some()
    }

    /// Returns `true` if a debounced preview lookup is currently waiting to fire.
    #[must_use]
    pub const fn has_pending_debounce(&self) -> bool {
        self.pending_debounce.is_some()
    }

    /// Re-enables link previews after a user clicked `×` to remove the preview.
    pub fn re_enable_preview(&mut self) {
        self.disabled_url = None;
        self.last_url = None;
        self.pending_debounce = None;
    }

    /// Extracts the first URL in `text` via `tg-text` and resolves its preview immediately through `backend`.
    pub fn sync_from_text(&mut self, text: &str, backend: &dyn HistoryBackend) {
        let detected = extract_first_url(text);
        self.pending_debounce = None;
        if detected == self.last_url {
            return;
        }
        self.last_url = detected.clone();
        let Some(url) = detected else {
            self.preview = None;
            self.disabled_url = None;
            return;
        };
        if self.disabled_url.as_deref() == Some(url.as_str())
            || self.disabled_url.as_deref() == Some("")
        {
            self.disabled_url = Some(url);
            self.preview = None;
            return;
        }
        self.preview = backend.resolve_link_preview(&url);
    }

    /// Debounced variant of [`Self::sync_from_text`] waiting [`LINK_PREVIEW_DEBOUNCE_MS`] after
    /// the last URL edit before calling `backend.resolve_link_preview`.
    ///
    /// Returns `true` if a debounce timer is still pending (caller should request a repaint).
    pub fn sync_from_text_debounced(
        &mut self,
        text: &str,
        now_ms: u64,
        backend: &dyn HistoryBackend,
    ) -> bool {
        let detected = extract_first_url(text);
        let Some(url) = detected else {
            self.last_url = None;
            self.preview = None;
            self.disabled_url = None;
            self.pending_debounce = None;
            return false;
        };
        if self.disabled_url.as_deref() == Some(url.as_str())
            || self.disabled_url.as_deref() == Some("")
        {
            self.disabled_url = Some(url.clone());
            self.last_url = Some(url);
            self.preview = None;
            self.pending_debounce = None;
            return false;
        }
        if self.last_url.as_deref() == Some(url.as_str()) {
            self.pending_debounce = None;
            return false;
        }
        match &self.pending_debounce {
            Some((pending_url, ready_at)) if pending_url == &url => {
                if now_ms >= *ready_at {
                    self.pending_debounce = None;
                    self.last_url = Some(url.clone());
                    self.preview = backend.resolve_link_preview(&url);
                    false
                } else {
                    true
                }
            }
            _ => {
                self.pending_debounce =
                    Some((url, now_ms.saturating_add(LINK_PREVIEW_DEBOUNCE_MS)));
                true
            }
        }
    }

    /// Paints the editable link preview strip inside `rect`.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, colors: &HistoryColors) {
        let Some(ref mut lp) = self.preview else {
            return;
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

        // Left vertical accent line
        let bar = Rect::from_min_size(
            Pos2::new(rect.left() + 14.0, rect.top() + 7.0),
            Vec2::new(2.5, rect.height() - 14.0),
        );
        ui.painter()
            .rect_filled(bar, CornerRadius::same(1), colors.accent);

        let mut text_x = bar.right() + 10.0;
        if let Some([r, g, b]) = lp.thumb_rgb {
            let thumb = Rect::from_min_size(Pos2::new(text_x, rect.top() + 8.0), Vec2::splat(32.0));
            ui.painter()
                .rect_filled(thumb, CornerRadius::same(4), Color32::from_rgb(r, g, b));
            text_x = thumb.right() + 8.0;
        }

        // Right controls: Large/Small toggle, Above/Below toggle, Disable `×`
        let close_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 20.0, rect.center().y),
            Vec2::splat(24.0),
        );
        let pos_rect = Rect::from_center_size(
            Pos2::new(close_rect.left() - 36.0, rect.center().y),
            Vec2::new(58.0, 22.0),
        );
        let size_rect = Rect::from_center_size(
            Pos2::new(pos_rect.left() - 34.0, rect.center().y),
            Vec2::new(54.0, 22.0),
        );

        let max_text_w = (size_rect.left() - text_x - 8.0).max(60.0);
        let header_galley = ui.painter().layout_no_wrap(
            format!("{} — {}", lp.site_name, lp.title),
            FontId::proportional(12.0),
            colors.active_text_fg,
        );
        ui.painter()
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(text_x, rect.top() + 6.0),
                Vec2::new(max_text_w, 17.0),
            ))
            .galley(
                Pos2::new(text_x, rect.top() + 6.0),
                header_galley,
                colors.active_text_fg,
            );

        let desc_galley = ui.painter().layout_no_wrap(
            lp.description.clone(),
            FontId::proportional(11.5),
            colors.sub_text_fg,
        );
        ui.painter()
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(text_x, rect.top() + 24.0),
                Vec2::new(max_text_w, 17.0),
            ))
            .galley(
                Pos2::new(text_x, rect.top() + 24.0),
                desc_galley,
                colors.sub_text_fg,
            );

        // Toggle Small / Large preview media
        let size_resp = ui.interact(size_rect, Id::unique("tg_lp_size"), Sense::click());
        if size_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(size_rect, CornerRadius::same(5), colors.bar_bg_over);
        ui.painter().text(
            size_rect.center(),
            Align2::CENTER_CENTER,
            if lp.large_media { "Large" } else { "Small" },
            FontId::proportional(11.0),
            colors.active_text_fg,
        );
        if size_resp
            .on_hover_text("Toggle link preview media size")
            .clicked()
        {
            lp.large_media = !lp.large_media;
        }

        // Toggle Above / Below text
        let pos_resp = ui.interact(pos_rect, Id::unique("tg_lp_pos"), Sense::click());
        if pos_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(pos_rect, CornerRadius::same(5), colors.bar_bg_over);
        ui.painter().text(
            pos_rect.center(),
            Align2::CENTER_CENTER,
            if lp.above_text { "Above" } else { "Below" },
            FontId::proportional(11.0),
            colors.active_text_fg,
        );
        if pos_resp
            .on_hover_text("Move link preview above or below text")
            .clicked()
        {
            lp.above_text = !lp.above_text;
        }

        // Remove / Disable preview
        let close_resp = ui.interact(close_rect, Id::unique("tg_lp_close"), Sense::click());
        if close_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(close_rect.center(), 12.0, colors.bar_bg_over);
        }
        ui.painter().text(
            close_rect.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(15.0),
            colors.sub_text_fg,
        );
        let disable_now = close_resp.on_hover_text("Disable link preview").clicked();

        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom()),
                Pos2::new(rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, colors.divider),
        );

        if disable_now {
            let taken = self.preview.take();
            self.disabled_url = self.last_url.clone().or_else(|| taken.map(|p| p.url));
        }
    }
}

/// Extracts the first URL in `text` using `tg-text::parse_entities`.
#[must_use]
pub fn extract_first_url(text: &str) -> Option<String> {
    let parsed = parse_entities(text, TextParseFlags::LINKS);
    for ent in &parsed.entities {
        if ent.entity_type() == EntityType::Url {
            let range = tg_ui::entity_bounds::entity_utf16_range(ent, tg_text::utf16_len(text))?;
            let start = tg_text::utf16_to_utf8_offset(text, range.start)?;
            let end = tg_text::utf16_to_utf8_offset(text, range.end)?;
            let slice = &text[start..end];
            if slice.contains("://") {
                return Some(slice.to_owned());
            }
            return Some(format!("https://{slice}"));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 5. Inline Bot Query Results Panel Hook
// ---------------------------------------------------------------------------

/// External hook invoked to fetch inline bot query results for `(bot_username, query)`.
pub type InlineBotQueryHook = Box<dyn FnMut(&str, &str) -> Vec<InlineBotResult> + Send>;

/// Stateful floating inline bot results panel above the compose field.
pub struct InlineBotPanelState {
    last_bot: String,
    last_query: String,
    results: Vec<InlineBotResult>,
    hook: Option<InlineBotQueryHook>,
}

impl fmt::Debug for InlineBotPanelState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InlineBotPanelState")
            .field("last_bot", &self.last_bot)
            .field("last_query", &self.last_query)
            .field("results", &self.results.len())
            .finish()
    }
}

impl Default for InlineBotPanelState {
    fn default() -> Self {
        Self::new()
    }
}

impl InlineBotPanelState {
    /// Creates an empty inline bot results panel state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_bot: String::new(),
            last_query: String::new(),
            results: Vec::new(),
            hook: None,
        }
    }

    /// Installs an external inline bot results resolver hook.
    pub fn set_hook(&mut self, hook: InlineBotQueryHook) {
        self.hook = Some(hook);
    }

    /// Clears active results.
    pub fn clear(&mut self) {
        self.last_bot.clear();
        self.last_query.clear();
        self.results.clear();
    }

    /// Synchronizes results with current compose text.
    pub fn sync_from_text(&mut self, text: &str, backend: &dyn HistoryBackend) {
        let Some((bot, query)) = parse_inline_bot_query(text) else {
            self.clear();
            return;
        };
        if self.last_bot == bot && self.last_query == query {
            return;
        }
        self.last_bot = bot.to_owned();
        self.last_query = query.to_owned();
        self.results = if let Some(ref mut hook) = self.hook {
            hook(bot, query)
        } else {
            backend.query_inline_bot(bot, query)
        };
    }

    /// Returns currently fetched inline bot results.
    #[must_use]
    pub fn results(&self) -> &[InlineBotResult] {
        &self.results
    }

    /// Paints the floating inline results panel above `compose_rect`, returning a clicked result.
    pub fn show(
        &self,
        ui: &mut Ui,
        compose_rect: Rect,
        colors: &HistoryColors,
    ) -> Option<InlineBotResult> {
        if self.results.is_empty() {
            return None;
        }
        let mut chosen = None;
        let panel_h = 184.0f32.min(34.0 + self.results.len() as f32 * 52.0);
        let panel_rect = Rect::from_min_size(
            Pos2::new(
                compose_rect.left() + 8.0,
                compose_rect.top() - panel_h - 4.0,
            ),
            Vec2::new((compose_rect.width() - 16.0).max(200.0), panel_h),
        );

        egui::Area::new(Id::unique("tg_inline_bot_results_panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(panel_rect.min)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_min_size(panel_rect.size() - Vec2::splat(16.0));
                        ui.set_max_size(panel_rect.size() - Vec2::splat(16.0));
                        ui.label(
                            egui::RichText::new(format!(
                                "Inline results via @{} ({})",
                                self.last_bot,
                                self.results.len()
                            ))
                            .size(11.5)
                            .color(colors.active_text_fg),
                        );
                        ui.add_space(4.0);

                        let is_mosaic = self
                            .results
                            .iter()
                            .all(|r| !matches!(r.kind, InlineBotResultKind::Article));

                        ScrollArea::vertical()
                            .id_salt("tg_inline_bot_scroll")
                            .show(ui, |ui| {
                                if is_mosaic {
                                    ui.horizontal_wrapped(|ui| {
                                        for res in &self.results {
                                            let [r, g, b] = res.thumb_rgb;
                                            let (tile, resp) = ui.allocate_exact_size(
                                                Vec2::new(112.0, 82.0),
                                                Sense::click(),
                                            );
                                            if resp.hovered() {
                                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                            }
                                            ui.painter().rect_filled(
                                                tile,
                                                CornerRadius::same(8),
                                                Color32::from_rgb(r, g, b),
                                            );
                                            let badge =
                                                if matches!(res.kind, InlineBotResultKind::Gif) {
                                                    "GIF"
                                                } else {
                                                    "PIC"
                                                };
                                            ui.painter().text(
                                                Pos2::new(tile.left() + 6.0, tile.top() + 5.0),
                                                Align2::LEFT_TOP,
                                                badge,
                                                FontId::proportional(10.0),
                                                Color32::WHITE,
                                            );
                                            ui.painter().text(
                                                Pos2::new(tile.center().x, tile.bottom() - 10.0),
                                                Align2::CENTER_BOTTOM,
                                                &res.title,
                                                FontId::proportional(11.5),
                                                Color32::WHITE,
                                            );
                                            if resp.clicked() {
                                                chosen = Some(res.clone());
                                            }
                                        }
                                    });
                                } else {
                                    for res in &self.results {
                                        let (row_r, resp) = ui.allocate_exact_size(
                                            Vec2::new(ui.available_width(), 44.0),
                                            Sense::click(),
                                        );
                                        if resp.hovered() {
                                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                            ui.painter().rect_filled(
                                                row_r,
                                                CornerRadius::same(6),
                                                colors.bar_bg_over,
                                            );
                                        }
                                        let [r, g, b] = res.thumb_rgb;
                                        let thumb = Rect::from_min_size(
                                            row_r.min + Vec2::new(4.0, 4.0),
                                            Vec2::splat(36.0),
                                        );
                                        ui.painter().rect_filled(
                                            thumb,
                                            CornerRadius::same(6),
                                            Color32::from_rgb(r, g, b),
                                        );
                                        let tx = thumb.right() + 8.0;
                                        ui.painter().text(
                                            Pos2::new(tx, row_r.top() + 5.0),
                                            Align2::LEFT_TOP,
                                            &res.title,
                                            FontId::proportional(12.5),
                                            colors.bold_fg,
                                        );
                                        ui.painter().text(
                                            Pos2::new(tx, row_r.top() + 23.0),
                                            Align2::LEFT_TOP,
                                            &res.description,
                                            FontId::proportional(11.5),
                                            colors.sub_text_fg,
                                        );
                                        if resp.clicked() {
                                            chosen = Some(res.clone());
                                        }
                                    }
                                }
                            });
                    });
            });

        chosen
    }
}

// ---------------------------------------------------------------------------
// 6. Send-As Peer Chooser & Message Effects Picker & Slow Mode Banner
// ---------------------------------------------------------------------------

/// Stateful Send-As peer chooser popup (`SendAsButton`).
#[derive(Debug, Clone, Default)]
pub struct SendAsChooserState {
    /// True while popup list is open.
    pub open: bool,
    /// Currently selected send-as peer id (`None` = default self peer).
    pub selected: Option<PeerId>,
}

impl SendAsChooserState {
    /// Paints the floating Send-As chooser popup above `anchor`.
    /// Returns `Some(peer_id)` when the user selects a new send-as identity.
    pub fn show_popup(
        &mut self,
        ui: &mut Ui,
        anchor: Pos2,
        peers: &[SendAsPeer],
        colors: &HistoryColors,
    ) -> Option<PeerId> {
        if !self.open || peers.is_empty() {
            return None;
        }
        let mut newly_selected = None;
        let h = (32.0 + peers.len() as f32 * 44.0).min(240.0);
        let pos = Pos2::new(anchor.x, (anchor.y - h - 6.0).max(8.0));

        egui::Area::new(Id::unique("tg_send_as_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_min_width(230.0);
                        ui.label(
                            egui::RichText::new("Send message as...")
                                .size(12.0)
                                .color(colors.active_text_fg),
                        );
                        ui.add_space(4.0);
                        for peer in peers {
                            let is_sel = self.selected == Some(peer.peer_id);
                            let (r, resp) =
                                ui.allocate_exact_size(Vec2::new(220.0, 40.0), Sense::click());
                            if resp.hovered() || is_sel {
                                ui.painter().rect_filled(
                                    r,
                                    CornerRadius::same(6),
                                    colors.bar_bg_over,
                                );
                            }
                            let [cr, cg, cb] = peer.avatar_rgb;
                            let av_c = Pos2::new(r.left() + 18.0, r.center().y);
                            ui.painter()
                                .circle_filled(av_c, 14.0, Color32::from_rgb(cr, cg, cb));
                            let initial = peer.title.chars().next().unwrap_or('?').to_string();
                            ui.painter().text(
                                av_c,
                                Align2::CENTER_CENTER,
                                initial,
                                FontId::proportional(12.0),
                                Color32::WHITE,
                            );
                            ui.painter().text(
                                Pos2::new(r.left() + 40.0, r.top() + 5.0),
                                Align2::LEFT_TOP,
                                &peer.title,
                                FontId::proportional(12.5),
                                colors.bold_fg,
                            );
                            ui.painter().text(
                                Pos2::new(r.left() + 40.0, r.top() + 22.0),
                                Align2::LEFT_TOP,
                                &peer.subtitle,
                                FontId::proportional(11.0),
                                colors.sub_text_fg,
                            );
                            if is_sel {
                                paint_select_check(
                                    ui.painter(),
                                    Pos2::new(r.right() - 16.0, r.center().y),
                                    14.0,
                                    colors.accent,
                                );
                            }
                            if resp.clicked() {
                                self.selected = Some(peer.peer_id);
                                newly_selected = Some(peer.peer_id);
                                self.open = false;
                            }
                        }
                    });
            });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
        }
        newly_selected
    }
}

/// Active message-effect playback entry rendered over the chat list via `tg-lottie`.
#[derive(Debug, Clone)]
pub struct ActiveMessageEffect {
    /// Target message ID that triggered the effect.
    pub msg_id: MsgId,
    /// Telegram effect ID.
    pub effect_id: u64,
    /// Representative emoji rendered at the center of the Lottie burst.
    pub emoji: String,
    /// Accent RGB color used for fallback procedural Lottie synthesis.
    pub accent_rgb: [u8; 3],
    /// Optional server `.tgs` decompressed Lottie JSON payload.
    pub lottie_json: Option<String>,
    /// Playback start timestamp in seconds.
    pub started_at: f64,
}

/// Stateful live Message Effects picker popup & `tg-lottie` effect playback controller
/// (`messages.getAvailableEffects`).
#[derive(Default)]
pub struct MessageEffectsPickerState {
    /// True while picker popup is open.
    pub open: bool,
    /// Selected effect id to attach to next sent message.
    pub selected_effect_id: Option<u64>,
    /// Live catalog loaded from `HistoryBackend::available_effects` (`messages.getAvailableEffects`).
    catalog: Vec<AvailableMessageEffect>,
    /// True once hydrated from a backend.
    catalog_loaded: bool,
    /// Currently playing send/receive message-effect animations.
    pub active_effects: Vec<ActiveMessageEffect>,
    /// Compiled `tg-lottie` compositions keyed by `effect_id`.
    lottie_comps: HashMap<u64, Option<Composition>>,
    /// Rasterized Lottie frames keyed by `(effect_id, step)`.
    lottie_frames: HashMap<(u64, u32), TextureHandle>,
}

impl fmt::Debug for MessageEffectsPickerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageEffectsPickerState")
            .field("open", &self.open)
            .field("selected_effect_id", &self.selected_effect_id)
            .field("catalog_len", &self.catalog.len())
            .field("catalog_loaded", &self.catalog_loaded)
            .field("active_effects", &self.active_effects.len())
            .finish()
    }
}

impl MessageEffectsPickerState {
    /// Duration of one message-effect Lottie playback burst in seconds.
    pub const PLAYBACK_DURATION_SECS: f64 = 1.8;

    /// Returns `true` when the live catalog has been hydrated from a backend.
    #[must_use]
    pub const fn is_catalog_loaded(&self) -> bool {
        self.catalog_loaded
    }

    /// Replaces the live effects catalog (from `HistoryBackend::available_effects`).
    pub fn sync_catalog(&mut self, effects: Vec<AvailableMessageEffect>) {
        if !effects.is_empty() {
            self.catalog = effects;
        }
        self.catalog_loaded = true;
    }

    /// Resets cached catalog hydration (e.g. on session switch).
    pub fn reset_catalog(&mut self) {
        self.catalog.clear();
        self.catalog_loaded = false;
        self.lottie_comps.clear();
        self.lottie_frames.clear();
    }

    /// Returns the active effects catalog slice (live catalog if loaded, otherwise empty).
    #[must_use]
    pub fn catalog(&self) -> &[AvailableMessageEffect] {
        &self.catalog
    }

    /// Finds an effect by ID in the hydrated live catalog.
    #[must_use]
    pub fn find_effect(&self, id: u64) -> Option<&AvailableMessageEffect> {
        self.catalog.iter().find(|e| e.id == id)
    }

    /// Resolves `(emoji, accent_rgb)` for `effect_id` using the hydrated live catalog first,
    /// then the built-in fallback table.
    #[must_use]
    pub fn resolve_effect_visual(&self, effect_id: u64) -> (String, [u8; 3]) {
        if let Some(eff) = self.find_effect(effect_id) {
            return (eff.emoji.clone(), eff.accent_rgb);
        }
        if let Some(eff) = crate::history::sections_model::find_message_effect(effect_id) {
            return (eff.emoji.to_owned(), eff.accent_rgb);
        }
        let (_, rgb) = effect_name_and_accent(effect_id, "✨");
        ("✨".to_owned(), rgb)
    }

    /// Triggers a `tg-lottie` message-effect burst for `msg_id` (on send or newly received message).
    pub fn trigger_effect(
        &mut self,
        msg_id: MsgId,
        effect_id: u64,
        lottie_json: Option<String>,
        now_secs: f64,
    ) {
        let (emoji, accent_rgb) = self.resolve_effect_visual(effect_id);
        if let Some(ref json) = lottie_json
            && let Ok(comp) = Composition::from_json(json)
        {
            self.lottie_comps.insert(effect_id, Some(comp));
            self.lottie_frames.retain(|&(eid, _), _| eid != effect_id);
        }
        self.active_effects.retain(|e| e.msg_id != msg_id);
        self.active_effects.push(ActiveMessageEffect {
            msg_id,
            effect_id,
            emoji,
            accent_rgb,
            lottie_json,
            started_at: now_secs,
        });
    }

    /// Renders a quantized `tg-lottie` frame for `effect` at elapsed time `elapsed_secs`.
    pub fn lottie_effect_frame(
        &mut self,
        ctx: &egui::Context,
        effect_id: u64,
        emoji: &str,
        accent_rgb: [u8; 3],
        lottie_json: Option<&str>,
        elapsed_secs: f64,
    ) -> Option<TextureHandle> {
        self.lottie_comps.entry(effect_id).or_insert_with(|| {
            lottie_json
                .and_then(|j| Composition::from_json(j).ok())
                .or_else(|| {
                    let fallback = animated_effect_lottie_json(emoji, accent_rgb);
                    Composition::from_json(&fallback).ok()
                })
        });
        let comp = self.lottie_comps.get(&effect_id)?.as_ref()?;
        let span = (comp.out_point - comp.in_point).max(1.0);
        let steps = 18u32;
        let frac = (elapsed_secs / Self::PLAYBACK_DURATION_SECS).clamp(0.0, 0.999);
        let step = (frac * f64::from(steps)).floor() as u32 % steps;
        let key = (effect_id, step);
        if let Some(tex) = self.lottie_frames.get(&key) {
            return Some(tex.clone());
        }
        let sample = comp.in_point + (f64::from(step) / f64::from(steps)) * span;
        let fb = render_composition(comp, sample, LottieSize::new(96, 96));
        let img =
            ColorImage::from_rgba_premultiplied([fb.width as usize, fb.height as usize], &fb.data);
        let tex = ctx.load_texture(
            format!("tg_msg_effect_lottie_{effect_id}_{step}"),
            img,
            TextureOptions::LINEAR,
        );
        self.lottie_frames.insert(key, tex.clone());
        Some(tex)
    }

    /// Paints any active send/receive message-effect Lottie bursts over `list_rect`.
    pub fn paint_active_effects(&mut self, ui: &mut Ui, list_rect: Rect, now_secs: f64) {
        self.active_effects
            .retain(|e| now_secs - e.started_at <= Self::PLAYBACK_DURATION_SECS);
        if self.active_effects.is_empty() || !list_rect.is_positive() {
            return;
        }
        let snapshot = self.active_effects.clone();
        let ctx = ui.ctx().clone();
        for (idx, eff) in snapshot.iter().enumerate() {
            let elapsed = (now_secs - eff.started_at).clamp(0.0, Self::PLAYBACK_DURATION_SECS);
            let progress = (elapsed / Self::PLAYBACK_DURATION_SECS) as f32;
            let center = Pos2::new(
                list_rect.center().x
                    + ((idx as f32) - (snapshot.len().saturating_sub(1) as f32) * 0.5) * 44.0,
                list_rect.bottom() - 88.0 - progress * 54.0,
            );
            let burst_size = 96.0 * (0.75 + 0.35 * progress);
            let burst_rect = Rect::from_center_size(center, Vec2::splat(burst_size));
            if let Some(tex) = self.lottie_effect_frame(
                &ctx,
                eff.effect_id,
                &eff.emoji,
                eff.accent_rgb,
                eff.lottie_json.as_deref(),
                elapsed,
            ) {
                ui.painter().image(
                    tex.id(),
                    burst_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            ui.painter().text(
                center,
                Align2::CENTER_CENTER,
                &eff.emoji,
                FontId::proportional(28.0),
                Color32::WHITE,
            );
        }
        ui.ctx().request_repaint();
    }

    /// Paints the floating effects picker popup above `anchor` using the hydrated live catalog
    /// (falling back to built-in effects when offline).
    pub fn show_popup(&mut self, ui: &mut Ui, anchor: Pos2, colors: &HistoryColors) {
        if !self.open {
            return;
        }
        let fallback: Vec<AvailableMessageEffect> = if self.catalog.is_empty() {
            MESSAGE_EFFECTS
                .iter()
                .map(|e| AvailableMessageEffect {
                    id: e.id,
                    emoji: e.emoji.to_owned(),
                    name: e.name.to_owned(),
                    premium_required: false,
                    static_icon_id: None,
                    effect_sticker_id: 0,
                    effect_animation_id: None,
                    lottie_json: None,
                    accent_rgb: e.accent_rgb,
                })
                .collect()
        } else {
            Vec::new()
        };
        let items: Vec<AvailableMessageEffect> = if self.catalog.is_empty() {
            fallback
        } else {
            self.catalog.clone()
        };
        self.show_popup_with_catalog(ui, anchor, &items, colors);
    }

    /// Paints the floating effects picker popup above `anchor` from an explicit `catalog`.
    pub fn show_popup_with_catalog(
        &mut self,
        ui: &mut Ui,
        anchor: Pos2,
        catalog: &[AvailableMessageEffect],
        colors: &HistoryColors,
    ) {
        if !self.open {
            return;
        }
        let pos = Pos2::new((anchor.x - 220.0).max(8.0), (anchor.y - 175.0).max(8.0));
        egui::Area::new(Id::unique("tg_msg_effects_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width(220.0);
                        ui.label(
                            egui::RichText::new("Message Effects")
                                .size(12.0)
                                .strong()
                                .color(colors.active_text_fg),
                        );
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            for eff in catalog {
                                let is_sel = self.selected_effect_id == Some(eff.id);
                                let [r, g, b] = eff.accent_rgb;
                                let bg = if is_sel {
                                    Color32::from_rgb(r, g, b)
                                } else {
                                    colors.bar_bg_over
                                };
                                let (tile, resp) =
                                    ui.allocate_exact_size(Vec2::new(64.0, 46.0), Sense::click());
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                }
                                ui.painter().rect_filled(tile, CornerRadius::same(8), bg);
                                ui.painter().text(
                                    Pos2::new(tile.center().x, tile.top() + 15.0),
                                    Align2::CENTER_CENTER,
                                    &eff.emoji,
                                    FontId::proportional(16.0),
                                    colors.text_fg,
                                );
                                if eff.premium_required {
                                    paint_star(
                                        ui.painter(),
                                        Pos2::new(tile.right() - 8.0, tile.top() + 8.0),
                                        4.2,
                                        if is_sel {
                                            Color32::WHITE
                                        } else {
                                            colors.accent
                                        },
                                    );
                                }
                                ui.painter().text(
                                    Pos2::new(tile.center().x, tile.bottom() - 8.0),
                                    Align2::CENTER_CENTER,
                                    &eff.name,
                                    FontId::proportional(9.5),
                                    if is_sel {
                                        Color32::WHITE
                                    } else {
                                        colors.sub_text_fg
                                    },
                                );
                                if resp.clicked() {
                                    self.selected_effect_id =
                                        if is_sel { None } else { Some(eff.id) };
                                    self.open = false;
                                }
                            }
                        });
                        if self.selected_effect_id.is_some() {
                            ui.add_space(4.0);
                            if ui.small_button("Clear effect").clicked() {
                                self.selected_effect_id = None;
                                self.open = false;
                            }
                        }
                    });
            });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
        }
    }
}

/// Paints a compact slow-mode countdown pill (`MM:SS`) inside `rect`.
pub fn paint_slow_mode_badge(
    painter: &Painter,
    rect: Rect,
    remaining_secs: u32,
    colors: &HistoryColors,
) {
    let label = SlowModeState::format_countdown(remaining_secs);
    painter.rect_filled(rect, CornerRadius::same(10), colors.bar_bg_over);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(11.0),
        colors.attention_fg,
    );
}

// ---------------------------------------------------------------------------
// 7. Full Emoji Reactions Picker with `tg-lottie` Animated Reactions
// ---------------------------------------------------------------------------

/// Popular quick reactions shown in the top strip of the reactions picker.
pub const QUICK_REACTIONS: &[&str] = &["👍", "❤️", "🔥", "🎉", "😂", "😮", "😢", "👏"];

/// Emoji category tabs exposed in the full reactions picker (`tg_text::emoji::Section`).
pub const REACTION_EMOJI_SECTIONS: &[(EmojiSection, &str)] = &[
    (EmojiSection::Recent, "Recent"),
    (EmojiSection::People, "Smileys"),
    (EmojiSection::Nature, "Nature"),
    (EmojiSection::Food, "Food"),
    (EmojiSection::Activity, "Activity"),
    (EmojiSection::Travel, "Travel"),
    (EmojiSection::Objects, "Objects"),
    (EmojiSection::Symbols, "Symbols"),
];

/// Full emoji reactions picker backed by `tg-text::emoji`, `tg-emoji` sprite atlas, and `tg-lottie`
/// animated reaction previews + bubble toggle bursts!
pub struct ReactionsPickerState {
    /// Target message id currently being reacted to (`None` when closed).
    pub target_msg_id: Option<MsgId>,
    /// Anchor position on screen.
    pub anchor_pos: Pos2,
    /// Active emoji section tab.
    pub active_section: EmojiSection,
    /// Search filter query.
    pub search_query: String,
    /// Sprite atlas texture cache (`tg-emoji`).
    emoji_cache: EmojiTextureCache,
    /// Compiled `tg-lottie` reaction compositions keyed by emoji string.
    lottie_comps: HashMap<String, Option<Composition>>,
    /// Rasterized Lottie frames keyed by `(emoji, step)`.
    lottie_frames: HashMap<(String, u32), TextureHandle>,
    /// Active toggle burst animations: `msg_id -> (emoji, started_at_secs)`.
    pub active_bursts: HashMap<MsgId, (String, f64)>,
}

impl fmt::Debug for ReactionsPickerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReactionsPickerState")
            .field("target_msg_id", &self.target_msg_id)
            .field("active_section", &self.active_section)
            .field("active_bursts", &self.active_bursts.len())
            .finish()
    }
}

impl Default for ReactionsPickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReactionsPickerState {
    /// Creates a closed reactions picker with `tg-emoji` and `tg-lottie` caches.
    #[must_use]
    pub fn new() -> Self {
        Self {
            target_msg_id: None,
            anchor_pos: Pos2::ZERO,
            active_section: EmojiSection::Recent,
            search_query: String::new(),
            emoji_cache: EmojiTextureCache::new(),
            lottie_comps: HashMap::new(),
            lottie_frames: HashMap::new(),
            active_bursts: HashMap::new(),
        }
    }

    /// Opens the full reactions picker anchored at `pos` for `msg_id`.
    pub fn open(&mut self, msg_id: MsgId, pos: Pos2) {
        self.target_msg_id = Some(msg_id);
        self.anchor_pos = pos;
        self.search_query.clear();
    }

    /// Closes the picker.
    pub fn close(&mut self) {
        self.target_msg_id = None;
    }

    /// Starts a short `tg-lottie` burst animation for `emoji` on `msg_id`.
    pub fn trigger_burst(&mut self, msg_id: MsgId, emoji: &str, now_secs: f64) {
        self.active_bursts
            .insert(msg_id, (emoji.to_owned(), now_secs));
    }

    /// Collects the emoji strings for the current tab or search query (backed by full `tg_text::emoji`).
    #[must_use]
    pub fn current_emoji_list(&self) -> Vec<String> {
        let q = self.search_query.trim();
        if !q.is_empty() {
            let suggs = emoji::get_suggestions(q);
            if !suggs.is_empty() {
                return suggs.into_iter().map(|s| s.emoji.to_owned()).collect();
            }
            // Fallback substring scan across sections.
            return DEFAULT_RECENT_EMOJI
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        }
        match self.active_section {
            EmojiSection::Recent => DEFAULT_RECENT_EMOJI
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            sec => emoji::section(sec)
                .into_iter()
                .map(|e| e.text().into_owned())
                .collect(),
        }
    }

    /// Renders a quantized `tg-lottie` animation frame for `emoji` at `time_secs`.
    pub fn lottie_reaction_frame(
        &mut self,
        ctx: &egui::Context,
        emoji_str: &str,
        time_secs: f64,
    ) -> Option<TextureHandle> {
        self.lottie_comps
            .entry(emoji_str.to_owned())
            .or_insert_with(|| {
                let json = animated_reaction_lottie_json(emoji_str);
                Composition::from_json(&json).ok()
            });
        let comp = self.lottie_comps.get(emoji_str)?.as_ref()?;
        let span = (comp.out_point - comp.in_point).max(1.0);
        let steps = 15u32;
        let frac = ((time_secs * 1.2) % 1.0).clamp(0.0, 0.999);
        let step = (frac * f64::from(steps)).floor() as u32 % steps;
        let key = (emoji_str.to_owned(), step);
        if let Some(tex) = self.lottie_frames.get(&key) {
            return Some(tex.clone());
        }
        let sample = comp.in_point + (f64::from(step) / f64::from(steps)) * span;
        let fb = render_composition(comp, sample, LottieSize::new(64, 64));
        let img =
            ColorImage::from_rgba_premultiplied([fb.width as usize, fb.height as usize], &fb.data);
        let tex = ctx.load_texture(
            format!("tg_react_lottie_{emoji_str}_{step}"),
            img,
            TextureOptions::LINEAR,
        );
        self.lottie_frames.insert(key, tex.clone());
        Some(tex)
    }

    /// Paints any active reaction toggle bursts (`tg-lottie`) over the list area.
    pub fn paint_active_bursts(&mut self, ui: &mut Ui, list_rect: Rect, now_secs: f64) {
        let expired: Vec<MsgId> = self
            .active_bursts
            .iter()
            .filter(|(_, (_, start))| now_secs - *start > 1.2)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.active_bursts.remove(&id);
        }
        if self.active_bursts.is_empty() {
            return;
        }
        let entries: Vec<(MsgId, String, f64)> = self
            .active_bursts
            .iter()
            .map(|(id, (em, st))| (*id, em.clone(), *st))
            .collect();
        let ctx = ui.ctx().clone();
        for (idx, (_, em, start)) in entries.into_iter().enumerate() {
            let t = (now_secs - start).clamp(0.0, 1.2);
            let center = Pos2::new(
                list_rect.center().x + (idx as f32) * 32.0,
                list_rect.bottom() - 72.0 - (t as f32) * 28.0,
            );
            let burst_rect = Rect::from_center_size(center, Vec2::splat(52.0));
            if let Some(tex) = self.lottie_reaction_frame(&ctx, &em, t) {
                ui.painter().image(
                    tex.id(),
                    burst_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            let emoji_rect = Rect::from_center_size(center, Vec2::splat(26.0));
            self.emoji_cache
                .paint_emoji_str(ui.painter(), &ctx, &em, emoji_rect, Color32::WHITE);
        }
        ui.ctx().request_repaint();
    }

    /// Paints the full emoji reactions picker popup when open.
    /// Returns `Some((msg_id, chosen_emoji))` when the user picks a reaction.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        now_secs: f64,
        colors: &HistoryColors,
    ) -> Option<(MsgId, String)> {
        let target_id = self.target_msg_id?;
        let mut selected = None;
        let mut close_now = false;
        let ctx = ui.ctx().clone();

        let picker_size = Vec2::new(340.0, 310.0);
        let screen = ui.ctx().viewport_rect();
        let pos = Pos2::new(
            self.anchor_pos
                .x
                .min(screen.right() - picker_size.x - 10.0)
                .max(10.0),
            self.anchor_pos
                .y
                .min(screen.bottom() - picker_size.y - 10.0)
                .max(10.0),
        );

        let emojis = self.current_emoji_list();

        egui::Area::new(Id::unique("tg_reactions_picker_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_size(picker_size);
                        ui.set_max_size(picker_size);

                        // Header + Quick Reactions bar
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Reactions")
                                    .strong()
                                    .size(13.0)
                                    .color(colors.bold_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").clicked() {
                                        close_now = true;
                                    }
                                },
                            );
                        });

                        // Quick reactions strip with `tg-lottie` hover effect!
                        ui.horizontal(|ui| {
                            for &q_em in QUICK_REACTIONS {
                                let (cell, resp) =
                                    ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    if let Some(tex) =
                                        self.lottie_reaction_frame(&ctx, q_em, now_secs)
                                    {
                                        ui.painter().image(
                                            tex.id(),
                                            cell.expand(4.0),
                                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                            Color32::WHITE,
                                        );
                                        ui.ctx().request_repaint();
                                    }
                                }
                                self.emoji_cache.paint_emoji_str(
                                    ui.painter(),
                                    &ctx,
                                    q_em,
                                    cell.shrink(4.0),
                                    Color32::WHITE,
                                );
                                if resp.clicked() {
                                    selected = Some((target_id, q_em.to_owned()));
                                    close_now = true;
                                }
                            }
                        });

                        ui.add_space(4.0);
                        ui.add(
                            TextEdit::singleline(&mut self.search_query)
                                .desired_width(f32::INFINITY)
                                .hint_text("Search all emoji reactions..."),
                        );
                        ui.add_space(4.0);

                        // Category tabs
                        ui.horizontal_wrapped(|ui| {
                            for &(sec, label) in REACTION_EMOJI_SECTIONS {
                                let active = self.active_section == sec;
                                if ui
                                    .selectable_label(active, egui::RichText::new(label).size(11.0))
                                    .clicked()
                                {
                                    self.active_section = sec;
                                }
                            }
                        });
                        ui.separator();

                        // Full scrollable emoji grid (8 columns)
                        ScrollArea::vertical()
                            .id_salt("tg_reactions_emoji_grid")
                            .max_height(168.0)
                            .show(ui, |ui| {
                                let cols = 8usize;
                                let cell_sz = 36.0;
                                for chunk in emojis.chunks(cols) {
                                    ui.horizontal(|ui| {
                                        for em in chunk {
                                            let (cell, resp) = ui.allocate_exact_size(
                                                Vec2::splat(cell_sz),
                                                Sense::click(),
                                            );
                                            if resp.hovered() {
                                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                                ui.painter().circle_filled(
                                                    cell.center(),
                                                    17.0,
                                                    colors.bar_bg_over,
                                                );
                                                if let Some(tex) =
                                                    self.lottie_reaction_frame(&ctx, em, now_secs)
                                                {
                                                    ui.painter().image(
                                                        tex.id(),
                                                        cell.expand(2.0),
                                                        Rect::from_min_max(
                                                            Pos2::ZERO,
                                                            Pos2::new(1.0, 1.0),
                                                        ),
                                                        Color32::WHITE,
                                                    );
                                                    ui.ctx().request_repaint();
                                                }
                                            }
                                            self.emoji_cache.paint_emoji_str(
                                                ui.painter(),
                                                &ctx,
                                                em,
                                                cell.shrink(5.0),
                                                Color32::WHITE,
                                            );
                                            if resp.clicked() {
                                                selected = Some((target_id, em.clone()));
                                                close_now = true;
                                            }
                                        }
                                    });
                                }
                            });
                    });
            });

        if close_now || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.target_msg_id = None;
        }
        if let Some((mid, ref em)) = selected {
            self.trigger_burst(mid, em, now_secs);
        }
        selected
    }

    /// Paints the reactions picker backed by the server's `messages.getAvailableReactions` catalog,
    /// highlighting `default_reaction` and allowing right-click to set a new default quick reaction.
    pub fn show_with_catalog(
        &mut self,
        ui: &mut Ui,
        now_secs: f64,
        available: &[AvailableReactionItem],
        default_reaction: &str,
        colors: &HistoryColors,
    ) -> Option<ReactionsPickerAction> {
        let target_id = self.target_msg_id?;
        let mut action = None;
        let mut close_now = false;
        let ctx = ui.ctx().clone();

        let picker_size = Vec2::new(340.0, 322.0);
        let screen = ui.ctx().viewport_rect();
        let pos = Pos2::new(
            self.anchor_pos
                .x
                .min(screen.right() - picker_size.x - 10.0)
                .max(10.0),
            self.anchor_pos
                .y
                .min(screen.bottom() - picker_size.y - 10.0)
                .max(10.0),
        );

        let quick_strip: Vec<String> = if available.is_empty() {
            QUICK_REACTIONS.iter().map(|s| (*s).to_owned()).collect()
        } else {
            let mut list: Vec<String> = available
                .iter()
                .filter(|r| !r.inactive)
                .take(8)
                .map(|r| r.emoji.clone())
                .collect();
            if !default_reaction.is_empty() && !list.iter().any(|e| e == default_reaction) {
                list.insert(0, default_reaction.to_owned());
                list.truncate(8);
            }
            list
        };
        let emojis = self.current_emoji_list();

        egui::Area::new(Id::unique("tg_reactions_picker_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_size(picker_size);
                        ui.set_max_size(picker_size);

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Reactions (Quick: {default_reaction} · right-click to set default)"
                                ))
                                .size(11.5)
                                .color(colors.bold_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").clicked() {
                                        close_now = true;
                                    }
                                },
                            );
                        });

                        ui.horizontal(|ui| {
                            for q_em in &quick_strip {
                                let (cell, resp) =
                                    ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                                if q_em == default_reaction {
                                    ui.painter().circle_stroke(
                                        cell.center(),
                                        16.0,
                                        Stroke::new(1.4, colors.accent),
                                    );
                                }
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    if let Some(tex) =
                                        self.lottie_reaction_frame(&ctx, q_em, now_secs)
                                    {
                                        ui.painter().image(
                                            tex.id(),
                                            cell.expand(4.0),
                                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                            Color32::WHITE,
                                        );
                                        ui.ctx().request_repaint();
                                    }
                                }
                                self.emoji_cache.paint_emoji_str(
                                    ui.painter(),
                                    &ctx,
                                    q_em,
                                    cell.shrink(4.0),
                                    Color32::WHITE,
                                );
                                if resp.secondary_clicked() {
                                    action = Some(ReactionsPickerAction::SetDefault(q_em.clone()));
                                } else if resp.clicked() {
                                    action =
                                        Some(ReactionsPickerAction::Pick(target_id, q_em.clone()));
                                    close_now = true;
                                }
                            }
                        });

                        ui.add_space(4.0);
                        ui.add(
                            TextEdit::singleline(&mut self.search_query)
                                .desired_width(f32::INFINITY)
                                .hint_text("Search all emoji reactions..."),
                        );
                        ui.add_space(4.0);

                        ui.horizontal_wrapped(|ui| {
                            for &(sec, label) in REACTION_EMOJI_SECTIONS {
                                let active = self.active_section == sec;
                                if ui
                                    .selectable_label(active, egui::RichText::new(label).size(11.0))
                                    .clicked()
                                {
                                    self.active_section = sec;
                                }
                            }
                        });
                        ui.separator();

                        ScrollArea::vertical()
                            .id_salt("tg_reactions_emoji_grid")
                            .max_height(168.0)
                            .show(ui, |ui| {
                                let cols = 8usize;
                                let cell_sz = 36.0;
                                for chunk in emojis.chunks(cols) {
                                    ui.horizontal(|ui| {
                                        for em in chunk {
                                            let (cell, resp) = ui.allocate_exact_size(
                                                Vec2::splat(cell_sz),
                                                Sense::click(),
                                            );
                                            if resp.hovered() {
                                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                                ui.painter().circle_filled(
                                                    cell.center(),
                                                    17.0,
                                                    colors.bar_bg_over,
                                                );
                                                if let Some(tex) =
                                                    self.lottie_reaction_frame(&ctx, em, now_secs)
                                                {
                                                    ui.painter().image(
                                                        tex.id(),
                                                        cell.expand(2.0),
                                                        Rect::from_min_max(
                                                            Pos2::ZERO,
                                                            Pos2::new(1.0, 1.0),
                                                        ),
                                                        Color32::WHITE,
                                                    );
                                                    ui.ctx().request_repaint();
                                                }
                                            }
                                            self.emoji_cache.paint_emoji_str(
                                                ui.painter(),
                                                &ctx,
                                                em,
                                                cell.shrink(5.0),
                                                Color32::WHITE,
                                            );
                                            if resp.secondary_clicked() {
                                                action = Some(ReactionsPickerAction::SetDefault(
                                                    em.clone(),
                                                ));
                                            } else if resp.clicked() {
                                                action = Some(ReactionsPickerAction::Pick(
                                                    target_id,
                                                    em.clone(),
                                                ));
                                                close_now = true;
                                            }
                                        }
                                    });
                                }
                            });
                    });
            });

        if close_now || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.target_msg_id = None;
        }
        if let Some(ReactionsPickerAction::Pick(mid, ref em)) = action {
            self.trigger_burst(mid, em, now_secs);
        }
        action
    }
}

/// Action returned by [`ReactionsPickerState::show_with_catalog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactionsPickerAction {
    /// Toggle a reaction on `MsgId`.
    Pick(MsgId, String),
    /// Update the user's default quick-reaction emoji (`messages.setDefaultReaction`).
    SetDefault(String),
}

// ---------------------------------------------------------------------------
// 8. Whole-Chat Translate Bar (`history_view_translate_bar.cpp`)
// ---------------------------------------------------------------------------

/// Height of the whole-chat translation bar docked under the top/pinned bars (`40px`).
pub const TRANSLATE_BAR_HEIGHT: f32 = 40.0;

/// Common target/excluded translation languages `(iso_code, display_name)`.
pub const SUPPORTED_TRANSLATION_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ar", "Arabic"),
    ("he", "Hebrew"),
    ("el", "Greek"),
    ("pt", "Portuguese"),
    ("it", "Italian"),
];

/// Returns the human-readable language name for an ISO 639-1 code.
#[must_use]
pub fn translation_language_name(code: &str) -> &str {
    let lower = code.trim();
    SUPPORTED_TRANSLATION_LANGUAGES
        .iter()
        .find(|&&(c, _)| c.eq_ignore_ascii_case(lower))
        .map_or(lower, |&(_, name)| name)
}

/// Lightweight script-based chat language detector used to decide when to show the whole-chat
/// Translate Bar (`history_view_translate_bar.cpp`).
///
/// Returns `Some(iso_code)` when a clear non-English script or language marker is detected across
/// recent messages, or `Some("en")` for Latin/ASCII chats.
#[must_use]
pub fn detect_chat_language(messages: &[HistoryMessage]) -> Option<&'static str> {
    let mut cyrillic = 0usize;
    let mut greek = 0usize;
    let mut arabic = 0usize;
    let mut hebrew = 0usize;
    let mut hangul = 0usize;
    let mut kana = 0usize;
    let mut cjk = 0usize;
    let mut latin = 0usize;

    for msg in messages.iter().rev().take(20) {
        if msg.is_service() || msg.text.text.trim().is_empty() {
            continue;
        }
        for ch in msg.text.text.chars() {
            let cp = ch as u32;
            match cp {
                0x0400..=0x04FF => cyrillic += 1,
                0x0370..=0x03FF => greek += 1,
                0x0600..=0x06FF => arabic += 1,
                0x0590..=0x05FF => hebrew += 1,
                0xAC00..=0xD7AF | 0x1100..=0x11FF => hangul += 1,
                0x3040..=0x30FF => kana += 1,
                0x4E00..=0x9FFF => cjk += 1,
                _ if ch.is_ascii_alphabetic() => latin += 1,
                _ => {}
            }
        }
    }

    let total = cyrillic + greek + arabic + hebrew + hangul + kana + cjk + latin;
    if total < 4 {
        return None;
    }
    let non_latin = [
        (cyrillic, "ru"),
        (greek, "el"),
        (arabic, "ar"),
        (hebrew, "he"),
        (hangul, "ko"),
        (kana, "ja"),
        (cjk, "zh"),
    ];
    if let Some(&(count, code)) = non_latin.iter().max_by_key(|&&(c, _)| c)
        && count * 3 >= total
    {
        return Some(code);
    }
    Some("en")
}

/// Actions emitted by [`TranslateBarState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateBarAction {
    /// Translate all loaded messages in this chat to `target_lang`.
    TranslateChat(String),
    /// Restore original text on all translated messages in this chat.
    ShowOriginal,
    /// Hide the translate bar for this peer (`messages.togglePeerTranslations(disabled = true)`).
    HideForPeer,
    /// Add `lang_code` to the user's "Do not translate" list.
    AddDoNotTranslate(String),
    /// Remove `lang_code` from the user's "Do not translate" list.
    RemoveDoNotTranslate(String),
    /// Change target language and re-translate if currently translated.
    ChangeTargetLanguage(String),
    /// Open the "About Cocoon" confidential-compute dialog (`history_view_translate_bar.cpp:498`).
    AboutCocoon,
}

/// Stateful whole-chat translation bar controller (`history_view_translate_bar.cpp`).
#[derive(Debug, Clone, Default)]
pub struct TranslateBarState {
    /// Active peer id tracked by this bar.
    pub peer_id: PeerId,
    /// True while whole-chat translation is toggled ON for `peer_id`.
    pub translated: bool,
    /// True if the user explicitly opened the bar (e.g. from a menu) even for matching language.
    pub manually_shown: bool,
    /// True while the "Do not translate / Language" options popup is open.
    pub menu_open: bool,
}

impl TranslateBarState {
    /// Resets per-peer bar state when switching chats.
    pub fn reset_for_peer(&mut self, peer_id: PeerId) {
        if self.peer_id != peer_id {
            self.peer_id = peer_id;
            self.translated = false;
            self.manually_shown = false;
            self.menu_open = false;
        }
    }

    /// Returns `true` when the translate bar should be rendered for the current chat.
    #[must_use]
    pub fn should_show(
        &self,
        detected_lang: Option<&str>,
        peer_disabled: bool,
        do_not_translate: &[String],
    ) -> bool {
        if peer_disabled {
            return false;
        }
        if self.translated || self.manually_shown {
            return true;
        }
        let Some(lang) = detected_lang else {
            return false;
        };
        !do_not_translate
            .iter()
            .any(|skip| skip.eq_ignore_ascii_case(lang))
    }

    /// Paints the whole-chat Translate Bar inside `rect`, returning any triggered [`TranslateBarAction`].
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        detected_lang: Option<&str>,
        target_lang: &str,
        do_not_translate: &[String],
        colors: &HistoryColors,
    ) -> Option<TranslateBarAction> {
        let mut action = None;

        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

        let close_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 20.0, rect.center().y),
            Vec2::splat(24.0),
        );
        let menu_rect = Rect::from_center_size(
            Pos2::new(close_rect.left() - 22.0, rect.center().y),
            Vec2::new(26.0, 24.0),
        );
        let main_rect = Rect::from_min_max(
            Pos2::new(rect.left() + 10.0, rect.top() + 4.0),
            Pos2::new(
                (menu_rect.left() - 8.0).max(rect.left() + 80.0),
                rect.bottom() - 4.0,
            ),
        );

        let target_name = translation_language_name(target_lang);
        let label = if self.translated {
            format!("Translated to {target_name}  ·  Show Original")
        } else if let Some(src) = detected_lang {
            format!(
                "Translate {} to {target_name}",
                translation_language_name(src)
            )
        } else {
            format!("Translate to {target_name}")
        };

        let main_resp = ui.interact(
            main_rect,
            Id::unique("tg_translate_bar_btn"),
            Sense::click(),
        );
        main_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
        if main_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(main_rect, CornerRadius::same(6), colors.bar_bg_over);
        }
        ui.painter().text(
            main_rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.5),
            colors.active_text_fg,
        );
        if main_resp.clicked() {
            if self.translated {
                self.translated = false;
                action = Some(TranslateBarAction::ShowOriginal);
            } else {
                self.translated = true;
                action = Some(TranslateBarAction::TranslateChat(target_lang.to_owned()));
            }
        }

        // Options menu button ("...") for "Do not translate", target language, & About Cocoon.
        let menu_resp = ui.interact(
            menu_rect,
            Id::unique("tg_translate_bar_menu"),
            Sense::click(),
        );
        menu_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Translation options"));
        if menu_resp.hovered() || self.menu_open {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(menu_rect.center(), 12.0, colors.bar_bg_over);
        }
        if menu_resp.has_focus() {
            ui.painter().circle_stroke(
                menu_rect.center(),
                12.0,
                Stroke::new(1.5, colors.active_text_fg),
            );
        }
        // Draw 3 crisp horizontal dots so the trigger never renders as a missing-glyph blank.
        let dot_c = menu_rect.center();
        for dx in [-4.5_f32, 0.0, 4.5] {
            ui.painter()
                .circle_filled(Pos2::new(dot_c.x + dx, dot_c.y), 1.6, colors.sub_text_fg);
        }
        if menu_resp
            .on_hover_text("Translation options & Do not translate")
            .clicked()
        {
            self.menu_open = !self.menu_open;
        }

        // Close / Hide for this chat (`messages.togglePeerTranslations(disabled = true)`)
        let close_resp = ui.interact(
            close_rect,
            Id::unique("tg_translate_bar_close"),
            Sense::click(),
        );
        close_resp.widget_info(|| {
            WidgetInfo::labeled(Role::Button, true, "Hide translate bar for this chat")
        });
        if close_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(close_rect.center(), 12.0, colors.bar_bg_over);
        }
        ui.painter().text(
            close_rect.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(15.0),
            colors.sub_text_fg,
        );
        if close_resp
            .on_hover_text("Hide translate bar for this chat")
            .clicked()
        {
            self.translated = false;
            self.manually_shown = false;
            self.menu_open = false;
            action = Some(TranslateBarAction::HideForPeer);
        }

        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom()),
                Pos2::new(rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, colors.divider),
        );

        // Options popover: "Do not translate <lang>", target language selector, skip-list toggles,
        // and Cocoon attribution entry (`history_view_translate_bar.cpp:487-515`).
        if self.menu_open {
            let screen = ui.ctx().viewport_rect();
            let popup_w = 276.0_f32.min((screen.width() - 16.0).max(220.0));
            let pop_x =
                (menu_rect.right() - popup_w).clamp(8.0, (screen.width() - popup_w - 8.0).max(8.0));
            let pop_pos = Pos2::new(pop_x, rect.bottom() + 4.0);
            let inner_w = (popup_w - 16.0).max(200.0);
            let mut close_menu = false;
            egui::Area::new(Id::unique("tg_translate_options_popup"))
                .order(egui::Order::Foreground)
                .fixed_pos(pop_pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE
                        .fill(colors.bar_bg)
                        .stroke(Stroke::new(1.0, colors.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_min_width(inner_w);
                            ui.set_max_width(inner_w);
                            if let Some(src) = detected_lang {
                                let src_name = translation_language_name(src);
                                let already_skipped =
                                    do_not_translate.iter().any(|l| l.eq_ignore_ascii_case(src));
                                if !already_skipped
                                    && ui
                                        .selectable_label(
                                            false,
                                            format!("Do not translate {src_name}"),
                                        )
                                        .clicked()
                                {
                                    self.translated = false;
                                    self.manually_shown = false;
                                    action =
                                        Some(TranslateBarAction::AddDoNotTranslate(src.to_owned()));
                                    close_menu = true;
                                }
                            }
                            if ui
                                .selectable_label(false, "Hide translate bar for this chat")
                                .clicked()
                            {
                                self.translated = false;
                                self.manually_shown = false;
                                action = Some(TranslateBarAction::HideForPeer);
                                close_menu = true;
                            }
                            ui.separator();
                            ui.label(
                                egui::RichText::new("Translate to:")
                                    .size(11.0)
                                    .color(colors.sub_text_fg),
                            );
                            ui.horizontal_wrapped(|ui| {
                                for &(code, name) in SUPPORTED_TRANSLATION_LANGUAGES.iter().take(8)
                                {
                                    let is_cur = code.eq_ignore_ascii_case(target_lang);
                                    if ui
                                        .selectable_label(
                                            is_cur,
                                            egui::RichText::new(name).size(11.5),
                                        )
                                        .clicked()
                                    {
                                        action = Some(TranslateBarAction::ChangeTargetLanguage(
                                            code.to_owned(),
                                        ));
                                        close_menu = true;
                                    }
                                }
                            });
                            ui.separator();
                            ui.label(
                                egui::RichText::new("Do not translate languages:")
                                    .size(11.0)
                                    .color(colors.sub_text_fg),
                            );
                            ui.horizontal_wrapped(|ui| {
                                let sel_bg = ui.visuals().selection.bg_fill;
                                for &(code, name) in SUPPORTED_TRANSLATION_LANGUAGES.iter().take(8)
                                {
                                    let skipped = do_not_translate
                                        .iter()
                                        .any(|l| l.eq_ignore_ascii_case(code));
                                    let check_painter =
                                        |p: &Painter, c: Pos2, sz: f32, col: Color32| {
                                            paint_select_check(p, c, sz, col);
                                        };
                                    let resp = selectable_icon_chip(
                                        ui,
                                        ("tg_dnt_lang_chip", code),
                                        skipped,
                                        name,
                                        11.0,
                                        colors.text_fg,
                                        sel_bg,
                                        colors.bar_bg_over,
                                        colors.active_text_fg,
                                        skipped.then_some(
                                            &check_painter as &dyn Fn(&Painter, Pos2, f32, Color32),
                                        ),
                                    );
                                    if resp.clicked() {
                                        action = Some(if skipped {
                                            TranslateBarAction::RemoveDoNotTranslate(
                                                code.to_owned(),
                                            )
                                        } else {
                                            TranslateBarAction::AddDoNotTranslate(code.to_owned())
                                        });
                                    }
                                }
                            });

                            // Cocoon attribution footer matching
                            // `Telegram/SourceFiles/history/view/history_view_translate_bar.cpp:487-515`.
                            ui.separator();
                            if paint_translate_cocoon_menu_row(ui, inner_w, colors) {
                                action = Some(TranslateBarAction::AboutCocoon);
                                close_menu = true;
                            }
                        });
                });
            if close_menu || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                self.menu_open = false;
            }
        }

        action
    }
}

/// Renders the clickable "Translations are powered by **Cocoon**. How does it work?" footer row
/// inside the translate bar options menu (`history_view_translate_bar.cpp:487-515`), using the
/// neutral `st::menu_icon_lock` icon instead of Telegram/Cocoon trademark logo artwork.
fn paint_translate_cocoon_menu_row(ui: &mut Ui, inner_w: f32, colors: &HistoryColors) -> bool {
    let link_phrase = tr::lng_translate_cocoon_link();
    let raw_menu = tr::lng_translate_cocoon_menu("", &link_phrase);
    let accessible = raw_menu.replace("**", "").trim().to_owned();
    let icon_col_w = 24.0;
    let pad_x = 6.0;
    let pad_y = 6.0;
    let text_w = (inner_w - icon_col_w - 2.0 * pad_x).max(120.0);

    let mut job = egui::text::LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: text_w,
            ..Default::default()
        },
        ..Default::default()
    };
    let sub_fmt = egui::text::TextFormat {
        font_id: FontId::new(11.5, family_regular()),
        color: colors.sub_text_fg,
        line_height: Some(15.5),
        ..Default::default()
    };
    let ui_palette = colors.ui_colors();
    let bold_fg = tg_ui::stand_in::Colors::color(&ui_palette, "windowBoldFg");
    let bold_fmt = egui::text::TextFormat {
        font_id: FontId::new(11.5, family_semibold()),
        color: bold_fg,
        line_height: Some(15.5),
        ..Default::default()
    };
    let link_fmt = egui::text::TextFormat {
        font_id: FontId::new(11.5, family_semibold()),
        color: colors.active_text_fg,
        line_height: Some(15.5),
        ..Default::default()
    };

    let trimmed = raw_menu.trim();
    let (prefix_part, has_link) = if let Some(pos) = trimmed.find(&link_phrase) {
        (&trimmed[..pos], true)
    } else {
        (trimmed, false)
    };
    let mut rest = prefix_part;
    let mut is_bold = false;
    while let Some(pos) = rest.find("**") {
        let seg = &rest[..pos];
        if !seg.is_empty() {
            job.append(
                seg,
                0.0,
                if is_bold {
                    bold_fmt.clone()
                } else {
                    sub_fmt.clone()
                },
            );
        }
        is_bold = !is_bold;
        rest = &rest[pos + 2..];
    }
    if !rest.is_empty() {
        job.append(rest, 0.0, if is_bold { bold_fmt } else { sub_fmt });
    }
    if has_link {
        job.append(&link_phrase, 0.0, link_fmt);
    }

    let galley = ui.painter().layout_job(job);
    let row_h = (galley.size().y + 2.0 * pad_y).max(34.0);
    let (row_rect, resp) = ui.allocate_exact_size(Vec2::new(inner_w, row_h), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &accessible));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(row_rect, CornerRadius::same(6), colors.bar_bg_over);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            row_rect,
            CornerRadius::same(6),
            Stroke::new(1.5, colors.active_text_fg),
            StrokeKind::Inside,
        );
    }

    let icon_rect = Rect::from_center_size(
        Pos2::new(row_rect.left() + pad_x + 8.0, row_rect.top() + pad_y + 9.0),
        Vec2::splat(16.0),
    );
    paint_style_icon(
        ui.painter(),
        icon_rect,
        st::menu_icon_lock,
        &ColorsResolver(&ui_palette),
        Some(colors.active_text_fg),
    );
    let text_pos = Pos2::new(row_rect.left() + pad_x + icon_col_w, row_rect.top() + pad_y);
    ui.painter().galley(text_pos, galley, colors.sub_text_fg);

    resp.clicked()
}

#[cfg(test)]
mod d3_temp_tests {
    use super::*;

    #[test]
    fn d3_round_video_uses_account_temp_dir_and_cleans_up() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let acc_round_dir = dir.path().join("temp_data").join("round_videos");

        set_active_round_video_temp_dir(Some(acc_round_dir.clone()));
        let mut recorder = RoundVideoRecorderState::default();
        assert_eq!(
            recorder.resolve_output_dir(),
            Some(acc_round_dir.clone()),
            "round video recorder must write under the active account temp folder, not %TEMP%"
        );

        std::fs::create_dir_all(&acc_round_dir).expect("mkdir");
        let captured = acc_round_dir.join("round_1.mp4");
        std::fs::write(&captured, b"fake round mp4").expect("write");
        recorder.last_captured_mp4 = Some(captured.to_string_lossy().into_owned());
        drop(recorder);
        assert!(
            !captured.exists(),
            "dropping RoundVideoRecorderState must clean up captured temporary MP4"
        );

        // Stale files left in the account round_videos dir are removed on startup/logout/exit cleanup.
        let stale = acc_round_dir.join("stale_round.mp4");
        std::fs::write(&stale, b"stale").expect("write stale");
        cleanup_round_video_temp_dir(&acc_round_dir);
        assert!(
            !stale.exists(),
            "cleanup must delete stale round video temp files"
        );

        set_active_round_video_temp_dir(None);

        // SEC3-B1: with no account temp folder there is no output directory at all (older builds
        // fell back to a PID-named folder in `%TEMP%`).
        let recorder = RoundVideoRecorderState::default();
        assert_eq!(
            recorder.resolve_output_dir(),
            None,
            "round video recorder must not fall back to %TEMP%"
        );
        let mut explicit = RoundVideoRecorderState::default();
        explicit.set_output_dir(acc_round_dir.clone());
        assert_eq!(explicit.resolve_output_dir(), Some(acc_round_dir));
    }
}
