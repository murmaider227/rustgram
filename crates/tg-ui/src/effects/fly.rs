//! Reaction fly, emoji fly, and message-sending text flight animations.
//!
//! Port of:
//! - `Telegram/SourceFiles/ui/effects/reaction_fly_animation.h` / `.cpp`
//! - `Telegram/SourceFiles/ui/effects/emoji_fly_animation.h` / `.cpp`
//! - `Telegram/SourceFiles/ui/effects/message_sending_text_flight.h` / `.cpp`
//!
//! Every effect respects both the global [`crate::anim::disabled`] state and
//! per-category power-saving flags (`EMOJI_REACTIONS`, `EMOJI_CHAT`, `CHAT_EFFECTS`,
//! `ANIMATIONS`).

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Vec2};

use crate::anim::{
    disabled as anim_disabled, ease_out_quint, interpolate_f32, interpolate_f64, slow_multiplier,
    with_alpha,
};

/// Main parabolic flight duration for a reaction (`kFlyDuration` = 300ms).
pub const REACTION_FLY_DURATION_MS: u32 = 300;

/// Number of celebratory mini emoji copies bursting around a reaction (`kMiniCopies` = 7).
pub const MINI_COPIES_COUNT: usize = 7;

/// Minimum mini-copy burst duration in milliseconds (`kMiniCopiesDurationMin` = 700ms).
pub const MINI_COPIES_DURATION_MIN_MS: u32 = 700;

/// Maximum mini-copy burst duration in milliseconds (`kMiniCopiesDurationMax` = 1400ms).
pub const MINI_COPIES_DURATION_MAX_MS: u32 = 1400;

/// Mini-copy scale-in ramp duration in milliseconds (`kMiniCopiesScaleInDuration` = 200ms).
pub const MINI_COPIES_SCALE_IN_MS: u32 = 200;

/// Mini-copy scale-out ramp duration in milliseconds (`kMiniCopiesScaleOutDuration` = 200ms).
pub const MINI_COPIES_SCALE_OUT_MS: u32 = 200;

/// Minimum peak scale for a mini copy (`kMiniCopiesMaxScaleMin` = 0.6).
pub const MINI_COPIES_MAX_SCALE_MIN: f64 = 0.6;

/// Maximum peak scale for a mini copy (`kMiniCopiesMaxScaleMax` = 0.9).
pub const MINI_COPIES_MAX_SCALE_MAX: f64 = 0.9;

/// Default upward arc apex offset in logical pixels (`st::reactionFlyUp`).
pub const DEFAULT_REACTION_FLY_UP: f64 = 32.0;

/// Total message-sending text flight duration in milliseconds (`kDuration` = 280ms).
pub const TEXT_FLIGHT_DURATION_MS: u32 = 280;

/// Horizontal slide duration in milliseconds (`kSlideDuration` = 160ms).
pub const TEXT_FLIGHT_SLIDE_DURATION_MS: u32 = 160;

/// Cross-fade duration in milliseconds (`kFadeDuration` = 110ms).
pub const TEXT_FLIGHT_FADE_DURATION_MS: u32 = 110;

/// Maximum lifetime before an unstarted/stalled text flight gives up (`kGiveUpTimeout` = 1000ms).
pub const TEXT_FLIGHT_GIVE_UP_MS: u32 = 1000;

/// Power-saving / animation gate for fly and send-flight effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlyEffectPolicy {
    /// Whether reaction fly animations are enabled (`!PowerSaving::kEmojiReactions`).
    pub reaction_fly_enabled: bool,
    /// Whether emoji fly / badge animations are enabled (`!PowerSaving::kEmojiChat`).
    pub emoji_fly_enabled: bool,
    /// Whether message-sending text flight is enabled (`!PowerSaving::kChatEffects`).
    pub text_flight_enabled: bool,
    /// Whether global UI animations are enabled (`!anim::disabled()`).
    pub animations_enabled: bool,
}

impl Default for FlyEffectPolicy {
    fn default() -> Self {
        Self::from_global(true, true, true)
    }
}

impl FlyEffectPolicy {
    /// Builds a policy combining [`crate::anim::disabled`] with per-category power-saving flags.
    #[must_use]
    pub fn from_global(
        reaction_fly_enabled: bool,
        emoji_fly_enabled: bool,
        text_flight_enabled: bool,
    ) -> Self {
        Self {
            reaction_fly_enabled,
            emoji_fly_enabled,
            text_flight_enabled,
            animations_enabled: !anim_disabled(),
        }
    }

    /// Explicitly enabled policy (useful for deterministic unit tests).
    #[must_use]
    pub const fn all_enabled() -> Self {
        Self {
            reaction_fly_enabled: true,
            emoji_fly_enabled: true,
            text_flight_enabled: true,
            animations_enabled: true,
        }
    }

    /// Explicitly disabled policy (power saving active).
    #[must_use]
    pub const fn all_disabled() -> Self {
        Self {
            reaction_fly_enabled: false,
            emoji_fly_enabled: false,
            text_flight_enabled: false,
            animations_enabled: false,
        }
    }

    /// Returns `true` when reaction fly animations are permitted.
    #[must_use]
    pub const fn allows_reaction_fly(self) -> bool {
        self.animations_enabled && self.reaction_fly_enabled
    }

    /// Returns `true` when emoji fly animations are permitted.
    #[must_use]
    pub const fn allows_emoji_fly(self) -> bool {
        self.animations_enabled && self.emoji_fly_enabled
    }

    /// Returns `true` when message-sending text flight is permitted.
    #[must_use]
    pub const fn allows_text_flight(self) -> bool {
        self.animations_enabled && self.text_flight_enabled
    }
}

/// Parabolic arc coefficients `y(t) = a * t^2 + b * t + from` whose apex reaches `top`
/// above `min(from, to)` (`Ui::FlyParabola` in `reaction_fly_animation.h`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlyParabola {
    /// Quadratic coefficient `a`.
    pub a: f64,
    /// Linear coefficient `b`.
    pub b: f64,
}

/// Computes the exact parabolic arc from `from` to `to` peaking `top` pixels above the
/// higher endpoint (`ComputeFlyParabola` in `reaction_fly_animation.cpp`).
#[must_use]
pub fn compute_fly_parabola(from: f64, to: f64, top: f64) -> FlyParabola {
    let y1 = to - from;
    let y0 = y1.min(0.0) - top;
    if y1.abs() < 1e-9 {
        let a = -4.0 * y0;
        let b = -a;
        return FlyParabola { a, b };
    }
    let ratio = y0 / y1;
    let root = (ratio * (ratio - 1.0)).max(0.0).sqrt();
    let t0 = if y1 > 0.0 { ratio + root } else { ratio - root };
    let denom = 1.0 - 2.0 * t0;
    let a = if denom.abs() < 1e-9 { 0.0 } else { y1 / denom };
    let b = y1 - a;
    FlyParabola { a, b }
}

impl FlyParabola {
    /// Evaluates the parabolic Y coordinate at `progress` in `[0.0, 1.0]` starting from `from`.
    #[must_use]
    pub fn eval(self, from: f64, progress: f64) -> f64 {
        let t = progress.clamp(0.0, 1.0);
        self.a * t * t + self.b * t + from
    }
}

/// A celebratory mini-emoji particle emitted after a reaction lands (`MiniCopy`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MiniCopy {
    /// Peak scale factor for this particle.
    pub max_scale: f64,
    /// Normalized duration fraction in `(0.0, 1.0]` relative to [`MINI_COPIES_DURATION_MAX_MS`].
    pub duration_fraction: f64,
    /// Upward arc apex offset in logical pixels.
    pub fly_up: f64,
    /// Horizontal displacement at the end of this particle's life.
    pub final_x: f64,
    /// Vertical displacement at the end of this particle's life.
    pub final_y: f64,
}

impl MiniCopy {
    /// Evaluates `(offset_x, offset_y, scale)` at normalized burst `progress` in `[0.0, 1.0]`.
    /// Returns `None` once `progress >= self.duration_fraction`.
    #[must_use]
    pub fn sample(self, progress: f64) -> Option<(f64, f64, f64)> {
        if progress < 0.0 || progress >= self.duration_fraction || self.duration_fraction <= 0.0 {
            return None;
        }
        let scale_in = f64::from(MINI_COPIES_SCALE_IN_MS) / f64::from(MINI_COPIES_DURATION_MAX_MS);
        let scale_out =
            f64::from(MINI_COPIES_SCALE_OUT_MS) / f64::from(MINI_COPIES_DURATION_MAX_MS);
        let value = (progress / self.duration_fraction).clamp(0.0, 1.0);
        let scale = if progress < scale_in {
            self.max_scale * (progress / scale_in)
        } else if progress <= self.duration_fraction - scale_out {
            self.max_scale
        } else {
            self.max_scale * ((self.duration_fraction - progress) / scale_out).max(0.0)
        };
        let parabola = compute_fly_parabola(0.0, self.final_y, self.fly_up);
        let x = interpolate_f64(0.0, self.final_x, value);
        let y = parabola.eval(0.0, value);
        Some((x, y, scale.max(0.0)))
    }
}

/// Configuration for launching a [`ReactionFlyAnimation`] (`Ui::ReactionFlyAnimationArgs`).
#[derive(Debug, Clone)]
pub struct ReactionFlyArgs {
    /// Emoji glyph or short label representing the reaction.
    pub emoji: String,
    /// Origin rectangle where the reaction was picked.
    pub fly_from: Rect,
    /// Destination rectangle of the reaction pill on the message bubble.
    pub target: Rect,
    /// Upward apex height above the higher endpoint (defaults to [`DEFAULT_REACTION_FLY_UP`]).
    pub fly_up: f64,
    /// Optional mini-copy scale multiplier (`1.0` default; `0.0` disables mini copies).
    pub mini_copy_multiplier: f64,
    /// Optional deterministic RNG seed for mini-copy scatter.
    pub seed: u64,
    /// Optional tint color for the burst halo.
    pub tint: Color32,
}

impl ReactionFlyArgs {
    /// Creates standard reaction fly arguments from `fly_from` to `target`.
    #[must_use]
    pub fn new(emoji: impl Into<String>, fly_from: Rect, target: Rect) -> Self {
        Self {
            emoji: emoji.into(),
            fly_from,
            target,
            fly_up: DEFAULT_REACTION_FLY_UP,
            mini_copy_multiplier: 1.0,
            seed: 0x5EED_CAFE,
            tint: Color32::from_rgb(0x40, 0xA7, 0xE3),
        }
    }
}

/// Current stage of a [`ReactionFlyAnimation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionFlyStage {
    /// Main emoji flying along its parabolic arc (`0..REACTION_FLY_DURATION_MS`).
    Flying,
    /// Settled on the pill while mini-copies burst outward (`0..MINI_COPIES_DURATION_MAX_MS`).
    Bursting,
    /// Completed and ready to be removed.
    Finished,
}

/// Animated parabolic reaction flight + celebratory mini-copy burst (`Ui::ReactionFlyAnimation`).
#[derive(Debug, Clone)]
pub struct ReactionFlyAnimation {
    emoji: String,
    fly_from: Rect,
    target: Rect,
    fly_up: f64,
    tint: Color32,
    started_sec: f64,
    fly_duration_sec: f64,
    burst_duration_sec: f64,
    mini_copies: Vec<MiniCopy>,
    finished: bool,
}

impl ReactionFlyAnimation {
    /// Starts a new reaction fly animation at `now_sec`.
    ///
    /// If `policy.allows_reaction_fly()` is `false`, returns an already-finished animation.
    #[must_use]
    pub fn start(args: ReactionFlyArgs, now_sec: f64, policy: FlyEffectPolicy) -> Self {
        let slow = f64::from(slow_multiplier());
        let fly_duration_sec = (f64::from(REACTION_FLY_DURATION_MS) * slow) / 1000.0;
        let burst_duration_sec = if args.mini_copy_multiplier > 0.0 {
            (f64::from(MINI_COPIES_DURATION_MAX_MS) * slow) / 1000.0
        } else {
            0.0
        };
        let mini_copies = if policy.allows_reaction_fly() && args.mini_copy_multiplier > 0.0 {
            let base_size = f64::from(args.target.width().max(args.target.height()).max(18.0));
            generate_mini_copies(
                base_size + base_size * 0.5,
                args.mini_copy_multiplier,
                args.seed,
            )
        } else {
            Vec::new()
        };

        Self {
            emoji: args.emoji,
            fly_from: args.fly_from,
            target: args.target,
            fly_up: if args.fly_up > 0.0 {
                args.fly_up
            } else {
                DEFAULT_REACTION_FLY_UP
            },
            tint: args.tint,
            started_sec: now_sec,
            fly_duration_sec,
            burst_duration_sec,
            mini_copies,
            finished: !policy.allows_reaction_fly(),
        }
    }

    /// Evaluates the current [`ReactionFlyStage`] at `now_sec`.
    #[must_use]
    pub fn stage_at(&self, now_sec: f64) -> ReactionFlyStage {
        if self.finished {
            return ReactionFlyStage::Finished;
        }
        let elapsed = (now_sec - self.started_sec).max(0.0);
        if elapsed < self.fly_duration_sec {
            ReactionFlyStage::Flying
        } else if !self.mini_copies.is_empty()
            && elapsed < self.fly_duration_sec + self.burst_duration_sec
        {
            ReactionFlyStage::Bursting
        } else {
            ReactionFlyStage::Finished
        }
    }

    /// Returns `true` while still on the parabolic flight leg (`flying()` in C++).
    #[must_use]
    pub fn flying(&self, now_sec: f64) -> bool {
        self.stage_at(now_sec) == ReactionFlyStage::Flying
    }

    /// Normalized flight progress in `[0.0, 1.0]` (`flyingProgress()` in C++).
    #[must_use]
    pub fn flying_progress(&self, now_sec: f64) -> f64 {
        if self.finished || self.fly_duration_sec <= 0.0 {
            return 1.0;
        }
        ((now_sec - self.started_sec).max(0.0) / self.fly_duration_sec).clamp(0.0, 1.0)
    }

    /// Normalized mini-copy burst progress in `[0.0, 1.0]`.
    #[must_use]
    pub fn burst_progress(&self, now_sec: f64) -> f64 {
        if self.finished || self.burst_duration_sec <= 0.0 {
            return 1.0;
        }
        let burst_elapsed = (now_sec - self.started_sec - self.fly_duration_sec).max(0.0);
        (burst_elapsed / self.burst_duration_sec).clamp(0.0, 1.0)
    }

    /// Returns `true` once both the flight and mini-copy burst have completed.
    #[must_use]
    pub fn finished(&self, now_sec: f64) -> bool {
        self.stage_at(now_sec) == ReactionFlyStage::Finished
    }

    /// Computes the interpolated bounding rectangle of the flying reaction at `now_sec`.
    #[must_use]
    pub fn rect_at(&self, now_sec: f64) -> Rect {
        let p = self.flying_progress(now_sec);
        let parabola = compute_fly_parabola(
            f64::from(self.fly_from.top()),
            f64::from(self.target.top()),
            self.fly_up,
        );
        let x = interpolate_f32(self.fly_from.left(), self.target.left(), p as f32);
        let y = parabola.eval(f64::from(self.fly_from.top()), p) as f32;
        let w = interpolate_f32(self.fly_from.width(), self.target.width(), p as f32);
        let h = interpolate_f32(self.fly_from.height(), self.target.height(), p as f32);
        Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
    }

    /// Read-only access to the generated mini copies.
    #[must_use]
    pub fn mini_copies(&self) -> &[MiniCopy] {
        &self.mini_copies
    }

    /// Paints the flying reaction or landing burst onto `painter`.
    /// Returns the painted bounding area (or `Rect::NOTHING` when finished).
    pub fn paint(&mut self, painter: &Painter, now_sec: f64, policy: FlyEffectPolicy) -> Rect {
        if !policy.allows_reaction_fly() {
            self.finished = true;
            return Rect::NOTHING;
        }
        match self.stage_at(now_sec) {
            ReactionFlyStage::Finished => {
                self.finished = true;
                Rect::NOTHING
            }
            ReactionFlyStage::Flying => {
                let rect = self.rect_at(now_sec);
                let font_size = (rect.height() * 0.85).clamp(12.0, 36.0);
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    &self.emoji,
                    FontId::proportional(font_size),
                    Color32::WHITE,
                );
                rect
            }
            ReactionFlyStage::Bursting => {
                let bp = self.burst_progress(now_sec);
                let center = self.target.center();
                let base_size = self.target.height().clamp(14.0, 28.0);

                // Expanding soft ring halo around the landing pill.
                if bp < 0.45 {
                    let ring_t = (bp / 0.45) as f32;
                    let radius = base_size * (0.4 + 0.95 * ring_t);
                    let ring_alpha = f64::from(1.0 - ring_t) * 0.55;
                    painter.circle_stroke(
                        center,
                        radius,
                        egui::Stroke::new(2.0, with_alpha(self.tint, ring_alpha)),
                    );
                }

                // Mini copies bursting outward along individual parabolas.
                let mut bounds = self.target;
                for mini in &self.mini_copies {
                    if let Some((ox, oy, scale)) = mini.sample(bp) {
                        let pos = Pos2::new(center.x + ox as f32, center.y + oy as f32);
                        let sz = (base_size * scale as f32).clamp(6.0, 24.0);
                        let alpha = (scale / mini.max_scale.max(0.01)).clamp(0.0, 1.0);
                        painter.text(
                            pos,
                            Align2::CENTER_CENTER,
                            &self.emoji,
                            FontId::proportional(sz),
                            with_alpha(Color32::WHITE, alpha),
                        );
                        bounds = bounds.union(Rect::from_center_size(pos, Vec2::splat(sz)));
                    }
                }
                bounds
            }
        }
    }
}

/// Emoji fly animation targeting a status badge or peer icon (`Ui::EmojiFlyAnimation`).
#[derive(Debug, Clone)]
pub struct EmojiFlyAnimation {
    inner: ReactionFlyAnimation,
}

impl EmojiFlyAnimation {
    /// Starts an emoji fly animation from `from_rect` to `badge_rect`.
    #[must_use]
    pub fn start(
        emoji: impl Into<String>,
        from_rect: Rect,
        badge_rect: Rect,
        now_sec: f64,
        policy: FlyEffectPolicy,
    ) -> Self {
        let adapted = FlyEffectPolicy {
            reaction_fly_enabled: policy.allows_emoji_fly(),
            emoji_fly_enabled: policy.emoji_fly_enabled,
            text_flight_enabled: policy.text_flight_enabled,
            animations_enabled: policy.animations_enabled,
        };
        let mut args = ReactionFlyArgs::new(emoji, from_rect, badge_rect);
        args.mini_copy_multiplier = 0.75;
        Self {
            inner: ReactionFlyAnimation::start(args, now_sec, adapted),
        }
    }

    /// Returns `true` when the emoji fly animation has finished.
    #[must_use]
    pub fn finished(&self, now_sec: f64) -> bool {
        self.inner.finished(now_sec)
    }

    /// Normalized flight progress in `[0.0, 1.0]`.
    #[must_use]
    pub fn flying_progress(&self, now_sec: f64) -> f64 {
        self.inner.flying_progress(now_sec)
    }

    /// Paints the current badge flight frame onto `painter`.
    pub fn paint(&mut self, painter: &Painter, now_sec: f64, policy: FlyEffectPolicy) -> Rect {
        let adapted = FlyEffectPolicy {
            reaction_fly_enabled: policy.allows_emoji_fly(),
            emoji_fly_enabled: policy.emoji_fly_enabled,
            text_flight_enabled: policy.text_flight_enabled,
            animations_enabled: policy.animations_enabled,
        };
        self.inner.paint(painter, now_sec, adapted)
    }
}

/// Evaluates the CSS-style cubic-bezier easing `(x1, y1, x2, y2)` at `t` in `[0.0, 1.0]`.
#[must_use]
pub fn cubic_bezier_ease(x1: f64, y1: f64, x2: f64, y2: f64, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    // Solve B_x(u) = t via Newton-Raphson with bisection fallback.
    let mut lo = 0.0;
    let mut hi = 1.0;
    let mut u = t;
    for _ in 0..8 {
        let bx = sample_bezier(x1, x2, u);
        let err = bx - t;
        if err.abs() < 1e-6 {
            break;
        }
        if err < 0.0 {
            lo = u;
        } else {
            hi = u;
        }
        let dx = sample_bezier_deriv(x1, x2, u);
        if dx.abs() > 1e-6 {
            let cand = u - err / dx;
            u = if cand > lo && cand < hi {
                cand
            } else {
                0.5 * (lo + hi)
            };
        } else {
            u = 0.5 * (lo + hi);
        }
    }
    sample_bezier(y1, y2, u).clamp(0.0, 1.0)
}

#[inline]
fn sample_bezier(p1: f64, p2: f64, u: f64) -> f64 {
    let inv = 1.0 - u;
    3.0 * inv * inv * u * p1 + 3.0 * inv * u * u * p2 + u * u * u
}

#[inline]
fn sample_bezier_deriv(p1: f64, p2: f64, u: f64) -> f64 {
    let inv = 1.0 - u;
    3.0 * inv * inv * p1 + 6.0 * inv * u * (p2 - p1) + 3.0 * u * u * (1.0 - p2)
}

/// Vertical rise curve for [`MessageSendingTextFlight`] (`RiseProgress` in
/// `message_sending_text_flight.cpp`: `CubicBezier(0.25, 0.1, 0.25, 1.0, progress)`).
#[must_use]
pub fn rise_progress(progress: f64) -> f64 {
    cubic_bezier_ease(0.25, 0.1, 0.25, 1.0, progress)
}

/// Computes the unrevealed vertical height of the destination bubble during text flight
/// (`RevealHeight` in `message_sending_text_flight.cpp`).
#[must_use]
pub fn reveal_height(height: f64, lift: f64, lead: f64, progress: f64) -> f64 {
    let rise = rise_progress(progress);
    if lift <= 0.0 {
        return interpolate_f64(height, 0.0, rise);
    }
    let start = height - lift;
    let follow = lead.clamp(0.0, start.max(0.0));
    let fade = (progress * f64::from(TEXT_FLIGHT_DURATION_MS)
        / f64::from(TEXT_FLIGHT_FADE_DURATION_MS))
    .clamp(0.0, 1.0);
    interpolate_f64(start - follow, 0.0, rise) + interpolate_f64(follow, 0.0, fade)
}

/// Sampled frame geometry & opacity of a [`MessageSendingTextFlight`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextFlightSample {
    /// Current screen-space position of the flying text bubble.
    pub rect: Rect,
    /// Overall normalized progress in `[0.0, 1.0]`.
    pub progress: f64,
    /// Horizontal slide progress (`easeOutQuint` over `160ms`).
    pub slide_progress: f64,
    /// Vertical rise progress (`CubicBezier(0.25, 0.1, 0.25, 1.0)` over `280ms`).
    pub rise_progress: f64,
    /// Bubble backdrop fade-in progress over `110ms`.
    pub fade_progress: f64,
}

/// Message-sending text flight animation (`Ui::MessageSendingTextFlight`).
///
/// Flies newly sent outgoing text smoothly from the compose input field rectangle up
/// into the outgoing message bubble position over `280ms` (`160ms` quintic horizontal
/// slide, `280ms` cubic-bezier vertical rise, `110ms` bubble cross-fade).
#[derive(Debug, Clone)]
pub struct MessageSendingTextFlight {
    /// Outgoing message text snippet being flown.
    pub text: String,
    /// Starting compose-field text rect.
    pub source_rect: Rect,
    /// Destination bubble rect in the history list.
    pub target_rect: Rect,
    /// Outgoing bubble background fill color.
    pub bubble_bg: Color32,
    /// Outgoing message text color.
    pub text_fg: Color32,
    /// Timestamp when the flight started (seconds).
    started_sec: f64,
    /// Effective total flight duration (seconds).
    duration_sec: f64,
    /// Effective horizontal slide duration (seconds).
    slide_duration_sec: f64,
    /// Effective fade duration (seconds).
    fade_duration_sec: f64,
    /// Give-up deadline (seconds).
    give_up_sec: f64,
    /// Whether already completed or aborted.
    finished: bool,
}

impl MessageSendingTextFlight {
    /// Starts a message-sending text flight animation from `source_rect` to `target_rect`.
    ///
    /// Immediately marks finished if `policy.allows_text_flight()` is `false` or `text` is empty.
    #[must_use]
    pub fn start(
        text: impl Into<String>,
        source_rect: Rect,
        target_rect: Rect,
        bubble_bg: Color32,
        text_fg: Color32,
        now_sec: f64,
        policy: FlyEffectPolicy,
    ) -> Self {
        let text = text.into();
        let slow = f64::from(slow_multiplier());
        let duration_sec = (f64::from(TEXT_FLIGHT_DURATION_MS) * slow) / 1000.0;
        let slide_duration_sec = (f64::from(TEXT_FLIGHT_SLIDE_DURATION_MS) * slow) / 1000.0;
        let fade_duration_sec = (f64::from(TEXT_FLIGHT_FADE_DURATION_MS) * slow) / 1000.0;
        let give_up_sec = now_sec + (f64::from(TEXT_FLIGHT_GIVE_UP_MS) * slow) / 1000.0;
        let finished = !policy.allows_text_flight() || text.trim().is_empty();

        Self {
            text,
            source_rect,
            target_rect,
            bubble_bg,
            text_fg,
            started_sec: now_sec,
            duration_sec,
            slide_duration_sec,
            fade_duration_sec,
            give_up_sec,
            finished,
        }
    }

    /// Updates the live target rect (e.g. if the history viewport scrolled mid-flight).
    pub fn set_target_rect(&mut self, target_rect: Rect) {
        self.target_rect = target_rect;
    }

    /// Returns `true` when the flight has completed or timed out past [`TEXT_FLIGHT_GIVE_UP_MS`].
    #[must_use]
    pub fn finished(&self, now_sec: f64) -> bool {
        if self.finished || self.duration_sec <= 0.0 {
            return true;
        }
        now_sec >= self.started_sec + self.duration_sec || now_sec >= self.give_up_sec
    }

    /// Evaluates the interpolated geometry and phase curves at `now_sec`.
    #[must_use]
    pub fn sample(&self, now_sec: f64) -> TextFlightSample {
        if self.finished || self.duration_sec <= 0.0 {
            return TextFlightSample {
                rect: self.target_rect,
                progress: 1.0,
                slide_progress: 1.0,
                rise_progress: 1.0,
                fade_progress: 1.0,
            };
        }
        let elapsed = (now_sec - self.started_sec).max(0.0);
        let progress = (elapsed / self.duration_sec).clamp(0.0, 1.0);
        let slide_raw = (elapsed / self.slide_duration_sec.max(1e-6)).clamp(0.0, 1.0);
        let slide_progress = ease_out_quint(1.0, slide_raw);
        let rise = rise_progress(progress);
        let fade_progress = (elapsed / self.fade_duration_sec.max(1e-6)).clamp(0.0, 1.0);

        let residual_x = self.source_rect.left() - self.target_rect.left();
        let residual_y = self.source_rect.top() - self.target_rect.top();
        let x = self.target_rect.left() + residual_x * (1.0 - slide_progress as f32);
        let y = self.target_rect.top() + residual_y * (1.0 - rise as f32);
        let w = interpolate_f32(
            self.source_rect.width(),
            self.target_rect.width(),
            slide_progress as f32,
        );
        let h = interpolate_f32(
            self.source_rect.height(),
            self.target_rect.height(),
            rise as f32,
        );

        TextFlightSample {
            rect: Rect::from_min_size(Pos2::new(x, y), Vec2::new(w.max(24.0), h.max(20.0))),
            progress,
            slide_progress,
            rise_progress: rise,
            fade_progress,
        }
    }

    /// Paints the flying message text + morphing outgoing bubble onto `painter`.
    pub fn paint(&mut self, painter: &Painter, now_sec: f64, policy: FlyEffectPolicy) -> bool {
        if !policy.allows_text_flight() || self.finished(now_sec) {
            self.finished = true;
            return false;
        }
        let s = self.sample(now_sec);
        let radius = (4.0 + 8.0 * s.fade_progress as f32).round() as u8;
        painter.rect_filled(
            s.rect,
            CornerRadius::same(radius),
            with_alpha(self.bubble_bg, 0.25 + 0.75 * s.fade_progress),
        );
        let preview: String = self.text.chars().take(64).collect();
        painter.text(
            Pos2::new(s.rect.left() + 10.0, s.rect.center().y),
            Align2::LEFT_CENTER,
            preview,
            FontId::proportional(13.5),
            with_alpha(self.text_fg, 0.85 + 0.15 * s.fade_progress),
        );
        true
    }
}

/// Unified container driving active reaction flies, emoji flies, and message-send text flights.
#[derive(Debug, Default)]
pub struct FlyEffectsManager {
    reactions: Vec<ReactionFlyAnimation>,
    emojis: Vec<EmojiFlyAnimation>,
    text_flights: Vec<MessageSendingTextFlight>,
}

impl FlyEffectsManager {
    /// Creates an empty effects manager.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            reactions: Vec::new(),
            emojis: Vec::new(),
            text_flights: Vec::new(),
        }
    }

    /// Launches a reaction fly animation if allowed by `policy`.
    pub fn push_reaction_fly(
        &mut self,
        args: ReactionFlyArgs,
        now_sec: f64,
        policy: FlyEffectPolicy,
    ) -> bool {
        if !policy.allows_reaction_fly() {
            return false;
        }
        let anim = ReactionFlyAnimation::start(args, now_sec, policy);
        if anim.finished(now_sec) {
            return false;
        }
        self.reactions.push(anim);
        true
    }

    /// Launches an emoji fly animation if allowed by `policy`.
    pub fn push_emoji_fly(
        &mut self,
        emoji: impl Into<String>,
        from_rect: Rect,
        badge_rect: Rect,
        now_sec: f64,
        policy: FlyEffectPolicy,
    ) -> bool {
        if !policy.allows_emoji_fly() {
            return false;
        }
        let anim = EmojiFlyAnimation::start(emoji, from_rect, badge_rect, now_sec, policy);
        if anim.finished(now_sec) {
            return false;
        }
        self.emojis.push(anim);
        true
    }

    /// Launches a message-sending text flight animation if allowed by `policy`.
    #[allow(clippy::too_many_arguments)]
    pub fn push_text_flight(
        &mut self,
        text: impl Into<String>,
        source_rect: Rect,
        target_rect: Rect,
        bubble_bg: Color32,
        text_fg: Color32,
        now_sec: f64,
        policy: FlyEffectPolicy,
    ) -> bool {
        if !policy.allows_text_flight() {
            return false;
        }
        let flight = MessageSendingTextFlight::start(
            text,
            source_rect,
            target_rect,
            bubble_bg,
            text_fg,
            now_sec,
            policy,
        );
        if flight.finished(now_sec) {
            return false;
        }
        self.text_flights.push(flight);
        true
    }

    /// Returns `true` if any fly or text-flight effect is currently active at `now_sec`.
    #[must_use]
    pub fn animating(&self, now_sec: f64) -> bool {
        self.reactions.iter().any(|r| !r.finished(now_sec))
            || self.emojis.iter().any(|e| !e.finished(now_sec))
            || self.text_flights.iter().any(|t| !t.finished(now_sec))
    }

    /// Returns total count of currently queued animations.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.reactions.len() + self.emojis.len() + self.text_flights.len()
    }

    /// Immediately clears all active fly and text-flight animations.
    pub fn clear(&mut self) {
        self.reactions.clear();
        self.emojis.clear();
        self.text_flights.clear();
    }

    /// Paints all active fly effects onto `ui` and prunes finished animations.
    pub fn paint(&mut self, ui: &mut egui::Ui, now_sec: f64, policy: FlyEffectPolicy) {
        let painter = ui.painter();
        self.reactions.retain_mut(|r| {
            r.paint(painter, now_sec, policy);
            !r.finished(now_sec)
        });
        self.emojis.retain_mut(|e| {
            e.paint(painter, now_sec, policy);
            !e.finished(now_sec)
        });
        self.text_flights
            .retain_mut(|t| t.paint(painter, now_sec, policy));
        if self.active_count() > 0 {
            ui.ctx().request_repaint();
        }
    }
}

fn generate_mini_copies(size: f64, mini_copy_multiplier: f64, seed: u64) -> Vec<MiniCopy> {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    if state == 0 {
        state = 1;
    }
    let mut next_unit = || -> f64 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 11) as f64) / ((1u64 << 53) as f64)
    };
    let mut copies = Vec::with_capacity(MINI_COPIES_COUNT);
    for _ in 0..MINI_COPIES_COUNT {
        let scale = MINI_COPIES_MAX_SCALE_MIN
            + (MINI_COPIES_MAX_SCALE_MAX - MINI_COPIES_MAX_SCALE_MIN) * next_unit();
        let max_scale = scale * mini_copy_multiplier;
        let duration_ms = f64::from(MINI_COPIES_DURATION_MIN_MS)
            + f64::from(MINI_COPIES_DURATION_MAX_MS - MINI_COPIES_DURATION_MIN_MS) * next_unit();
        let fly_up = (size * 0.25) + (size * 0.65) * next_unit();
        let final_x = -size + (2.0 * size) * next_unit();
        let final_y = (size * 0.75) + (size * 0.25) * next_unit();
        copies.push(MiniCopy {
            max_scale,
            duration_fraction: (duration_ms / f64::from(MINI_COPIES_DURATION_MAX_MS))
                .clamp(0.1, 1.0),
            fly_up,
            final_x,
            final_y,
        });
    }
    copies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fly_parabola_hits_endpoints_and_apex() {
        // Level flight: from=100, to=100, top=40 -> apex at 60 at t=0.5.
        let p_level = compute_fly_parabola(100.0, 100.0, 40.0);
        assert!((p_level.eval(100.0, 0.0) - 100.0).abs() < 1e-6);
        assert!((p_level.eval(100.0, 0.5) - 60.0).abs() < 1e-6);
        assert!((p_level.eval(100.0, 1.0) - 100.0).abs() < 1e-6);

        // Downward flight: from=50, to=150, top=30 -> apex at min(50, 150) - 30 = 20.
        let p_down = compute_fly_parabola(50.0, 150.0, 30.0);
        assert!((p_down.eval(50.0, 0.0) - 50.0).abs() < 1e-6);
        assert!((p_down.eval(50.0, 1.0) - 150.0).abs() < 1e-6);
        let t_apex = -p_down.b / (2.0 * p_down.a);
        assert!(t_apex > 0.0 && t_apex < 1.0);
        assert!((p_down.eval(50.0, t_apex) - 20.0).abs() < 1e-5);

        // Upward flight: from=200, to=80, top=25 -> apex at 55.
        let p_up = compute_fly_parabola(200.0, 80.0, 25.0);
        assert!((p_up.eval(200.0, 0.0) - 200.0).abs() < 1e-6);
        assert!((p_up.eval(200.0, 1.0) - 80.0).abs() < 1e-6);
        let t_apex_up = -p_up.b / (2.0 * p_up.a);
        assert!((p_up.eval(200.0, t_apex_up) - 55.0).abs() < 1e-5);
    }

    #[test]
    fn reaction_fly_stages_timing_and_mini_copies() {
        let from = Rect::from_min_size(Pos2::new(40.0, 200.0), Vec2::splat(28.0));
        let target = Rect::from_min_size(Pos2::new(180.0, 120.0), Vec2::splat(20.0));
        let anim = ReactionFlyAnimation::start(
            ReactionFlyArgs::new("🔥", from, target),
            10.0,
            FlyEffectPolicy::all_enabled(),
        );

        assert_eq!(anim.mini_copies().len(), MINI_COPIES_COUNT);
        assert_eq!(anim.stage_at(10.0), ReactionFlyStage::Flying);
        assert!(anim.flying(10.15));
        assert!((anim.flying_progress(10.15) - 0.5).abs() < 1e-6);

        // Mid-flight arc is above the linear midpoint due to parabolic lift.
        let mid_rect = anim.rect_at(10.15);
        assert!(mid_rect.top() < (from.top() + target.top()) * 0.5);

        // At 10.30s -> flight complete, enters Bursting stage.
        assert_eq!(anim.stage_at(10.35), ReactionFlyStage::Bursting);
        assert!(!anim.flying(10.35));
        assert!(!anim.finished(10.35));

        // After 300ms + 1400ms = 1.70s -> finished.
        assert!(anim.finished(11.75));
        assert_eq!(anim.stage_at(11.75), ReactionFlyStage::Finished);
    }

    #[test]
    fn text_flight_timing_curves_and_reveal_height() {
        assert!((rise_progress(0.0) - 0.0).abs() < 1e-6);
        assert!((rise_progress(1.0) - 1.0).abs() < 1e-6);
        assert!(rise_progress(0.5) > 0.5); // ease-out cubic bezier rises quickly

        assert!((reveal_height(100.0, 0.0, 0.0, 0.0) - 100.0).abs() < 1e-5);
        assert!((reveal_height(100.0, 0.0, 0.0, 1.0) - 0.0).abs() < 1e-5);
        assert!(reveal_height(100.0, 20.0, 15.0, 0.5) < 80.0);

        let src = Rect::from_min_size(Pos2::new(20.0, 500.0), Vec2::new(220.0, 32.0));
        let dst = Rect::from_min_size(Pos2::new(240.0, 420.0), Vec2::new(180.0, 38.0));
        let flight = MessageSendingTextFlight::start(
            "Hello world",
            src,
            dst,
            Color32::from_rgb(0xEF, 0xFD, 0xDE),
            Color32::BLACK,
            5.0,
            FlyEffectPolicy::all_enabled(),
        );

        assert!(!flight.finished(5.0));
        let s0 = flight.sample(5.0);
        assert!((s0.rect.left() - src.left()).abs() < 1e-3);
        assert!((s0.rect.top() - src.top()).abs() < 1e-3);

        // At 160ms horizontal slide reaches target X while vertical rise is still in flight.
        let s_slide = flight.sample(5.16);
        assert!((s_slide.slide_progress - 1.0).abs() < 1e-6);
        assert!((s_slide.rect.left() - dst.left()).abs() < 1e-3);
        assert!(s_slide.rise_progress < 1.0);
        assert!((s_slide.fade_progress - 1.0).abs() < 1e-6);

        // At 280ms flight completes.
        assert!(flight.finished(5.28));
        let s_end = flight.sample(5.28);
        assert!((s_end.rect.top() - dst.top()).abs() < 1e-3);
    }

    #[test]
    fn fly_effects_respect_power_saving_flags() {
        let r1 = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::splat(24.0));
        let r2 = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::splat(24.0));
        let mut mgr = FlyEffectsManager::new();

        // Power saving disabled -> nothing queued.
        assert!(!mgr.push_reaction_fly(
            ReactionFlyArgs::new("👍", r1, r2),
            1.0,
            FlyEffectPolicy::all_disabled(),
        ));
        assert!(!mgr.push_emoji_fly("✨", r1, r2, 1.0, FlyEffectPolicy::all_disabled()));
        assert!(!mgr.push_text_flight(
            "Hi",
            r1,
            r2,
            Color32::WHITE,
            Color32::BLACK,
            1.0,
            FlyEffectPolicy::all_disabled(),
        ));
        assert_eq!(mgr.active_count(), 0);

        // Selective policy: allow emoji fly only, block reaction fly & text flight.
        let emoji_only = FlyEffectPolicy {
            reaction_fly_enabled: false,
            emoji_fly_enabled: true,
            text_flight_enabled: false,
            animations_enabled: true,
        };
        assert!(!mgr.push_reaction_fly(ReactionFlyArgs::new("👍", r1, r2), 1.0, emoji_only));
        assert!(mgr.push_emoji_fly("✨", r1, r2, 1.0, emoji_only));
        assert!(!mgr.push_text_flight(
            "Hi",
            r1,
            r2,
            Color32::WHITE,
            Color32::BLACK,
            1.0,
            emoji_only,
        ));
        assert_eq!(mgr.active_count(), 1);
        assert!(mgr.animating(1.1));
    }
}
