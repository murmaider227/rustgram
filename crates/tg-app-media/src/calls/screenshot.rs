//! Headless offscreen `egui` software mesh rasterizer and pure-Rust PNG encoder used by tests
//! to capture pixel-accurate screenshots of the Calls UI into `crates/tg-app/docs/calls*.png`.

use std::collections::HashMap;
use std::path::Path;

use egui::{
    ClippedPrimitive, Context, ImageData, Pos2, RawInput, Rect, TextureId, epaint::Primitive,
};

/// RGBA8 image buffer rendered offscreen from an [`egui::Context`] frame.
pub struct OffscreenBuffer {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Premultiplied/straight sRGBA pixel bytes (`width * height * 4`).
    pub pixels: Vec<u8>,
}

struct CpuTexture {
    width: usize,
    height: usize,
    rgba: Vec<[u8; 4]>,
}

/// Renders one `egui` UI frame offscreen at `(width, height)` with 1.0 DPI scale and returns the
/// rasterized RGBA image.
pub fn render_egui_offscreen(
    width: usize,
    height: usize,
    bg: egui::Color32,
    mut draw: impl FnMut(&mut egui::Ui),
) -> OffscreenBuffer {
    let ctx = Context::default();
    tg_ui::fonts::ensure_tdesktop_fonts(&ctx);
    let mut textures: HashMap<TextureId, CpuTexture> = HashMap::new();
    let mut last_primitives: Vec<ClippedPrimitive> = Vec::new();

    // Run 2 passes so egui lays out fonts, sizes, and animations cleanly.
    for frame in 0..2 {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::Vec2::new(width as f32, height as f32),
            )),
            time: Some(frame as f64 * 0.016),
            ..RawInput::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            draw(ui);
        });

        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                apply_texture_delta(&mut textures, *id, delta);
            }
        }
        last_primitives = ctx.tessellate(output.shapes, 1.0);
        for id in &output.textures_delta.free {
            textures.remove(id);
        }
        output.textures_delta.clear();
    }

    let mut pixels = vec![0u8; width * height * 4];
    for chunk in pixels.as_chunks_mut::<4>().0 {
        chunk[0] = bg.r();
        chunk[1] = bg.g();
        chunk[2] = bg.b();
        chunk[3] = 255;
    }

    for clipped in &last_primitives {
        if let Primitive::Mesh(mesh) = &clipped.primitive {
            rasterize_mesh(
                &mut pixels,
                width,
                height,
                clipped.clip_rect,
                mesh,
                textures.get(&mesh.texture_id),
            );
        }
    }

    OffscreenBuffer {
        width,
        height,
        pixels,
    }
}

fn apply_texture_delta(
    textures: &mut HashMap<TextureId, CpuTexture>,
    id: TextureId,
    delta: &egui::epaint::ImageDelta,
) {
    let (patch_w, patch_h, patch_rgba): (usize, usize, Vec<[u8; 4]>) = match &delta.image {
        ImageData::Color(c) => (
            c.size[0],
            c.size[1],
            c.pixels
                .iter()
                .map(|p| [p.r(), p.g(), p.b(), p.a()])
                .collect(),
        ),
    };

    if let Some(pos) = delta.pos {
        if let Some(tex) = textures.get_mut(&id) {
            for py in 0..patch_h {
                let dy = pos[1] + py;
                if dy >= tex.height {
                    continue;
                }
                for px in 0..patch_w {
                    let dx = pos[0] + px;
                    if dx >= tex.width {
                        continue;
                    }
                    tex.rgba[dy * tex.width + dx] = patch_rgba[py * patch_w + px];
                }
            }
        }
    } else {
        textures.insert(
            id,
            CpuTexture {
                width: patch_w,
                height: patch_h,
                rgba: patch_rgba,
            },
        );
    }
}

fn rasterize_mesh(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    clip_rect: Rect,
    mesh: &egui::Mesh,
    texture: Option<&CpuTexture>,
) {
    let clip_min_x = (clip_rect.min.x.floor() as i32).max(0);
    let clip_min_y = (clip_rect.min.y.floor() as i32).max(0);
    let clip_max_x = (clip_rect.max.x.ceil() as i32).min(width as i32);
    let clip_max_y = (clip_rect.max.y.ceil() as i32).min(height as i32);
    if clip_min_x >= clip_max_x || clip_min_y >= clip_max_y {
        return;
    }

    for tri in mesh.indices.as_chunks::<3>().0 {
        let v0 = mesh.vertices[tri[0] as usize];
        let v1 = mesh.vertices[tri[1] as usize];
        let v2 = mesh.vertices[tri[2] as usize];

        let min_x = v0.pos.x.min(v1.pos.x).min(v2.pos.x).floor() as i32;
        let max_x = v0.pos.x.max(v1.pos.x).max(v2.pos.x).ceil() as i32;
        let min_y = v0.pos.y.min(v1.pos.y).min(v2.pos.y).floor() as i32;
        let max_y = v0.pos.y.max(v1.pos.y).max(v2.pos.y).ceil() as i32;

        let x0 = min_x.max(clip_min_x);
        let x1 = max_x.min(clip_max_x - 1);
        let y0 = min_y.max(clip_min_y);
        let y1 = max_y.min(clip_max_y - 1);
        if x0 > x1 || y0 > y1 {
            continue;
        }

        let area = edge_fn(v0.pos, v1.pos, v2.pos);
        if area.abs() < 1e-6 {
            continue;
        }
        let inv_area = 1.0 / area;

        for y in y0..=y1 {
            let py = y as f32 + 0.5;
            for x in x0..=x1 {
                let px = x as f32 + 0.5;
                let p = Pos2::new(px, py);
                let w0 = edge_fn(v1.pos, v2.pos, p) * inv_area;
                let w1 = edge_fn(v2.pos, v0.pos, p) * inv_area;
                let w2 = edge_fn(v0.pos, v1.pos, p) * inv_area;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }

                // Interpolate premultiplied vertex colors (`epaint::Color32` is premultiplied sRGBA).
                let vr = (w0 * f32::from(v0.color.r())
                    + w1 * f32::from(v1.color.r())
                    + w2 * f32::from(v2.color.r()))
                    / 255.0;
                let vg = (w0 * f32::from(v0.color.g())
                    + w1 * f32::from(v1.color.g())
                    + w2 * f32::from(v2.color.g()))
                    / 255.0;
                let vb = (w0 * f32::from(v0.color.b())
                    + w1 * f32::from(v1.color.b())
                    + w2 * f32::from(v2.color.b()))
                    / 255.0;
                let va = (w0 * f32::from(v0.color.a())
                    + w1 * f32::from(v1.color.a())
                    + w2 * f32::from(v2.color.a()))
                    / 255.0;

                let (sr, sg, sb, sa) = if let Some(tex) = texture {
                    let u = (w0 * v0.uv.x + w1 * v1.uv.x + w2 * v2.uv.x).clamp(0.0, 1.0);
                    let v = (w0 * v0.uv.y + w1 * v1.uv.y + w2 * v2.uv.y).clamp(0.0, 1.0);
                    let [tr, tg, tb, ta] = sample_bilinear(tex, u, v);
                    (vr * tr, vg * tg, vb * tb, va * ta)
                } else {
                    (vr, vg, vb, va)
                };

                let dst_idx = ((y as usize) * width + (x as usize)) * 4;
                let dr = f32::from(pixels[dst_idx]) / 255.0;
                let dg = f32::from(pixels[dst_idx + 1]) / 255.0;
                let db = f32::from(pixels[dst_idx + 2]) / 255.0;
                let inv_a = (1.0 - sa).clamp(0.0, 1.0);

                pixels[dst_idx] = ((sr + dr * inv_a).clamp(0.0, 1.0) * 255.0).round() as u8;
                pixels[dst_idx + 1] = ((sg + dg * inv_a).clamp(0.0, 1.0) * 255.0).round() as u8;
                pixels[dst_idx + 2] = ((sb + db * inv_a).clamp(0.0, 1.0) * 255.0).round() as u8;
                pixels[dst_idx + 3] = 255;
            }
        }
    }
}

fn edge_fn(a: Pos2, b: Pos2, c: Pos2) -> f32 {
    (c.x - a.x) * (b.y - a.y) - (c.y - a.y) * (b.x - a.x)
}

fn sample_bilinear(tex: &CpuTexture, u: f32, v: f32) -> [f32; 4] {
    if tex.width == 0 || tex.height == 0 {
        return [1.0; 4];
    }
    let fx = u * (tex.width as f32 - 1.0).max(0.0);
    let fy = v * (tex.height as f32 - 1.0).max(0.0);
    let x0 = (fx.floor() as usize).min(tex.width - 1);
    let y0 = (fy.floor() as usize).min(tex.height - 1);
    let x1 = (x0 + 1).min(tex.width - 1);
    let y1 = (y0 + 1).min(tex.height - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;

    let c00 = tex.rgba[y0 * tex.width + x0];
    let c10 = tex.rgba[y0 * tex.width + x1];
    let c01 = tex.rgba[y1 * tex.width + x0];
    let c11 = tex.rgba[y1 * tex.width + x1];

    let mut out = [0.0_f32; 4];
    for ch in 0..4 {
        let top = f32::from(c00[ch]) * (1.0 - tx) + f32::from(c10[ch]) * tx;
        let bot = f32::from(c01[ch]) * (1.0 - tx) + f32::from(c11[ch]) * tx;
        out[ch] = (top * (1.0 - ty) + bot * ty) / 255.0;
    }
    out
}

/// Writes an RGBA8 buffer to `path` as a standard valid PNG file using pure Rust (stored zlib
/// blocks + IEEE CRC-32 + Adler-32, zero external dependencies).
pub fn save_rgba_png(path: &Path, buf: &OffscreenBuffer) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut raw = Vec::with_capacity(buf.height * (1 + buf.width * 4));
    for y in 0..buf.height {
        raw.push(0u8); // Filter type 0 (None)
        let row_start = y * buf.width * 4;
        raw.extend_from_slice(&buf.pixels[row_start..row_start + buf.width * 4]);
    }

    let zlib = zlib_store(&raw);
    let mut out = Vec::with_capacity(64 + zlib.len());
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // PNG signature

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(buf.width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(buf.height as u32).to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_png_chunk(&mut out, b"IHDR", &ihdr);
    write_png_chunk(&mut out, b"IDAT", &zlib);
    write_png_chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out)
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(chunk_type);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn zlib_store(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 6 + (data.len() / 65_535 + 1) * 5);
    out.push(0x78); // CMF: deflate, 32K window
    out.push(0x01); // FLG: no dict, check bits
    let mut offset = 0usize;
    while offset < data.len() {
        let remaining = data.len() - offset;
        let block_len = remaining.min(65_535);
        let is_final = offset + block_len >= data.len();
        out.push(u8::from(is_final));
        let len_u16 = block_len as u16;
        out.extend_from_slice(&len_u16.to_le_bytes());
        out.extend_from_slice(&(!len_u16).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + block_len]);
        offset += block_len;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
