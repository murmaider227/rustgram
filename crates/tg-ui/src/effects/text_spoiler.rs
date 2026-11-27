//! The animated particle cover of hidden spoiler text.
//!
//! Port of the text descriptor of `Telegram/lib_ui/ui/effects/spoiler_mess.cpp`
//! (`DefaultDescriptorText`, `GenerateSpoilerMess`, `FillSpoilerRect`): 9000 particles of
//! 1.5-2 px rounded rectangles on a 128 px square canvas that tiles, 60 frames of 33 ms, each
//! particle fading in for 200 ms, shown for 200 ms and fading out for 200 ms while it drifts
//! 4-8 px. The particles take the text palette's `spoilerFg`; nothing is painted behind them.
//!
//! tdesktop renders all 60 frames into one image (`128 x 128 x 60` pixels per scale) and
//! copies the current frame over each spoiler rectangle. Here the frames are kept as particle
//! positions (a few bytes each) and the current frame is drawn as textured quads from a 5-sprite
//! texture: the same pixels, a fraction of the memory, and no work for frames nobody shows.

use std::sync::Arc;
use std::time::Duration;

use egui::{
    Color32, ColorImage, Context, Id, Mesh, Painter, Pos2, Rect, Shape, TextureHandle,
    TextureOptions, Vec2, pos2,
};

use crate::anim::disabled;

/// `kDefaultFramesCount`.
pub const TEXT_SPOILER_FRAMES: usize = 60;
/// `kDefaultFrameDuration`, in milliseconds.
pub const TEXT_SPOILER_FRAME_MS: u64 = 33;
/// `particlesCount` of `DefaultDescriptorText`.
const PARTICLES: usize = 9000;
/// `canvasSize` of `DefaultDescriptorText` before scaling.
const CANVAS: f32 = 128.0;
const FADE_IN_MS: i64 = 200;
const SHOWN_MS: i64 = 200;
const FADE_OUT_MS: i64 = 200;
const LIFETIME_MS: i64 = FADE_IN_MS + SHOWN_MS + FADE_OUT_MS;
const FULL_MS: i64 = TEXT_SPOILER_FRAMES as i64 * TEXT_SPOILER_FRAME_MS as i64;
const SIZE_MIN: f32 = 1.5;
const SIZE_MAX: f32 = 2.0;
const SPEED_MIN: f64 = 4.0;
const SPEED_MAX: f64 = 8.0;
const SPRITES: usize = 5;

/// One particle drawn in one frame: its sprite's top-left in device pixels of the canvas
/// (wrapped copies can start left of or above it) and its opacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dot {
    x: i16,
    y: i16,
    sprite: u8,
    alpha: u8,
}

/// The particle frames for one interface scale and the sprite texture they are drawn with.
pub struct TextSpoilerMess {
    /// Canvas side in device pixels (`style::ConvertScale(128) * ratio`).
    canvas: i16,
    /// Sprite side in device pixels (`2 + ceil(particleSizeMax)`).
    sprite: i16,
    frames: Vec<Vec<Dot>>,
    texture: Option<TextureHandle>,
}

impl std::fmt::Debug for TextSpoilerMess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextSpoilerMess")
            .field("canvas", &self.canvas)
            .field("sprite", &self.sprite)
            .field("frames", &self.frames.len())
            .finish_non_exhaustive()
    }
}

/// SplitMix64: a fixed sequence, so every run draws the same cover.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `RandomIndex(count)`: uniform in `0..count`.
    fn index(&mut self, count: usize) -> usize {
        (self.next() % count.max(1) as u64) as usize
    }
}

struct Particle {
    start: i64,
    sprite: u8,
    x: i64,
    y: i64,
    dx: f64,
    dy: f64,
}

/// `RandomSpeed`: a direction on the unit circle and a speed in `speedMin..=speedMax` per
/// lifetime, as device pixels per millisecond.
fn random_speed(random: &mut Random, scale: f64) -> (f64, f64) {
    let count = PARTICLES;
    let value = random.index(2 * count + 2);
    let negative = value < count + 1;
    let module = if negative { value } else { value - count - 1 };
    let speed_min = SPEED_MIN * scale;
    let speed_max = SPEED_MAX * scale;
    let speed = speed_min + (speed_max - speed_min) * module as f64 / count as f64;
    let lifetime = LIFETIME_MS as f64;
    let max = (speed_max * lifetime).ceil() as usize;
    let k = speed / lifetime;
    let x = (random.index(2 * max + 1) as f64 - max as f64) / max as f64;
    let y = (1.0 - x * x).max(0.0).sqrt() * if negative { -1.0 } else { 1.0 };
    (k * x, k * y)
}

/// `GenerateSprite`: sprite `index` as alpha coverage of a rounded rectangle at `(1, 1)`.
fn sprite_alpha(index: usize, side: usize, scale: f32) -> Vec<u8> {
    let middle = SPRITES / 2;
    let min = SIZE_MIN * scale;
    let delta = (SIZE_MAX - SIZE_MIN) * scale;
    let width = if index < middle {
        min + delta * (middle - index) as f32 / middle as f32
    } else {
        min
    };
    let height = if index > middle {
        min + delta * (index - middle) as f32 / (SPRITES - 1 - middle) as f32
    } else {
        min
    };
    let radius = min / 2.0;
    let (left, top) = (1.0_f32, 1.0_f32);
    let (right, bottom) = (left + width, top + height);
    let inside = |x: f32, y: f32| -> bool {
        if x < left || x > right || y < top || y > bottom {
            return false;
        }
        let cx = x.clamp(left + radius, right - radius);
        let cy = y.clamp(top + radius, bottom - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    const SUB: usize = 8;
    let mut out = vec![0u8; side * side];
    for py in 0..side {
        for px in 0..side {
            let mut hits = 0usize;
            for sy in 0..SUB {
                for sx in 0..SUB {
                    let x = px as f32 + (sx as f32 + 0.5) / SUB as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SUB as f32;
                    hits += usize::from(inside(x, y));
                }
            }
            out[py * side + px] = ((hits * 255 + SUB * SUB / 2) / (SUB * SUB)) as u8;
        }
    }
    out
}

impl TextSpoilerMess {
    /// `GenerateSpoilerMess(DefaultDescriptorText())` for the interface scale `scale`
    /// (device pixels per logical pixel).
    #[must_use]
    pub fn generate(scale: f32) -> Self {
        let scale = scale.clamp(0.5, 8.0);
        let canvas = (CANVAS * scale).round().max(16.0) as i64;
        let sprite = 2 + (SIZE_MAX * scale).ceil() as i64;
        let mut random = Random(0x5350_4F49_4C45_5221);
        let particles: Vec<Particle> = (0..PARTICLES)
            .map(|index| {
                let (dx, dy) = random_speed(&mut random, f64::from(scale));
                Particle {
                    start: index as i64 * FULL_MS / PARTICLES as i64,
                    sprite: random.index(SPRITES) as u8,
                    x: random.index(canvas as usize) as i64,
                    y: random.index(canvas as usize) as i64,
                    dx,
                    dy,
                }
            })
            .collect();
        let wrap = |value: i64| value.rem_euclid(canvas);
        let mut frames = Vec::with_capacity(TEXT_SPOILER_FRAMES);
        for frame in 0..TEXT_SPOILER_FRAMES {
            let time = frame as i64 * TEXT_SPOILER_FRAME_MS as i64;
            let mut dots = Vec::new();
            let mut paint_at = |p: &Particle, now: i64| {
                if now <= 0 || now >= LIFETIME_MS {
                    return;
                }
                let x = wrap(p.x + (now as f64 * p.dx).round() as i64);
                let y = wrap(p.y + (now as f64 * p.dy).round() as i64);
                let opacity = if now < FADE_IN_MS {
                    now as f64 / FADE_IN_MS as f64
                } else if now > LIFETIME_MS - FADE_OUT_MS {
                    (LIFETIME_MS - now) as f64 / FADE_OUT_MS as f64
                } else {
                    1.0
                };
                let alpha = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
                let mut push = |x: i64, y: i64| {
                    dots.push(Dot {
                        x: x as i16,
                        y: y as i16,
                        sprite: p.sprite,
                        alpha,
                    });
                };
                push(x, y);
                if x + sprite > canvas {
                    push(x - canvas, y);
                    if y + sprite > canvas {
                        push(x, y - canvas);
                        push(x - canvas, y - canvas);
                    }
                } else if y + sprite > canvas {
                    push(x, y - canvas);
                }
            };
            for p in &particles {
                paint_at(p, time - p.start);
                paint_at(p, time + FULL_MS - p.start);
            }
            dots.shrink_to_fit();
            frames.push(dots);
        }
        Self {
            canvas: canvas as i16,
            sprite: sprite as i16,
            frames,
            texture: None,
        }
    }

    /// The five sprites side by side, white with the coverage as alpha (premultiplied).
    fn sprite_image(&self, scale: f32) -> ColorImage {
        let side = self.sprite as usize;
        let mut image = ColorImage::filled([side * SPRITES, side], Color32::TRANSPARENT);
        for index in 0..SPRITES {
            let alpha = sprite_alpha(index, side, scale.clamp(0.5, 8.0));
            for y in 0..side {
                for x in 0..side {
                    image[(index * side + x, y)] = Color32::from_white_alpha(alpha[y * side + x]);
                }
            }
        }
        image
    }

    /// How many particles frame `frame` draws (tests).
    #[must_use]
    pub fn frame_particle_count(&self, frame: usize) -> usize {
        self.frames.get(frame).map_or(0, Vec::len)
    }

    /// Canvas side in device pixels.
    #[must_use]
    pub const fn canvas_px(&self) -> i16 {
        self.canvas
    }
}

/// The cover for the scale of `ctx`, generated and uploaded on first use.
fn mess_for(ctx: &Context) -> Arc<TextSpoilerMess> {
    let scale = ctx.pixels_per_point();
    let id = Id::unique(("tg_text_spoiler_mess", (scale * 100.0).round() as i32));
    if let Some(mess) = ctx.data(|d| d.get_temp::<Arc<TextSpoilerMess>>(id)) {
        return mess;
    }
    let mut mess = TextSpoilerMess::generate(scale);
    let image = mess.sprite_image(scale);
    mess.texture = Some(ctx.load_texture("tg_text_spoiler_sprites", image, TextureOptions::LINEAR));
    let mess = Arc::new(mess);
    ctx.data_mut(|d| d.insert_temp(id, Arc::clone(&mess)));
    mess
}

fn paused_pass_id() -> Id {
    Id::unique("tg_text_spoiler_paused_pass")
}

/// Stops the text spoiler covers painted in the rest of this pass from asking for the next
/// frame: the window is inactive or a layer covers the chat (`PaintContext::pausedSpoiler`).
/// They keep the frame they show; call it every pass while that lasts.
pub fn pause_text_spoilers_this_pass(ctx: &Context) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(paused_pass_id(), pass));
}

/// Whether [`pause_text_spoilers_this_pass`] was called in this pass.
#[must_use]
pub fn text_spoilers_paused(ctx: &Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(paused_pass_id())) == Some(pass)
}

/// The frame shown at `time_secs` (`SpoilerAnimation::index`) and how long it stays.
#[must_use]
pub fn text_spoiler_frame_at(time_secs: f64) -> (usize, Duration) {
    if disabled() {
        return (0, Duration::MAX);
    }
    let ms = (time_secs.max(0.0) * 1000.0) as u64;
    let frame = (ms / TEXT_SPOILER_FRAME_MS) as usize % TEXT_SPOILER_FRAMES;
    let left = TEXT_SPOILER_FRAME_MS - ms % TEXT_SPOILER_FRAME_MS;
    (frame, Duration::from_millis(left.max(1)))
}

/// Widest gap between two runs of one spoiler: a font change (a bold word) can leave a
/// fraction of a pixel between them. A space between two spoilers is wider (about 3.5 px).
const RUN_GAP: f32 = 1.5;

/// Joins the rectangles of one line that touch or overlap (`Renderer::fillRectsFromRanges`):
/// neighbouring spoiler runs (a bold word inside a spoiler) become one rectangle, so no
/// particle is drawn twice.
#[must_use]
pub fn merge_spoiler_rects(rects: &[Rect]) -> Vec<Rect> {
    let mut sorted: Vec<Rect> = rects.iter().copied().filter(|r| r.is_positive()).collect();
    sorted.sort_by(|a, b| {
        a.min
            .y
            .total_cmp(&b.min.y)
            .then(a.min.x.total_cmp(&b.min.x))
    });
    let mut out: Vec<Rect> = Vec::with_capacity(sorted.len());
    for rect in sorted {
        if let Some(last) = out.last_mut()
            && (last.min.y - rect.min.y).abs() < 0.5
            && (last.max.y - rect.max.y).abs() < 0.5
            && rect.min.x <= last.max.x + RUN_GAP
        {
            last.max.x = last.max.x.max(rect.max.x);
            continue;
        }
        out.push(rect);
    }
    out
}

/// Paints the particle cover over `rects` (`Renderer::paintSpoilerRects` with
/// `FillSpoilerRect`): the canvas tiles from `anchor` (the text's top-left, so the particles of
/// one text line up across its lines), each rectangle shows the part of the current frame
/// under it. `color` is the palette's `spoilerFg`; `opacity` fades the cover while the text is
/// being revealed. Schedules the repaint for the next frame.
pub fn paint_text_spoiler(
    ctx: &Context,
    painter: &Painter,
    rects: &[Rect],
    anchor: Pos2,
    color: Color32,
    opacity: f32,
) {
    if rects.is_empty() || opacity <= 0.0 {
        return;
    }
    let clip = painter.clip_rect();
    if !rects.iter().any(|r| r.intersects(clip)) {
        return;
    }
    let mess = mess_for(ctx);
    let Some(texture) = mess.texture.as_ref() else {
        return;
    };
    let (frame, next) = text_spoiler_frame_at(ctx.input(|i| i.time));
    if next != Duration::MAX && !text_spoilers_paused(ctx) {
        ctx.request_repaint_after(next);
    }
    let Some(dots) = mess.frames.get(frame) else {
        return;
    };
    let scale = ctx.pixels_per_point();
    let inv = 1.0 / scale;
    let tile = f32::from(mess.canvas) * inv;
    let sprite = f32::from(mess.sprite) * inv;
    let atlas_w = f32::from(mess.sprite) * SPRITES as f32;
    let atlas_h = f32::from(mess.sprite);
    let tint = color.gamma_multiply(opacity.clamp(0.0, 1.0));

    let mut mesh = Mesh::with_texture(texture.id());
    for rect in rects {
        let target = rect.intersect(clip);
        if !target.is_positive() {
            continue;
        }
        let first_x = ((target.min.x - anchor.x) / tile).floor() as i32;
        let last_x = ((target.max.x - anchor.x) / tile).ceil() as i32;
        let first_y = ((target.min.y - anchor.y) / tile).floor() as i32;
        let last_y = ((target.max.y - anchor.y) / tile).ceil() as i32;
        for ty in first_y..last_y {
            for tx in first_x..last_x {
                let cell_min = anchor + Vec2::new(tx as f32 * tile, ty as f32 * tile);
                let cell = Rect::from_min_size(cell_min, Vec2::splat(tile)).intersect(target);
                if !cell.is_positive() {
                    continue;
                }
                for dot in dots {
                    let min = cell_min + Vec2::new(f32::from(dot.x), f32::from(dot.y)) * inv;
                    let quad = Rect::from_min_size(min, Vec2::splat(sprite));
                    let shown = quad.intersect(cell);
                    if !shown.is_positive() {
                        continue;
                    }
                    let u0 = f32::from(dot.sprite) * f32::from(mess.sprite);
                    let uv = Rect::from_min_max(
                        pos2(
                            (u0 + (shown.min.x - quad.min.x) * scale) / atlas_w,
                            ((shown.min.y - quad.min.y) * scale) / atlas_h,
                        ),
                        pos2(
                            (u0 + (shown.max.x - quad.min.x) * scale) / atlas_w,
                            ((shown.max.y - quad.min.y) * scale) / atlas_h,
                        ),
                    );
                    let alpha = f32::from(dot.alpha) / 255.0;
                    mesh.add_rect_with_uv(shown, uv, tint.gamma_multiply(alpha));
                }
            }
        }
    }
    if !mesh.is_empty() {
        painter.add(Shape::mesh(mesh));
    }
}

/// Splits the spoiler `rects` at the `selected` text rectangles of the same lines
/// (`Renderer::pushSpoilerRange`): the parts outside the selection, then the parts inside it.
#[must_use]
pub fn split_spoiler_rects(rects: &[Rect], selected: &[Rect]) -> (Vec<Rect>, Vec<Rect>) {
    if selected.is_empty() {
        return (rects.to_vec(), Vec::new());
    }
    let mut outside = Vec::new();
    let mut inside = Vec::new();
    for rect in rects {
        // The selection of this line: rectangles that share most of its height.
        let mut spans: Vec<(f32, f32)> = selected
            .iter()
            .filter(|s| {
                let overlap = s.max.y.min(rect.max.y) - s.min.y.max(rect.min.y);
                overlap > 0.5 * rect.height().min(s.height())
            })
            .map(|s| (s.min.x.max(rect.min.x), s.max.x.min(rect.max.x)))
            .filter(|(from, till)| from < till)
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut x = rect.min.x;
        for (from, till) in spans {
            let from = from.max(x);
            if from > x {
                outside.push(Rect::from_x_y_ranges(x..=from, rect.y_range()));
            }
            if till > from {
                inside.push(Rect::from_x_y_ranges(from..=till, rect.y_range()));
            }
            x = x.max(till);
        }
        if x < rect.max.x {
            outside.push(Rect::from_x_y_ranges(x..=rect.max.x, rect.y_range()));
        }
    }
    (outside, inside)
}

/// [`paint_text_spoiler`] with the cover over the `selected` text in `select_color`
/// (`TextPalette::selectSpoilerFg`) and the rest in `color` (`Renderer::paintSpoilerRects`).
#[allow(clippy::too_many_arguments)]
pub fn paint_text_spoiler_selected(
    ctx: &Context,
    painter: &Painter,
    rects: &[Rect],
    selected: &[Rect],
    anchor: Pos2,
    color: Color32,
    select_color: Color32,
    opacity: f32,
) {
    let (outside, inside) = split_spoiler_rects(rects, selected);
    paint_text_spoiler(ctx, painter, &outside, anchor, color, opacity);
    paint_text_spoiler(ctx, painter, &inside, anchor, select_color, opacity);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_selection_splits_the_cover() {
        let line = |x0: f32, x1: f32| Rect::from_min_max(pos2(x0, 0.0), pos2(x1, 20.0));
        // A spoiler from 10 to 100, the selection from 40 to 70 of the same line.
        let (outside, inside) = split_spoiler_rects(&[line(10.0, 100.0)], &[line(40.0, 70.0)]);
        assert_eq!(outside, vec![line(10.0, 40.0), line(70.0, 100.0)]);
        assert_eq!(inside, vec![line(40.0, 70.0)]);

        // A selection on the next line does not touch it.
        let next = Rect::from_min_max(pos2(0.0, 20.0), pos2(200.0, 40.0));
        let (outside, inside) = split_spoiler_rects(&[line(10.0, 100.0)], &[next]);
        assert_eq!(outside, vec![line(10.0, 100.0)]);
        assert!(inside.is_empty());

        // A selection over the whole spoiler and beyond.
        let (outside, inside) = split_spoiler_rects(&[line(10.0, 100.0)], &[line(0.0, 300.0)]);
        assert!(outside.is_empty());
        assert_eq!(inside, vec![line(10.0, 100.0)]);
    }

    #[test]
    fn frames_hold_the_particles_alive_at_their_time() {
        let mess = TextSpoilerMess::generate(1.0);
        assert_eq!(mess.canvas_px(), 128);
        assert_eq!(mess.sprite, 4);
        for frame in [0, 17, 59] {
            let count = mess.frame_particle_count(frame);
            // 9000 particles, each alive 600 of 1980 ms: about 2727 per frame (plus the
            // wrapped copies at the canvas edges).
            assert!((2600..3200).contains(&count), "frame {frame}: {count}");
        }
        for dots in &mess.frames {
            for dot in dots {
                assert!(dot.x > -mess.sprite && dot.x < mess.canvas);
                assert!(dot.y > -mess.sprite && dot.y < mess.canvas);
                assert!(usize::from(dot.sprite) < SPRITES);
            }
        }
    }

    #[test]
    fn the_cover_is_the_same_on_every_run() {
        let a = TextSpoilerMess::generate(1.0);
        let b = TextSpoilerMess::generate(1.0);
        assert_eq!(a.frames, b.frames);
    }

    #[test]
    fn sprites_are_soft_dots_of_one_and_a_half_to_two_pixels() {
        let thin = sprite_alpha(2, 4, 1.0);
        let wide = sprite_alpha(0, 4, 1.0);
        let coverage = |a: &[u8]| a.iter().map(|v| u32::from(*v)).sum::<u32>() as f32 / 255.0;
        // A 1.5 x 1.5 rounded square, and a 2 x 1.5 one.
        assert!((1.6..2.3).contains(&coverage(&thin)), "{}", coverage(&thin));
        assert!(coverage(&wide) > coverage(&thin));
        // Nothing outside the sprite's one-pixel margin on the left and top.
        assert!(thin[0] == 0 && thin[1] == 0 && thin[4] == 0);
    }

    #[test]
    fn frames_follow_the_clock() {
        assert_eq!(text_spoiler_frame_at(0.0).0, 0);
        assert_eq!(text_spoiler_frame_at(0.034).0, 1);
        assert_eq!(text_spoiler_frame_at(1.98).0, 0);
        let (_, left) = text_spoiler_frame_at(0.040);
        assert_eq!(left, Duration::from_millis(26));
    }

    #[test]
    fn touching_rects_of_a_line_are_merged() {
        let a = Rect::from_min_max(pos2(0.0, 0.0), pos2(40.0, 18.0));
        let b = Rect::from_min_max(pos2(40.0, 0.0), pos2(70.0, 18.0));
        let c = Rect::from_min_max(pos2(0.0, 18.0), pos2(30.0, 36.0));
        let merged = merge_spoiler_rects(&[b, c, a]);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0],
            Rect::from_min_max(pos2(0.0, 0.0), pos2(70.0, 18.0))
        );
        assert_eq!(merged[1], c);
        // A bold run leaves a fraction of a pixel; a space between two spoilers does not join.
        let bold = Rect::from_min_max(pos2(70.6, 0.0), pos2(90.0, 18.0));
        let apart = Rect::from_min_max(pos2(93.5, 0.0), pos2(120.0, 18.0));
        let merged = merge_spoiler_rects(&[a, b, bold, apart]);
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert_eq!(merged[0].max.x, 90.0);
    }

    fn painted(rect: Rect) -> Vec<Shape> {
        let ctx = Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            paint_text_spoiler(
                ui.ctx(),
                &painter,
                &[rect],
                rect.min,
                Color32::from_rgb(0xA0, 0xAC, 0xB6),
                1.0,
            );
        });
        output.textures_delta.clear();
        output.shapes.into_iter().map(|s| s.shape).collect()
    }

    #[test]
    fn the_cover_is_dense_particles_with_no_box_behind_them() {
        let rect = Rect::from_min_size(pos2(10.0, 10.0), Vec2::new(200.0, 18.0));
        let shapes = painted(rect);
        let meshes: Vec<&Mesh> = shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Mesh(m) => Some(m.as_ref()),
                _ => None,
            })
            .collect();
        // Only the particle mesh: no filled rectangle behind it.
        assert_eq!(shapes.len(), 1, "{shapes:?}");
        assert_eq!(meshes.len(), 1);
        let quads = meshes[0].vertices.len() / 4;
        // tdesktop's density: about one particle per 6 square pixels (3600 px here).
        assert!((450..800).contains(&quads), "{quads} particles");
        for v in &meshes[0].vertices {
            assert!(rect.expand(0.01).contains(v.pos), "{:?} outside", v.pos);
        }
    }

    /// How soon the pass that paints a cover asks for the next one, with or without the pause.
    fn repaint_delay(paused: bool) -> Duration {
        let ctx = Context::default();
        let rect = Rect::from_min_size(pos2(10.0, 10.0), Vec2::new(80.0, 18.0));
        // egui asks for two more passes after the first one by itself; the fourth shows what
        // the cover asks for.
        let mut output = None;
        for _ in 0..4 {
            let mut pass = ctx.run_ui(egui::RawInput::default(), |ui| {
                if paused {
                    pause_text_spoilers_this_pass(ui.ctx());
                }
                let painter = ui.painter().clone();
                paint_text_spoiler(ui.ctx(), &painter, &[rect], rect.min, Color32::GRAY, 1.0);
            });
            pass.textures_delta.clear();
            output = Some(pass);
        }
        let Some(output) = output else {
            return Duration::ZERO;
        };
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay)
    }

    #[test]
    fn a_paused_cover_asks_for_no_next_frame() {
        // TGB-3029: `pausedSpoiler` while the window is inactive or the chat is covered.
        assert!(repaint_delay(false) <= Duration::from_millis(TEXT_SPOILER_FRAME_MS));
        assert_eq!(repaint_delay(true), Duration::MAX);
        // The pause lasts one pass: the next pass animates again unless paused again.
        let ctx = Context::default();
        let mut first = ctx.run_ui(egui::RawInput::default(), |ui| {
            pause_text_spoilers_this_pass(ui.ctx());
            assert!(text_spoilers_paused(ui.ctx()));
        });
        first.textures_delta.clear();
        let mut second = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(!text_spoilers_paused(ui.ctx()));
        });
        second.textures_delta.clear();
    }
}
