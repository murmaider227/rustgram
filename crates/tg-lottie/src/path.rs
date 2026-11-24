//! Bezier path geometry, Lottie shape builders, and Trim Paths subdivision.
//!
//! Builds exact cubic Bezier subpaths for rectangles, ellipses, freeform paths,
//! stars/polygons, and applies Simultaneous or Individual Trim Paths before
//! converting into `tiny_skia::Path`.

use std::f32::consts::PI;

use tiny_skia::{Path, PathBuilder, Transform};

use crate::model::{
    Direction, EllipseShape, LottiePathData, PathShape, PolystarShape, PolystarType, RectShape,
    TrimMode,
};

/// Cubic bezier approximation constant for quarter circles: `4/3 * (sqrt(2) - 1)`.
const KAPPA: f32 = 0.552_284_8;

/// Polystar tangent factor matching Bodymovin / rlottie (`0.47829`).
const POLYSTAR_MAGIC_NUMBER: f32 = 0.478_29;

/// A single cubic Bezier segment from `p0` to `p3` with control points `p1`, `p2`.
#[derive(Debug, Clone, Copy)]
pub struct CubicSeg {
    /// Start point.
    pub p0: [f32; 2],
    /// First control point.
    pub p1: [f32; 2],
    /// Second control point.
    pub p2: [f32; 2],
    /// End point.
    pub p3: [f32; 2],
}

impl CubicSeg {
    /// Creates a straight-line segment encoded as a cubic Bezier.
    #[must_use]
    pub fn line(a: [f32; 2], b: [f32; 2]) -> Self {
        Self {
            p0: a,
            p1: a,
            p2: b,
            p3: b,
        }
    }

    /// Evaluates the cubic Bezier at parameter `t in [0, 1]`.
    #[must_use]
    pub fn point(&self, t: f32) -> [f32; 2] {
        let inv = 1.0 - t;
        let b0 = inv * inv * inv;
        let b1 = 3.0 * inv * inv * t;
        let b2 = 3.0 * inv * t * t;
        let b3 = t * t * t;
        [
            b0 * self.p0[0] + b1 * self.p1[0] + b2 * self.p2[0] + b3 * self.p3[0],
            b0 * self.p0[1] + b1 * self.p1[1] + b2 * self.p2[1] + b3 * self.p3[1],
        ]
    }

    /// Approximates the arc length of this segment via polyline subdivision.
    #[must_use]
    pub fn arc_length(&self) -> f32 {
        const N: usize = 16;
        let mut prev = self.p0;
        let mut len = 0.0_f32;
        for i in 1..=N {
            let cur = self.point(i as f32 / N as f32);
            len += (cur[0] - prev[0]).hypot(cur[1] - prev[1]);
            prev = cur;
        }
        len
    }

    /// Splits this segment at `t in [0, 1]` using De Casteljau's algorithm.
    #[must_use]
    pub fn split(&self, t: f32) -> (Self, Self) {
        let lerp = |a: [f32; 2], b: [f32; 2]| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        let p01 = lerp(self.p0, self.p1);
        let p12 = lerp(self.p1, self.p2);
        let p23 = lerp(self.p2, self.p3);
        let p012 = lerp(p01, p12);
        let p123 = lerp(p12, p23);
        let mid = lerp(p012, p123);
        (
            Self {
                p0: self.p0,
                p1: p01,
                p2: p012,
                p3: mid,
            },
            Self {
                p0: mid,
                p1: p123,
                p2: p23,
                p3: self.p3,
            },
        )
    }

    /// Extracts the sub-segment over parameter range `[t0, t1]` (`0 <= t0 <= t1 <= 1`).
    #[must_use]
    pub fn sub_segment(&self, t0: f32, t1: f32) -> Self {
        let a = t0.clamp(0.0, 1.0);
        let b = t1.clamp(a, 1.0);
        let (_, right) = self.split(a);
        let denom = 1.0 - a;
        let local_b = if denom > 1e-6 {
            ((b - a) / denom).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let (mid, _) = right.split(local_b);
        mid
    }

    /// Finds parameter `t in [0, 1]` corresponding to arc-length distance `target_len`.
    #[must_use]
    pub fn t_for_distance(&self, target_len: f32) -> f32 {
        const N: usize = 32;
        if target_len <= 0.0 {
            return 0.0;
        }
        let mut prev = self.p0;
        let mut acc = 0.0_f32;
        for i in 1..=N {
            let cur = self.point(i as f32 / N as f32);
            let step = (cur[0] - prev[0]).hypot(cur[1] - prev[1]);
            if acc + step >= target_len && step > 1e-7 {
                let frac = (target_len - acc) / step;
                return ((i - 1) as f32 + frac) / N as f32;
            }
            acc += step;
            prev = cur;
        }
        1.0
    }

    /// Reverses the direction of this segment.
    #[must_use]
    pub fn reversed(&self) -> Self {
        Self {
            p0: self.p3,
            p1: self.p2,
            p2: self.p1,
            p3: self.p0,
        }
    }

    /// Applies a `tiny_skia::Transform` to all four control points.
    #[must_use]
    pub fn transform(&self, ts: Transform) -> Self {
        let mut pts = [
            tiny_skia::Point::from_xy(self.p0[0], self.p0[1]),
            tiny_skia::Point::from_xy(self.p1[0], self.p1[1]),
            tiny_skia::Point::from_xy(self.p2[0], self.p2[1]),
            tiny_skia::Point::from_xy(self.p3[0], self.p3[1]),
        ];
        ts.map_points(&mut pts);
        Self {
            p0: [pts[0].x, pts[0].y],
            p1: [pts[1].x, pts[1].y],
            p2: [pts[2].x, pts[2].y],
            p3: [pts[3].x, pts[3].y],
        }
    }
}

/// A single continuous subpath made of connected cubic Bezier segments.
#[derive(Debug, Clone, Default)]
pub struct SubPath {
    /// Connected cubic segments.
    pub segments: Vec<CubicSeg>,
    /// Whether the subpath is closed.
    pub closed: bool,
}

impl SubPath {
    /// Total arc length of this subpath.
    #[must_use]
    pub fn arc_length(&self) -> f32 {
        self.segments.iter().map(CubicSeg::arc_length).sum()
    }

    /// Reverses the subpath direction.
    pub fn reverse(&mut self) {
        self.segments.reverse();
        for seg in &mut self.segments {
            *seg = seg.reversed();
        }
    }

    /// Transforms all segments in place.
    pub fn transform(&mut self, ts: Transform) {
        for seg in &mut self.segments {
            *seg = seg.transform(ts);
        }
    }

    /// Extracts an open subpath covering arc-length interval `[start_dist, end_dist]`.
    #[must_use]
    pub fn slice_distance(&self, start_dist: f32, end_dist: f32) -> SubPath {
        if self.segments.is_empty() || end_dist <= start_dist + 1e-6 {
            return SubPath::default();
        }
        let mut out = Vec::new();
        let mut acc = 0.0_f32;
        for seg in &self.segments {
            let len = seg.arc_length();
            let seg_end = acc + len;
            if seg_end > start_dist + 1e-6 && acc < end_dist - 1e-6 && len > 1e-7 {
                let d0 = (start_dist - acc).max(0.0);
                let d1 = (end_dist - acc).min(len);
                let t0 = if d0 <= 1e-6 {
                    0.0
                } else {
                    seg.t_for_distance(d0)
                };
                let t1 = if d1 >= len - 1e-6 {
                    1.0
                } else {
                    seg.t_for_distance(d1)
                };
                if t1 > t0 + 1e-6 {
                    out.push(seg.sub_segment(t0, t1));
                }
            }
            acc = seg_end;
        }
        SubPath {
            segments: out,
            closed: false,
        }
    }
}

/// Compound Bezier path containing zero or more subpaths.
#[derive(Debug, Clone, Default)]
pub struct BezierPath {
    /// Subpaths in draw order.
    pub subpaths: Vec<SubPath>,
}

impl BezierPath {
    /// Returns `true` if there are no segments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subpaths.iter().all(|s| s.segments.is_empty())
    }

    /// Appends another `BezierPath`'s subpaths into `self`.
    pub fn extend(&mut self, other: BezierPath) {
        self.subpaths.extend(other.subpaths);
    }

    /// Transforms every subpath in place.
    pub fn transform(&mut self, ts: Transform) {
        for sp in &mut self.subpaths {
            sp.transform(ts);
        }
    }

    /// Converts this `BezierPath` into a `tiny_skia::Path`.
    #[must_use]
    pub fn to_skia_path(&self) -> Option<Path> {
        let mut pb = PathBuilder::new();
        let mut any = false;
        for sp in &self.subpaths {
            if sp.segments.is_empty() {
                continue;
            }
            let first = sp.segments[0].p0;
            pb.move_to(first[0], first[1]);
            for seg in &sp.segments {
                pb.cubic_to(
                    seg.p1[0], seg.p1[1], seg.p2[0], seg.p2[1], seg.p3[0], seg.p3[1],
                );
            }
            if sp.closed {
                pb.close();
            }
            any = true;
        }
        if !any {
            return None;
        }
        pb.finish()
    }

    /// Applies Lottie Trim Paths (`start_pct`, `end_pct` in 0..100, `offset_deg` in degrees).
    #[must_use]
    pub fn trim(&self, start_pct: f32, end_pct: f32, offset_deg: f32, mode: TrimMode) -> Self {
        let mut s = start_pct.clamp(0.0, 100.0) / 100.0;
        let mut e = end_pct.clamp(0.0, 100.0) / 100.0;
        if s > e {
            std::mem::swap(&mut s, &mut e);
        }
        let span = e - s;
        if span <= 1e-6 {
            return Self::default();
        }
        if span >= 1.0 - 1e-6 {
            return self.clone();
        }

        let offset = (offset_deg / 360.0).rem_euclid(1.0);
        let a = (s + offset).rem_euclid(1.0);
        let b = a + span;

        match mode {
            TrimMode::Simultaneous => {
                let mut out = Vec::new();
                for sp in &self.subpaths {
                    let len = sp.arc_length();
                    if len <= 1e-6 {
                        continue;
                    }
                    if b <= 1.0 + 1e-6 {
                        let trimmed = sp.slice_distance(a * len, b.min(1.0) * len);
                        if !trimmed.segments.is_empty() {
                            out.push(trimmed);
                        }
                    } else {
                        // Wrap around the closed seam (or split into two pieces if open).
                        let part1 = sp.slice_distance(a * len, len);
                        let part2 = sp.slice_distance(0.0, (b - 1.0) * len);
                        if sp.closed && !part1.segments.is_empty() && !part2.segments.is_empty() {
                            let mut merged = part1.segments;
                            merged.extend(part2.segments);
                            out.push(SubPath {
                                segments: merged,
                                closed: false,
                            });
                        } else {
                            if !part1.segments.is_empty() {
                                out.push(part1);
                            }
                            if !part2.segments.is_empty() {
                                out.push(part2);
                            }
                        }
                    }
                }
                Self { subpaths: out }
            }
            TrimMode::Individual => {
                let lengths: Vec<f32> = self.subpaths.iter().map(SubPath::arc_length).collect();
                let total: f32 = lengths.iter().sum();
                if total <= 1e-6 {
                    return Self::default();
                }

                let mut intervals = Vec::new();
                if b <= 1.0 + 1e-6 {
                    intervals.push((a * total, b.min(1.0) * total));
                } else {
                    intervals.push((a * total, total));
                    intervals.push((0.0, (b - 1.0) * total));
                }

                let mut out = Vec::new();
                for (iv_start, iv_end) in intervals {
                    let mut cursor = 0.0_f32;
                    for (sp, &len) in self.subpaths.iter().zip(&lengths) {
                        let sp_end = cursor + len;
                        if sp_end > iv_start + 1e-6 && cursor < iv_end - 1e-6 && len > 1e-6 {
                            let local_start = (iv_start - cursor).max(0.0);
                            let local_end = (iv_end - cursor).min(len);
                            let piece = sp.slice_distance(local_start, local_end);
                            if !piece.segments.is_empty() {
                                out.push(piece);
                            }
                        }
                        cursor = sp_end;
                    }
                }
                Self { subpaths: out }
            }
        }
    }
}

/// Builds a `BezierPath` from raw Lottie path data (`v`, `i`, `o`, `c`).
#[must_use]
pub fn build_lottie_path(data: &LottiePathData, direction: Direction) -> BezierPath {
    let n = data.vertices.len();
    if n < 2 {
        return BezierPath::default();
    }
    let count = if data.closed { n } else { n - 1 };
    let mut segments = Vec::with_capacity(count);
    for idx in 0..count {
        let next = (idx + 1) % n;
        let v0 = data.vertices[idx];
        let v1 = data.vertices[next];
        let o0 = data.out_tangents.get(idx).copied().unwrap_or([0.0, 0.0]);
        let i1 = data.in_tangents.get(next).copied().unwrap_or([0.0, 0.0]);
        segments.push(CubicSeg {
            p0: v0,
            p1: [v0[0] + o0[0], v0[1] + o0[1]],
            p2: [v1[0] + i1[0], v1[1] + i1[1]],
            p3: v1,
        });
    }
    let mut sp = SubPath {
        segments,
        closed: data.closed,
    };
    if direction == Direction::CounterClockwise {
        sp.reverse();
    }
    BezierPath { subpaths: vec![sp] }
}

/// Evaluates a `PathShape` at `frame`.
#[must_use]
pub fn eval_path_shape(shape: &PathShape, frame: f64) -> BezierPath {
    let data = shape.path.eval(frame);
    build_lottie_path(&data, shape.direction)
}

/// Evaluates a `RectShape` at `frame` into a `BezierPath`.
#[must_use]
pub fn eval_rect_shape(shape: &RectShape, frame: f64) -> BezierPath {
    let pos = shape.position.eval(frame);
    let size = shape.size.eval(frame);
    let hw = size[0].abs() * 0.5;
    let hh = size[1].abs() * 0.5;
    if hw <= 1e-6 || hh <= 1e-6 {
        return BezierPath::default();
    }
    let r = shape.roundness.eval(frame).max(0.0).min(hw).min(hh);
    let left = pos[0] - hw;
    let right = pos[0] + hw;
    let top = pos[1] - hh;
    let bottom = pos[1] + hh;

    let mut segments = Vec::with_capacity(8);
    if r <= 1e-5 {
        // Lottie rect starts at top-right, goes clockwise: TR -> BR -> BL -> TL -> TR
        segments.push(CubicSeg::line([right, top], [right, bottom]));
        segments.push(CubicSeg::line([right, bottom], [left, bottom]));
        segments.push(CubicSeg::line([left, bottom], [left, top]));
        segments.push(CubicSeg::line([left, top], [right, top]));
    } else {
        let c = r * KAPPA;
        // Start at top-right corner's right edge (right, top + r)
        if hh - r > 1e-5 {
            segments.push(CubicSeg::line([right, top + r], [right, bottom - r]));
        }
        // Bottom-right arc
        segments.push(CubicSeg {
            p0: [right, bottom - r],
            p1: [right, bottom - r + c],
            p2: [right - r + c, bottom],
            p3: [right - r, bottom],
        });
        if hw - r > 1e-5 {
            segments.push(CubicSeg::line([right - r, bottom], [left + r, bottom]));
        }
        // Bottom-left arc
        segments.push(CubicSeg {
            p0: [left + r, bottom],
            p1: [left + r - c, bottom],
            p2: [left, bottom - r + c],
            p3: [left, bottom - r],
        });
        if hh - r > 1e-5 {
            segments.push(CubicSeg::line([left, bottom - r], [left, top + r]));
        }
        // Top-left arc
        segments.push(CubicSeg {
            p0: [left, top + r],
            p1: [left, top + r - c],
            p2: [left + r - c, top],
            p3: [left + r, top],
        });
        if hw - r > 1e-5 {
            segments.push(CubicSeg::line([left + r, top], [right - r, top]));
        }
        // Top-right arc
        segments.push(CubicSeg {
            p0: [right - r, top],
            p1: [right - r + c, top],
            p2: [right, top + r - c],
            p3: [right, top + r],
        });
    }

    let mut sp = SubPath {
        segments,
        closed: true,
    };
    if shape.direction == Direction::CounterClockwise {
        sp.reverse();
    }
    BezierPath { subpaths: vec![sp] }
}

/// Evaluates an `EllipseShape` at `frame` into a 4-cubic `BezierPath`.
#[must_use]
pub fn eval_ellipse_shape(shape: &EllipseShape, frame: f64) -> BezierPath {
    let pos = shape.position.eval(frame);
    let size = shape.size.eval(frame);
    let rx = size[0].abs() * 0.5;
    let ry = size[1].abs() * 0.5;
    if rx <= 1e-6 || ry <= 1e-6 {
        return BezierPath::default();
    }
    let cx = pos[0];
    let cy = pos[1];
    let kx = rx * KAPPA;
    let ky = ry * KAPPA;

    // Lottie ellipse starts at top (cx, cy - ry) and proceeds clockwise.
    let segments = vec![
        CubicSeg {
            p0: [cx, cy - ry],
            p1: [cx + kx, cy - ry],
            p2: [cx + rx, cy - ky],
            p3: [cx + rx, cy],
        },
        CubicSeg {
            p0: [cx + rx, cy],
            p1: [cx + rx, cy + ky],
            p2: [cx + kx, cy + ry],
            p3: [cx, cy + ry],
        },
        CubicSeg {
            p0: [cx, cy + ry],
            p1: [cx - kx, cy + ry],
            p2: [cx - rx, cy + ky],
            p3: [cx - rx, cy],
        },
        CubicSeg {
            p0: [cx - rx, cy],
            p1: [cx - rx, cy - ky],
            p2: [cx - kx, cy - ry],
            p3: [cx, cy - ry],
        },
    ];
    let mut sp = SubPath {
        segments,
        closed: true,
    };
    if shape.direction == Direction::CounterClockwise {
        sp.reverse();
    }
    BezierPath { subpaths: vec![sp] }
}

/// Evaluates a `PolystarShape` (Star or Polygon) at `frame` into a `BezierPath`.
#[must_use]
pub fn eval_polystar_shape(shape: &PolystarShape, frame: f64) -> BezierPath {
    let pos = shape.position.eval(frame);
    let pts = shape.points.eval(frame).clamp(3.0, 256.0);
    let rot_rad = (shape.rotation.eval(frame) - 90.0).to_radians();
    let outer_r = shape.outer_radius.eval(frame);
    let outer_round = shape.outer_roundness.eval(frame) / 100.0;

    let mut sp = match shape.star_type {
        PolystarType::Polygon => {
            let num_pts = pts.floor() as usize;
            let angle_per_pt = (2.0 * PI) / num_pts as f32;
            let mut cur_angle = rot_rad;
            let mut vertices = Vec::with_capacity(num_pts);
            let mut tangents = Vec::with_capacity(num_pts);

            for _ in 0..num_pts {
                let x = pos[0] + outer_r * cur_angle.cos();
                let y = pos[1] + outer_r * cur_angle.sin();
                vertices.push([x, y]);
                let tangent_len = if outer_round.abs() > 1e-6 {
                    let circ = 2.0 * PI * outer_r / num_pts as f32;
                    circ * outer_round * 0.25
                } else {
                    0.0
                };
                // Tangent perpendicular to radius vector
                tangents.push([
                    -cur_angle.sin() * tangent_len,
                    cur_angle.cos() * tangent_len,
                ]);
                cur_angle += angle_per_pt;
            }

            let mut segments = Vec::with_capacity(num_pts);
            for i in 0..num_pts {
                let next = (i + 1) % num_pts;
                let p0 = vertices[i];
                let p3 = vertices[next];
                let t0 = tangents[i];
                let t1 = tangents[next];
                segments.push(CubicSeg {
                    p0,
                    p1: [p0[0] + t0[0], p0[1] + t0[1]],
                    p2: [p3[0] - t1[0], p3[1] - t1[1]],
                    p3,
                });
            }
            SubPath {
                segments,
                closed: true,
            }
        }
        PolystarType::Star => {
            let inner_r = shape.inner_radius.eval(frame);
            let inner_round = shape.inner_roundness.eval(frame) / 100.0;

            let full_pts = pts.floor() as usize;
            let partial = pts - pts.floor();
            let angle_per_pt = (2.0 * PI) / pts;
            let half_angle = angle_per_pt * 0.5;
            let total_steps = if partial > 1e-5 {
                (full_pts + 1) * 2
            } else {
                full_pts * 2
            };

            let mut cur_angle = rot_rad;
            if partial > 1e-5 {
                cur_angle += half_angle * (1.0 - partial);
            }

            let mut segments = Vec::with_capacity(total_steps);
            let mut prev_x = pos[0] + outer_r * cur_angle.cos();
            let mut prev_y = pos[1] + outer_r * cur_angle.sin();
            let mut is_outer = false; // next vertex is inner

            for step in 0..total_steps {
                let r = if is_outer { outer_r } else { inner_r };
                let d_angle = if partial > 1e-5 && (step == 1 || step == total_steps - 1) {
                    half_angle * partial
                } else {
                    half_angle
                };
                cur_angle += d_angle;
                let x = pos[0] + r * cur_angle.cos();
                let y = pos[1] + r * cur_angle.sin();

                if inner_round.abs() <= 1e-6 && outer_round.abs() <= 1e-6 {
                    segments.push(CubicSeg::line([prev_x, prev_y], [x, y]));
                } else {
                    let prev_angle = (prev_y - pos[1]).atan2(prev_x - pos[0]) - PI * 0.5;
                    let cp1_dx = prev_angle.cos();
                    let cp1_dy = prev_angle.sin();

                    let next_angle = (y - pos[1]).atan2(x - pos[0]) - PI * 0.5;
                    let cp2_dx = next_angle.cos();
                    let cp2_dy = next_angle.sin();

                    let cp1_round = if is_outer { inner_round } else { outer_round };
                    let cp2_round = if is_outer { outer_round } else { inner_round };
                    let cp1_r = if is_outer { inner_r } else { outer_r };
                    let cp2_r = if is_outer { outer_r } else { inner_r };

                    let cp1_x = cp1_r * cp1_round * POLYSTAR_MAGIC_NUMBER * cp1_dx / pts;
                    let cp1_y = cp1_r * cp1_round * POLYSTAR_MAGIC_NUMBER * cp1_dy / pts;
                    let cp2_x = cp2_r * cp2_round * POLYSTAR_MAGIC_NUMBER * cp2_dx / pts;
                    let cp2_y = cp2_r * cp2_round * POLYSTAR_MAGIC_NUMBER * cp2_dy / pts;

                    segments.push(CubicSeg {
                        p0: [prev_x, prev_y],
                        p1: [prev_x - cp1_x, prev_y - cp1_y],
                        p2: [x + cp2_x, y + cp2_y],
                        p3: [x, y],
                    });
                }

                prev_x = x;
                prev_y = y;
                is_outer = !is_outer;
            }
            SubPath {
                segments,
                closed: true,
            }
        }
    };

    if shape.direction == Direction::CounterClockwise {
        sp.reverse();
    }
    BezierPath { subpaths: vec![sp] }
}
