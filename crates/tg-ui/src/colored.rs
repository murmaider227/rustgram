//! `Images::Colored`: the selection tint of stickers, large emoji and custom emoji.

use egui::{Color32, ColorImage};

/// Lays `add` over the image where it is opaque (a selected sticker or large emoji gets
/// `msgStickerOverlay`), in tdesktop's fixed-point arithmetic on premultiplied pixels. Alpha
/// is kept.
pub fn colored(image: &mut ColorImage, add: Color32) {
    let [r, g, b, ca] = add.to_srgba_unmultiplied().map(u32::from);
    let (cr, cg, cb) = (r * (ca + 1), g * (ca + 1), b * (ca + 1));
    let ra = (0x100 - ca) * 0x100;
    for px in &mut image.pixels {
        let [pr, pg, pb, pa] = px.to_array().map(u32::from);
        let a = pa + 1;
        // Each sum is at most 0x100 * 0xFF * 0x100 + 0x100 * 0xFF * 0x100 < 2^25, and the
        // result after `>> 16` is at most 0xFF.
        let mix = |p: u32, c: u32| ((ra * p + a * c) >> 16).min(0xFF) as u8;
        *px = Color32::from_rgba_premultiplied(mix(pr, cr), mix(pg, cg), mix(pb, cb), pa as u8);
    }
}
