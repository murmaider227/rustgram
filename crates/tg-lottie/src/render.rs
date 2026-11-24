//! Software rasterizer for Lottie compositions built on `tiny-skia`.
//!
//! Renders premultiplied RGBA frames at any size with antialiasing, supporting
//! precomps, solid/null/shape layers, parenting, masks (Add/Subtract/Intersect/
//! Difference, inverted, opacity, expansion), track mattes (Alpha/Luma and
//! inverted variants), linear/radial gradients, strokes with dashes, trim paths,
//! repeaters, and blend modes.
//!
//! Aspect-fit centered output matches `Lottie::Instance::renderToPrepared` in
//! `lottie/details/lottie_instance.cpp`.

use std::collections::HashSet;

use tiny_skia::{
    BlendMode as SkBlendMode, Color as SkColor, FillRule as SkFillRule, GradientStop, LineCap,
    LineJoin, LinearGradient, Mask as SkMask, Paint, PathBuilder, Pixmap, PixmapPaint, Point,
    RadialGradient, Rect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use crate::common::{FrameBuffer, Size};
use crate::model::{
    BlendMode, Composition, DashItem, FillRule, GradientData, GradientType, Layer, LayerKind,
    LineCap as LottieLineCap, LineJoin as LottieLineJoin, Mask, MaskMode, MatteType,
    RepeaterComposite, RepeaterShape, ShapeItem, TransformProp, TrimMode,
};
use crate::path::{
    BezierPath, build_lottie_path, eval_ellipse_shape, eval_path_shape, eval_polystar_shape,
    eval_rect_shape,
};

/// Maximum precomp recursion depth to guard against cyclic asset references.
const MAX_PRECOMP_DEPTH: usize = 32;

/// Maximum parent chain traversal depth to guard against cyclic parenting.
const MAX_PARENT_DEPTH: usize = 64;

/// Maximum repeater copies rendered per repeater item to prevent DoS.
const MAX_REPEATER_COPIES: usize = 256;

/// Renders `comp` at `frame` into `target`, uniformly fitting and centering the
/// composition inside `target`'s dimensions (leaving margins transparent).
///
/// Mirrors `Lottie::Instance::renderToPrepared` in `lottie_instance.cpp`.
pub fn render_composition_into(comp: &Composition, frame: f64, target: &mut FrameBuffer) {
    let dst_w = target.width;
    let dst_h = target.height;
    if dst_w == 0 || dst_h == 0 {
        return;
    }
    if comp.width == 0 || comp.height == 0 {
        target.clear();
        return;
    }

    let scale = (dst_w as f64 / comp.width as f64).min(dst_h as f64 / comp.height as f64);
    let fit_w = ((comp.width as f64 * scale).round() as u32).clamp(1, dst_w);
    let fit_h = ((comp.height as f64 * scale).round() as u32).clamp(1, dst_h);
    let offset_x = ((dst_w - fit_w) / 2) as f32;
    let offset_y = ((dst_h - fit_h) / 2) as f32;
    let sx = fit_w as f32 / comp.width as f32;
    let sy = fit_h as f32 / comp.height as f32;

    let Some(mut pixmap) = Pixmap::new(dst_w, dst_h) else {
        target.clear();
        return;
    };

    let root_ts =
        Transform::from_translate(offset_x, offset_y).pre_concat(Transform::from_scale(sx, sy));

    // Clip to the fitted composition viewport so content outside [0, w] x [0, h] doesn't spill
    // into letterbox margins.
    let mut clip_mask = None;
    if (fit_w < dst_w || fit_h < dst_h)
        && let Some(mut m) = SkMask::new(dst_w, dst_h)
        && let Some(rect) = Rect::from_xywh(offset_x, offset_y, fit_w as f32, fit_h as f32)
    {
        let path = PathBuilder::from_rect(rect);
        m.fill_path(&path, SkFillRule::Winding, false, Transform::identity());
        clip_mask = Some(m);
    }

    if let Some(mask) = clip_mask {
        if let Some(mut inner) = Pixmap::new(dst_w, dst_h) {
            render_layers(comp, &comp.layers, frame, root_ts, 0, &mut inner);
            pixmap.draw_pixmap(
                0,
                0,
                inner.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                Some(&mask),
            );
        }
    } else {
        render_layers(comp, &comp.layers, frame, root_ts, 0, &mut pixmap);
    }

    target
        .data
        .resize((dst_w as usize) * (dst_h as usize) * 4, 0);
    target.data.copy_from_slice(pixmap.data());
}

/// Renders `comp` at `frame` into a freshly allocated `FrameBuffer` of `size`.
#[must_use]
pub fn render_composition(comp: &Composition, frame: f64, size: Size) -> FrameBuffer {
    let mut buf = FrameBuffer::new(size);
    render_composition_into(comp, frame, &mut buf);
    buf
}

fn eval_transform_matrix(tr: &TransformProp, frame: f64, auto_orient: bool) -> Transform {
    let anchor = tr.anchor.eval(frame);
    let pos = tr.position.eval(frame);
    let scale = tr.scale.eval(frame);
    let mut rot = tr.rotation.eval(frame);
    if auto_orient {
        rot += tr.position.eval_tangent_angle_deg(frame);
    }
    let skew = tr.skew.eval(frame);
    let skew_axis = tr.skew_axis.eval(frame);

    let mut m = Transform::from_translate(pos[0], pos[1]);
    if rot.abs() > 1e-6 {
        m = m.pre_concat(Transform::from_rotate(rot));
    }
    if skew.abs() > 1e-6 {
        let sk_rad = (-skew).clamp(-89.0, 89.0).to_radians();
        if skew_axis.abs() > 1e-6 {
            m = m.pre_concat(Transform::from_rotate(-skew_axis));
        }
        m = m.pre_concat(Transform::from_skew(sk_rad.tan(), 0.0));
        if skew_axis.abs() > 1e-6 {
            m = m.pre_concat(Transform::from_rotate(skew_axis));
        }
    }
    m = m.pre_concat(Transform::from_scale(scale[0] / 100.0, scale[1] / 100.0));
    m = m.pre_concat(Transform::from_translate(-anchor[0], -anchor[1]));
    m
}

fn layer_world_transform(
    layers: &[Layer],
    layer_idx: usize,
    frame: f64,
    root_ts: Transform,
) -> Transform {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut current = Some(layer_idx);
    while let Some(idx) = current {
        if chain.len() >= MAX_PARENT_DEPTH || !visited.insert(idx) {
            break;
        }
        chain.push(idx);
        current = layers[idx]
            .parent
            .and_then(|pid| layers.iter().position(|l| l.index == Some(pid)));
    }

    let mut ts = root_ts;
    for &idx in chain.iter().rev() {
        let l = &layers[idx];
        let local = eval_transform_matrix(&l.transform, frame, l.auto_orient);
        ts = ts.pre_concat(local);
    }
    ts
}

fn layer_is_active(layer: &Layer, frame: f64) -> bool {
    if layer.hidden {
        return false;
    }
    // Lottie layers are active for [ip, op).
    frame >= layer.in_point - 1e-6 && frame < layer.out_point - 1e-6
}

fn map_blend_mode(mode: BlendMode) -> SkBlendMode {
    match mode {
        BlendMode::Normal => SkBlendMode::SourceOver,
        BlendMode::Multiply => SkBlendMode::Multiply,
        BlendMode::Screen => SkBlendMode::Screen,
        BlendMode::Overlay => SkBlendMode::Overlay,
        BlendMode::Darken => SkBlendMode::Darken,
        BlendMode::Lighten => SkBlendMode::Lighten,
        BlendMode::ColorDodge => SkBlendMode::ColorDodge,
        BlendMode::ColorBurn => SkBlendMode::ColorBurn,
        BlendMode::HardLight => SkBlendMode::HardLight,
        BlendMode::SoftLight => SkBlendMode::SoftLight,
        BlendMode::Difference => SkBlendMode::Difference,
        BlendMode::Exclusion => SkBlendMode::Exclusion,
        BlendMode::Hue => SkBlendMode::Hue,
        BlendMode::Saturation => SkBlendMode::Saturation,
        BlendMode::Color => SkBlendMode::Color,
        BlendMode::Luminosity => SkBlendMode::Luminosity,
    }
}

fn render_layers(
    comp: &Composition,
    layers: &[Layer],
    frame: f64,
    root_ts: Transform,
    depth: usize,
    canvas: &mut Pixmap,
) {
    if depth >= MAX_PRECOMP_DEPTH || layers.is_empty() {
        return;
    }

    // Identify which layers act purely as track matte sources so they are not drawn directly.
    let mut matte_sources = vec![false; layers.len()];
    let mut matte_for_layer: Vec<Option<usize>> = vec![None; layers.len()];

    for (i, layer) in layers.iter().enumerate() {
        if layer.is_matte_source {
            matte_sources[i] = true;
        }
        if layer.matte_type.is_some() {
            let src = if let Some(tp) = layer.matte_parent {
                layers.iter().position(|l| l.index == Some(tp))
            } else if i > 0 {
                Some(i - 1)
            } else {
                None
            };
            if let Some(src_idx) = src {
                matte_sources[src_idx] = true;
                matte_for_layer[i] = Some(src_idx);
            }
        }
    }

    // Lottie layers are ordered top-to-bottom (index 0 is topmost), so paint in reverse.
    for i in (0..layers.len()).rev() {
        if matte_sources[i] {
            continue;
        }
        let layer = &layers[i];
        if !layer_is_active(layer, frame) {
            continue;
        }
        let matte_src = matte_for_layer[i];
        render_single_layer(comp, layers, i, frame, root_ts, depth, matte_src, canvas);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_single_layer(
    comp: &Composition,
    layers: &[Layer],
    layer_idx: usize,
    frame: f64,
    root_ts: Transform,
    depth: usize,
    matte_source_idx: Option<usize>,
    canvas: &mut Pixmap,
) {
    let layer = &layers[layer_idx];
    let opacity = (layer.transform.opacity.eval(frame) / 100.0).clamp(0.0, 1.0);
    if opacity <= 1e-4 {
        return;
    }
    if matches!(layer.kind, LayerKind::Null | LayerKind::Unsupported) {
        return;
    }

    let world_ts = layer_world_transform(layers, layer_idx, frame, root_ts);
    let has_masks = layer.masks.iter().any(|m| m.mode != MaskMode::None);
    let has_matte = layer.matte_type.is_some() && matte_source_idx.is_some();
    let needs_offscreen = opacity < 1.0 - 1e-4
        || has_masks
        || has_matte
        || layer.blend_mode != BlendMode::Normal
        || matches!(layer.kind, LayerKind::Precomp { .. });

    if !needs_offscreen {
        render_layer_content(comp, layer, frame, world_ts, depth, canvas);
        return;
    }

    let Some(mut layer_buf) = Pixmap::new(canvas.width(), canvas.height()) else {
        return;
    };
    render_layer_content(comp, layer, frame, world_ts, depth, &mut layer_buf);

    if has_masks {
        apply_masks(&layer.masks, frame, world_ts, &mut layer_buf);
    }

    if let (Some(matte_type), Some(src_idx)) = (layer.matte_type, matte_source_idx) {
        let Some(mut matte_buf) = Pixmap::new(canvas.width(), canvas.height()) else {
            return;
        };
        let matte_layer = &layers[src_idx];
        // Note: matte source may be hidden in AE (`td: 1`) yet still used as matte; check time range.
        if frame >= matte_layer.in_point - 1e-6 && frame < matte_layer.out_point - 1e-6 {
            let matte_opacity = (matte_layer.transform.opacity.eval(frame) / 100.0).clamp(0.0, 1.0);
            if matte_opacity > 1e-4 {
                let matte_ts = layer_world_transform(layers, src_idx, frame, root_ts);
                render_layer_content(comp, matte_layer, frame, matte_ts, depth, &mut matte_buf);
                if matte_layer.masks.iter().any(|m| m.mode != MaskMode::None) {
                    apply_masks(&matte_layer.masks, frame, matte_ts, &mut matte_buf);
                }
                if matte_opacity < 1.0 - 1e-4 {
                    scale_pixmap_alpha(&mut matte_buf, matte_opacity);
                }
            }
        }
        apply_track_matte(&mut layer_buf, &matte_buf, matte_type);
    }

    let paint = PixmapPaint {
        opacity,
        blend_mode: map_blend_mode(layer.blend_mode),
        quality: tiny_skia::FilterQuality::Nearest,
    };
    canvas.draw_pixmap(
        0,
        0,
        layer_buf.as_ref(),
        &paint,
        Transform::identity(),
        None,
    );
}

fn render_layer_content(
    comp: &Composition,
    layer: &Layer,
    frame: f64,
    world_ts: Transform,
    depth: usize,
    canvas: &mut Pixmap,
) {
    match &layer.kind {
        LayerKind::Solid {
            width,
            height,
            color,
        } => {
            if *width <= 0.0 || *height <= 0.0 {
                return;
            }
            if let Some(rect) = Rect::from_xywh(0.0, 0.0, *width, *height) {
                let path = PathBuilder::from_rect(rect);
                let mut paint = Paint {
                    anti_alias: true,
                    ..Paint::default()
                };
                paint.set_color_rgba8(color.r, color.g, color.b, color.a);
                canvas.fill_path(&path, &paint, SkFillRule::Winding, world_ts, None);
            }
        }
        LayerKind::Precomp {
            ref_id,
            width,
            height,
            time_remap,
        } => {
            let Some(asset) = comp.assets.get(ref_id) else {
                return;
            };
            let asset_fr = asset.frame_rate.unwrap_or(comp.frame_rate);
            let sub_frame = if let Some(tm) = time_remap {
                f64::from(tm.eval(frame)) * asset_fr
            } else {
                (frame - layer.start_time) / layer.stretch
            };

            let clip_w = if *width > 0.0 {
                *width
            } else {
                asset.width.unwrap_or(comp.width) as f32
            };
            let clip_h = if *height > 0.0 {
                *height
            } else {
                asset.height.unwrap_or(comp.height) as f32
            };

            if clip_w > 0.0 && clip_h > 0.0 {
                let Some(mut pre_buf) = Pixmap::new(canvas.width(), canvas.height()) else {
                    return;
                };
                render_layers(
                    comp,
                    &asset.layers,
                    sub_frame,
                    world_ts,
                    depth + 1,
                    &mut pre_buf,
                );
                let mut mask_opt = None;
                if let Some(rect) = Rect::from_xywh(0.0, 0.0, clip_w, clip_h)
                    && let Some(mut m) = SkMask::new(canvas.width(), canvas.height())
                {
                    let clip_path = PathBuilder::from_rect(rect);
                    m.fill_path(&clip_path, SkFillRule::Winding, true, world_ts);
                    mask_opt = Some(m);
                }
                canvas.draw_pixmap(
                    0,
                    0,
                    pre_buf.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    mask_opt.as_ref(),
                );
            } else {
                render_layers(comp, &asset.layers, sub_frame, world_ts, depth + 1, canvas);
            }
        }
        LayerKind::Shape { shapes } => {
            let mut empty_trims = Vec::new();
            render_shape_list(shapes, frame, world_ts, 1.0, &mut empty_trims, canvas);
        }
        LayerKind::Null | LayerKind::Unsupported => {}
    }
}

// ---------------------------------------------------------------------------
// Masks & Track Mattes
// ---------------------------------------------------------------------------

fn scale_pixmap_alpha(pixmap: &mut Pixmap, factor: f32) {
    let f = factor.clamp(0.0, 1.0);
    for px in pixmap.data_mut().as_chunks_mut::<4>().0 {
        px[0] = ((f32::from(px[0]) * f).round()).clamp(0.0, 255.0) as u8;
        px[1] = ((f32::from(px[1]) * f).round()).clamp(0.0, 255.0) as u8;
        px[2] = ((f32::from(px[2]) * f).round()).clamp(0.0, 255.0) as u8;
        px[3] = ((f32::from(px[3]) * f).round()).clamp(0.0, 255.0) as u8;
    }
}

fn rasterize_mask_alpha(
    mask: &Mask,
    frame: f64,
    ts: Transform,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let len = (width as usize) * (height as usize);
    let mut alpha = vec![0u8; len];
    let data = mask.path.eval(frame);
    let bp = build_lottie_path(&data, crate::model::Direction::Clockwise);
    let Some(sk_path) = bp.to_skia_path() else {
        if mask.inverted {
            alpha.fill(255);
        }
        return alpha;
    };

    let Some(mut pm) = Pixmap::new(width, height) else {
        return alpha;
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(SkColor::WHITE);
    pm.fill_path(&sk_path, &paint, SkFillRule::Winding, ts, None);

    // Mask expansion: dilate (x > 0) or erode (x < 0) via round-joined stroke of width 2*|x|.
    let expansion = mask.expansion.eval(frame);
    if expansion.abs() > 0.25 {
        let stroke = Stroke {
            width: expansion.abs() * 2.0,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        if expansion > 0.0 {
            pm.stroke_path(&sk_path, &paint, &stroke, ts, None);
        } else {
            let mut clear_paint = Paint {
                anti_alias: true,
                blend_mode: SkBlendMode::Clear,
                ..Paint::default()
            };
            clear_paint.set_color(SkColor::WHITE);
            pm.stroke_path(&sk_path, &clear_paint, &stroke, ts, None);
        }
    }

    let opacity = (mask.opacity.eval(frame) / 100.0).clamp(0.0, 1.0);
    for (dst, px) in alpha.iter_mut().zip(pm.data().as_chunks::<4>().0) {
        let mut a = px[3];
        if mask.inverted {
            a = 255 - a;
        }
        *dst = ((f32::from(a) * opacity).round()).clamp(0.0, 255.0) as u8;
    }
    alpha
}

fn apply_masks(masks: &[Mask], frame: f64, ts: Transform, layer_buf: &mut Pixmap) {
    let active: Vec<&Mask> = masks.iter().filter(|m| m.mode != MaskMode::None).collect();
    if active.is_empty() {
        return;
    }
    let w = layer_buf.width();
    let h = layer_buf.height();
    let len = (w as usize) * (h as usize);

    // If the first active mask is Subtract or Intersect (or inverted), start from full coverage.
    let first_is_subtractive = matches!(active[0].mode, MaskMode::Subtract | MaskMode::Intersect);
    let mut combined = if first_is_subtractive {
        vec![255u8; len]
    } else {
        vec![0u8; len]
    };

    for mask in active {
        let cur = rasterize_mask_alpha(mask, frame, ts, w, h);
        match mask.mode {
            MaskMode::None => {}
            MaskMode::Add => {
                for (c, &m) in combined.iter_mut().zip(&cur) {
                    *c = (*c).saturating_add(m);
                }
            }
            MaskMode::Subtract => {
                for (c, &m) in combined.iter_mut().zip(&cur) {
                    *c = (*c).saturating_sub(m);
                }
            }
            MaskMode::Intersect => {
                for (c, &m) in combined.iter_mut().zip(&cur) {
                    *c = ((u16::from(*c) * u16::from(m) + 127) / 255) as u8;
                }
            }
            MaskMode::Difference => {
                for (c, &m) in combined.iter_mut().zip(&cur) {
                    *c = (*c).abs_diff(m);
                }
            }
        }
    }

    for (px, &m) in layer_buf
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(&combined)
    {
        if m == 255 {
            continue;
        }
        if m == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 0;
        } else {
            let mf = u16::from(m);
            px[0] = ((u16::from(px[0]) * mf + 127) / 255) as u8;
            px[1] = ((u16::from(px[1]) * mf + 127) / 255) as u8;
            px[2] = ((u16::from(px[2]) * mf + 127) / 255) as u8;
            px[3] = ((u16::from(px[3]) * mf + 127) / 255) as u8;
        }
    }
}

fn apply_track_matte(target: &mut Pixmap, matte: &Pixmap, mode: MatteType) {
    for (dst, src) in target
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(matte.data().as_chunks::<4>().0)
    {
        // Premultiplied RGBA: src[0..3] are already multiplied by alpha.
        let factor_u8: u8 = match mode {
            MatteType::Alpha => src[3],
            MatteType::AlphaInverted => 255 - src[3],
            MatteType::Luma => {
                let luma = 0.2126 * f32::from(src[0])
                    + 0.7152 * f32::from(src[1])
                    + 0.0722 * f32::from(src[2]);
                luma.round().clamp(0.0, 255.0) as u8
            }
            MatteType::LumaInverted => {
                let luma = 0.2126 * f32::from(src[0])
                    + 0.7152 * f32::from(src[1])
                    + 0.0722 * f32::from(src[2]);
                255 - (luma.round().clamp(0.0, 255.0) as u8)
            }
        };
        if factor_u8 == 255 {
            continue;
        }
        if factor_u8 == 0 {
            dst[0] = 0;
            dst[1] = 0;
            dst[2] = 0;
            dst[3] = 0;
        } else {
            let f = u16::from(factor_u8);
            dst[0] = ((u16::from(dst[0]) * f + 127) / 255) as u8;
            dst[1] = ((u16::from(dst[1]) * f + 127) / 255) as u8;
            dst[2] = ((u16::from(dst[2]) * f + 127) / 255) as u8;
            dst[3] = ((u16::from(dst[3]) * f + 127) / 255) as u8;
        }
    }
}

// ---------------------------------------------------------------------------
// Shape stack evaluation (Groups, Trim Paths, Repeaters, Fills, Strokes)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct ActiveTrim {
    start: f32,
    end: f32,
    offset: f32,
    mode: TrimMode,
}

/// Evaluates and paints a slice of `ShapeItem`s belonging to a layer or group.
///
/// Follows Lottie / After Effects stack semantics:
/// - A `Transform` (`tr`) item applies to the entire group.
/// - A `Repeater` (`rp`) repeats all items preceding it in this list with progressive
///   transform and start/end opacity interpolation.
/// - `TrimPaths` (`tm`) applies to all path generators preceding it in this list (and
///   propagates into preceding subgroups).
/// - Styles (`fl`, `st`, `gf`, `gs`) apply to all path generators preceding them in the list.
/// - Paint order is bottom-to-top (reverse of the JSON array order), so earlier items
///   appear visually on top of later items.
fn render_shape_list(
    items: &[ShapeItem],
    frame: f64,
    parent_ts: Transform,
    parent_opacity: f32,
    inherited_trims: &mut Vec<ActiveTrim>,
    canvas: &mut Pixmap,
) {
    // Extract group transform (if any) from this list.
    let mut group_ts = parent_ts;
    let mut group_opacity = parent_opacity;
    for item in items {
        if let ShapeItem::Transform(tr) = item {
            let local = eval_transform_matrix(tr, frame, false);
            group_ts = group_ts.pre_concat(local);
            group_opacity *= (tr.opacity.eval(frame) / 100.0).clamp(0.0, 1.0);
        }
    }
    if group_opacity <= 1e-4 {
        return;
    }

    // Check for Repeaters from back to front: the last non-hidden repeater wraps everything before it.
    if let Some(rp_idx) = items
        .iter()
        .rposition(|it| matches!(it, ShapeItem::Repeater(rp) if !rp.hidden))
        && let ShapeItem::Repeater(rp) = &items[rp_idx]
    {
        render_with_repeater(
            &items[..rp_idx],
            rp,
            frame,
            group_ts,
            group_opacity,
            inherited_trims,
            canvas,
        );
        // Any non-repeater items after the repeater (rare) are painted normally.
        if rp_idx + 1 < items.len() {
            render_shape_list_no_repeater(
                &items[(rp_idx + 1)..],
                frame,
                group_ts,
                group_opacity,
                inherited_trims,
                canvas,
            );
        }
        return;
    }

    render_shape_list_no_repeater(
        items,
        frame,
        group_ts,
        group_opacity,
        inherited_trims,
        canvas,
    );
}

fn repeater_copy_transform(rp: &RepeaterShape, frame: f64, t: f32) -> Transform {
    let anchor = rp.transform.anchor.eval(frame);
    let pos = rp.transform.position.eval(frame);
    let scale = rp.transform.scale.eval(frame);
    let rot = rp.transform.rotation.eval(frame);

    let sx = (scale[0] / 100.0).powf(t);
    let sy = (scale[1] / 100.0).powf(t);

    // At t = 0 this evaluates to identity: T(a) * T(p*t) * R(r*t) * S(s^t) * T(-a).
    let mut m = Transform::from_translate(anchor[0] + pos[0] * t, anchor[1] + pos[1] * t);
    let angle = rot * t;
    if angle.abs() > 1e-6 {
        m = m.pre_concat(Transform::from_rotate(angle));
    }
    m = m.pre_concat(Transform::from_scale(sx, sy));
    m = m.pre_concat(Transform::from_translate(-anchor[0], -anchor[1]));
    m
}

#[allow(clippy::too_many_arguments)]
fn render_with_repeater(
    prefix_items: &[ShapeItem],
    rp: &RepeaterShape,
    frame: f64,
    group_ts: Transform,
    group_opacity: f32,
    inherited_trims: &mut Vec<ActiveTrim>,
    canvas: &mut Pixmap,
) {
    let copies_f = rp.copies.eval(frame).max(0.0);
    if copies_f <= 1e-4 {
        return;
    }
    let count = (copies_f.ceil() as usize).min(MAX_REPEATER_COPIES);
    let offset = rp.offset.eval(frame);
    let so = (rp.start_opacity.eval(frame) / 100.0).clamp(0.0, 1.0);
    let eo = (rp.end_opacity.eval(frame) / 100.0).clamp(0.0, 1.0);

    let indices: Vec<usize> = match rp.composite {
        // Below: copy 0 is topmost, so paint highest copy first down to 0.
        RepeaterComposite::Below => (0..count).rev().collect(),
        // Above: copy 0 is bottommost, so paint 0 up to count - 1.
        RepeaterComposite::Above => (0..count).collect(),
    };

    for idx in indices {
        let t = idx as f32 + offset;
        let copy_ts = group_ts.pre_concat(repeater_copy_transform(rp, frame, t));
        let frac = if count > 1 {
            idx as f32 / (count - 1) as f32
        } else {
            0.0
        };
        let mut copy_op = so + (eo - so) * frac;
        if idx + 1 == count {
            let rem = copies_f - (idx as f32);
            if rem < 1.0 {
                copy_op *= rem.clamp(0.0, 1.0);
            }
        }
        let final_op = group_opacity * copy_op;
        if final_op <= 1e-4 {
            continue;
        }
        render_shape_list(
            prefix_items,
            frame,
            copy_ts,
            final_op,
            inherited_trims,
            canvas,
        );
    }
}

fn render_shape_list_no_repeater(
    items: &[ShapeItem],
    frame: f64,
    group_ts: Transform,
    group_opacity: f32,
    inherited_trims: &mut Vec<ActiveTrim>,
    canvas: &mut Pixmap,
) {
    // Collect active trims defined in this list (with their index so they affect items before them).
    let local_trims: Vec<(usize, ActiveTrim)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, it)| match it {
            ShapeItem::TrimPaths(tm) if !tm.hidden => Some((
                idx,
                ActiveTrim {
                    start: tm.start.eval(frame),
                    end: tm.end.eval(frame),
                    offset: tm.offset.eval(frame),
                    mode: tm.mode,
                },
            )),
            _ => None,
        })
        .collect();

    // Paint items in reverse order (bottom-to-top in Lottie's layer/shape stack).
    for (idx, item) in items.iter().enumerate().rev() {
        match item {
            ShapeItem::Group(g) if !g.hidden => {
                // Propagate trims that come *after* this group in the parent list.
                let pushed_before = inherited_trims.len();
                for &(tm_idx, trim) in &local_trims {
                    if tm_idx > idx {
                        inherited_trims.push(trim);
                    }
                }
                render_shape_list(
                    &g.items,
                    frame,
                    group_ts,
                    group_opacity,
                    inherited_trims,
                    canvas,
                );
                inherited_trims.truncate(pushed_before);
            }
            ShapeItem::Fill(fill) if !fill.hidden => {
                let path = collect_paths(&items[..idx], idx, frame, &local_trims, inherited_trims);
                if let Some(sk_path) = path.to_skia_path() {
                    let c = fill.color.eval(frame);
                    let op =
                        (fill.opacity.eval(frame) / 100.0).clamp(0.0, 1.0) * group_opacity * c[3];
                    if op > 1e-4 {
                        let mut paint = Paint {
                            anti_alias: true,
                            ..Paint::default()
                        };
                        if let Some(col) = SkColor::from_rgba(c[0], c[1], c[2], op) {
                            paint.set_color(col);
                            canvas.fill_path(
                                &sk_path,
                                &paint,
                                map_fill_rule(fill.fill_rule),
                                group_ts,
                                None,
                            );
                        }
                    }
                }
            }
            ShapeItem::Stroke(st) if !st.hidden => {
                let path = collect_paths(&items[..idx], idx, frame, &local_trims, inherited_trims);
                if let Some(sk_path) = path.to_skia_path() {
                    let width = st.width.eval(frame);
                    let c = st.color.eval(frame);
                    let op =
                        (st.opacity.eval(frame) / 100.0).clamp(0.0, 1.0) * group_opacity * c[3];
                    if width > 1e-4 && op > 1e-4 {
                        let mut paint = Paint {
                            anti_alias: true,
                            ..Paint::default()
                        };
                        if let Some(col) = SkColor::from_rgba(c[0], c[1], c[2], op) {
                            paint.set_color(col);
                            let stroke = build_stroke(
                                width,
                                st.line_cap,
                                st.line_join,
                                st.miter_limit,
                                &st.dashes,
                                frame,
                            );
                            canvas.stroke_path(&sk_path, &paint, &stroke, group_ts, None);
                        }
                    }
                }
            }
            ShapeItem::GradientFill(gf) if !gf.hidden => {
                let path = collect_paths(&items[..idx], idx, frame, &local_trims, inherited_trims);
                if let Some(sk_path) = path.to_skia_path() {
                    let op = (gf.opacity.eval(frame) / 100.0).clamp(0.0, 1.0) * group_opacity;
                    if op > 1e-4
                        && let Some(shader) = build_gradient_shader(&gf.gradient, frame, op)
                    {
                        let paint = Paint {
                            shader,
                            anti_alias: true,
                            ..Paint::default()
                        };
                        canvas.fill_path(
                            &sk_path,
                            &paint,
                            map_fill_rule(gf.fill_rule),
                            group_ts,
                            None,
                        );
                    }
                }
            }
            ShapeItem::GradientStroke(gs) if !gs.hidden => {
                let path = collect_paths(&items[..idx], idx, frame, &local_trims, inherited_trims);
                if let Some(sk_path) = path.to_skia_path() {
                    let width = gs.width.eval(frame);
                    let op = (gs.opacity.eval(frame) / 100.0).clamp(0.0, 1.0) * group_opacity;
                    if width > 1e-4
                        && op > 1e-4
                        && let Some(shader) = build_gradient_shader(&gs.gradient, frame, op)
                    {
                        let paint = Paint {
                            shader,
                            anti_alias: true,
                            ..Paint::default()
                        };
                        let stroke = build_stroke(
                            width,
                            gs.line_cap,
                            gs.line_join,
                            gs.miter_limit,
                            &gs.dashes,
                            frame,
                        );
                        canvas.stroke_path(&sk_path, &paint, &stroke, group_ts, None);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Collects all geometry paths from `prefix` (items preceding a style at `style_idx`),
/// applying any local trims and inherited trims.
fn collect_paths(
    prefix: &[ShapeItem],
    _style_idx: usize,
    frame: f64,
    local_trims: &[(usize, ActiveTrim)],
    inherited_trims: &[ActiveTrim],
) -> BezierPath {
    let mut combined = BezierPath::default();

    for (i, item) in prefix.iter().enumerate() {
        let mut bp = match item {
            ShapeItem::Rect(rc) if !rc.hidden => eval_rect_shape(rc, frame),
            ShapeItem::Ellipse(el) if !el.hidden => eval_ellipse_shape(el, frame),
            ShapeItem::Path(sh) if !sh.hidden => eval_path_shape(sh, frame),
            ShapeItem::Polystar(sr) if !sr.hidden => eval_polystar_shape(sr, frame),
            ShapeItem::Group(g) if !g.hidden => collect_group_paths(&g.items, frame),
            _ => continue,
        };
        if bp.is_empty() {
            continue;
        }
        // Apply trims that come after this path item within the same group.
        for &(tm_idx, trim) in local_trims {
            if tm_idx > i && trim.mode == TrimMode::Simultaneous {
                bp = bp.trim(trim.start, trim.end, trim.offset, trim.mode);
            }
        }
        combined.extend(bp);
    }

    // Apply Individual local trims across the combined path.
    for &(_, trim) in local_trims {
        if trim.mode == TrimMode::Individual {
            combined = combined.trim(trim.start, trim.end, trim.offset, trim.mode);
        }
    }
    // Apply inherited trims from enclosing groups.
    for &trim in inherited_trims {
        combined = combined.trim(trim.start, trim.end, trim.offset, trim.mode);
    }

    combined
}

/// Recursively collects geometry paths from a subgroup that has no own paint styles
/// (or contributes paths to an outer group's style), transformed by the subgroup's transform.
fn collect_group_paths(items: &[ShapeItem], frame: f64) -> BezierPath {
    // If the subgroup defines its own fill/stroke styles, it paints itself rather than leaking
    // its paths into an outer fill/stroke.
    let has_own_paint = items.iter().any(|it| {
        matches!(
            it,
            ShapeItem::Fill(_)
                | ShapeItem::Stroke(_)
                | ShapeItem::GradientFill(_)
                | ShapeItem::GradientStroke(_)
        )
    });
    if has_own_paint {
        return BezierPath::default();
    }

    let mut local_ts = Transform::identity();
    for it in items {
        if let ShapeItem::Transform(tr) = it {
            local_ts = local_ts.pre_concat(eval_transform_matrix(tr, frame, false));
        }
    }

    let local_trims: Vec<(usize, ActiveTrim)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, it)| match it {
            ShapeItem::TrimPaths(tm) if !tm.hidden => Some((
                idx,
                ActiveTrim {
                    start: tm.start.eval(frame),
                    end: tm.end.eval(frame),
                    offset: tm.offset.eval(frame),
                    mode: tm.mode,
                },
            )),
            _ => None,
        })
        .collect();

    let mut bp = collect_paths(items, items.len(), frame, &local_trims, &[]);
    if !local_ts.is_identity() {
        bp.transform(local_ts);
    }
    bp
}

fn map_fill_rule(rule: FillRule) -> SkFillRule {
    match rule {
        FillRule::NonZero => SkFillRule::Winding,
        FillRule::EvenOdd => SkFillRule::EvenOdd,
    }
}

fn build_stroke(
    width: f32,
    cap: LottieLineCap,
    join: LottieLineJoin,
    miter_limit: f32,
    dashes: &[DashItem],
    frame: f64,
) -> Stroke {
    let line_cap = match cap {
        LottieLineCap::Butt => LineCap::Butt,
        LottieLineCap::Round => LineCap::Round,
        LottieLineCap::Square => LineCap::Square,
    };
    let line_join = match join {
        LottieLineJoin::Miter => LineJoin::Miter,
        LottieLineJoin::Round => LineJoin::Round,
        LottieLineJoin::Bevel => LineJoin::Bevel,
    };

    let dash = build_stroke_dash(dashes, frame);

    Stroke {
        width,
        miter_limit: miter_limit.max(1.0),
        line_cap,
        line_join,
        dash,
    }
}

fn build_stroke_dash(dashes: &[DashItem], frame: f64) -> Option<StrokeDash> {
    if dashes.is_empty() {
        return None;
    }
    let mut pattern = Vec::new();
    let mut offset = 0.0_f32;
    for item in dashes {
        let v = item.value.eval(frame);
        if item.kind == 'o' {
            offset = v;
        } else {
            pattern.push(v.max(0.0));
        }
    }
    if pattern.is_empty() {
        return None;
    }
    if pattern.len() == 1 {
        pattern.push(pattern[0]);
    } else if pattern.len() % 2 == 1 {
        let copy = pattern.clone();
        pattern.extend(copy);
    }
    if pattern.iter().copied().sum::<f32>() <= 1e-4 {
        return None;
    }
    StrokeDash::new(pattern, offset)
}

fn build_gradient_shader(grad: &GradientData, frame: f64, opacity: f32) -> Option<Shader<'static>> {
    let raw = grad.colors.eval(frame);
    let stops = parse_gradient_stops(&raw, grad.stop_count, opacity);
    if stops.len() < 2 {
        return None;
    }

    let s = grad.start.eval(frame);
    let e = grad.end.eval(frame);

    match grad.gradient_type {
        GradientType::Linear => LinearGradient::new(
            Point::from_xy(s[0], s[1]),
            Point::from_xy(e[0], e[1]),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ),
        GradientType::Radial => {
            let dx = e[0] - s[0];
            let dy = e[1] - s[1];
            let radius = dx.hypot(dy).max(1e-4);
            let h_pct = (grad.highlight_length.eval(frame) / 100.0).clamp(-0.99, 0.99);
            let h_ang = grad.highlight_angle.eval(frame).to_radians();
            let base_ang = dy.atan2(dx);
            let fx = s[0] + radius * h_pct * (base_ang + h_ang).cos();
            let fy = s[1] + radius * h_pct * (base_ang + h_ang).sin();

            RadialGradient::new(
                Point::from_xy(fx, fy),
                Point::from_xy(s[0], s[1]),
                radius,
                stops,
                SpreadMode::Pad,
                Transform::identity(),
            )
        }
    }
}

/// Parses Lottie's flat gradient stop array (`4 * stop_count` RGB stops followed by
/// optional `2 * M` alpha stops) and merges color + opacity stops cleanly.
fn parse_gradient_stops(raw: &[f32], color_count: usize, global_opacity: f32) -> Vec<GradientStop> {
    let count = color_count.min(raw.len() / 4);
    if count == 0 {
        return Vec::new();
    }

    let mut rgb_stops: Vec<(f32, [f32; 3])> = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * 4;
        let pos = raw[base].clamp(0.0, 1.0);
        let r = raw[base + 1].clamp(0.0, 1.0);
        let g = raw[base + 2].clamp(0.0, 1.0);
        let b = raw[base + 3].clamp(0.0, 1.0);
        rgb_stops.push((pos, [r, g, b]));
    }
    rgb_stops.sort_by(|a, b| a.0.total_cmp(&b.0));

    let alpha_slice = &raw[(count * 4)..];
    let alpha_count = alpha_slice.len() / 2;
    let mut alpha_stops: Vec<(f32, f32)> = Vec::with_capacity(alpha_count);
    for i in 0..alpha_count {
        let pos = alpha_slice[i * 2].clamp(0.0, 1.0);
        let a = alpha_slice[i * 2 + 1].clamp(0.0, 1.0);
        alpha_stops.push((pos, a));
    }
    alpha_stops.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut positions: Vec<f32> = rgb_stops.iter().map(|s| s.0).collect();
    for &(p, _) in &alpha_stops {
        if !positions
            .iter()
            .any(|&existing| (existing - p).abs() < 1e-5)
        {
            positions.push(p);
        }
    }
    positions.sort_by(f32::total_cmp);

    let mut out = Vec::with_capacity(positions.len());
    let mut first_color = None;
    for pos in positions {
        let rgb = sample_rgb_stops(&rgb_stops, pos);
        let a = if alpha_stops.is_empty() {
            1.0
        } else {
            sample_alpha_stops(&alpha_stops, pos)
        };
        let final_a = (a * global_opacity).clamp(0.0, 1.0);
        if let Some(col) = SkColor::from_rgba(rgb[0], rgb[1], rgb[2], final_a) {
            if first_color.is_none() {
                first_color = Some(col);
            }
            out.push(GradientStop::new(pos, col));
        }
    }
    if out.len() == 1
        && let Some(c) = first_color
    {
        out.push(GradientStop::new(1.0, c));
    }
    out
}

fn sample_rgb_stops(stops: &[(f32, [f32; 3])], pos: f32) -> [f32; 3] {
    if pos <= stops[0].0 {
        return stops[0].1;
    }
    let last = stops[stops.len() - 1];
    if pos >= last.0 {
        return last.1;
    }
    for pair in stops.windows(2) {
        if pos <= pair[1].0 {
            let span = pair[1].0 - pair[0].0;
            let t = if span > 1e-6 {
                ((pos - pair[0].0) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return [
                pair[0].1[0] + (pair[1].1[0] - pair[0].1[0]) * t,
                pair[0].1[1] + (pair[1].1[1] - pair[0].1[1]) * t,
                pair[0].1[2] + (pair[1].1[2] - pair[0].1[2]) * t,
            ];
        }
    }
    last.1
}

fn sample_alpha_stops(stops: &[(f32, f32)], pos: f32) -> f32 {
    if pos <= stops[0].0 {
        return stops[0].1;
    }
    let last = stops[stops.len() - 1];
    if pos >= last.0 {
        return last.1;
    }
    for pair in stops.windows(2) {
        if pos <= pair[1].0 {
            let span = pair[1].0 - pair[0].0;
            let t = if span > 1e-6 {
                ((pos - pair[0].0) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return pair[0].1 + (pair[1].1 - pair[0].1) * t;
        }
    }
    last.1
}
