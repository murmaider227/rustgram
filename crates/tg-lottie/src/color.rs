//! Color replacement and Fitzpatrick emoji skin tone modifiers.
//!
//! Port of `lottie/details/lottie_instance.cpp` color replacement handling and
//! Telegram's rlottie/tlottie Fitzpatrick skin-tone color tables.

use std::collections::HashMap;

use crate::common::{ColorReplacements, SkinModifier};
use crate::model::{AnimatedColor, AnimatedGradient, Composition, Layer, LayerKind, ShapeItem};

/// Returns the Fitzpatrick skin tone RGB replacement table for `modifier`.
///
/// Maps Telegram's default animated-emoji yellow skin shades (`0xRRGGBB`) to the
/// corresponding Fitzpatrick type 1-2 (`Color1`) through type 6 (`Color5`) shades.
#[must_use]
pub fn fitzpatrick_replacements(modifier: SkinModifier) -> &'static [(u32, u32)] {
    match modifier {
        SkinModifier::None => &[],
        // Fitzpatrick Type 1-2 (Light)
        SkinModifier::Color1 => &[
            (0xf77e41, 0xca907a),
            (0xffb139, 0xedc5a5),
            (0xffd140, 0xf7e3cf),
            (0xffdf79, 0xfbf2e8),
            (0xffbe3b, 0xf7e3cf),
            (0xe09602, 0xd4a988),
        ],
        // Fitzpatrick Type 3 (Medium-Light)
        SkinModifier::Color2 => &[
            (0xf77e41, 0xb57c56),
            (0xffb139, 0xdeae87),
            (0xffd140, 0xefc9a8),
            (0xffdf79, 0xf6ddc5),
            (0xffbe3b, 0xefc9a8),
            (0xe09602, 0xc3926b),
        ],
        // Fitzpatrick Type 4 (Medium)
        SkinModifier::Color3 => &[
            (0xf77e41, 0x935d38),
            (0xffb139, 0xb88156),
            (0xffd140, 0xcf9b6f),
            (0xffdf79, 0xdcb08a),
            (0xffbe3b, 0xcf9b6f),
            (0xe09602, 0xa26c45),
        ],
        // Fitzpatrick Type 5 (Medium-Dark)
        SkinModifier::Color4 => &[
            (0xf77e41, 0x6e4326),
            (0xffb139, 0x8f5b38),
            (0xffd140, 0xa8734d),
            (0xffdf79, 0xb8855f),
            (0xffbe3b, 0xa8734d),
            (0xe09602, 0x7b4c2d),
        ],
        // Fitzpatrick Type 6 (Dark)
        SkinModifier::Color5 => &[
            (0xf77e41, 0x3c2519),
            (0xffb139, 0x563928),
            (0xffd140, 0x6c4c39),
            (0xffdf79, 0x7d5d4a),
            (0xffbe3b, 0x6c4c39),
            (0xe09602, 0x4b3122),
        ],
    }
}

/// Builds the combined replacement lookup map (`source_rgb -> target_rgb`).
#[must_use]
pub fn build_replacement_map(replacements: &ColorReplacements) -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    for &(src, dst) in fitzpatrick_replacements(replacements.modifier) {
        map.insert(src & 0x00FF_FFFF, dst & 0x00FF_FFFF);
    }
    for &(src, dst) in &replacements.replacements {
        map.insert(src & 0x00FF_FFFF, dst & 0x00FF_FFFF);
    }
    map
}

/// Applies color replacements in place to every solid layer, fill, stroke, and gradient
/// across the composition and all precomposition assets.
pub fn apply_color_replacements(comp: &mut Composition, replacements: &ColorReplacements) {
    let map = build_replacement_map(replacements);
    if map.is_empty() {
        return;
    }
    for layer in &mut comp.layers {
        replace_in_layer(layer, &map);
    }
    for asset in comp.assets.values_mut() {
        for layer in &mut asset.layers {
            replace_in_layer(layer, &map);
        }
    }
}

fn rgb_f32_to_u32(rgb: [f32; 3]) -> u32 {
    let r = (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u32;
    (r << 16) | (g << 8) | b
}

fn u32_to_rgb_f32(rgb: u32) -> [f32; 3] {
    [
        ((rgb >> 16) & 0xFF) as f32 / 255.0,
        ((rgb >> 8) & 0xFF) as f32 / 255.0,
        (rgb & 0xFF) as f32 / 255.0,
    ]
}

fn replace_rgb_f32(rgb: &mut [f32; 3], map: &HashMap<u32, u32>) {
    let key = rgb_f32_to_u32(*rgb);
    if let Some(&dst) = map.get(&key) {
        *rgb = u32_to_rgb_f32(dst);
    }
}

fn replace_rgba_f32(rgba: &mut [f32; 4], map: &HashMap<u32, u32>) {
    let mut rgb = [rgba[0], rgba[1], rgba[2]];
    replace_rgb_f32(&mut rgb, map);
    rgba[0] = rgb[0];
    rgba[1] = rgb[1];
    rgba[2] = rgb[2];
}

fn replace_in_animated_color(prop: &mut AnimatedColor, map: &HashMap<u32, u32>) {
    match prop {
        AnimatedColor::Static(c) => replace_rgba_f32(c, map),
        AnimatedColor::Keyframed(kfs) => {
            for kf in kfs {
                replace_rgba_f32(&mut kf.start, map);
                if let Some(ref mut end) = kf.end {
                    replace_rgba_f32(end, map);
                }
            }
        }
    }
}

fn replace_in_gradient_stops(values: &mut [f32], color_stops: usize, map: &HashMap<u32, u32>) {
    let max_stops = color_stops.min(values.len() / 4);
    for i in 0..max_stops {
        let base = i * 4;
        let mut rgb = [values[base + 1], values[base + 2], values[base + 3]];
        replace_rgb_f32(&mut rgb, map);
        values[base + 1] = rgb[0];
        values[base + 2] = rgb[1];
        values[base + 3] = rgb[2];
    }
}

fn replace_in_animated_gradient(
    grad: &mut AnimatedGradient,
    color_stops: usize,
    map: &HashMap<u32, u32>,
) {
    match grad {
        AnimatedGradient::Static(v) => replace_in_gradient_stops(v, color_stops, map),
        AnimatedGradient::Keyframed(kfs) => {
            for kf in kfs {
                replace_in_gradient_stops(&mut kf.start, color_stops, map);
                if let Some(ref mut end) = kf.end {
                    replace_in_gradient_stops(end, color_stops, map);
                }
            }
        }
    }
}

fn replace_in_layer(layer: &mut Layer, map: &HashMap<u32, u32>) {
    match &mut layer.kind {
        LayerKind::Solid { color, .. } => {
            let key = color.to_rgb_u32();
            if let Some(&dst) = map.get(&key) {
                let a = color.a;
                let mut c = crate::common::Color::from_rgb_u32(dst);
                c.a = a;
                *color = c;
            }
        }
        LayerKind::Shape { shapes } => {
            replace_in_shapes(shapes, map);
        }
        LayerKind::Precomp { .. } | LayerKind::Null | LayerKind::Unsupported => {}
    }
}

fn replace_in_shapes(shapes: &mut [ShapeItem], map: &HashMap<u32, u32>) {
    for shape in shapes {
        match shape {
            ShapeItem::Group(g) => replace_in_shapes(&mut g.items, map),
            ShapeItem::Fill(f) => replace_in_animated_color(&mut f.color, map),
            ShapeItem::Stroke(s) => replace_in_animated_color(&mut s.color, map),
            ShapeItem::GradientFill(gf) => {
                replace_in_animated_gradient(&mut gf.gradient.colors, gf.gradient.stop_count, map);
            }
            ShapeItem::GradientStroke(gs) => {
                replace_in_animated_gradient(&mut gs.gradient.colors, gs.gradient.stop_count, map);
            }
            _ => {}
        }
    }
}
