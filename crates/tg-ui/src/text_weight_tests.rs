//! Stroke weight of rendered text against tdesktop (TGB-3014).
//!
//! Renders one message line through the app's real text pipeline (bundled Open Sans,
//! [`ThemeManager::apply_to_egui`] visuals) and composites the glyph atlas the way
//! `egui_glow` does (premultiplied colours, blending in gamma space), then measures the
//! coverage of the glyph pixels like the reference captures were measured.
//!
//! Reference numbers: tdesktop on Windows draws text with the GDI font engine
//! (`core/launcher.cpp`, `windows:fontengine=gdi`) and system ClearType. GDI's own render of
//! the same line with the same `OpenSans-Regular.ttf` matched an official 7.4.6 capture at
//! 100 % exactly (ink 870.4, mean coverage 0.541, 54.7 % solid pixels).

use std::collections::HashMap;

use egui::epaint::{Primitive, Vertex};
use egui::{Color32, FontId, Pos2, RawInput, Rect, TextureId, Vec2};
use tg_theme::EmbeddedType;

use crate::theme::ThemeManager;

/// The measured line (first line of the uiparity/02 incoming bubble).
const LINE: &str = "Hi from Account B! Testing incoming message bubble and reply";

struct Texture {
    width: usize,
    height: usize,
    /// Premultiplied RGBA, as egui uploads it.
    pixels: Vec<[u8; 4]>,
}

/// One rendered line: RGB pixels of a `width` x `height` frame.
struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 3]>,
}

fn apply_delta(
    textures: &mut HashMap<TextureId, Texture>,
    id: TextureId,
    delta: &egui::epaint::ImageDelta,
) {
    let egui::ImageData::Color(image) = &delta.image;
    let (w, h) = (image.size[0], image.size[1]);
    let src: Vec<[u8; 4]> = image.pixels.iter().map(|p| p.to_array()).collect();
    match delta.pos {
        Some(pos) => {
            if let Some(tex) = textures.get_mut(&id) {
                for y in 0..h {
                    for x in 0..w {
                        let (dx, dy) = (pos[0] + x, pos[1] + y);
                        if dx < tex.width && dy < tex.height {
                            tex.pixels[dy * tex.width + dx] = src[y * w + x];
                        }
                    }
                }
            }
        }
        None => {
            textures.insert(
                id,
                Texture {
                    width: w,
                    height: h,
                    pixels: src,
                },
            );
        }
    }
}

fn sample(tex: &Texture, u: f32, v: f32) -> [f32; 4] {
    let fx = (u * tex.width as f32 - 0.5).clamp(0.0, (tex.width - 1) as f32);
    let fy = (v * tex.height as f32 - 0.5).clamp(0.0, (tex.height - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(tex.width - 1), (y0 + 1).min(tex.height - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let p = |x: usize, y: usize| tex.pixels[y * tex.width + x];
    let mut out = [0.0; 4];
    for (i, o) in out.iter_mut().enumerate() {
        let top = f32::from(p(x0, y0)[i]) * (1.0 - tx) + f32::from(p(x1, y0)[i]) * tx;
        let bottom = f32::from(p(x0, y1)[i]) * (1.0 - tx) + f32::from(p(x1, y1)[i]) * tx;
        *o = (top * (1.0 - ty) + bottom * ty) / 255.0;
    }
    out
}

/// `egui_glow`'s fragment shader and blend: `src = vertex_colour * texel` (both premultiplied,
/// gamma space), `dst = src + dst * (1 - src.a)`.
fn draw_triangle(frame: &mut Frame, ppp: f32, v: [Vertex; 3], tex: &Texture) {
    let p: Vec<[f32; 2]> = v.iter().map(|v| [v.pos.x * ppp, v.pos.y * ppp]).collect();
    let edge = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
        (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
    };
    let area = edge(p[0], p[1], p[2]);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = p
        .iter()
        .map(|q| q[0])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_x = (p.iter().map(|q| q[0]).fold(f32::MIN, f32::max).ceil() as usize).min(frame.width);
    let min_y = p
        .iter()
        .map(|q| q[1])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_y = (p.iter().map(|q| q[1]).fold(f32::MIN, f32::max).ceil() as usize).min(frame.height);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let c = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = edge(p[1], p[2], c) / area;
            let w1 = edge(p[2], p[0], c) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                continue;
            }
            let w = [w0, w1, w2];
            let u = (0..3).map(|i| v[i].uv.x * w[i]).sum::<f32>();
            let t = (0..3).map(|i| v[i].uv.y * w[i]).sum::<f32>();
            let texel = sample(tex, u, t);
            let mut src = [0.0f32; 4];
            for (ch, s) in src.iter_mut().enumerate() {
                let vc = (0..3)
                    .map(|i| f32::from(v[i].color.to_array()[ch]) * w[i])
                    .sum::<f32>()
                    / 255.0;
                *s = vc * texel[ch];
            }
            let dst = &mut frame.pixels[y * frame.width + x];
            for ch in 0..3 {
                dst[ch] = src[ch] * 255.0 + dst[ch] * (1.0 - src[3]);
            }
        }
    }
}

/// Render [`LINE`] at `size_pt` with the visuals of `theme`, at `ppp` physical px per point.
fn render_line(
    theme: EmbeddedType,
    text: &str,
    font: FontId,
    ppp: f32,
    fg: Color32,
    bg: Color32,
    tweak: impl Fn(&egui::Context),
) -> Frame {
    let ctx = egui::Context::default();
    ThemeManager::with_embedded(theme).apply_to_egui(&ctx);
    tweak(&ctx);
    let size_pt_frame = Vec2::new(
        font.size * text.len() as f32 * 0.6 + 16.0,
        font.size * 2.0 + 8.0,
    );
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size_pt_frame)),
        ..Default::default()
    };
    let mut textures = HashMap::new();
    let mut shapes = Vec::new();
    for _ in 0..3 {
        let mut output = ctx.run_ui(input.clone(), |ui| {
            ui.ctx().set_pixels_per_point(ppp);
            ui.painter().text(
                Pos2::new(8.0, 4.0),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                fg,
            );
        });
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                apply_delta(&mut textures, *id, delta);
            }
        }
        output.textures_delta.clear();
        shapes = output.shapes;
    }
    let primitives = ctx.tessellate(shapes, ppp);
    let width = (size_pt_frame.x * ppp).round() as usize;
    let height = (size_pt_frame.y * ppp).round() as usize;
    let bg = [f32::from(bg.r()), f32::from(bg.g()), f32::from(bg.b())];
    let mut frame = Frame {
        width,
        height,
        pixels: vec![bg; width * height],
    };
    for clipped in primitives {
        if let Primitive::Mesh(mesh) = clipped.primitive
            && let Some(tex) = textures.get(&mesh.texture_id)
        {
            for tri in mesh.indices.as_chunks::<3>().0 {
                let v = [0, 1, 2].map(|i| mesh.vertices[tri[i] as usize]);
                draw_triangle(&mut frame, ppp, v, tex);
            }
        }
    }
    frame
}

/// Stroke-weight metrics (the same as the measurement script used for TGB-3014), for text of colour `fg` on
/// the flat background `bg`.
#[derive(Debug, Clone, Copy)]
struct Weight {
    /// Sum of coverage (stroke darkness x stroke length).
    ink: f32,
    /// Ink extent in px (columns with coverage > 0.15).
    width: usize,
    /// Mean coverage of glyph pixels (coverage > 0.05).
    mean_cov: f32,
    /// Share of glyph pixels with coverage > 0.5, in %.
    solid: f32,
}

impl Weight {
    fn ink_per_px(self) -> f32 {
        self.ink / self.width.max(1) as f32
    }
}

fn measure(frame: &Frame, fg: Color32, bg: Color32) -> Weight {
    let fg = [f32::from(fg.r()), f32::from(fg.g()), f32::from(fg.b())];
    let bg = [f32::from(bg.r()), f32::from(bg.g()), f32::from(bg.b())];
    let cov: Vec<f32> = frame
        .pixels
        .iter()
        .map(|p| {
            let mut sum = 0.0;
            for ch in 0..3 {
                let span = fg[ch] - bg[ch];
                if span.abs() > 1.0 {
                    sum += ((p[ch].round() - bg[ch]) / span).clamp(0.0, 1.0);
                }
            }
            sum / 3.0
        })
        .collect();
    let glyph: Vec<f32> = cov.iter().copied().filter(|c| *c > 0.05).collect();
    let cols: Vec<usize> = (0..frame.width)
        .filter(|x| (0..frame.height).any(|y| cov[y * frame.width + x] > 0.15))
        .collect();
    let width = match (cols.first(), cols.last()) {
        (Some(a), Some(b)) => b - a + 1,
        _ => 0,
    };
    Weight {
        ink: cov.iter().sum(),
        width,
        mean_cov: glyph.iter().sum::<f32>() / glyph.len().max(1) as f32,
        solid: glyph.iter().filter(|c| **c > 0.5).count() as f32 * 100.0
            / glyph.len().max(1) as f32,
    }
}

fn save_png(frame: &Frame, path: &std::path::Path) {
    let mut img = image::RgbImage::new(frame.width as u32, frame.height as u32);
    for (i, p) in frame.pixels.iter().enumerate() {
        img.put_pixel(
            (i % frame.width) as u32,
            (i / frame.width) as u32,
            image::Rgb(p.map(|c| c.round().clamp(0.0, 255.0) as u8)),
        );
    }
    let _ = img.save(path);
}

fn theme_colors(theme: EmbeddedType) -> (Color32, Color32) {
    use crate::theme::Colors as _;
    let manager = ThemeManager::with_embedded(theme);
    (manager.color("historyTextInFg"), manager.color("msgInBg"))
}

/// One measured sample: tdesktop's ink per px of line width at a font pixel size.
struct Reference {
    theme: EmbeddedType,
    semibold: bool,
    ppp: f32,
    /// tdesktop's font size in px at this scale (`ConvertScale(13)`: 13 at 100 %, 19 at 150 %).
    size_px: f32,
    ink_per_px: f32,
}

/// GDI ClearType renders of Open Sans with tdesktop's `OpenSans-*.ttf`, black `historyTextInFg`
/// on `msgInBg` (classic) and `#f5f5f5` on `#182533` (night). The 13 px regular day render
/// equals the official 7.4.6 capture of the same line (uiparity/02) to the pixel.
const REFERENCES: [Reference; 8] = [
    Reference {
        theme: EmbeddedType::Default,
        semibold: false,
        ppp: 1.0,
        size_px: 13.0,
        ink_per_px: 2.333,
    },
    Reference {
        theme: EmbeddedType::Default,
        semibold: false,
        ppp: 1.5,
        size_px: 19.0,
        ink_per_px: 3.249,
    },
    Reference {
        theme: EmbeddedType::Default,
        semibold: true,
        ppp: 1.0,
        size_px: 13.0,
        ink_per_px: 2.882,
    },
    Reference {
        theme: EmbeddedType::Default,
        semibold: true,
        ppp: 1.5,
        size_px: 19.0,
        ink_per_px: 4.804,
    },
    Reference {
        theme: EmbeddedType::Night,
        semibold: false,
        ppp: 1.0,
        size_px: 13.0,
        ink_per_px: 2.573,
    },
    Reference {
        theme: EmbeddedType::Night,
        semibold: false,
        ppp: 1.5,
        size_px: 19.0,
        ink_per_px: 3.538,
    },
    Reference {
        theme: EmbeddedType::Night,
        semibold: true,
        ppp: 1.0,
        size_px: 13.0,
        ink_per_px: 3.133,
    },
    Reference {
        theme: EmbeddedType::Night,
        semibold: true,
        ppp: 1.5,
        size_px: 19.0,
        ink_per_px: 5.086,
    },
];

/// The chat-list name used for the semibold samples.
const NAME: &str = "Saved Messages";

/// Render a reference sample with the app's own visuals; returns (frame, weight).
fn render_reference(r: &Reference, tweak: impl Fn(&egui::Context)) -> (Frame, Weight) {
    let (fg, bg) = theme_colors(r.theme);
    let (text, family) = if r.semibold {
        (NAME, crate::fonts::family_semibold())
    } else {
        (LINE, crate::fonts::family_regular())
    };
    let frame = render_line(
        r.theme,
        text,
        FontId::new(13.0, family),
        r.ppp,
        fg,
        bg,
        tweak,
    );
    let weight = measure(&frame, fg, bg);
    (frame, weight)
}

/// Relative stroke weight against tdesktop: ink per px of width per px of font size, so the
/// 19.5 px egui renders at 150 % compare with tdesktop's 19 px.
fn relative_weight(r: &Reference, w: Weight) -> f32 {
    (w.ink_per_px() / (13.0 * r.ppp)) / (r.ink_per_px / r.size_px)
}

/// Grayscale coverage cannot match ClearType's hinting at every size; within this band the
/// strokes look the same weight side by side.
const WEIGHT_TOLERANCE: f32 = 0.08;

#[test]
fn text_stroke_weight_matches_tdesktop_in_day_and_night_themes_at_100_and_150_percent() {
    let mut failures = Vec::new();
    for r in &REFERENCES {
        let (_, w) = render_reference(r, |_| {});
        let rel = relative_weight(r, w);
        if (rel - 1.0).abs() > WEIGHT_TOLERANCE {
            failures.push(format!(
                "{:?} {} at {}%: {:+.1}% ink per px",
                r.theme,
                if r.semibold { "semibold" } else { "regular" },
                r.ppp * 100.0,
                (rel - 1.0) * 100.0
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "stroke weight differs from tdesktop: {failures:#?}"
    );
}

#[test]
fn palette_visuals_pick_the_tdesktop_coverage_curve() {
    use egui::epaint::FontColorTransferFunction as Tf;
    let day = crate::theme::palette_visuals(&ThemeManager::with_embedded(EmbeddedType::Default));
    assert_eq!(
        day.text_options.color_transfer_function,
        Tf::Gamma(crate::fonts::LIGHT_TEXT_COVERAGE_GAMMA)
    );
    let night = crate::theme::palette_visuals(&ThemeManager::with_embedded(EmbeddedType::Night));
    assert_eq!(night.text_options.color_transfer_function, Tf::Off);
}

/// Snapshot tool for the report: `TGFORK_FONT_WEIGHT_DIR=<dir> cargo test -p tg-ui
/// text_weight_dump -- --ignored --nocapture` prints every sample and writes one PNG each,
/// with the app's visuals (`app`) and with egui's stock curves (`egui`, the old look).
#[test]
#[ignore = "measurement tool, writes PNGs"]
fn text_weight_dump() {
    let out = std::env::var_os("TGFORK_FONT_WEIGHT_DIR").map(std::path::PathBuf::from);
    for r in &REFERENCES {
        for variant in ["app", "egui"] {
            let (frame, w) = render_reference(r, |ctx| {
                if variant == "egui" {
                    ctx.all_styles_mut(|s| {
                        s.visuals.text_options.color_transfer_function = if s.visuals.dark_mode {
                            egui::epaint::FontColorTransferFunction::DARK_MODE_DEFAULT
                        } else {
                            egui::epaint::FontColorTransferFunction::LIGHT_MODE_DEFAULT
                        };
                    });
                }
            });
            let label = if r.semibold { "semibold" } else { "regular" };
            let pct = (r.ppp * 100.0) as u32;
            println!(
                "{:?} {label} {pct}% {variant}: ink {:.1} width {} ink/w {:.3} mean_cov {:.3} solid {:.1}% vs tdesktop {:+.1}%",
                r.theme,
                w.ink,
                w.width,
                w.ink_per_px(),
                w.mean_cov,
                w.solid,
                (relative_weight(r, w) - 1.0) * 100.0
            );
            if let Some(dir) = &out {
                save_png(
                    &frame,
                    &dir.join(format!("{:?}_{label}_{pct}_{variant}.png", r.theme)),
                );
            }
        }
    }
}
