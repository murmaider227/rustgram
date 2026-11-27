//! Headless software rasterizer for egui frames, used by tests that write documentation screenshots
//! (`crates/tg-app/docs/*.png`). Compiled only for tests or with the `soft-raster` feature, so it
//! never ships in the app.
//!
//! egui hands out premultiplied sRGBA vertex colours and texture pixels; the rasterizer keeps them
//! premultiplied, samples textures bilinearly at texel centres and blends with "source over" onto an
//! opaque background.

use std::collections::HashMap;
use std::path::Path;

use egui::epaint::{ImageDelta, Primitive, Vertex};
use egui::{ClippedPrimitive, Color32, Context, ImageData, Pos2, RawInput, Rect, TextureId, Vec2};

/// Opaque RGBA8 image rendered from an egui frame.
pub struct RgbaFrame {
    /// Width in physical pixels.
    pub width: usize,
    /// Height in physical pixels.
    pub height: usize,
    /// Row-major pixels; alpha is always 255.
    pub pixels: Vec<[u8; 4]>,
}

impl RgbaFrame {
    /// The pixels as one flat `width * height * 4` byte buffer.
    pub fn to_rgba8(&self) -> Vec<u8> {
        self.pixels.iter().flatten().copied().collect()
    }

    /// Encodes the frame as a PNG file in memory.
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let encoded = u32::try_from(self.width)
            .ok()
            .zip(u32::try_from(self.height).ok())
            .and_then(|(w, h)| image::RgbaImage::from_raw(w, h, self.to_rgba8()))
            .map(|img| img.write_to(&mut out, image::ImageFormat::Png));
        match encoded {
            Some(Ok(())) => out.into_inner(),
            _ => Vec::new(),
        }
    }

    /// Writes the frame as a PNG file, creating the parent directory.
    pub fn save_png(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let png = self.to_png();
        if png.is_empty() {
            return Err(std::io::Error::other("PNG encoding failed"));
        }
        std::fs::write(path, png)
    }
}

/// How to run and rasterize the frame.
#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Screen size in egui points.
    pub logical_size: Vec2,
    /// Physical pixels per point.
    pub pixels_per_point: f32,
    /// Number of egui passes before the captured one is tessellated (fonts and sizes settle).
    pub passes: u32,
    /// Opaque colour the frame starts from.
    pub background: Color32,
    /// Wrap `draw` in a frameless `CentralPanel` (the documentation screenshots do).
    pub central_panel: bool,
}

impl RenderOptions {
    /// The documentation screenshot setup: three passes, white background, frameless central panel.
    pub fn docs(logical_w: u32, logical_h: u32, pixels_per_point: f32) -> Self {
        Self {
            logical_size: Vec2::new(logical_w as f32, logical_h as f32),
            pixels_per_point,
            passes: 3,
            background: Color32::WHITE,
            central_panel: true,
        }
    }
}

/// Runs `draw` in `ctx` and rasterizes the last pass.
pub fn render(
    ctx: &Context,
    options: &RenderOptions,
    mut draw: impl FnMut(&mut egui::Ui),
) -> RgbaFrame {
    let ppp = options.pixels_per_point;
    let mut textures = TextureStore::default();
    let mut primitives: Vec<ClippedPrimitive> = Vec::new();
    for pass in 0..options.passes.max(1) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, options.logical_size)),
            time: Some(f64::from(pass) * 0.016),
            ..RawInput::default()
        };
        ctx.set_pixels_per_point(ppp);
        let mut output = ctx.run_ui(input, |ui| {
            if options.central_panel {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| draw(ui));
            } else {
                draw(ui);
            }
        });
        textures.apply(&output.textures_delta);
        output.textures_delta.clear();
        primitives = ctx.tessellate(std::mem::take(&mut output.shapes), ppp);
    }

    let width = (options.logical_size.x * ppp).round().max(0.0) as usize;
    let height = (options.logical_size.y * ppp).round().max(0.0) as usize;
    let mut frame = RgbaFrame {
        width,
        height,
        pixels: vec![options.background.to_opaque().to_array(); width * height],
    };
    for clipped in &primitives {
        if let Primitive::Mesh(mesh) = &clipped.primitive {
            rasterize_mesh(
                &mut frame,
                clipped.clip_rect,
                ppp,
                mesh,
                textures.get(mesh.texture_id),
            );
        }
    }
    frame
}

/// [`render`] with [`RenderOptions::docs`], encoded as PNG.
pub fn render_egui_to_png(
    ctx: &Context,
    logical_w: u32,
    logical_h: u32,
    pixels_per_point: f32,
    draw: impl FnMut(&mut egui::Ui),
) -> Vec<u8> {
    render(
        ctx,
        &RenderOptions::docs(logical_w, logical_h, pixels_per_point),
        draw,
    )
    .to_png()
}

struct Texture {
    width: usize,
    height: usize,
    /// Premultiplied sRGBA (`Color32::to_array`).
    pixels: Vec<[u8; 4]>,
}

#[derive(Default)]
struct TextureStore {
    textures: HashMap<TextureId, Texture>,
}

impl TextureStore {
    fn get(&self, id: TextureId) -> Option<&Texture> {
        self.textures.get(&id)
    }

    fn apply(&mut self, delta: &egui::TexturesDelta) {
        for (id, deltas) in &delta.set {
            for image_delta in deltas {
                self.apply_one(*id, image_delta);
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    fn apply_one(&mut self, id: TextureId, delta: &ImageDelta) {
        let ImageData::Color(image) = &delta.image;
        let [w, h] = image.size;
        let Some([ox, oy]) = delta.pos else {
            self.textures.insert(
                id,
                Texture {
                    width: w,
                    height: h,
                    pixels: image.pixels.iter().map(|c| c.to_array()).collect(),
                },
            );
            return;
        };
        let Some(tex) = self.textures.get_mut(&id) else {
            return;
        };
        for (src_row, y) in image.pixels.chunks_exact(w.max(1)).zip(oy..tex.height) {
            let row_start = y * tex.width;
            for (src, x) in src_row.iter().zip(ox..tex.width) {
                tex.pixels[row_start + x] = src.to_array();
            }
        }
    }
}

fn rasterize_mesh(
    frame: &mut RgbaFrame,
    clip_rect: Rect,
    ppp: f32,
    mesh: &egui::Mesh,
    texture: Option<&Texture>,
) {
    let clip_min_x = (clip_rect.min.x * ppp).floor().max(0.0) as i64;
    let clip_min_y = (clip_rect.min.y * ppp).floor().max(0.0) as i64;
    let clip_max_x = (clip_rect.max.x * ppp).ceil().min(frame.width as f32) as i64;
    let clip_max_y = (clip_rect.max.y * ppp).ceil().min(frame.height as f32) as i64;
    if clip_min_x >= clip_max_x || clip_min_y >= clip_max_y {
        return;
    }
    let clip = (clip_min_x, clip_min_y, clip_max_x, clip_max_y);
    for tri in mesh.indices.as_chunks::<3>().0 {
        let vertex = |i: u32| mesh.vertices.get(i as usize).copied();
        if let (Some(v0), Some(v1), Some(v2)) = (vertex(tri[0]), vertex(tri[1]), vertex(tri[2])) {
            rasterize_triangle(frame, clip, ppp, [v0, v1, v2], texture);
        }
    }
}

fn rasterize_triangle(
    frame: &mut RgbaFrame,
    (clip_min_x, clip_min_y, clip_max_x, clip_max_y): (i64, i64, i64, i64),
    ppp: f32,
    [v0, v1, v2]: [Vertex; 3],
    texture: Option<&Texture>,
) {
    let (x0, y0) = (v0.pos.x * ppp, v0.pos.y * ppp);
    let (x1, y1) = (v1.pos.x * ppp, v1.pos.y * ppp);
    let (x2, y2) = (v2.pos.x * ppp, v2.pos.y * ppp);
    let area = (x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;

    let bx0 = (x0.min(x1).min(x2).floor() as i64).max(clip_min_x);
    let bx1 = (x0.max(x1).max(x2).ceil() as i64).min(clip_max_x - 1);
    let by0 = (y0.min(y1).min(y2).floor() as i64).max(clip_min_y);
    let by1 = (y0.max(y1).max(y2).ceil() as i64).min(clip_max_y - 1);
    if bx0 > bx1 || by0 > by1 {
        return;
    }

    let colors = [
        v0.color.to_array(),
        v1.color.to_array(),
        v2.color.to_array(),
    ];
    // Fill rule: a pixel centre exactly on an edge belongs to one of the two triangles sharing
    // that edge, else the shared diagonal of every quad is blended twice.
    let owned = [
        owns_edge((x1, y1), (x2, y2), (x0, y0)),
        owns_edge((x2, y2), (x0, y0), (x1, y1)),
        owns_edge((x0, y0), (x1, y1), (x2, y2)),
    ];
    let inside = |w: f32, owned: bool| w > EDGE_EPSILON || (w >= -EDGE_EPSILON && owned);
    for py in by0..=by1 {
        let cy = py as f32 + 0.5;
        for px in bx0..=bx1 {
            let cx = px as f32 + 0.5;
            let w0 = ((x1 - cx) * (y2 - cy) - (y1 - cy) * (x2 - cx)) * inv_area;
            let w1 = ((x2 - cx) * (y0 - cy) - (y2 - cy) * (x0 - cx)) * inv_area;
            let w2 = ((x0 - cx) * (y1 - cy) - (y0 - cy) * (x1 - cx)) * inv_area;
            if !(inside(w0, owned[0]) && inside(w1, owned[1]) && inside(w2, owned[2])) {
                continue;
            }
            let mut src = [0.0f32; 4];
            for (ch, out) in src.iter_mut().enumerate() {
                *out = (w0 * f32::from(colors[0][ch])
                    + w1 * f32::from(colors[1][ch])
                    + w2 * f32::from(colors[2][ch]))
                .clamp(0.0, 255.0);
            }
            if let Some(tex) = texture {
                let u = (w0 * v0.uv.x + w1 * v1.uv.x + w2 * v2.uv.x).clamp(0.0, 1.0);
                let v = (w0 * v0.uv.y + w1 * v1.uv.y + w2 * v2.uv.y).clamp(0.0, 1.0);
                let texel = sample_bilinear(tex, u, v);
                for (out, t) in src.iter_mut().zip(texel) {
                    *out *= t / 255.0;
                }
            }
            if src[3] <= 0.5 {
                continue;
            }
            // Both coordinates are inside the clip rect, which is inside the frame.
            let dst = &mut frame.pixels[py as usize * frame.width + px as usize];
            let inv_a = 1.0 - src[3] / 255.0;
            for ch in 0..3 {
                dst[ch] = (src[ch] + f32::from(dst[ch]) * inv_a)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            dst[3] = 255;
        }
    }
}

/// Barycentric slack for "exactly on the edge" (float rounding of the edge functions).
const EDGE_EPSILON: f32 = 1e-5;

/// Whether the edge `a`-`b` of a triangle whose third vertex is `c` owns the pixel centres lying
/// on it: the one whose inward normal points down, or right when the edge is vertical. Of two
/// triangles sharing an edge exactly one owns it.
fn owns_edge(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let (mut nx, mut ny) = (-(b.1 - a.1), b.0 - a.0);
    if nx * (c.0 - a.0) + ny * (c.1 - a.1) < 0.0 {
        (nx, ny) = (-nx, -ny);
    }
    ny > 0.0 || (ny == 0.0 && nx > 0.0)
}

fn sample_bilinear(tex: &Texture, u: f32, v: f32) -> [f32; 4] {
    if tex.width == 0 || tex.height == 0 || tex.pixels.len() < tex.width * tex.height {
        return [255.0; 4];
    }
    let fx = (u * tex.width as f32 - 0.5).clamp(0.0, (tex.width - 1) as f32);
    let fy = (v * tex.height as f32 - 0.5).clamp(0.0, (tex.height - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let x1 = (x0 + 1).min(tex.width - 1);
    let y1 = (y0 + 1).min(tex.height - 1);
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let at = |x: usize, y: usize| tex.pixels[y * tex.width + x];
    let (p00, p10, p01, p11) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let mut out = [0.0f32; 4];
    for (ch, o) in out.iter_mut().enumerate() {
        let top = f32::from(p00[ch]) * (1.0 - tx) + f32::from(p10[ch]) * tx;
        let bottom = f32::from(p01[ch]) * (1.0 - tx) + f32::from(p11[ch]) * tx;
        *o = top * (1.0 - ty) + bottom * ty;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_a_rect_at_physical_scale_and_keeps_the_background_elsewhere() {
        let ctx = Context::default();
        let options = RenderOptions {
            background: Color32::from_rgb(10, 20, 30),
            central_panel: false,
            ..RenderOptions::docs(20, 10, 2.0)
        };
        let frame = render(&ctx, &options, |ui| {
            ui.painter().rect_filled(
                Rect::from_min_size(Pos2::new(2.0, 2.0), Vec2::new(4.0, 4.0)),
                0.0,
                Color32::from_rgb(200, 0, 0),
            );
        });
        assert_eq!((frame.width, frame.height), (40, 20));
        assert_eq!(frame.pixels[8 * 40 + 8], [200, 0, 0, 255]);
        assert_eq!(frame.pixels[0], [10, 20, 30, 255]);
        assert_eq!(frame.pixels[19 * 40 + 39], [10, 20, 30, 255]);
    }

    #[test]
    fn blends_translucent_fill_over_the_background() {
        let ctx = Context::default();
        let options = RenderOptions {
            central_panel: false,
            ..RenderOptions::docs(4, 4, 1.0)
        };
        let frame = render(&ctx, &options, |ui| {
            ui.painter().rect_filled(
                Rect::from_min_size(Pos2::ZERO, Vec2::splat(4.0)),
                0.0,
                Color32::from_black_alpha(128),
            );
        });
        let [r, g, b, a] = frame.pixels[5];
        assert_eq!(a, 255);
        assert!((126..=128).contains(&r) && r == g && g == b, "{r} {g} {b}");
    }

    #[test]
    fn text_draws_ink_through_the_font_atlas() {
        let ctx = Context::default();
        ctx.set_visuals(egui::Visuals::light());
        let png = render_egui_to_png(&ctx, 80, 30, 1.5, |ui| {
            ui.label("Rustgram");
        });
        assert!(png.starts_with(&[137, 80, 78, 71]));
        let frame = render(&ctx, &RenderOptions::docs(80, 30, 1.5), |ui| {
            ui.label("Rustgram");
        });
        let dark = frame.pixels.iter().filter(|p| p[0] < 128).count();
        assert!(dark > 20, "text left {dark} dark pixels");
    }

    #[test]
    fn shared_quad_diagonal_is_blended_once() {
        // The pixel centre (1.5, 1.5) lies on the quad's diagonal: both triangles touch it.
        let ctx = Context::default();
        let options = RenderOptions {
            central_panel: false,
            ..RenderOptions::docs(4, 4, 1.0)
        };
        let frame = render(&ctx, &options, |ui| {
            ui.painter().rect_filled(
                Rect::from_min_size(Pos2::ZERO, Vec2::splat(4.0)),
                0.0,
                Color32::from_black_alpha(128),
            );
        });
        for y in 1..3 {
            for x in 1..3 {
                assert_eq!(frame.pixels[y * 4 + x][0], 127, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn partial_texture_update_outside_the_texture_is_ignored() {
        let mut store = TextureStore::default();
        let id = TextureId::Managed(7);
        let full = ImageDelta::full(
            egui::ColorImage::new([2, 2], vec![Color32::RED; 4]),
            egui::TextureOptions::LINEAR,
        );
        store.apply_one(id, &full);
        let patch = ImageDelta::partial(
            [1, 1],
            egui::ColorImage::new([3, 3], vec![Color32::BLUE; 9]),
            egui::TextureOptions::LINEAR,
        );
        store.apply_one(id, &patch);
        let tex = store.get(id).map(|t| t.pixels.clone());
        assert_eq!(
            tex,
            Some(vec![
                Color32::RED.to_array(),
                Color32::RED.to_array(),
                Color32::RED.to_array(),
                Color32::BLUE.to_array(),
            ])
        );
    }
}
