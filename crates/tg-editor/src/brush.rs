//! Brush stroke model, point smoothing, quadratic Bézier path building,
//! arrowhead geometry, and `tiny-skia` rasterization.
//!
//! Port of `editor/scene/scene_item_canvas.h`, `editor/scene/scene_item_canvas.cpp`,
//! and `editor/scene/scene_item_line.cpp`.

use serde::{Deserialize, Serialize};
use tiny_skia::{
    BlendMode, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

use crate::common::BrushTool;
use crate::geometry::{Color, PointF, RectF, SizeF};

/// Minimum distance between consecutive stroke points at zoom 1.0 (`kMinPointDistanceBase`).
pub const MIN_POINT_DISTANCE_BASE: f64 = 2.0;

/// Maximum distance between consecutive points before subdividing (`kMaxPointDistance`).
pub const MAX_POINT_DISTANCE: f64 = 15.0;

/// Smoothing blend weight (`kSmoothingStrength`).
pub const SMOOTHING_STRENGTH: f64 = 0.5;

/// Segment overlap count for incremental rendering (`kSegmentOverlap`).
pub const SEGMENT_OVERLAP: usize = 3;

/// Max jump ratio from `(0, 0)` on the second point (`kOriginStartJumpRatio`).
pub const ORIGIN_START_JUMP_RATIO: f64 = 0.25;

/// Marker opacity multiplier (`photoEditorMarkerOpacity`).
pub const MARKER_OPACITY: f64 = 0.35;

/// Marker width multiplier (`photoEditorMarkerSizeMultiplier`).
pub const MARKER_SIZE_MULTIPLIER: f64 = 2.5;

/// Arrowhead wing length factor (`photoEditorArrowHeadLengthFactor`).
pub const ARROW_HEAD_LENGTH_FACTOR: f64 = 2.5;

/// Minimum distance back from arrow tip for direction calculation (`photoEditorArrowHeadMinDistanceFactor`).
pub const ARROW_HEAD_MIN_DISTANCE_FACTOR: f64 = 1.5;

/// Arrowhead half-angle in degrees (`photoEditorArrowHeadAngleDegrees`).
pub const ARROW_HEAD_ANGLE_DEGREES: f64 = 26.0;

/// Blur brush width multiplier (`photoEditorBlurSizeMultiplier`).
pub const BLUR_SIZE_MULTIPLIER: f64 = 3.0;

/// Single sample point along a brush stroke (`ItemCanvas::StrokePoint`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    /// Position in scene coordinates.
    pub pos: PointF,
    /// Normalized pen/touch pressure (`0.0..=1.0`, default `1.0`).
    #[serde(default = "default_pressure")]
    pub pressure: f64,
    /// Monotonic timestamp in milliseconds.
    #[serde(default)]
    pub time: i64,
}

const fn default_pressure() -> f64 {
    1.0
}

impl StrokePoint {
    /// Creates a stroke point with pressure `1.0` and timestamp `0`.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self {
            pos: PointF::new(x, y),
            pressure: 1.0,
            time: 0,
        }
    }

    /// Creates a stroke point with explicit pressure and timestamp.
    #[must_use]
    pub const fn with_pressure(x: f64, y: f64, pressure: f64, time: i64) -> Self {
        Self {
            pos: PointF::new(x, y),
            pressure,
            time,
        }
    }
}

/// Complete vector description of a freehand brush stroke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushStroke {
    /// Drawing tool (`Pen`, `Arrow`, `Marker`, `Eraser`, `Blur`).
    pub tool: BrushTool,
    /// Brush color (for `Eraser` and `Blur`, rasterization uses opaque black mask).
    pub color: Color,
    /// Base brush size in scene pixels (`1.0..=25.0`).
    pub size: f64,
    /// Sampled stroke points in scene coordinates.
    pub points: Vec<StrokePoint>,
}

impl BrushStroke {
    /// Creates a new stroke with the given tool, color, base size, and points.
    #[must_use]
    pub fn new(tool: BrushTool, color: Color, size: f64, points: Vec<StrokePoint>) -> Self {
        Self {
            tool,
            color,
            size,
            points,
        }
    }

    /// Effective rendered stroke width for a given pressure (`ItemCanvas::strokeWidth`).
    #[must_use]
    pub fn effective_width(&self, pressure: f64) -> f64 {
        let mut w = self.size * pressure;
        match self.tool {
            BrushTool::Marker => w *= MARKER_SIZE_MULTIPLIER,
            BrushTool::Blur => w *= BLUR_SIZE_MULTIPLIER,
            _ => {}
        }
        w.max(0.5)
    }

    /// Effective rendered color (`ItemCanvas::strokeColor`).
    #[must_use]
    pub fn effective_color(&self) -> Color {
        match self.tool {
            BrushTool::Eraser | BrushTool::Blur => Color::BLACK,
            BrushTool::Marker => self.color.with_alpha_factor(MARKER_OPACITY),
            BrushTool::Pen | BrushTool::Arrow => self.color,
        }
    }

    /// Arrowhead wing length (`ItemCanvas::arrowHeadLength`).
    #[must_use]
    pub fn arrow_head_length(&self) -> f64 {
        self.size * ARROW_HEAD_LENGTH_FACTOR
    }

    /// Bounding margin around points required to contain the full stroke + arrowhead.
    #[must_use]
    pub fn margin(&self) -> f64 {
        let extra = if self.tool == BrushTool::Arrow {
            self.arrow_head_length()
        } else {
            0.0
        };
        self.effective_width(1.0) + extra
    }

    /// Axis-aligned bounding box covering all stroke points plus brush margin.
    #[must_use]
    pub fn bounding_rect(&self) -> RectF {
        let mut iter = self.points.iter().filter(|p| p.pos.is_finite());
        let Some(first) = iter.next() else {
            return RectF::default();
        };
        let mut min_x = first.pos.x;
        let mut max_x = first.pos.x;
        let mut min_y = first.pos.y;
        let mut max_y = first.pos.y;
        for pt in iter {
            min_x = min_x.min(pt.pos.x);
            max_x = max_x.max(pt.pos.x);
            min_y = min_y.min(pt.pos.y);
            max_y = max_y.max(pt.pos.y);
        }
        RectF::from_edges(min_x, min_y, max_x, max_y).expanded(self.margin())
    }
}

/// Interactive stroke collector matching `ItemCanvas::addStrokePoint`.
#[derive(Debug, Clone)]
pub struct StrokeBuilder {
    tool: BrushTool,
    color: Color,
    size: f64,
    zoom: f64,
    scene_size: SizeF,
    points: Vec<StrokePoint>,
    last_time: i64,
}

impl StrokeBuilder {
    /// Starts a new stroke builder.
    #[must_use]
    pub fn new(tool: BrushTool, color: Color, size: f64, zoom: f64, scene_size: SizeF) -> Self {
        Self {
            tool,
            color,
            size,
            zoom: if zoom > 0.0 { zoom } else { 1.0 },
            scene_size,
            points: Vec::new(),
            last_time: 0,
        }
    }

    /// Adds a raw input point with distance filtering and linear subdivision,
    /// matching `ItemCanvas::addStrokePoint`.
    pub fn push_point(&mut self, point: PointF, pressure: f64, time: i64) {
        if !point.is_finite() {
            return;
        }
        let pressure = pressure.clamp(0.0, 1.0);

        if self.points.len() == 1 && self.points[0].pos.is_null() && !point.is_null() {
            let max_start_jump =
                self.scene_size.width.max(self.scene_size.height) * ORIGIN_START_JUMP_RATIO;
            if max_start_jump > 0.0 && self.points[0].pos.distance_to(point) > max_start_jump {
                self.points[0] = StrokePoint {
                    pos: point,
                    pressure,
                    time,
                };
                self.last_time = time;
                return;
            }
        }

        if let Some(last) = self.points.last().copied() {
            let distance = point.distance_to(last.pos);
            let min_distance = if self.zoom > 1.0 {
                MIN_POINT_DISTANCE_BASE / self.zoom
            } else {
                MIN_POINT_DISTANCE_BASE * self.zoom
            };
            if distance < min_distance {
                return;
            }
            let max_distance = if self.zoom > 1.0 {
                MAX_POINT_DISTANCE / self.zoom
            } else {
                MAX_POINT_DISTANCE
            };
            if distance > max_distance {
                let steps = (distance / max_distance).ceil() as usize;
                for i in 1..steps {
                    let t = i as f64 / steps as f64;
                    let interpolated = PointF::new(
                        last.pos.x * (1.0 - t) + point.x * t,
                        last.pos.y * (1.0 - t) + point.y * t,
                    );
                    let interp_pressure = last.pressure * (1.0 - t) + pressure * t;
                    let interp_time = self.last_time + ((time - self.last_time) as f64 * t) as i64;
                    self.points.push(StrokePoint {
                        pos: interpolated,
                        pressure: interp_pressure,
                        time: interp_time,
                    });
                }
            }
        }

        self.points.push(StrokePoint {
            pos: point,
            pressure,
            time,
        });
        self.last_time = time;
    }

    /// Returns the currently accumulated points.
    #[must_use]
    pub fn points(&self) -> &[StrokePoint] {
        &self.points
    }

    /// Finalizes the builder into a [`BrushStroke`], returning `None` if fewer than 2 points
    /// were recorded (matching `ItemCanvas::handleMouseReleaseEvent`).
    #[must_use]
    pub fn finish(self) -> Option<BrushStroke> {
        if self.points.len() < 2 {
            return None;
        }
        Some(BrushStroke {
            tool: self.tool,
            color: self.color,
            size: self.size,
            points: self.points,
        })
    }
}

/// Applies one pass of tdesktop's midpoint Laplacian smoothing (`ItemCanvas::smoothStroke`).
///
/// Anchors indices `0`, `1`, and `last` when `points.len() >= 4`.
#[must_use]
pub fn smooth_stroke_once(points: &[StrokePoint]) -> Vec<StrokePoint> {
    if points.len() < 4 {
        return points.to_vec();
    }
    let half_strength = SMOOTHING_STRENGTH / 2.0;
    let inv_strength = 1.0 - SMOOTHING_STRENGTH;
    let mut result = Vec::with_capacity(points.len());
    result.push(points[0]);
    result.push(points[1]);
    for i in 2..points.len() - 1 {
        let prev = points[i - 1].pos;
        let curr = points[i].pos;
        let next = points[i + 1].pos;
        let smoothed = PointF::new(
            curr.x * inv_strength + (prev.x + next.x) * half_strength,
            curr.y * inv_strength + (prev.y + next.y) * half_strength,
        );
        result.push(StrokePoint {
            pos: smoothed,
            pressure: points[i].pressure,
            time: points[i].time,
        });
    }
    result.push(points[points.len() - 1]);
    result
}

/// Applies tdesktop's standard 2-pass smoothing (`ItemCanvas::drawIncrementalStroke`).
#[must_use]
pub fn smooth_stroke(points: &[StrokePoint]) -> Vec<StrokePoint> {
    if points.len() < 4 {
        return points.to_vec();
    }
    let pass1 = smooth_stroke_once(points);
    smooth_stroke_once(&pass1)
}

/// Builds a smooth quadratic Bézier midpoint path (`ItemCanvas::renderSegment`).
#[must_use]
pub fn build_stroke_path(points: &[StrokePoint], start_idx: usize) -> Option<Path> {
    if points.len() < 2 || start_idx >= points.len() - 1 {
        return None;
    }
    if !points.last()?.pos.is_finite() || !points[start_idx].pos.is_finite() {
        return None;
    }
    let mut pb = PathBuilder::new();
    let start = points[start_idx].pos;
    pb.move_to(start.x as f32, start.y as f32);
    for i in start_idx..points.len() - 1 {
        let p0 = points[i].pos;
        let p1 = points[i + 1].pos;
        if !p0.is_finite() || !p1.is_finite() {
            return None;
        }
        let ctrl = PointF::new((p0.x + p1.x) / 2.0, (p0.y + p1.y) / 2.0);
        if i == start_idx {
            pb.line_to(ctrl.x as f32, ctrl.y as f32);
        } else {
            pb.quad_to(p0.x as f32, p0.y as f32, ctrl.x as f32, ctrl.y as f32);
        }
    }
    let last = points[points.len() - 1].pos;
    pb.line_to(last.x as f32, last.y as f32);
    pb.finish()
}

/// Computes the two wing endpoints `(left, right)` of an arrowhead at the end of `points`
/// (`ItemCanvas::drawArrowHead`).
#[must_use]
pub fn compute_arrow_head_wings(
    points: &[StrokePoint],
    base_size: f64,
) -> Option<(PointF, PointF)> {
    if points.len() < 2 {
        return None;
    }
    let tip = points.last()?.pos;
    let min_distance = base_size * ARROW_HEAD_MIN_DISTANCE_FACTOR;
    let mut base = points[0].pos;
    for i in (0..points.len() - 1).rev() {
        let p = points[i].pos;
        if tip.distance_to(p) >= min_distance {
            base = p;
            break;
        }
    }
    let dx = tip.x - base.x;
    let dy = tip.y - base.y;
    let length = dx.hypot(dy);
    if length <= 0.0 || !length.is_finite() {
        return None;
    }
    let dir_x = dx / length;
    let dir_y = dy / length;
    let angle = ARROW_HEAD_ANGLE_DEGREES.to_radians();
    let sin_a = angle.sin();
    let cos_a = angle.cos();
    let head_len = base_size * ARROW_HEAD_LENGTH_FACTOR;

    let rot = |s: f64, c: f64| PointF::new(dir_x * c - dir_y * s, dir_x * s + dir_y * c);
    let left_dir = rot(sin_a, cos_a);
    let right_dir = rot(-sin_a, cos_a);
    let left = PointF::new(tip.x - left_dir.x * head_len, tip.y - left_dir.y * head_len);
    let right = PointF::new(
        tip.x - right_dir.x * head_len,
        tip.y - right_dir.y * head_len,
    );
    Some((left, right))
}

/// Builds the arrowhead path (`ItemCanvas::drawArrowHead`).
#[must_use]
pub fn build_arrow_head_path(points: &[StrokePoint], base_size: f64) -> Option<Path> {
    let tip = points.last()?.pos;
    let (left, right) = compute_arrow_head_wings(points, base_size)?;
    let mut pb = PathBuilder::new();
    pb.move_to(tip.x as f32, tip.y as f32);
    pb.line_to(left.x as f32, left.y as f32);
    pb.move_to(tip.x as f32, tip.y as f32);
    pb.line_to(right.x as f32, right.y as f32);
    pb.finish()
}

/// Rasterizes a [`BrushStroke`] into `pixmap` using `transform` (mapping scene coordinates
/// to pixmap pixels).
///
/// For `Marker`, renders onto a temporary stroke layer with `BlendMode::Source` and composites
/// with `SourceOver` so self-overlapping segments do not double-darken.
pub fn rasterize_stroke(pixmap: &mut Pixmap, stroke: &BrushStroke, transform: Transform) {
    if stroke.points.len() < 2 {
        return;
    }
    let smoothed = smooth_stroke(&stroke.points);
    let Some(path) = build_stroke_path(&smoothed, 0) else {
        return;
    };
    let avg_pressure =
        smoothed.iter().map(|p| p.pressure).sum::<f64>() / smoothed.len().max(1) as f64;
    let width = stroke.effective_width(avg_pressure) as f32;
    let color = stroke.effective_color();

    let skia_stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };

    if stroke.tool == BrushTool::Marker {
        if let Some(mut layer) = Pixmap::new(pixmap.width(), pixmap.height()) {
            let mut paint = Paint {
                anti_alias: true,
                blend_mode: BlendMode::Source,
                ..Paint::default()
            };
            paint.set_color(color.to_skia());
            layer.stroke_path(&path, &paint, &skia_stroke, transform, None);
            pixmap.draw_pixmap(
                0,
                0,
                layer.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        return;
    }

    let mut paint = Paint {
        anti_alias: true,
        blend_mode: BlendMode::SourceOver,
        ..Paint::default()
    };
    paint.set_color(color.to_skia());
    pixmap.stroke_path(&path, &paint, &skia_stroke, transform, None);

    if stroke.tool == BrushTool::Arrow
        && let Some(arrow_path) = build_arrow_head_path(&stroke.points, stroke.size)
    {
        let tip_pressure = stroke.points.last().map_or(1.0, |p| p.pressure);
        let arrow_stroke = Stroke {
            width: stroke.effective_width(tip_pressure) as f32,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&arrow_path, &paint, &arrow_stroke, transform, None);
    }
}

/// Rasterizes an eraser stroke mask into a `Pixmap` of dimensions `(width, height)`
/// using `transform`.
#[must_use]
pub fn rasterize_stroke_mask(
    width: u32,
    height: u32,
    stroke: &BrushStroke,
    transform: Transform,
) -> Option<Pixmap> {
    let mut mask = Pixmap::new(width, height)?;
    let mask_stroke = BrushStroke {
        tool: if stroke.tool == BrushTool::Blur {
            BrushTool::Blur
        } else {
            BrushTool::Eraser
        },
        color: Color::BLACK,
        size: stroke.size,
        points: stroke.points.clone(),
    };
    rasterize_stroke(&mut mask, &mask_stroke, transform);
    Some(mask)
}
