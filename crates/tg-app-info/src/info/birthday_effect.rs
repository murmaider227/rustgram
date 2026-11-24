//! Profile birthday confetti & cascading age digits celebration overlay.
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/profile/info_profile_birthday_effect.h` & `.cpp`
//! - `Telegram/SourceFiles/ui/effects/fireworks_animation.h` & `.cpp`

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use tg_ui::fonts::family_semibold;
use tg_ui::tg_style::st;

use crate::info::values::Birthday;

/// Total birthday celebration lifecycle duration in seconds (`kDuration = 4200ms`).
pub const BIRTHDAY_EFFECT_DURATION_SECS: f64 = 4.2;
/// Maximum per-tick frame step in seconds (`kMaxFrameStep = 20ms`).
const MAX_FRAME_STEP_SECS: f64 = 0.020;
/// Progress fraction at which confetti begins fading out (`kFadeOutFrom = 0.9`).
const FADE_OUT_FROM: f64 = 0.9;
/// Digit cascade wave length (`kCascadeWaveLength = 1.8`).
const CASCADE_WAVE_LENGTH: f64 = 1.8;
/// Fraction of each digit's wave spent scaling in (`kCascadeScalePart = 0.4`).
const CASCADE_SCALE_PART: f64 = 0.4;
/// Horizontal digit pitch relative to digit side (`kDigitPitchFactor = 0.88`).
const DIGIT_PITCH_FACTOR: f64 = 0.88;
/// Initial vertical offset of rising digits below the userpic (`kDigitSourceBelowFactor = 2`).
const DIGIT_SOURCE_BELOW_FACTOR: f64 = 2.0;

/// Initial firework particle count (`kParticlesCount = 60`).
const PARTICLES_COUNT: usize = 60;
/// Secondary falling confetti particle count (`kFallCount = 30`).
const FALL_COUNT: usize = 30;
/// Virtual reference space width (`kFireworkWidth = 480`).
const FIREWORK_WIDTH: f64 = 480.0;
/// Virtual reference space height (`kFireworkHeight = 320`).
const FIREWORK_HEIGHT: f64 = 320.0;

/// Exact 6-color festive palette from `PrepareBrushes()` in `ui/effects/fireworks_animation.cpp:22-31`.
pub const CONFETTI_PALETTE: [Color32; 6] = [
    Color32::from_rgb(0xE8, 0xBC, 0x2C),
    Color32::from_rgb(0xD0, 0x04, 0x9E),
    Color32::from_rgb(0x02, 0xCB, 0xFE),
    Color32::from_rgb(0x57, 0x23, 0xFD),
    Color32::from_rgb(0xFE, 0x8C, 0x27),
    Color32::from_rgb(0x6C, 0xB8, 0x59),
];

/// Returns today's calendar `(year, month, day)` in UTC from `SystemTime::now()`.
///
/// Uses Howard Hinnant's civil-from-days algorithm with zero external dependencies.
#[must_use]
pub fn current_utc_ymd() -> (u16, u8, u8) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ymd_from_unix_secs(secs)
}

/// Converts a Unix timestamp in seconds into `(year, month, day)` (`month: 1..=12`, `day: 1..=31`).
#[must_use]
pub fn ymd_from_unix_secs(secs: i64) -> (u16, u8, u8) {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + i64::from(m <= 2)).clamp(1, i64::from(u16::MAX)) as u16;
    (year, m as u8, d as u8)
}

/// Returns `true` when `birthday` matches the given `(month, day)`.
#[must_use]
pub const fn is_birthday_today(birthday: Birthday, today_month: u8, today_day: u8) -> bool {
    birthday.month == today_month && birthday.day == today_day
}

/// Computes the peer's age on `today_year`, or `0` when the birth year is absent or in the future.
#[must_use]
pub const fn birthday_age(birthday: Birthday, today_year: u16) -> u16 {
    match birthday.year {
        Some(year) if today_year > year => today_year - year,
        _ => 0,
    }
}

/// Cascade wave timing helper (`Cascade()` in `info_profile_birthday_effect.cpp:54-68`).
#[must_use]
pub fn cascade(full_progress: f64, position: f64, count: f64, wave_length: f64) -> f64 {
    if count <= 0.0 {
        return full_progress;
    }
    let wave_duration = (1.0 / count) * wave_length.min(count);
    let wave_offset = (position / count) * (1.0 - wave_duration);
    ((full_progress - wave_offset) / wave_duration).clamp(0.0, 1.0)
}

/// Quintic ease-out (`anim::easeOutQuint`).
#[inline]
fn ease_out_quint(t: f64) -> f64 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv * inv * inv
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParticleShape {
    Circle,
    Rectangle,
}

#[derive(Debug, Clone)]
struct ConfettiParticle {
    x: f64,
    y: f64,
    move_x: f64,
    move_y: f64,
    size: f64,
    rotation_deg: f64,
    color_idx: usize,
    shape: ParticleShape,
    right: bool,
    x_finished: u8,
    finished_start: u8,
    finished: bool,
}

/// Deterministic xorshift64 PRNG so birthday confetti renders reproducibly in tests & UI.
#[derive(Debug, Clone)]
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 16) as u32
    }

    fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u32() as usize) % bound
        }
    }

    fn next_f64(&mut self) -> f64 {
        f64::from(self.next_u32()) / f64::from(u32::MAX)
    }
}

/// Stateful single-shot birthday confetti & rising age-digit celebration rendered over `ProfileCover`.
///
/// Respects reduced-motion / `PowerSavingFlags::CHAT_EFFECTS`: when `reduced_motion` is true,
/// the effect completes immediately with zero motion frames.
#[derive(Debug, Clone)]
pub struct ProfileBirthdayEffect {
    age: u16,
    progress: f64,
    last_tick_secs: Option<f64>,
    particles: Vec<ConfettiParticle>,
    falling_down: usize,
    started_fall: bool,
    speed_coef: f64,
    rng: XorShift64,
    finished: bool,
    reduced_motion: bool,
    frames_rendered: u32,
}

impl ProfileBirthdayEffect {
    /// Starts a new profile birthday celebration seeded by `seed` (typically `peer_id.0 as u64`).
    #[must_use]
    pub fn start(seed: u64, age: u16, reduced_motion: bool) -> Self {
        let mut rng = XorShift64::new(seed ^ 0xB1A7_4DA7_2026);
        let mut particles = Vec::with_capacity(PARTICLES_COUNT + FALL_COUNT);
        if !reduced_motion {
            for _ in 0..PARTICLES_COUNT {
                particles.push(Self::init_particle(&mut rng, false));
            }
        }
        Self {
            age,
            progress: 0.0,
            last_tick_secs: None,
            particles,
            falling_down: 0,
            started_fall: false,
            speed_coef: 1.0,
            rng,
            finished: reduced_motion,
            reduced_motion,
            frames_rendered: 0,
        }
    }

    /// Returns `true` while the confetti animation still has active frames to draw.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        !self.finished
    }

    /// Returns normalized lifecycle progress in `0.0..=1.0`.
    #[must_use]
    pub const fn progress(&self) -> f64 {
        self.progress
    }

    /// Returns the number of live (unfinished) confetti particles.
    #[must_use]
    pub fn active_particle_count(&self) -> usize {
        self.particles.iter().filter(|p| !p.finished).count()
    }

    /// Returns the celebratory age displayed in rising digits (`0` if birth year unknown).
    #[must_use]
    pub const fn age(&self) -> u16 {
        self.age
    }

    /// Returns how many animation frames have been painted so far.
    #[must_use]
    pub const fn frames_rendered(&self) -> u32 {
        self.frames_rendered
    }

    /// Advances physics and timeline progress to `now_secs` (`BirthdayEffect::tick` +
    /// `FireworksAnimation::update`).
    pub fn tick(&mut self, now_secs: f64, paused: bool) {
        if self.finished || self.reduced_motion {
            self.finished = true;
            return;
        }
        let delta = match self.last_tick_secs {
            Some(prev) => (now_secs - prev).clamp(0.0, MAX_FRAME_STEP_SECS),
            None => 0.016,
        };
        self.last_tick_secs = Some(now_secs);
        if paused {
            return;
        }
        self.progress = (self.progress + delta / BIRTHDAY_EFFECT_DURATION_SECS).clamp(0.0, 1.0);

        let move_coef = (delta * 1000.0) / 16.0;
        let mut all_particles_done = true;
        for particle in &mut self.particles {
            if particle.finished {
                continue;
            }
            particle.x += particle.move_x * move_coef;
            particle.y += particle.move_y * move_coef;

            if particle.x_finished != 0 {
                let dp = 0.5;
                if particle.x_finished == 1 {
                    particle.move_x += dp * move_coef * 0.05;
                    if particle.move_x >= dp {
                        particle.x_finished = 2;
                    }
                } else {
                    particle.move_x -= dp * move_coef * 0.05;
                    if particle.move_x <= -dp {
                        particle.x_finished = 1;
                    }
                }
            } else if particle.right {
                if particle.move_x < 0.0 {
                    particle.move_x += move_coef * 0.05;
                    if particle.move_x >= 0.0 {
                        particle.move_x = 0.0;
                        particle.x_finished = particle.finished_start;
                    }
                }
            } else if particle.move_x > 0.0 {
                particle.move_x -= move_coef * 0.05;
                if particle.move_x <= 0.0 {
                    particle.move_x = 0.0;
                    particle.x_finished = particle.finished_start;
                }
            }

            let y_edge = -0.5;
            let was_negative = particle.move_y < y_edge;
            if particle.move_y > y_edge {
                particle.move_y += (1.0 / 3.0) * move_coef * self.speed_coef;
            } else {
                particle.move_y += (1.0 / 3.0) * move_coef;
            }
            if was_negative && particle.move_y > y_edge {
                self.falling_down += 1;
            }
            if particle.shape == ParticleShape::Rectangle {
                particle.rotation_deg = (particle.rotation_deg + move_coef * 10.0) % 360.0;
            }
            if particle.y >= FIREWORK_HEIGHT {
                particle.finished = true;
            } else {
                all_particles_done = false;
            }
        }

        if self.falling_down >= PARTICLES_COUNT / 2 && self.speed_coef > 0.2 {
            if !self.started_fall {
                self.started_fall = true;
                for _ in 0..FALL_COUNT {
                    self.particles
                        .push(Self::init_particle(&mut self.rng, true));
                }
                all_particles_done = false;
            }
            self.speed_coef = (self.speed_coef - move_coef * 0.15).max(0.2);
        }

        if self.progress >= 1.0 || all_particles_done {
            self.finished = true;
        }
    }

    /// Paints the confetti burst and rising age digits inside `cover_rect` (clipped cleanly to
    /// `cover_rect` so particles never bleed outside the profile header).
    pub fn paint(
        &mut self,
        painter: &Painter,
        cover_rect: Rect,
        userpic_rect: Rect,
        accent: Color32,
    ) {
        if self.finished || self.reduced_motion || !cover_rect.is_positive() {
            return;
        }
        self.frames_rendered = self.frames_rendered.saturating_add(1);
        let opacity = if self.progress < FADE_OUT_FROM {
            1.0_f32
        } else {
            (1.0 - (self.progress - FADE_OUT_FROM) / (1.0 - FADE_OUT_FROM)).clamp(0.0, 1.0) as f32
        };
        if opacity <= 0.01 {
            return;
        }

        let clipped = painter.with_clip_rect(cover_rect);
        let confetti_side = (cover_rect
            .width()
            .min(st::info_profile_birthday_confetti_size.0 as f32))
        .max(120.0);
        let fx_rect = Rect::from_center_size(
            Pos2::new(
                userpic_rect.center().x.max(cover_rect.center().x * 0.55),
                cover_rect.center().y,
            ),
            Vec2::new(confetti_side.max(cover_rect.width()), cover_rect.height()),
        );

        // 1. Confetti particles (`FireworksAnimation::paintParticle`)
        let small_side = 2.0_f32;
        for particle in &self.particles {
            if particle.finished {
                continue;
            }
            let x =
                fx_rect.left() + (particle.x * f64::from(fx_rect.width()) / FIREWORK_WIDTH) as f32;
            let y =
                fx_rect.top() + (particle.y * f64::from(fx_rect.height()) / FIREWORK_HEIGHT) as f32;
            let base = CONFETTI_PALETTE[particle.color_idx % CONFETTI_PALETTE.len()];
            let col = Color32::from_rgba_unmultiplied(
                base.r(),
                base.g(),
                base.b(),
                (opacity * 235.0).round() as u8,
            );
            let sz = particle.size as f32;
            match particle.shape {
                ParticleShape::Circle => {
                    clipped.circle_filled(Pos2::new(x, y), sz * 0.5, col);
                }
                ParticleShape::Rectangle => {
                    let rad = (particle.rotation_deg as f32).to_radians();
                    let (s, c) = rad.sin_cos();
                    let hw = sz * 0.5;
                    let hh = small_side;
                    let corners = [
                        Pos2::new(-hw, -hh),
                        Pos2::new(hw, -hh),
                        Pos2::new(hw, hh),
                        Pos2::new(-hw, hh),
                    ]
                    .into_iter()
                    .map(|p| Pos2::new(x + p.x * c - p.y * s, y + p.x * s + p.y * c))
                    .collect();
                    clipped.add(Shape::convex_polygon(corners, col, Stroke::NONE));
                }
            }
        }

        // 2. Cascading festive age digits (`BirthdayEffect::paintDigits`)
        if self.age > 0 {
            let text = self.age.to_string();
            let digits: Vec<char> = text.chars().collect();
            let count = digits.len();
            if count > 0 {
                let max_digit_side =
                    (cover_rect.height() * 0.42).min(st::info_profile_birthday_digit_size.0 as f32);
                let sz = f64::from(max_digit_side.max(24.0));
                let pitch = sz * DIGIT_PITCH_FACTOR;
                let source_x = f64::from(userpic_rect.center().x);
                let source_y = f64::from(userpic_rect.top())
                    + f64::from(userpic_rect.height()) * DIGIT_SOURCE_BELOW_FACTOR;
                let row_left = (f64::from(cover_rect.left() + cover_rect.right())
                    - pitch * (count.saturating_sub(1) as f64))
                    * 0.5;
                let travel_x = row_left - source_x;
                let travel_y = source_y - f64::from(cover_rect.top()) + sz;
                let t = self.progress;

                for (i, ch) in digits.iter().enumerate().rev() {
                    let local = cascade(t, i as f64, count as f64, CASCADE_WAVE_LENGTH);
                    let scale = ease_out_quint((local / CASCADE_SCALE_PART).clamp(0.0, 1.0));
                    if scale <= 0.02 {
                        continue;
                    }
                    let cx = (source_x + pitch * (i as f64) + local * travel_x) as f32;
                    let cy = (source_y - travel_y * t * t) as f32;
                    let draw_side = (sz * scale) as f32;
                    let badge_col = CONFETTI_PALETTE[(i + 1) % CONFETTI_PALETTE.len()];
                    let fill = Color32::from_rgba_unmultiplied(
                        badge_col.r(),
                        badge_col.g(),
                        badge_col.b(),
                        (opacity * 220.0).round() as u8,
                    );
                    let ring = Color32::from_rgba_unmultiplied(
                        accent.r(),
                        accent.g(),
                        accent.b(),
                        (opacity * 180.0).round() as u8,
                    );
                    let center = Pos2::new(cx, cy);
                    clipped.circle_filled(center, draw_side * 0.44, fill);
                    clipped.circle_stroke(center, draw_side * 0.44, Stroke::new(1.5, ring));
                    clipped.text(
                        center,
                        Align2::CENTER_CENTER,
                        ch.to_string(),
                        FontId::new((draw_side * 0.56).max(10.0), family_semibold()),
                        Color32::from_rgba_unmultiplied(
                            255,
                            255,
                            255,
                            (opacity * 255.0).round() as u8,
                        ),
                    );
                }
            }
        }
    }

    fn init_particle(rng: &mut XorShift64, falling: bool) -> ConfettiParticle {
        let color_idx = rng.next_usize(CONFETTI_PALETTE.len());
        let shape = if rng.next_usize(2) == 1 {
            ParticleShape::Rectangle
        } else {
            ParticleShape::Circle
        };
        let right = rng.next_usize(2) == 1;
        let finished_start = 1 + rng.next_usize(2) as u8;
        let (size, rotation_deg) = match shape {
            ParticleShape::Circle => (6.0 + rng.next_f64() * 3.0, 0.0),
            ParticleShape::Rectangle => (6.0 + rng.next_f64() * 6.0, rng.next_usize(360) as f64),
        };
        let (x, y, move_x, move_y) = if falling {
            let py = -rng.next_f64() * FIREWORK_HEIGHT * 1.2;
            let px = rng.next_f64() * FIREWORK_WIDTH;
            let mx = (rng.next_f64() - 0.5) * 1.4;
            let my = 1.2 + rng.next_f64() * 2.0;
            (px, py, mx, my)
        } else {
            let px = if right {
                FIREWORK_WIDTH * 0.92
            } else {
                FIREWORK_WIDTH * 0.08
            };
            let py = FIREWORK_HEIGHT * (0.45 + rng.next_f64() * 0.35);
            let speed_x = 3.0 + rng.next_f64() * 8.5;
            let mx = if right { -speed_x } else { speed_x };
            let my = -(5.5 + rng.next_f64() * 8.0);
            (px, py, mx, my)
        };
        ConfettiParticle {
            x,
            y,
            move_x,
            move_y,
            size,
            rotation_deg,
            color_idx,
            shape,
            right,
            x_finished: 0,
            finished_start,
            finished: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn birthday_today_and_age_helpers_match_calendar() {
        let bday = Birthday {
            day: 24,
            month: 11,
            year: Some(1996),
        };
        assert!(is_birthday_today(bday, 11, 24));
        assert!(!is_birthday_today(bday, 11, 25));
        assert_eq!(birthday_age(bday, 2026), 30);

        let no_year = Birthday {
            day: 24,
            month: 11,
            year: None,
        };
        assert_eq!(birthday_age(no_year, 2026), 0);
        // 2026-11-24 00:00:00 UTC
        assert_eq!(ymd_from_unix_secs(1_795_478_400), (2026, 11, 24));
    }

    #[test]
    fn confetti_ticks_deterministically_and_completes_within_duration() {
        let mut eff = ProfileBirthdayEffect::start(1001, 32, false);
        assert!(eff.is_active());
        assert_eq!(eff.active_particle_count(), PARTICLES_COUNT);

        let mut t = 0.0;
        for _ in 0..300 {
            eff.tick(t, false);
            t += 0.02;
        }
        assert!(!eff.is_active());
        assert!(eff.progress() >= 0.99 || eff.active_particle_count() == 0);
    }

    #[test]
    fn reduced_motion_skips_particles_immediately() {
        let eff = ProfileBirthdayEffect::start(1001, 32, true);
        assert!(!eff.is_active());
        assert_eq!(eff.active_particle_count(), 0);
    }
}
