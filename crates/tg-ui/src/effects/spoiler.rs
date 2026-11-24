//! Animated sparkling particle veil for hidden spoiler spans.
//!
//! Port of `Telegram/lib_ui/ui/effects/spoiler_mess.h` and
//! `Telegram/lib_ui/ui/effects/spoiler_mess.cpp`.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect};

use crate::anim::{disabled, with_alpha};

/// Deterministic LCG-generated sparkling particles for a spoiler region.
///
/// Mirrors the particle lifecycle (fade-in 200ms, shown 200ms, fade-out 200ms,
/// drifting at 4..8 px over lifetime) from `DefaultDescriptorText()` in
/// `spoiler_mess.cpp`.
pub fn paint_spoiler_particles(
    ctx: &egui::Context,
    painter: &Painter,
    rect: Rect,
    particle_color: Color32,
    bg_color: Color32,
    seed: u64,
) {
    // Soft translucent backing so hidden text isn't legible underneath.
    painter.rect_filled(rect, CornerRadius::same(3), bg_color);

    let area = (rect.width() * rect.height()).max(1.0);
    // Target density similar to tdesktop's text spoiler (~1 particle per 14 px^2).
    let count = ((area / 14.0) as usize).clamp(10, 180);
    let now_ms = if disabled() {
        0.0
    } else {
        ctx.request_repaint();
        ctx.input(|i| i.time) * 1000.0
    };

    const FADE_IN: f64 = 200.0;
    const SHOWN: f64 = 200.0;
    const FADE_OUT: f64 = 200.0;
    const LIFETIME: f64 = FADE_IN + SHOWN + FADE_OUT;
    const PERIOD: f64 = 1980.0; // 60 frames * 33ms

    let clipped = painter.with_clip_rect(painter.clip_rect().intersect(rect));
    let mut rng = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);

    for i in 0..count {
        let rx = next_unit(&mut rng);
        let ry = next_unit(&mut rng);
        let angle = next_unit(&mut rng) * std::f64::consts::TAU;
        let speed = 4.0 + next_unit(&mut rng) * 4.0;
        let size = 0.8 + (next_unit(&mut rng) as f32) * 0.7;

        let start = (i as f64 / count as f64) * PERIOD;
        let phase = (now_ms - start).rem_euclid(PERIOD);

        let alpha = if disabled() {
            0.55
        } else if phase < FADE_IN {
            phase / FADE_IN
        } else if phase < FADE_IN + SHOWN {
            1.0
        } else if phase < LIFETIME {
            1.0 - (phase - FADE_IN - SHOWN) / FADE_OUT
        } else {
            continue;
        };

        let travel = (phase / LIFETIME) * speed;
        let x = rect.min.x + (rx as f32) * rect.width() + (angle.cos() * travel) as f32;
        let y = rect.min.y + (ry as f32) * rect.height() + (angle.sin() * travel) as f32;

        // Wrap particles smoothly inside rect.
        let wx = rect.min.x + (x - rect.min.x).rem_euclid(rect.width().max(1.0));
        let wy = rect.min.y + (y - rect.min.y).rem_euclid(rect.height().max(1.0));

        clipped.circle_filled(
            Pos2::new(wx, wy),
            size,
            with_alpha(particle_color, alpha * 0.85),
        );
    }
}

#[inline]
fn next_unit(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let top = (*state >> 11) as f64;
    top / ((1u64 << 53) as f64)
}
