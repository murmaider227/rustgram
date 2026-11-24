//! Thanos delete disintegration particle effect.
//!
//! Port of `Telegram/SourceFiles/ui/effects/thanos_effect.h`,
//! `thanos_effect.cpp`, `thanos_effect_renderer.h`, and `thanos_effect_renderer.cpp`.
//!
//! Disintegrates deleted message bubbles into a deterministic drifting particle cloud
//! whose phase speed ramps smoothly with a quintic smootherstep and fades out with a
//! cubic ease-out curve, strictly respecting global/power-saving animation settings.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Vec2};

use crate::anim::{disabled as anim_disabled, slow_multiplier, with_alpha};

/// Total phase duration before a disintegrating item completes (`kMaxPhaseDuration`).
pub const MAX_PHASE_DURATION: f64 = 6.0;

/// Base phase progression rate in phase-units per second (`kPhaseSpeed`).
pub const PHASE_SPEED: f64 = 1.65;

/// Velocity integration multiplier (`kTimeStepMultiplier`).
pub const TIME_STEP_MULTIPLIER: f64 = 1.65;

/// Phase value at which acceleration begins (`kAccelerationStartPhase`).
pub const ACCELERATION_START_PHASE: f64 = 1.0;

/// Phase span over which speed ramps to [`ACCELERATION_MAX_MULTIPLIER`] (`kAccelerationRampPhase`).
pub const ACCELERATION_RAMP_PHASE: f64 = 2.5;

/// Peak animation speed multiplier (`kAccelerationMaxMultiplier`).
pub const ACCELERATION_MAX_MULTIPLIER: f64 = 2.2;

/// Phase value at which particles begin shrinking/disappearing (`kDisappearStartPhase`).
pub const DISAPPEAR_START_PHASE: f64 = MAX_PHASE_DURATION * 0.15;

/// Phase span over which disappearance progresses (`kDisappearDuration`).
pub const DISAPPEAR_DURATION: f64 = MAX_PHASE_DURATION - DISAPPEAR_START_PHASE;

/// Maximum GPU particle resolution budget (`kMaxParticleCount` in `thanos_effect_renderer.cpp`).
pub const MAX_PARTICLE_COUNT: u32 = 120_000;

/// Software-rasterized egui particle budget per deleted bubble to stay 60 FPS smooth.
pub const MAX_SOFTWARE_PARTICLES: u32 = 512;

/// Maximum per-frame delta time in seconds (~15 FPS cap, `crl::time(66)` in C++).
pub const MAX_FRAME_DT_SECS: f64 = 0.066;

/// Minimum per-frame delta time in seconds (`crl::time(1)` in C++).
pub const MIN_FRAME_DT_SECS: f64 = 0.001;

/// Power-saving / animation gate for Thanos disintegration effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThanosPolicy {
    /// Whether chat effects are enabled in power-saving settings (`!PowerSaving::kChatEffects`).
    pub chat_effects_enabled: bool,
    /// Whether global UI animations are enabled (`!anim::disabled()`).
    pub animations_enabled: bool,
}

impl Default for ThanosPolicy {
    fn default() -> Self {
        Self::from_global(true)
    }
}

impl ThanosPolicy {
    /// Creates a policy combining the global [`crate::anim::disabled`] state with the
    /// per-setting `chat_effects_enabled` flag.
    #[must_use]
    pub fn from_global(chat_effects_enabled: bool) -> Self {
        Self {
            chat_effects_enabled,
            animations_enabled: !anim_disabled(),
        }
    }

    /// Explicitly enabled policy (useful for deterministic tests).
    #[must_use]
    pub const fn enabled() -> Self {
        Self {
            chat_effects_enabled: true,
            animations_enabled: true,
        }
    }

    /// Explicitly disabled policy (e.g. power saving active).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            chat_effects_enabled: false,
            animations_enabled: false,
        }
    }

    /// Returns `true` when the Thanos effect is allowed to play.
    #[must_use]
    pub const fn allows_effect(self) -> bool {
        self.chat_effects_enabled && self.animations_enabled
    }
}

/// Quintic smootherstep acceleration curve (`AnimationSpeedMultiplier` in `thanos_effect_renderer.cpp`).
///
/// Returns `1.0` for `phase <= 1.0`, ramps smoothly via `6t^5 - 15t^4 + 10t^3` across
/// `[1.0, 3.5]`, and plateaus at [`ACCELERATION_MAX_MULTIPLIER`] (`2.2`).
#[must_use]
pub fn animation_speed_multiplier(phase: f64) -> f64 {
    if phase <= ACCELERATION_START_PHASE {
        return 1.0;
    }
    let t = ((phase - ACCELERATION_START_PHASE) / ACCELERATION_RAMP_PHASE).clamp(0.0, 1.0);
    let smooth = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    1.0 + ((ACCELERATION_MAX_MULTIPLIER - 1.0) * smooth)
}

/// Cubic ease-out disappearance factor (`DisappearProgress` in `thanos_effect_renderer.cpp`).
///
/// Returns `0.0` until `phase == DISAPPEAR_START_PHASE` (`0.9`), then rises monotonically
/// to `1.0` at `phase == MAX_PHASE_DURATION` (`6.0`).
#[must_use]
pub fn disappear_progress(phase: f64) -> f64 {
    let t = ((phase - DISAPPEAR_START_PHASE) / DISAPPEAR_DURATION).clamp(0.0, 1.0);
    let one_minus = 1.0 - t;
    1.0 - (one_minus * one_minus * one_minus)
}

/// Computes the `(cols, rows)` particle grid dimensions preserving aspect ratio without
/// exceeding `max_particles` (`createAnimatingItem` in `thanos_effect_renderer.cpp`).
#[must_use]
pub fn compute_particle_grid(width: u32, height: u32, max_particles: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_particles == 0 {
        return (0, 0);
    }
    let total = u64::from(width) * u64::from(height);
    if total <= u64::from(max_particles) {
        return (width, height);
    }
    let aspect_ratio = f64::from(width) / f64::from(height);
    let max_p = f64::from(max_particles);
    let rows = ((max_p / aspect_ratio).sqrt() as u32).max(1);
    let cols = ((max_p / f64::from(rows)) as u32).max(1);
    (cols, rows)
}

/// Single simulated particle belonging to a [`ThanosItem`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThanosParticle {
    /// Initial normalized column coordinate in `[0.0, 1.0]`.
    pub u: f32,
    /// Initial normalized row coordinate in `[0.0, 1.0]`.
    pub v: f32,
    /// Current horizontal displacement in logical pixels.
    pub offset_x: f32,
    /// Current vertical displacement in logical pixels.
    pub offset_y: f32,
    /// Horizontal velocity in logical pixels per second.
    pub vel_x: f32,
    /// Vertical velocity in logical pixels per second.
    pub vel_y: f32,
    /// Delay phase offset before this particle starts drifting (sweeps left-to-right).
    pub delay_phase: f32,
    /// Normalized remaining lifetime in `[0.0, 1.0]`.
    pub lifetime: f32,
    /// Per-particle decay speed multiplier.
    pub decay_rate: f32,
    /// Base particle tint color sampled from the bubble.
    pub color: Color32,
}

/// One disintegrating message bubble rectangle managed by [`ThanosEffect`].
#[derive(Debug, Clone)]
pub struct ThanosItem {
    /// Original screen-space rectangle of the deleted item.
    pub rect: Rect,
    /// Current phase in `[0.0, MAX_PHASE_DURATION]`.
    pub phase: f64,
    /// Particle grid columns.
    pub particle_cols: u32,
    /// Particle grid rows.
    pub particle_rows: u32,
    /// Simulated particles.
    pub particles: Vec<ThanosParticle>,
}

impl ThanosItem {
    /// Creates a new disintegrating item for `rect` using `base_color` and `accent_color`
    /// (plus deterministic `seed`) to populate a software particle grid.
    #[must_use]
    pub fn new(
        rect: Rect,
        base_color: Color32,
        accent_color: Color32,
        seed: u32,
        max_particles: u32,
    ) -> Option<Self> {
        let w = rect.width().round().max(0.0) as u32;
        let h = rect.height().round().max(0.0) as u32;
        if w == 0 || h == 0 {
            return None;
        }
        let (cols, rows) = compute_particle_grid(w, h, max_particles.max(1));
        if cols == 0 || rows == 0 {
            return None;
        }
        let mut rng = seed.wrapping_mul(0x9E37_79B9) ^ 0xA5A5_5A5A;
        if rng == 0 {
            rng = 1;
        }

        let mut particles = Vec::with_capacity((cols * rows) as usize);
        for row in 0..rows {
            let v = (row as f32 + 0.5) / rows as f32;
            for col in 0..cols {
                let u = (col as f32 + 0.5) / cols as f32;
                let r1 = xorshift_unit(&mut rng);
                let r2 = xorshift_unit(&mut rng);
                let r3 = xorshift_unit(&mut rng);
                let r4 = xorshift_unit(&mut rng);

                // Sweep disintegration gently across the bubble with per-particle jitter.
                let delay_phase = (u * 1.15 + r1 * 0.45).clamp(0.0, 2.0);
                // Drift predominantly right & slightly upward like tdesktop's compute shader.
                let vel_x = 18.0 + r2 * 46.0 + u * 24.0;
                let vel_y = -28.0 + (r3 - 0.35) * 42.0;
                let decay_rate = 0.24 + r4 * 0.16;
                let color = if r1 > 0.72 { accent_color } else { base_color };

                particles.push(ThanosParticle {
                    u,
                    v,
                    offset_x: 0.0,
                    offset_y: 0.0,
                    vel_x,
                    vel_y,
                    delay_phase,
                    lifetime: 1.0,
                    decay_rate,
                    color,
                });
            }
        }

        Some(Self {
            rect,
            phase: 0.0,
            particle_cols: cols,
            particle_rows: rows,
            particles,
        })
    }

    /// Normalized completion progress in `[0.0, 1.0]`.
    #[must_use]
    pub fn progress(&self) -> f64 {
        (self.phase / MAX_PHASE_DURATION).clamp(0.0, 1.0)
    }

    /// Returns `true` when the item has reached [`MAX_PHASE_DURATION`].
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.phase >= MAX_PHASE_DURATION
    }

    /// Advances simulation by `dt_secs` (clamped to `[MIN_FRAME_DT_SECS, MAX_FRAME_DT_SECS]`).
    pub fn step(&mut self, dt_secs: f64) {
        if self.is_finished() {
            return;
        }
        let clamped_dt = dt_secs.clamp(MIN_FRAME_DT_SECS, MAX_FRAME_DT_SECS);
        let slow = f64::from(slow_multiplier());
        let anim_dt = (clamped_dt / slow) * animation_speed_multiplier(self.phase);
        self.phase = (self.phase + anim_dt * PHASE_SPEED).min(MAX_PHASE_DURATION);

        let step = (anim_dt * TIME_STEP_MULTIPLIER) as f32;
        let phase_f = self.phase as f32;
        for p in &mut self.particles {
            if phase_f < p.delay_phase || p.lifetime <= 0.0 {
                continue;
            }
            p.offset_x += p.vel_x * step;
            p.offset_y += p.vel_y * step;
            // Gentle upward buoyancy + lateral turbulence.
            p.vel_x += 6.5 * step;
            p.vel_y -= 4.0 * step;
            p.lifetime = (p.lifetime - p.decay_rate * step).max(0.0);
        }
    }

    /// Paints this disintegrating item onto `painter`.
    pub fn paint(&self, painter: &Painter) {
        if self.is_finished() || self.particle_cols == 0 || self.particle_rows == 0 {
            return;
        }
        let inv_disappear = (1.0 - disappear_progress(self.phase)) as f32;
        if inv_disappear <= 0.005 {
            return;
        }
        let cell_w = (self.rect.width() / self.particle_cols as f32).max(1.5);
        let cell_h = (self.rect.height() / self.particle_rows as f32).max(1.5);

        for p in &self.particles {
            if p.lifetime <= 0.01 {
                continue;
            }
            let alpha = f64::from((p.lifetime * inv_disappear).clamp(0.0, 1.0));
            if alpha <= 0.01 {
                continue;
            }
            let scale = (0.35 + 0.65 * inv_disappear * p.lifetime).clamp(0.2, 1.0);
            let cx = self.rect.left() + p.u * self.rect.width() + p.offset_x;
            let cy = self.rect.top() + p.v * self.rect.height() + p.offset_y;
            let sz = Vec2::new((cell_w * scale).max(1.2), (cell_h * scale).max(1.2));
            let quad = Rect::from_center_size(Pos2::new(cx, cy), sz);
            painter.rect_filled(quad, CornerRadius::same(1), with_alpha(p.color, alpha));
        }
    }
}

/// Controller managing active Thanos delete disintegration items (`Ui::ThanosEffect`).
#[derive(Debug, Default)]
pub struct ThanosEffect {
    items: Vec<ThanosItem>,
    last_time_sec: Option<f64>,
    seed_counter: u32,
}

impl ThanosEffect {
    /// Creates an empty Thanos effect controller.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            items: Vec::new(),
            last_time_sec: None,
            seed_counter: 1,
        }
    }

    /// Adds a deleted bubble rect to disintegrate if `policy` permits animations.
    ///
    /// Returns `true` if the item was queued for animation, or `false` if skipped
    /// (empty rect or power-saving / animations disabled).
    pub fn add_item(
        &mut self,
        rect: Rect,
        base_color: Color32,
        accent_color: Color32,
        policy: ThanosPolicy,
    ) -> bool {
        if !policy.allows_effect() {
            return false;
        }
        let seed = self.seed_counter;
        self.seed_counter = self.seed_counter.wrapping_add(1);
        let Some(item) =
            ThanosItem::new(rect, base_color, accent_color, seed, MAX_SOFTWARE_PARTICLES)
        else {
            return false;
        };
        self.items.push(item);
        true
    }

    /// Returns `true` if any disintegration animation is still active.
    #[must_use]
    pub fn animating(&self) -> bool {
        !self.items.is_empty()
    }

    /// Returns the number of currently animating items.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.items.len()
    }

    /// Read-only access to active items.
    #[must_use]
    pub fn items(&self) -> &[ThanosItem] {
        &self.items
    }

    /// Immediately finishes and clears all active disintegration items (`finishNow`).
    pub fn finish_all(&mut self) {
        self.items.clear();
        self.last_time_sec = None;
    }

    /// Advances all active items by `dt_secs`, pruning completed ones.
    ///
    /// If `policy.allows_effect()` is `false`, all active items finish immediately.
    /// Returns `true` if items are still animating after this step.
    pub fn step_dt(&mut self, dt_secs: f64, policy: ThanosPolicy) -> bool {
        if !policy.allows_effect() {
            self.finish_all();
            return false;
        }
        for item in &mut self.items {
            item.step(dt_secs);
        }
        self.items.retain(|item| !item.is_finished());
        if self.items.is_empty() {
            self.last_time_sec = None;
        }
        !self.items.is_empty()
    }

    /// Advances simulation using wall-clock seconds `now_sec`, returning whether
    /// items are still animating.
    pub fn advance_to(&mut self, now_sec: f64, policy: ThanosPolicy) -> bool {
        if !policy.allows_effect() {
            self.finish_all();
            return false;
        }
        if self.items.is_empty() {
            self.last_time_sec = None;
            return false;
        }
        let dt = match self.last_time_sec {
            Some(prev) => (now_sec - prev).max(MIN_FRAME_DT_SECS),
            None => 1.0 / 60.0,
        };
        self.last_time_sec = Some(now_sec);
        self.step_dt(dt, policy)
    }

    /// Steps and paints all active disintegration items onto `ui`'s painter.
    ///
    /// Automatically requests an egui repaint while animating.
    pub fn paint(&mut self, ui: &mut egui::Ui, now_sec: f64, policy: ThanosPolicy) {
        if !self.advance_to(now_sec, policy) {
            return;
        }
        let painter = ui.painter();
        for item in &self.items {
            item.paint(painter);
        }
        if self.animating() {
            ui.ctx().request_repaint();
        }
    }
}

#[inline]
fn xorshift_unit(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x as f32) / (u32::MAX as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_multiplier_and_disappear_curves_match_tdesktop() {
        assert!((animation_speed_multiplier(0.0) - 1.0).abs() < 1e-9);
        assert!((animation_speed_multiplier(1.0) - 1.0).abs() < 1e-9);
        // Midpoint of ramp [1.0, 3.5] is 2.25 where smootherstep(0.5) = 0.5 -> 1.6.
        assert!((animation_speed_multiplier(2.25) - 1.6).abs() < 1e-9);
        assert!((animation_speed_multiplier(3.5) - ACCELERATION_MAX_MULTIPLIER).abs() < 1e-9);
        assert!((animation_speed_multiplier(6.0) - ACCELERATION_MAX_MULTIPLIER).abs() < 1e-9);

        assert!((disappear_progress(0.0) - 0.0).abs() < 1e-9);
        assert!((disappear_progress(DISAPPEAR_START_PHASE) - 0.0).abs() < 1e-9);
        let mid = DISAPPEAR_START_PHASE + DISAPPEAR_DURATION * 0.5;
        // 1 - 0.5^3 = 0.875
        assert!((disappear_progress(mid) - 0.875).abs() < 1e-9);
        assert!((disappear_progress(MAX_PHASE_DURATION) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn particle_grid_preserves_aspect_ratio_within_budget() {
        assert_eq!(compute_particle_grid(20, 10, 500), (20, 10));
        let (cols, rows) = compute_particle_grid(400, 200, MAX_SOFTWARE_PARTICLES);
        assert!(cols > 0 && rows > 0);
        assert!(cols * rows <= MAX_SOFTWARE_PARTICLES);
        assert!(cols > rows);
        assert_eq!(compute_particle_grid(0, 100, 500), (0, 0));
    }

    #[test]
    fn thanos_lifecycle_progresses_and_finishes() {
        let mut effect = ThanosEffect::new();
        let rect = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(200.0, 64.0));
        assert!(effect.add_item(
            rect,
            Color32::from_rgb(0xEF, 0xFD, 0xDE),
            Color32::from_rgb(0x40, 0xA7, 0xE3),
            ThanosPolicy::enabled(),
        ));
        assert!(effect.animating());
        assert_eq!(effect.active_count(), 1);
        assert_eq!(effect.items()[0].progress(), 0.0);

        // Step partway through animation.
        assert!(effect.step_dt(0.05, ThanosPolicy::enabled()));
        let p1 = effect.items()[0].progress();
        assert!(p1 > 0.0 && p1 < 1.0);

        // Step until completion (~2.5s wall time due to speed ramp).
        let mut steps = 0;
        while effect.animating() && steps < 300 {
            effect.step_dt(0.033, ThanosPolicy::enabled());
            steps += 1;
        }
        assert!(!effect.animating());
        assert!(steps > 20 && steps < 250);
    }

    #[test]
    fn thanos_respects_power_saving_policy() {
        let mut effect = ThanosEffect::new();
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(120.0, 48.0));

        // Disabled policy refuses new items.
        assert!(!effect.add_item(
            rect,
            Color32::WHITE,
            Color32::BLUE,
            ThanosPolicy::disabled()
        ));
        assert!(!effect.animating());

        // Enabling allows adding, and subsequently disabling finishes immediately.
        assert!(effect.add_item(rect, Color32::WHITE, Color32::BLUE, ThanosPolicy::enabled()));
        assert!(effect.animating());
        assert!(!effect.step_dt(0.016, ThanosPolicy::disabled()));
        assert!(!effect.animating());
    }
}
