//! Thanos delete effect: a deleted message turns to dust.
//!
//! Port of tdesktop `ui/effects/thanos_effect.cpp`, `thanos_effect_renderer.cpp` and its shaders
//! (`Telegram/shaders/thanos_init.comp`, `thanos_update.comp`, `thanos.vert`, `thanos.frag`).
//! The snapshot of the deleted message is cut into a grid of particles, one per logical pixel
//! (fewer for big messages). From left to right the particles start to fly off in random
//! directions, float up, shrink and fade out; the effect lasts about 2.3 s.
//!
//! tdesktop simulates the particles in GPU compute shaders and draws them instanced. Here the same
//! formulas run on the CPU over a smaller particle budget, and each item is one textured egui mesh
//! that samples the snapshot texture: no custom GPU pipeline, and the frame cost stays flat.

use egui::{Color32, ColorImage, CornerRadius, Id, LayerId, Mesh, Order, Pos2, Rect, Vec2};

use crate::anim::{disabled as anim_disabled, slow_multiplier};

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

/// tdesktop's particle cap per item (`kMaxParticleCount` in `thanos_effect_renderer.cpp`).
pub const MAX_PARTICLE_COUNT: u32 = 120_000;

/// Particle cap per item here: a text message of up to ~24k logical pixels keeps one particle
/// per pixel like tdesktop; bigger media get bigger particles.
pub const MAX_ITEM_PARTICLES: u32 = 24_000;

/// Particles shared by the items of one deletion, so deleting a screenful at once stays smooth.
pub const BATCH_PARTICLE_BUDGET: u32 = 72_000;

/// Smallest per-item particle count when the batch budget is split.
pub const MIN_ITEM_PARTICLES: u32 = 2_000;

/// Maximum per-frame delta time in seconds (`crl::time(66)`: one slow frame cannot teleport
/// particles).
pub const MAX_FRAME_DT_SECS: f64 = 0.066;

/// Minimum per-frame delta time in seconds (`crl::time(1)`).
pub const MIN_FRAME_DT_SECS: f64 = 0.001;

/// Window in which a particle column starts moving (`easeInWindow`'s `windowSize`).
const EASE_IN_WINDOW: f32 = 0.8;

/// Phase span of the left-to-right start sweep (`easeInDuration`).
const EASE_IN_DURATION: f32 = 0.8;

/// Upward acceleration in logical pixels per time step squared (`velocity.y += 80 * timeStep`).
const LIFT: f32 = 80.0;

/// Lifetime lost per time step (`lifetime - 0.6 * timeStep`).
const LIFETIME_DECAY: f32 = 0.6;

/// Lifetime below which a particle fades out (`clamp(inLifetime / 0.6, 0, 1)`).
const FADE_LIFETIME: f32 = 0.6;

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

    /// Returns `true` when the Thanos effect is allowed to play (`ThanosEffect::Supported`).
    #[must_use]
    pub const fn allows_effect(self) -> bool {
        self.chat_effects_enabled && self.animations_enabled
    }
}

/// Quintic smootherstep acceleration curve (`AnimationSpeedMultiplier`).
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

/// Cubic ease-out disappearance factor (`DisappearProgress`).
///
/// Returns `0.0` until `phase == DISAPPEAR_START_PHASE` (`0.9`), then rises monotonically
/// to `1.0` at `phase == MAX_PHASE_DURATION` (`6.0`).
#[must_use]
pub fn disappear_progress(phase: f64) -> f64 {
    let t = ((phase - DISAPPEAR_START_PHASE) / DISAPPEAR_DURATION).clamp(0.0, 1.0);
    let one_minus = 1.0 - t;
    1.0 - (one_minus * one_minus * one_minus)
}

/// Computes the `(cols, rows)` particle grid: one particle per pixel while the item has at most
/// `max_particles` pixels, else the largest grid of the same aspect ratio within the cap
/// (`createAnimatingItem`).
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

/// Particle cap for each item of a deletion of `items` messages.
#[must_use]
pub fn particle_budget(items: usize) -> u32 {
    let items = u32::try_from(items.max(1)).unwrap_or(u32::MAX);
    (BATCH_PARTICLE_BUDGET / items).clamp(MIN_ITEM_PARTICLES, MAX_ITEM_PARTICLES)
}

/// The shaders' integer hash (`hash` in `thanos_init.comp`).
#[must_use]
fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x045d_9f3b);
    x ^= x >> 16;
    x = x.wrapping_mul(0x045d_9f3b);
    x ^= x >> 16;
    x
}

/// `hashFloat`: the hash mapped to `[0, 1]`.
#[must_use]
fn hash_float(x: u32) -> f32 {
    hash(x) as f32 / u32::MAX as f32
}

/// How far particle column `x_fraction` has started moving at `effect_fraction` of the
/// start sweep (`easeInWindow`): columns start from left to right.
#[must_use]
pub fn ease_in_window(effect_fraction: f32, x_fraction: f32) -> f32 {
    let window_start = -EASE_IN_WINDOW;
    let window_end = 1.0;
    let window_pos = (1.0 - effect_fraction) * window_start + effect_fraction * window_end;
    let window_t = (x_fraction - window_pos).clamp(0.0, EASE_IN_WINDOW) / EASE_IN_WINDOW;
    1.0 - window_t
}

/// One particle: a cell of the snapshot. Screen coordinates (y grows down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThanosParticle {
    /// Displacement from the cell's place in logical pixels.
    pub offset: Vec2,
    /// Velocity in logical pixels per time step.
    pub velocity: Vec2,
    /// Remaining lifetime; the particle fades out below `0.6`.
    pub lifetime: f32,
}

/// One disintegrating message managed by [`ThanosEffect`].
#[derive(Clone)]
pub struct ThanosItem {
    /// Screen rectangle the snapshot was taken from.
    pub rect: Rect,
    /// Current phase in `[0.0, MAX_PHASE_DURATION]`.
    pub phase: f64,
    /// Particle grid columns.
    pub particle_cols: u32,
    /// Particle grid rows.
    pub particle_rows: u32,
    /// Particles in the shaders' order: `gid = x + y * cols`, row `y` counted from the bottom.
    pub particles: Vec<ThanosParticle>,
    texture: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for ThanosItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThanosItem")
            .field("rect", &self.rect)
            .field("phase", &self.phase)
            .field("particle_cols", &self.particle_cols)
            .field("particle_rows", &self.particle_rows)
            .field("textured", &self.texture.is_some())
            .finish_non_exhaustive()
    }
}

impl ThanosItem {
    /// Creates the particles of an item covering `rect` (`thanos_init.comp` with `seed`), or
    /// `None` for an empty rect.
    #[must_use]
    pub fn new(rect: Rect, seed: u32, max_particles: u32) -> Option<Self> {
        let w = rect.width().round().max(0.0) as u32;
        let h = rect.height().round().max(0.0) as u32;
        let (cols, rows) = compute_particle_grid(w, h, max_particles.max(1));
        if cols == 0 || rows == 0 {
            return None;
        }
        let count = cols * rows;
        let particles = (0..count)
            .map(|gid| {
                let s = gid.wrapping_mul(3).wrapping_add(seed);
                let direction = hash_float(s) * std::f32::consts::TAU;
                let speed = (0.1 + hash_float(s.wrapping_add(1)) * 0.1) * 320.0;
                // The shader's y axis points up; the screen's points down.
                let velocity = Vec2::new(direction.cos() * speed, -direction.sin() * speed);
                let lifetime = 1.5 + hash_float(s.wrapping_add(2)) * 1.5;
                ThanosParticle {
                    offset: Vec2::ZERO,
                    velocity,
                    lifetime,
                }
            })
            .collect();
        Some(Self {
            rect,
            phase: 0.0,
            particle_cols: cols,
            particle_rows: rows,
            particles,
            texture: None,
        })
    }

    /// Gives the item the snapshot its particles show.
    #[must_use]
    pub fn with_texture(mut self, texture: egui::TextureHandle) -> Self {
        self.texture = Some(texture);
        self
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

    /// Advances the item by `dt_secs` of frame time (`ThanosEffectRenderer::render` and
    /// `thanos_update.comp`).
    pub fn step(&mut self, dt_secs: f64) {
        if self.is_finished() {
            return;
        }
        let slow = f64::from(slow_multiplier());
        let animation_time_step = (dt_secs / slow) * animation_speed_multiplier(self.phase);
        self.phase += animation_time_step * PHASE_SPEED;
        let time_step = (animation_time_step * TIME_STEP_MULTIPLIER) as f32;
        let effect_fraction = (self.phase as f32).clamp(0.0, EASE_IN_DURATION) / EASE_IN_DURATION;
        let cols = self.particle_cols.max(1) as usize;
        for (gid, p) in self.particles.iter_mut().enumerate() {
            let x_fraction = (gid % cols) as f32 / cols as f32;
            let fraction = ease_in_window(effect_fraction, x_fraction);
            if fraction <= 0.0 {
                continue;
            }
            let step = time_step * fraction;
            p.offset += p.velocity * step;
            p.velocity.y -= LIFT * step;
            p.lifetime = (p.lifetime - LIFETIME_DECAY * step).max(0.0);
        }
    }

    /// The textured mesh of the particles (`thanos.vert` / `thanos.frag`), or `None` without a
    /// snapshot or once everything has faded.
    #[must_use]
    pub fn mesh(&self) -> Option<Mesh> {
        let texture = self.texture.as_ref()?.id();
        let inverse_disappear = (1.0 - disappear_progress(self.phase)) as f32;
        if inverse_disappear <= 0.0 || self.is_finished() {
            return None;
        }
        let cols = self.particle_cols as usize;
        let rows = self.particle_rows as usize;
        let cell = Vec2::new(
            self.rect.width() / cols as f32,
            self.rect.height() / rows as f32,
        );
        let quad = cell * inverse_disappear;
        let mut mesh = Mesh::with_texture(texture);
        mesh.reserve_vertices(self.particles.len() * 4);
        mesh.reserve_triangles(self.particles.len() * 2);
        for (gid, p) in self.particles.iter().enumerate() {
            let alpha = (p.lifetime / FADE_LIFETIME).clamp(0.0, 1.0) * inverse_disappear;
            if alpha <= 0.0 {
                continue;
            }
            let x = gid % cols;
            let row = rows - 1 - gid / cols;
            let top_left = Pos2::new(
                self.rect.left() + x as f32 * cell.x,
                self.rect.top() + row as f32 * cell.y,
            ) + p.offset;
            let uv = Rect::from_min_max(
                Pos2::new(x as f32 / cols as f32, row as f32 / rows as f32),
                Pos2::new((x + 1) as f32 / cols as f32, (row + 1) as f32 / rows as f32),
            );
            let a = (alpha * 255.0).round() as u8;
            mesh.add_rect_with_uv(
                Rect::from_center_size(top_left + cell * 0.5, quad),
                uv,
                Color32::from_rgba_premultiplied(a, a, a, a),
            );
        }
        Some(mesh)
    }
}

/// Cuts `rect` (points) out of a window screenshot (physical pixels) and clears what lies
/// outside the rounded `corners`, so only the message itself turns to dust (tdesktop renders the
/// view alone onto a transparent image in `ThanosEffectController::captureView`).
#[must_use]
pub fn snapshot_region(
    shot: &ColorImage,
    rect: Rect,
    pixels_per_point: f32,
    corners: CornerRadius,
) -> Option<ColorImage> {
    let [shot_w, shot_h] = shot.size;
    let left = (rect.left() * pixels_per_point).round().max(0.0) as usize;
    let top = (rect.top() * pixels_per_point).round().max(0.0) as usize;
    let right = ((rect.right() * pixels_per_point).round().max(0.0) as usize).min(shot_w);
    let bottom = ((rect.bottom() * pixels_per_point).round().max(0.0) as usize).min(shot_h);
    if left >= right || top >= bottom {
        return None;
    }
    let (w, h) = (right - left, bottom - top);
    let mut pixels = Vec::with_capacity(w * h);
    for y in top..bottom {
        pixels.extend_from_slice(&shot.pixels[y * shot_w + left..y * shot_w + right]);
    }
    let radius = |r: u8| f32::from(r) * pixels_per_point;
    let (nw, ne, sw, se) = (
        radius(corners.nw),
        radius(corners.ne),
        radius(corners.sw),
        radius(corners.se),
    );
    let (wf, hf) = (w as f32, h as f32);
    for y in 0..h {
        let py = y as f32 + 0.5;
        for x in 0..w {
            let px = x as f32 + 0.5;
            let (r, cx, cy) = if px < wf * 0.5 {
                if py < hf * 0.5 {
                    (nw, nw, nw)
                } else {
                    (sw, sw, hf - sw)
                }
            } else if py < hf * 0.5 {
                (ne, wf - ne, ne)
            } else {
                (se, wf - se, hf - se)
            };
            if r <= 0.0 {
                continue;
            }
            let outside_x = if px < wf * 0.5 { px < cx } else { px > cx };
            let outside_y = if py < hf * 0.5 { py < cy } else { py > cy };
            if !(outside_x && outside_y) {
                continue;
            }
            let distance = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let coverage = (r - distance + 0.5).clamp(0.0, 1.0);
            if coverage < 1.0 {
                let pixel = &mut pixels[y * w + x];
                *pixel = pixel.gamma_multiply(coverage);
            }
        }
    }
    Some(ColorImage::new([w, h], pixels))
}

/// Controller managing active Thanos items (`Ui::ThanosEffect` with its renderer).
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
            seed_counter: 0,
        }
    }

    /// Starts dissolving `snapshot` shown at `rect` (`ThanosEffect::addItem`).
    ///
    /// Returns `false` when `policy` forbids the effect or the rect is empty.
    pub fn add_item(
        &mut self,
        ctx: &egui::Context,
        snapshot: ColorImage,
        rect: Rect,
        max_particles: u32,
        policy: ThanosPolicy,
    ) -> bool {
        if !policy.allows_effect() {
            return false;
        }
        let seed = self.seed_counter;
        let Some(item) = ThanosItem::new(rect, seed, max_particles) else {
            return false;
        };
        self.seed_counter = self.seed_counter.wrapping_add(1);
        let texture = ctx.load_texture(
            format!("tg_thanos_{seed}"),
            snapshot,
            egui::TextureOptions::LINEAR,
        );
        self.items.push(item.with_texture(texture));
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

    /// Immediately finishes and clears all active items (`finishNow`).
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
        let dt = dt_secs.clamp(MIN_FRAME_DT_SECS, MAX_FRAME_DT_SECS);
        for item in &mut self.items {
            item.step(dt);
        }
        self.items.retain(|item| !item.is_finished());
        if self.items.is_empty() {
            self.last_time_sec = None;
        }
        !self.items.is_empty()
    }

    /// Advances to wall-clock `now_sec`. The first frame after a pause counts as the longest
    /// frame, like the renderer's `_lastFrameTime` left from its previous run.
    pub fn advance_to(&mut self, now_sec: f64, policy: ThanosPolicy) -> bool {
        if !policy.allows_effect() {
            self.finish_all();
            return false;
        }
        if self.items.is_empty() {
            self.last_time_sec = None;
            return false;
        }
        let dt = self
            .last_time_sec
            .map_or(MAX_FRAME_DT_SECS, |prev| now_sec - prev);
        self.last_time_sec = Some(now_sec);
        self.step_dt(dt, policy)
    }

    /// Steps and paints all active items above the whole window (tdesktop's effect surface is a
    /// child of the window raised over everything, `WA_AlwaysStackOnTop`).
    pub fn paint(&mut self, ui: &mut egui::Ui, now_sec: f64, policy: ThanosPolicy) {
        if !self.advance_to(now_sec, policy) {
            return;
        }
        let painter = ui.ctx().layer_painter(LayerId::new(
            Order::Foreground,
            Id::unique("tg_thanos_effect"),
        ));
        for item in &self.items {
            if let Some(mesh) = item.mesh() {
                painter.add(egui::Shape::mesh(mesh));
            }
        }
        ui.ctx().request_repaint();
    }
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
        let (cols, rows) = compute_particle_grid(400, 200, MAX_ITEM_PARTICLES);
        assert!(cols > 0 && rows > 0);
        assert!(cols * rows <= MAX_ITEM_PARTICLES);
        assert!(cols > rows);
        assert_eq!(compute_particle_grid(0, 100, 500), (0, 0));
        assert_eq!(particle_budget(1), MAX_ITEM_PARTICLES);
        assert_eq!(particle_budget(6), 12_000);
        assert_eq!(particle_budget(500), MIN_ITEM_PARTICLES);
    }

    /// A text bubble keeps one particle per logical pixel, as in tdesktop (the old port capped
    /// every item at 512 particles of a flat colour).
    #[test]
    fn a_text_bubble_has_one_particle_per_pixel() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(240.0, 52.0));
        let item = ThanosItem::new(rect, 0, MAX_ITEM_PARTICLES).expect("item");
        assert_eq!((item.particle_cols, item.particle_rows), (240, 52));
        assert_eq!(item.particles.len(), 240 * 52);
    }

    /// `thanos_init.comp`: speed 32..64 px per step in a random direction, lifetime 1.5..3.
    #[test]
    fn particles_start_like_the_init_shader() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(64.0, 32.0));
        let item = ThanosItem::new(rect, 7, MAX_ITEM_PARTICLES).expect("item");
        let mut ups = 0;
        for (gid, p) in item.particles.iter().enumerate() {
            let speed = p.velocity.length();
            assert!((31.9..=64.1).contains(&speed), "speed {speed}");
            assert!((1.5..=3.0).contains(&p.lifetime));
            assert_eq!(p.offset, Vec2::ZERO);
            let s = (gid as u32) * 3 + 7;
            let direction = hash_float(s) * std::f32::consts::TAU;
            assert!((p.velocity.x - direction.cos() * speed).abs() < 1e-3);
            if p.velocity.y < 0.0 {
                ups += 1;
            }
        }
        // Directions are uniform: about half start upward.
        let share = ups as f32 / item.particles.len() as f32;
        assert!((0.4..0.6).contains(&share), "{share}");
    }

    /// Columns start moving from left to right over the first 0.8 of phase (`easeInWindow`),
    /// and the particles float up (`velocity.y += 80 * timeStep` in the shader's up axis).
    #[test]
    fn the_left_edge_starts_first_and_the_dust_floats_up() {
        assert_eq!(ease_in_window(0.0, 0.0), 0.0);
        assert_eq!(ease_in_window(1.0, 0.99), 1.0);
        assert!(ease_in_window(0.5, 0.0) > ease_in_window(0.5, 0.9));

        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 10.0));
        let mut item = ThanosItem::new(rect, 3, MAX_ITEM_PARTICLES).expect("item");
        item.step(0.1);
        let moved = |item: &ThanosItem, x: usize| {
            (0..item.particle_rows as usize)
                .map(|y| item.particles[y * 100 + x].offset.length())
                .sum::<f32>()
        };
        assert!(moved(&item, 0) > 0.0, "the left edge moves first");
        assert_eq!(moved(&item, 99), 0.0, "the right edge still waits");

        let mean_dy = |item: &ThanosItem| {
            item.particles.iter().map(|p| p.offset.y).sum::<f32>() / item.particles.len() as f32
        };
        for _ in 0..40 {
            item.step(1.0 / 60.0);
        }
        assert!(mean_dy(&item) < -5.0, "the dust rises: {}", mean_dy(&item));
    }

    /// The whole effect takes about 2.3 s of 60 fps frames, as tdesktop's phase clock does.
    #[test]
    fn thanos_lifecycle_progresses_and_finishes() {
        let mut effect = ThanosEffect::new();
        let rect = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(200.0, 64.0));
        let item = ThanosItem::new(rect, 0, MAX_ITEM_PARTICLES).expect("item");
        effect.items.push(item);
        assert!(effect.animating());
        assert_eq!(effect.items()[0].progress(), 0.0);
        assert!(effect.step_dt(0.05, ThanosPolicy::enabled()));
        let p1 = effect.items()[0].progress();
        assert!(p1 > 0.0 && p1 < 1.0);
        let mut frames = 1;
        while effect.animating() && frames < 400 {
            effect.step_dt(1.0 / 60.0, ThanosPolicy::enabled());
            frames += 1;
        }
        assert!(!effect.animating());
        let secs = frames as f64 / 60.0;
        assert!((2.0..2.6).contains(&secs), "{secs} s");
    }

    #[test]
    fn thanos_respects_power_saving_policy() {
        let ctx = egui::Context::default();
        let mut effect = ThanosEffect::new();
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(12.0, 8.0));
        let snapshot = || ColorImage::filled([12, 8], Color32::WHITE);

        assert!(!effect.add_item(&ctx, snapshot(), rect, 100, ThanosPolicy::disabled()));
        assert!(!effect.animating());

        assert!(effect.add_item(&ctx, snapshot(), rect, 100, ThanosPolicy::enabled()));
        assert!(effect.animating());
        assert!(effect.items()[0].mesh().is_some());
        assert!(!effect.step_dt(0.016, ThanosPolicy::disabled()));
        assert!(!effect.animating());
    }

    /// At phase 0 the particles tile the snapshot exactly: the message looks unchanged until the
    /// particles start to move.
    #[test]
    fn the_first_frame_shows_the_snapshot_in_place() {
        let ctx = egui::Context::default();
        let mut effect = ThanosEffect::new();
        let rect = Rect::from_min_size(Pos2::new(5.0, 7.0), Vec2::new(4.0, 2.0));
        let snapshot = ColorImage::filled([4, 2], Color32::RED);
        assert!(effect.add_item(&ctx, snapshot, rect, 100, ThanosPolicy::enabled()));
        let mesh = effect.items()[0].mesh().expect("mesh");
        assert_eq!(mesh.vertices.len(), 8 * 4);
        let bounds = mesh.calc_bounds();
        assert_eq!(bounds, rect);
        assert!(mesh.vertices.iter().all(|v| v.color == Color32::WHITE));
    }

    #[test]
    fn the_snapshot_keeps_only_the_rounded_bubble() {
        let shot = ColorImage::filled([40, 30], Color32::from_rgb(200, 220, 180));
        let rect = Rect::from_min_size(Pos2::new(5.0, 5.0), Vec2::new(20.0, 10.0));
        let image = snapshot_region(&shot, rect, 1.0, CornerRadius::same(4)).expect("crop");
        assert_eq!(image.size, [20, 10]);
        assert_eq!(image.pixels[0], Color32::TRANSPARENT, "the corner is cut");
        assert_eq!(
            image.pixels[5 * 20 + 10],
            shot.pixels[0],
            "the middle is kept"
        );

        let scaled = snapshot_region(&shot, rect, 1.5, CornerRadius::ZERO).expect("crop");
        assert_eq!(scaled.size, [30, 15]);
        assert!(
            snapshot_region(
                &shot,
                rect.translate(Vec2::new(100.0, 0.0)),
                1.0,
                CornerRadius::ZERO
            )
            .is_none()
        );
    }
}
