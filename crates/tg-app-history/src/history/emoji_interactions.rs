//! Interactive emoji tap animations (`SendMessageEmojiInteraction`,
//! `SendMessageEmojiInteractionSeen`) and chat overlay playback.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/emoji_interactions.{h,cpp}` and
//! `Telegram/SourceFiles/history/view/history_view_emoji_interactions.{h,cpp}`.

use std::collections::HashMap;

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_lottie::{Composition, Size as LottieSize, render_composition};

use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

pub use tg_app_core::core_types::dto::emoji_interactions::*;

#[derive(Debug, Clone)]
struct ActiveOverlayPlay {
    msg_id: MsgId,
    emoticon: String,
    anchor: Pos2,
    outgoing: bool,
    started_ms: u64,
    shift: Vec2,
    variant_index: usize,
    /// Optional server Lottie animation `(document_id, duration_ms)` from `EmojiStickersPack`.
    lottie: Option<(u64, u64)>,
}

#[derive(Debug, Clone)]
struct DelayedOverlayPlay {
    msg_id: MsgId,
    emoticon: String,
    anchor: Pos2,
    outgoing: bool,
    should_have_started_at_ms: u64,
    variant_index: usize,
    lottie: Option<(u64, String)>,
}

/// View-layer overlay animation engine for interactive emoji taps (`HistoryView::EmojiInteractions`).
///
/// Renders server Lottie animations from `EmojiStickersPack` (`InputStickerSetAnimatedEmojiAnimations`)
/// via `tg-lottie` when available, falling back smoothly to the procedural burst effect when the
/// server pack or `.tgs` asset has not finished loading.
#[derive(Default)]
pub struct HistoryViewEmojiInteractions {
    plays: Vec<ActiveOverlayPlay>,
    delayed: Vec<DelayedOverlayPlay>,
    rng_state: u64,
    lottie_comps: HashMap<u64, Option<Composition>>,
    lottie_textures: HashMap<(u64, u32, bool), TextureHandle>,
    lottie_frames_painted: u64,
    fallback_frames_painted: u64,
}

impl std::fmt::Debug for HistoryViewEmojiInteractions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryViewEmojiInteractions")
            .field("plays", &self.plays)
            .field("delayed_len", &self.delayed.len())
            .field("cached_lottie_comps", &self.lottie_comps.len())
            .field("cached_lottie_textures", &self.lottie_textures.len())
            .field("lottie_frames_painted", &self.lottie_frames_painted)
            .field("fallback_frames_painted", &self.fallback_frames_painted)
            .finish()
    }
}

impl HistoryViewEmojiInteractions {
    /// Creates an empty overlay animation manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            plays: Vec::new(),
            delayed: Vec::new(),
            rng_state: 0xD1B5_4A32_D192_ED03,
            lottie_comps: HashMap::new(),
            lottie_textures: HashMap::new(),
            lottie_frames_painted: 0,
            fallback_frames_painted: 0,
        }
    }

    /// Returns the number of currently animating overlays.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.plays.len()
    }

    /// Returns the number of currently active overlays playing a server Lottie animation.
    #[must_use]
    pub fn active_lottie_count(&self) -> usize {
        self.plays.iter().filter(|p| p.lottie.is_some()).count()
    }

    /// Returns the number of delayed overlay plays queued behind the concurrency cap.
    #[must_use]
    pub fn delayed_count(&self) -> usize {
        self.delayed.len()
    }

    /// Total frames painted using a server Lottie animation from `EmojiStickersPack`.
    #[must_use]
    pub const fn lottie_frames_painted(&self) -> u64 {
        self.lottie_frames_painted
    }

    /// Total frames painted using the procedural burst fallback.
    #[must_use]
    pub const fn fallback_frames_painted(&self) -> u64 {
        self.fallback_frames_painted
    }

    fn random_shift(&mut self, emoji_size: f32) -> Vec2 {
        // Matches `GenerateRandomShift`: random offset in [-0.08..0.08] of emoji size.
        let max_shift = emoji_size * 2.0 / 25.0;
        let mut step = || {
            self.rng_state = self
                .rng_state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let frac = ((self.rng_state >> 33) as f32) / (u32::MAX as f32 * 0.5) - 1.0;
            frac * max_shift
        };
        Vec2::new(step(), step())
    }

    fn prepare_lottie(&mut self, lottie: Option<(u64, String)>) -> Option<(u64, u64)> {
        let (doc_id, json) = lottie?;
        let comp = self
            .lottie_comps
            .entry(doc_id)
            .or_insert_with(|| Composition::from_json(&json).ok())
            .as_ref()?;
        let frame_span = (comp.out_point - comp.in_point).max(1.0);
        let fps = comp.frame_rate.clamp(1.0, 60.0);
        let duration_ms = ((frame_span / fps) * 1000.0).round().max(100.0) as u64;
        Some((doc_id, duration_ms))
    }

    /// Starts or queues a procedural fallback overlay animation anchored at `anchor`
    /// (`EmojiInteractions::play`).
    pub fn play(
        &mut self,
        msg_id: MsgId,
        emoticon: String,
        anchor: Pos2,
        outgoing: bool,
        variant_index: usize,
        now_ms: u64,
    ) {
        self.play_with_lottie(
            msg_id,
            emoticon,
            anchor,
            outgoing,
            variant_index,
            None,
            now_ms,
        );
    }

    /// Starts or queues an overlay animation anchored at `anchor`, playing the server Lottie
    /// `(doc_id, lottie_json)` when present and valid, or falling back to the procedural burst.
    #[allow(clippy::too_many_arguments)]
    pub fn play_with_lottie(
        &mut self,
        msg_id: MsgId,
        emoticon: String,
        anchor: Pos2,
        outgoing: bool,
        variant_index: usize,
        lottie: Option<(u64, String)>,
        now_ms: u64,
    ) {
        if self.plays.is_empty() {
            let lottie_meta = self.prepare_lottie(lottie);
            let shift = self.random_shift(100.0);
            self.plays.push(ActiveOverlayPlay {
                msg_id,
                emoticon,
                anchor,
                outgoing,
                started_ms: now_ms,
                shift,
                variant_index,
                lottie: lottie_meta,
            });
        } else {
            self.delayed.push(DelayedOverlayPlay {
                msg_id,
                emoticon,
                anchor,
                outgoing,
                should_have_started_at_ms: now_ms,
                variant_index,
                lottie,
            });
            self.check_delayed(now_ms);
        }
    }

    /// Removes any active or delayed plays bound to `msg_id` (`EmojiInteractions::viewRemoved`).
    pub fn view_removed(&mut self, msg_id: MsgId) {
        self.plays.retain(|p| p.msg_id != msg_id);
        self.delayed.retain(|d| d.msg_id != msg_id);
    }

    /// Clears all active and delayed plays (e.g. on chat switch).
    pub fn clear(&mut self) {
        self.plays.clear();
        self.delayed.clear();
    }

    fn play_duration_ms(play: &ActiveOverlayPlay) -> u64 {
        play.lottie
            .map_or(EMOJI_INTERACTION_PLAY_DURATION_MS, |(_, dur)| dur)
    }

    fn check_delayed(&mut self, now_ms: u64) {
        if self.delayed.is_empty() || self.plays.len() >= EMOJI_INTERACTION_MAX_PLAYS {
            return;
        }
        let mut with_too_little_delay = false;
        let mut with_half_played = false;
        for play in &self.plays {
            let elapsed = now_ms.saturating_sub(play.started_ms);
            if elapsed < EMOJI_INTERACTION_MIN_DELAY_MS {
                with_too_little_delay = true;
                break;
            }
            if elapsed * 2 > Self::play_duration_ms(play) {
                with_half_played = true;
            }
        }
        if with_too_little_delay {
            return;
        }
        if self.plays.len() >= EMOJI_INTERACTION_MAX_PLAYS_SMALL_DELAY && !with_half_played {
            return;
        }
        let valid_idx = self.delayed.iter().position(|d| {
            d.should_have_started_at_ms
                .saturating_add(EMOJI_INTERACTION_MAX_DELAY_MS)
                > now_ms
        });
        let Some(idx) = valid_idx else {
            self.delayed.clear();
            return;
        };
        let next = self.delayed.remove(idx);
        self.delayed.drain(..idx);
        let lottie_meta = self.prepare_lottie(next.lottie);
        let shift = self.random_shift(100.0);
        self.plays.push(ActiveOverlayPlay {
            msg_id: next.msg_id,
            emoticon: next.emoticon,
            anchor: next.anchor,
            outgoing: next.outgoing,
            started_ms: now_ms,
            shift,
            variant_index: next.variant_index,
            lottie: lottie_meta,
        });
    }

    fn lottie_frame_texture(
        &mut self,
        ctx: &egui::Context,
        doc_id: u64,
        elapsed_ms: u64,
        mirror_horizontal: bool,
    ) -> Option<TextureHandle> {
        let comp = self.lottie_comps.get(&doc_id)?.as_ref()?;
        let frame_span = (comp.out_point - comp.in_point).max(1.0);
        let fps = comp.frame_rate.clamp(1.0, 60.0);
        let total_steps = (frame_span.round() as u32).clamp(1, 60);
        let elapsed_frames = ((elapsed_ms as f64 / 1000.0) * fps).min(frame_span - 0.001);
        let step_idx = ((elapsed_frames / frame_span) * f64::from(total_steps))
            .floor()
            .max(0.0) as u32;
        let step_idx = step_idx.min(total_steps.saturating_sub(1));

        let key = (doc_id, step_idx, mirror_horizontal);
        if let Some(tex) = self.lottie_textures.get(&key) {
            return Some(tex.clone());
        }

        let sample_frame =
            comp.in_point + (f64::from(step_idx) / f64::from(total_steps)) * frame_span;
        let render_px = EMOJI_INTERACTION_LOTTIE_RENDER_PX;
        let fb = render_composition(comp, sample_frame, LottieSize::new(render_px, render_px));
        if fb.width == 0 || fb.height == 0 {
            return None;
        }
        let pixels = if mirror_horizontal {
            mirror_rgba_horizontal(&fb.data, fb.width as usize, fb.height as usize)
        } else {
            fb.data
        };
        let img =
            ColorImage::from_rgba_premultiplied([fb.width as usize, fb.height as usize], &pixels);
        let tex = ctx.load_texture(
            format!(
                "tg_emoji_inter_{doc_id}_{step_idx}_{}",
                if mirror_horizontal { "m" } else { "n" }
            ),
            img,
            TextureOptions::LINEAR,
        );
        self.lottie_textures.insert(key, tex.clone());
        Some(tex)
    }

    /// Paints all active emoji interaction overlays inside `clip_rect` and advances playback.
    pub fn paint_overlays(
        &mut self,
        ui: &mut Ui,
        clip_rect: Rect,
        now_ms: u64,
        colors: &HistoryColors,
    ) {
        self.check_delayed(now_ms);
        if self.plays.is_empty() {
            return;
        }
        let ctx = ui.ctx().clone();
        let painter = ui.painter().with_clip_rect(clip_rect);
        let play_count = self.plays.len();
        for idx in 0..play_count {
            let play = self.plays[idx].clone();
            let elapsed = now_ms.saturating_sub(play.started_ms);
            let center = play.anchor + play.shift;

            if let Some((doc_id, _duration_ms)) = play.lottie {
                // Mirror incoming (left-aligned) animations horizontally to match tdesktop
                // `history_view_emoji_interactions.cpp:445-448`.
                let mirror_horizontal = !play.outgoing;
                if let Some(tex) =
                    self.lottie_frame_texture(&ctx, doc_id, elapsed, mirror_horizontal)
                {
                    let size = EMOJI_INTERACTION_EFFECT_SIZE_PX;
                    let shift_x = if play.outgoing {
                        -size * 0.22
                    } else {
                        size * 0.22
                    };
                    let target =
                        Rect::from_center_size(center + Vec2::new(shift_x, 0.0), Vec2::splat(size));
                    painter.image(
                        tex.id(),
                        target,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    self.lottie_frames_painted = self.lottie_frames_painted.saturating_add(1);
                    continue;
                }
            }

            // Procedural burst fallback when the server EmojiStickersPack is not yet loaded.
            let progress =
                (elapsed as f32 / EMOJI_INTERACTION_PLAY_DURATION_MS as f32).clamp(0.0, 1.0);
            let alpha = ((1.0 - progress).powf(0.75) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            let dir = if play.outgoing { -1.0_f32 } else { 1.0_f32 };

            // Expanding radial burst particles (8 sparks mirrored by outgoing/incoming side).
            let particle_count = 8usize;
            for i in 0..particle_count {
                let angle_base = (i as f32 / particle_count as f32) * std::f32::consts::TAU
                    + (play.variant_index as f32 * 0.45);
                let dist = 16.0 + progress * 92.0;
                let px = center.x
                    + dir * (angle_base.cos().abs() * 0.45 + 0.55) * angle_base.cos() * dist;
                let py = center.y + angle_base.sin() * dist - progress * 18.0;
                let radius = (4.5 * (1.0 - progress * 0.7)).max(1.2);
                let spark_color = match (i + play.variant_index) % 3 {
                    0 => Color32::from_rgba_unmultiplied(
                        colors.accent.r(),
                        colors.accent.g(),
                        colors.accent.b(),
                        alpha,
                    ),
                    1 => Color32::from_rgba_unmultiplied(255, 200, 70, alpha),
                    _ => Color32::from_rgba_unmultiplied(255, 110, 140, alpha),
                };
                painter.circle_filled(Pos2::new(px, py), radius, spark_color);
            }

            // Enlarged pulsing emoji burst glyph.
            let scale_size = 34.0 + (progress * std::f32::consts::PI).sin() * 26.0;
            painter.text(
                center + Vec2::new(dir * progress * 24.0, -progress * 26.0),
                Align2::CENTER_CENTER,
                &play.emoticon,
                FontId::proportional(scale_size),
                Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
            );
            self.fallback_frames_painted = self.fallback_frames_painted.saturating_add(1);
        }

        self.plays
            .retain(|p| now_ms.saturating_sub(p.started_ms) < Self::play_duration_ms(p));
        self.check_delayed(now_ms);
        if !self.plays.is_empty() || !self.delayed.is_empty() {
            ui.ctx().request_repaint();
        }
    }
}

fn mirror_rgba_horizontal(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    let row_bytes = width * 4;
    if src.len() < row_bytes * height {
        return src.to_vec();
    }
    let mut dst = vec![0u8; row_bytes * height];
    for y in 0..height {
        let row_off = y * row_bytes;
        for x in 0..width {
            let src_off = row_off + x * 4;
            let dst_off = row_off + (width - 1 - x) * 4;
            dst[dst_off..dst_off + 4].copy_from_slice(&src[src_off..src_off + 4]);
        }
    }
    dst
}
