//! Lottie JSON data model, parser, and keyframe/easing evaluator.
//!
//! Parses the full Telegram `.tgs` Lottie subset cleanly from `serde_json::Value`
//! with strict recursion guards and zero panics on malformed input.

use std::collections::HashMap;

use serde_json::Value;

use crate::common::Color;
use crate::error::Error;

/// Maximum nesting depth for shape groups and precomps to defend against hostile JSON.
const MAX_PARSE_DEPTH: usize = 64;

/// Maximum number of layers accepted in a single composition/precomp.
const MAX_LAYERS: usize = 4096;

/// Maximum number of keyframes per property.
const MAX_KEYFRAMES: usize = 16_384;

/// Maximum number of vertices in a single bezier path.
const MAX_VERTICES: usize = 16_384;

/// Parsed top-level Lottie composition.
#[derive(Debug, Clone)]
pub struct Composition {
    /// Lottie format version string (e.g. `"5.5.2"`).
    pub version: String,
    /// Composition name.
    pub name: String,
    /// Native frame rate in frames per second (`fr`).
    pub frame_rate: f64,
    /// In point in frames (`ip`).
    pub in_point: f64,
    /// Out point in frames (`op`).
    pub out_point: f64,
    /// Composition width in pixels (`w`).
    pub width: u32,
    /// Composition height in pixels (`h`).
    pub height: u32,
    /// Precomposition assets keyed by asset `id`.
    pub assets: HashMap<String, PrecompAsset>,
    /// Layers ordered top-to-bottom as in Lottie JSON.
    pub layers: Vec<Layer>,
    /// Named timeline markers (`markers`).
    pub markers: Vec<Marker>,
}

impl Composition {
    /// Parses a Lottie JSON string into a `Composition`.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let root: Value =
            serde_json::from_str(json).map_err(|e| Error::ParseFailed(e.to_string()))?;
        Self::from_value(&root)
    }

    /// Parses a Lottie `serde_json::Value` into a `Composition`.
    pub fn from_value(root: &Value) -> Result<Self, Error> {
        let obj = root
            .as_object()
            .ok_or_else(|| Error::ParseFailed("root JSON must be an object".into()))?;

        let frame_rate = get_f64(obj.get("fr")).unwrap_or(0.0);
        let in_point = get_f64(obj.get("ip")).unwrap_or(0.0);
        let out_point = get_f64(obj.get("op")).unwrap_or(0.0);
        let width_f = get_f64(obj.get("w")).unwrap_or(0.0);
        let height_f = get_f64(obj.get("h")).unwrap_or(0.0);

        if !frame_rate.is_finite()
            || frame_rate <= 0.0
            || !in_point.is_finite()
            || !out_point.is_finite()
            || out_point < in_point
            || !width_f.is_finite()
            || !height_f.is_finite()
            || width_f <= 0.0
            || height_f <= 0.0
        {
            return Err(Error::ParseFailed(
                "invalid composition dimensions, frame rate, or frame range".into(),
            ));
        }

        let width = width_f.round().clamp(1.0, u32::MAX as f64) as u32;
        let height = height_f.round().clamp(1.0, u32::MAX as f64) as u32;

        let version = obj
            .get("v")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = obj
            .get("nm")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let mut assets = HashMap::new();
        if let Some(arr) = obj.get("assets").and_then(Value::as_array) {
            for item in arr.iter().take(1024) {
                if let Some(asset) = parse_precomp_asset(item) {
                    assets.insert(asset.id.clone(), asset);
                }
            }
        }

        let layers = obj
            .get("layers")
            .and_then(Value::as_array)
            .map(|arr| parse_layers(arr))
            .unwrap_or_default();

        let markers = obj
            .get("markers")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(parse_marker).collect())
            .unwrap_or_default();

        Ok(Self {
            version,
            name,
            frame_rate,
            in_point,
            out_point,
            width,
            height,
            assets,
            layers,
            markers,
        })
    }

    /// Total number of frames (`ceil(op - ip)` or rounded duration, at least 0).
    #[must_use]
    pub fn total_frames(&self) -> u32 {
        let diff = (self.out_point - self.in_point).max(0.0);
        diff.round() as u32
    }

    /// Looks up a timeline marker by its comment/name.
    #[must_use]
    pub fn marker(&self, name: &str) -> Option<&Marker> {
        self.markers.iter().find(|m| m.comment == name)
    }
}

/// Named timeline marker (`markers[]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    /// Marker comment/label (`cm`).
    pub comment: String,
    /// Start frame (`tm`).
    pub time: f64,
    /// Duration in frames (`dr`).
    pub duration: f64,
}

/// Precomposition asset (`assets[]` with `layers`).
#[derive(Debug, Clone)]
pub struct PrecompAsset {
    /// Asset identifier referenced by `Precomp` layers (`id`).
    pub id: String,
    /// Optional explicit width (`w`).
    pub width: Option<u32>,
    /// Optional explicit height (`h`).
    pub height: Option<u32>,
    /// Optional asset frame rate (`fr`).
    pub frame_rate: Option<f64>,
    /// Child layers inside this precomposition.
    pub layers: Vec<Layer>,
}

/// Layer blend mode (`bm`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlendMode {
    /// Normal source-over blending (`bm == 0`).
    #[default]
    Normal,
    /// Multiply (`bm == 1`).
    Multiply,
    /// Screen (`bm == 2`).
    Screen,
    /// Overlay (`bm == 3`).
    Overlay,
    /// Darken (`bm == 4`).
    Darken,
    /// Lighten (`bm == 5`).
    Lighten,
    /// Color Dodge (`bm == 6`).
    ColorDodge,
    /// Color Burn (`bm == 7`).
    ColorBurn,
    /// Hard Light (`bm == 8`).
    HardLight,
    /// Soft Light (`bm == 9`).
    SoftLight,
    /// Difference (`bm == 10`).
    Difference,
    /// Exclusion (`bm == 11`).
    Exclusion,
    /// Hue (`bm == 12`).
    Hue,
    /// Saturation (`bm == 13`).
    Saturation,
    /// Color (`bm == 14`).
    Color,
    /// Luminosity (`bm == 15`).
    Luminosity,
}

/// Track matte type (`tt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatteType {
    /// Alpha matte (`tt == 1`).
    Alpha,
    /// Alpha inverted matte (`tt == 2`).
    AlphaInverted,
    /// Luma matte (`tt == 3`).
    Luma,
    /// Luma inverted matte (`tt == 4`).
    LumaInverted,
}

/// A single Lottie layer.
#[derive(Debug, Clone)]
pub struct Layer {
    /// Layer index (`ind`) used by `parent` and `tp`.
    pub index: Option<i64>,
    /// Layer name (`nm`).
    pub name: String,
    /// Parent layer index (`parent`).
    pub parent: Option<i64>,
    /// Layer hidden flag (`hd`).
    pub hidden: bool,
    /// In point in frames (`ip`).
    pub in_point: f64,
    /// Out point in frames (`op`).
    pub out_point: f64,
    /// Start time offset (`st`).
    pub start_time: f64,
    /// Time stretch factor (`sr`, default `1.0`).
    pub stretch: f64,
    /// Layer transform (`ks`).
    pub transform: TransformProp,
    /// Auto-orient along motion path (`ao == 1`).
    pub auto_orient: bool,
    /// Layer blend mode (`bm`).
    pub blend_mode: BlendMode,
    /// Track matte mode (`tt`).
    pub matte_type: Option<MatteType>,
    /// Explicit matte source layer index (`tp`, Lottie 5.9+).
    pub matte_parent: Option<i64>,
    /// Whether this layer is marked as a track matte source (`td == 1`).
    pub is_matte_source: bool,
    /// Layer masks (`masksProperties`).
    pub masks: Vec<Mask>,
    /// Layer-type-specific payload.
    pub kind: LayerKind,
}

/// Type-specific payload of a `Layer`.
#[derive(Debug, Clone)]
pub enum LayerKind {
    /// Precomposition layer (`ty == 0`).
    Precomp {
        /// Referenced asset ID (`refId`).
        ref_id: String,
        /// Precomp clip width (`w`).
        width: f32,
        /// Precomp clip height (`h`).
        height: f32,
        /// Optional time remapping property in seconds (`tm`).
        time_remap: Option<AnimatedScalar>,
    },
    /// Solid color rectangle layer (`ty == 1`).
    Solid {
        /// Solid width (`sw`).
        width: f32,
        /// Solid height (`sh`).
        height: f32,
        /// Solid color (`sc`).
        color: Color,
    },
    /// Null transform-only layer (`ty == 3`).
    Null,
    /// Vector shape layer (`ty == 4`).
    Shape {
        /// Top-level shape items (`shapes`).
        shapes: Vec<ShapeItem>,
    },
    /// Unsupported layer type (parsed cleanly and ignored during rendering).
    Unsupported,
}

/// Mask compositing mode (`masksProperties[].mode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MaskMode {
    /// No effect (`"n"`).
    None,
    /// Additive mask (`"a"`).
    #[default]
    Add,
    /// Subtractive mask (`"s"`).
    Subtract,
    /// Intersection mask (`"i"`).
    Intersect,
    /// Difference mask (`"f"`).
    Difference,
}

/// A single vector mask on a layer.
#[derive(Debug, Clone)]
pub struct Mask {
    /// Mask mode.
    pub mode: MaskMode,
    /// Whether the mask shape is inverted (`inv`).
    pub inverted: bool,
    /// Mask bezier path (`pt`).
    pub path: AnimatedPath,
    /// Mask opacity 0..100 (`o`).
    pub opacity: AnimatedScalar,
    /// Mask expansion in pixels (`x`).
    pub expansion: AnimatedScalar,
}

/// Layer or group transform property (`ks` or `tr`).
#[derive(Debug, Clone)]
pub struct TransformProp {
    /// Anchor point (`a`).
    pub anchor: AnimatedVec2,
    /// Position (`p`, either unified or separated `x`/`y`).
    pub position: AnimatedPosition,
    /// Scale percentage (`s`, default `[100, 100]`).
    pub scale: AnimatedVec2,
    /// Rotation in degrees (`r` or `rz`).
    pub rotation: AnimatedScalar,
    /// Opacity 0..100 (`o`, default `100`).
    pub opacity: AnimatedScalar,
    /// Skew angle in degrees (`sk`).
    pub skew: AnimatedScalar,
    /// Skew axis angle in degrees (`sa`).
    pub skew_axis: AnimatedScalar,
}

impl Default for TransformProp {
    fn default() -> Self {
        Self {
            anchor: AnimatedVec2::Static([0.0, 0.0]),
            position: AnimatedPosition::Combined(AnimatedVec2::Static([0.0, 0.0])),
            scale: AnimatedVec2::Static([100.0, 100.0]),
            rotation: AnimatedScalar::Static(0.0),
            opacity: AnimatedScalar::Static(100.0),
            skew: AnimatedScalar::Static(0.0),
            skew_axis: AnimatedScalar::Static(0.0),
        }
    }
}

/// Position property supporting both unified 2D keyframes and separated `x`/`y` tracks.
#[derive(Debug, Clone)]
pub enum AnimatedPosition {
    /// Combined 2D position (`s` absent or false).
    Combined(AnimatedVec2),
    /// Separated dimensions (`s: true`, `x`, `y`).
    Separated {
        /// X coordinate track.
        x: AnimatedScalar,
        /// Y coordinate track.
        y: AnimatedScalar,
    },
}

impl AnimatedPosition {
    /// Evaluates the `[x, y]` position at `frame`.
    #[must_use]
    pub fn eval(&self, frame: f64) -> [f32; 2] {
        match self {
            Self::Combined(v) => v.eval(frame),
            Self::Separated { x, y } => [x.eval(frame), y.eval(frame)],
        }
    }

    /// Evaluates the motion tangent angle (degrees) at `frame` for auto-orient layers.
    #[must_use]
    pub fn eval_tangent_angle_deg(&self, frame: f64) -> f32 {
        let eps = 0.05;
        let p0 = self.eval(frame - eps);
        let p1 = self.eval(frame + eps);
        let dx = p1[0] - p0[0];
        let dy = p1[1] - p0[1];
        if dx * dx + dy * dy < 1e-10 {
            0.0
        } else {
            dy.atan2(dx).to_degrees()
        }
    }
}

/// Per-dimension cubic-bezier easing handles for a keyframe.
#[derive(Debug, Clone)]
pub struct EasingHandles {
    /// Out-tangent X values (per dimension, clamped to `[0, 1]`).
    pub out_x: Vec<f32>,
    /// Out-tangent Y values (per dimension).
    pub out_y: Vec<f32>,
    /// In-tangent X values (per dimension, clamped to `[0, 1]`).
    pub in_x: Vec<f32>,
    /// In-tangent Y values (per dimension).
    pub in_y: Vec<f32>,
}

impl EasingHandles {
    /// Linear easing handles `(0, 0) -> (1, 1)`.
    #[must_use]
    pub fn linear() -> Self {
        Self {
            out_x: vec![0.0],
            out_y: vec![0.0],
            in_x: vec![1.0],
            in_y: vec![1.0],
        }
    }

    /// Evaluates the cubic-bezier easing for dimension `dim` at normalized time `t in [0, 1]`.
    #[must_use]
    pub fn ease(&self, dim: usize, t: f32) -> f32 {
        let x1 = get_dim(&self.out_x, dim, 0.0).clamp(0.0, 1.0);
        let y1 = get_dim(&self.out_y, dim, 0.0);
        let x2 = get_dim(&self.in_x, dim, 1.0).clamp(0.0, 1.0);
        let y2 = get_dim(&self.in_y, dim, 1.0);
        cubic_bezier_ease(x1, y1, x2, y2, t)
    }
}

fn get_dim(v: &[f32], dim: usize, default: f32) -> f32 {
    if v.is_empty() {
        default
    } else if dim < v.len() {
        v[dim]
    } else {
        v[v.len() - 1]
    }
}

/// Solves a 1D cubic bezier easing curve `(0,0) -> (x1,y1) -> (x2,y2) -> (1,1)` at `t`.
#[must_use]
pub fn cubic_bezier_ease(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    if (x1 - y1).abs() < 1e-6 && (x2 - y2).abs() < 1e-6 {
        return t;
    }

    // Solve B_x(u) = t using Newton-Raphson with bisection fallback.
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;

    let sample_x = |u: f32| ((ax * u + bx) * u + cx) * u;
    let sample_dx = |u: f32| (3.0 * ax * u + 2.0 * bx) * u + cx;

    let mut u = t;
    let mut solved = false;
    for _ in 0..8 {
        let err = sample_x(u) - t;
        if err.abs() < 1e-6 {
            solved = true;
            break;
        }
        let d = sample_dx(u);
        if d.abs() < 1e-6 {
            break;
        }
        u = (u - err / d).clamp(0.0, 1.0);
    }

    if !solved {
        let mut lo = 0.0_f32;
        let mut hi = 1.0_f32;
        u = t;
        for _ in 0..20 {
            let x = sample_x(u);
            if (x - t).abs() < 1e-6 {
                break;
            }
            if t > x {
                lo = u;
            } else {
                hi = u;
            }
            u = 0.5 * (lo + hi);
        }
    }

    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    ((ay * u + by) * u + cy) * u
}

/// Generic keyframe entry.
#[derive(Debug, Clone)]
pub struct Keyframe<T> {
    /// Keyframe timestamp in frames (`t`).
    pub time: f64,
    /// Value at `time` (`s`).
    pub start: T,
    /// Optional explicit end value (`e`, legacy Lottie < 5).
    pub end: Option<T>,
    /// Hold keyframe flag (`h == 1`).
    pub hold: bool,
    /// Temporal easing handles (`o` and `i`).
    pub easing: EasingHandles,
    /// Spatial out-tangent (`to`, used for 2D position curves).
    pub spatial_out: Option<[f32; 2]>,
    /// Spatial in-tangent (`ti`, used for 2D position curves).
    pub spatial_in: Option<[f32; 2]>,
}

/// Animated scalar property.
#[derive(Debug, Clone)]
pub enum AnimatedScalar {
    /// Constant value.
    Static(f32),
    /// Keyframed timeline.
    Keyframed(Vec<Keyframe<f32>>),
}

impl AnimatedScalar {
    /// Evaluates the scalar at `frame`.
    #[must_use]
    pub fn eval(&self, frame: f64) -> f32 {
        match self {
            Self::Static(v) => *v,
            Self::Keyframed(kfs) => eval_keyframes(kfs, frame, 0.0, |a, b, kf, t| {
                let p = kf.easing.ease(0, t);
                a + (b - a) * p
            }),
        }
    }
}

/// Animated 2D vector property.
#[derive(Debug, Clone)]
pub enum AnimatedVec2 {
    /// Constant 2D value.
    Static([f32; 2]),
    /// Keyframed timeline.
    Keyframed(Vec<Keyframe<[f32; 2]>>),
}

impl AnimatedVec2 {
    /// Evaluates the 2D vector at `frame` (supporting per-dimension easing and spatial bezier tangents).
    #[must_use]
    pub fn eval(&self, frame: f64) -> [f32; 2] {
        match self {
            Self::Static(v) => *v,
            Self::Keyframed(kfs) => eval_keyframes(kfs, frame, [0.0, 0.0], |a, b, kf, t| {
                let has_spatial = kf
                    .spatial_out
                    .is_some_and(|v| v[0].abs() > 1e-6 || v[1].abs() > 1e-6)
                    || kf
                        .spatial_in
                        .is_some_and(|v| v[0].abs() > 1e-6 || v[1].abs() > 1e-6);
                if has_spatial {
                    let p = kf.easing.ease(0, t);
                    let to = kf.spatial_out.unwrap_or([0.0, 0.0]);
                    let ti = kf.spatial_in.unwrap_or([0.0, 0.0]);
                    eval_spatial_cubic(
                        a,
                        [a[0] + to[0], a[1] + to[1]],
                        [b[0] + ti[0], b[1] + ti[1]],
                        b,
                        p,
                    )
                } else {
                    let px = kf.easing.ease(0, t);
                    let py = kf.easing.ease(1, t);
                    [a[0] + (b[0] - a[0]) * px, a[1] + (b[1] - a[1]) * py]
                }
            }),
        }
    }
}

/// Evaluates a 2D spatial cubic bezier with arc-length parameterization.
fn eval_spatial_cubic(
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    frac: f32,
) -> [f32; 2] {
    let f = frac.clamp(0.0, 1.0);
    const STEPS: usize = 32;
    let mut lengths = [0.0_f32; STEPS + 1];
    let mut prev = p0;
    let mut acc = 0.0_f32;
    for (i, slot) in lengths.iter_mut().enumerate().skip(1) {
        let u = i as f32 / STEPS as f32;
        let cur = cubic_point_2d(p0, p1, p2, p3, u);
        acc += (cur[0] - prev[0]).hypot(cur[1] - prev[1]);
        *slot = acc;
        prev = cur;
    }
    if acc < 1e-6 {
        return cubic_point_2d(p0, p1, p2, p3, f);
    }
    let target = f * acc;
    let mut lo = 0;
    let mut hi = STEPS;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if lengths[mid] < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let seg_len = lengths[hi] - lengths[lo];
    let local = if seg_len > 1e-6 {
        (target - lengths[lo]) / seg_len
    } else {
        0.0
    };
    let u = (lo as f32 + local) / STEPS as f32;
    cubic_point_2d(p0, p1, p2, p3, u)
}

fn cubic_point_2d(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], u: f32) -> [f32; 2] {
    let inv = 1.0 - u;
    let b0 = inv * inv * inv;
    let b1 = 3.0 * inv * inv * u;
    let b2 = 3.0 * inv * u * u;
    let b3 = u * u * u;
    [
        b0 * p0[0] + b1 * p1[0] + b2 * p2[0] + b3 * p3[0],
        b0 * p0[1] + b1 * p1[1] + b2 * p2[1] + b3 * p3[1],
    ]
}

/// Animated RGBA color in normalized `[0.0, 1.0]` space.
#[derive(Debug, Clone)]
pub enum AnimatedColor {
    /// Constant RGBA color.
    Static([f32; 4]),
    /// Keyframed RGBA color.
    Keyframed(Vec<Keyframe<[f32; 4]>>),
}

impl AnimatedColor {
    /// Evaluates the normalized `[r, g, b, a]` color at `frame`.
    #[must_use]
    pub fn eval(&self, frame: f64) -> [f32; 4] {
        match self {
            Self::Static(v) => *v,
            Self::Keyframed(kfs) => {
                eval_keyframes(kfs, frame, [0.0, 0.0, 0.0, 1.0], |a, b, kf, t| {
                    let p = kf.easing.ease(0, t);
                    [
                        (a[0] + (b[0] - a[0]) * p).clamp(0.0, 1.0),
                        (a[1] + (b[1] - a[1]) * p).clamp(0.0, 1.0),
                        (a[2] + (b[2] - a[2]) * p).clamp(0.0, 1.0),
                        (a[3] + (b[3] - a[3]) * p).clamp(0.0, 1.0),
                    ]
                })
            }
        }
    }
}

/// Raw Lottie bezier path data (`v`, `i`, `o`, `c`).
#[derive(Debug, Clone, Default)]
pub struct LottiePathData {
    /// Vertices (`v`).
    pub vertices: Vec<[f32; 2]>,
    /// Relative in-tangents (`i`).
    pub in_tangents: Vec<[f32; 2]>,
    /// Relative out-tangents (`o`).
    pub out_tangents: Vec<[f32; 2]>,
    /// Closed path flag (`c`).
    pub closed: bool,
}

/// Animated bezier path property.
#[derive(Debug, Clone)]
pub enum AnimatedPath {
    /// Static path.
    Static(LottiePathData),
    /// Keyframed morphing path.
    Keyframed(Vec<Keyframe<LottiePathData>>),
}

impl AnimatedPath {
    /// Evaluates the bezier path at `frame`.
    #[must_use]
    pub fn eval(&self, frame: f64) -> LottiePathData {
        match self {
            Self::Static(p) => p.clone(),
            Self::Keyframed(kfs) => {
                eval_keyframes_ref(kfs, frame, LottiePathData::default(), |a, b, kf, t| {
                    let p = kf.easing.ease(0, t);
                    let n = a.vertices.len().min(b.vertices.len());
                    let mut vertices = Vec::with_capacity(n);
                    let mut in_tangents = Vec::with_capacity(n);
                    let mut out_tangents = Vec::with_capacity(n);
                    for idx in 0..n {
                        let va = a.vertices[idx];
                        let vb = b.vertices[idx];
                        vertices.push([va[0] + (vb[0] - va[0]) * p, va[1] + (vb[1] - va[1]) * p]);

                        let ia = a.in_tangents.get(idx).copied().unwrap_or([0.0, 0.0]);
                        let ib = b.in_tangents.get(idx).copied().unwrap_or([0.0, 0.0]);
                        in_tangents
                            .push([ia[0] + (ib[0] - ia[0]) * p, ia[1] + (ib[1] - ia[1]) * p]);

                        let oa = a.out_tangents.get(idx).copied().unwrap_or([0.0, 0.0]);
                        let ob = b.out_tangents.get(idx).copied().unwrap_or([0.0, 0.0]);
                        out_tangents
                            .push([oa[0] + (ob[0] - oa[0]) * p, oa[1] + (ob[1] - oa[1]) * p]);
                    }
                    LottiePathData {
                        vertices,
                        in_tangents,
                        out_tangents,
                        closed: a.closed || b.closed,
                    }
                })
            }
        }
    }
}

/// Animated flat gradient stop array (`g.k`).
#[derive(Debug, Clone)]
pub enum AnimatedGradient {
    /// Static flat stop values.
    Static(Vec<f32>),
    /// Keyframed flat stop values.
    Keyframed(Vec<Keyframe<Vec<f32>>>),
}

impl AnimatedGradient {
    /// Evaluates the flat gradient stop array at `frame`.
    #[must_use]
    pub fn eval(&self, frame: f64) -> Vec<f32> {
        match self {
            Self::Static(v) => v.clone(),
            Self::Keyframed(kfs) => eval_keyframes_ref(kfs, frame, Vec::new(), |a, b, kf, t| {
                let p = kf.easing.ease(0, t);
                let n = a.len().min(b.len());
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    out.push(a[i] + (b[i] - a[i]) * p);
                }
                out
            }),
        }
    }
}

fn eval_keyframes<T: Copy>(
    kfs: &[Keyframe<T>],
    frame: f64,
    default: T,
    lerp: impl Fn(T, T, &Keyframe<T>, f32) -> T,
) -> T {
    if kfs.is_empty() {
        return default;
    }
    if frame <= kfs[0].time {
        return kfs[0].start;
    }
    let last = &kfs[kfs.len() - 1];
    if frame >= last.time {
        return last.end.unwrap_or(last.start);
    }
    for i in 0..(kfs.len() - 1) {
        let cur = &kfs[i];
        let next = &kfs[i + 1];
        if frame < next.time {
            if cur.hold {
                return cur.start;
            }
            let span = next.time - cur.time;
            if span <= 1e-9 {
                return cur.start;
            }
            let t = ((frame - cur.time) / span).clamp(0.0, 1.0) as f32;
            let end_val = cur.end.unwrap_or(next.start);
            return lerp(cur.start, end_val, cur, t);
        }
    }
    last.end.unwrap_or(last.start)
}

fn eval_keyframes_ref<T: Clone>(
    kfs: &[Keyframe<T>],
    frame: f64,
    default: T,
    lerp: impl Fn(&T, &T, &Keyframe<T>, f32) -> T,
) -> T {
    if kfs.is_empty() {
        return default;
    }
    if frame <= kfs[0].time {
        return kfs[0].start.clone();
    }
    let last = &kfs[kfs.len() - 1];
    if frame >= last.time {
        return last.end.as_ref().unwrap_or(&last.start).clone();
    }
    for i in 0..(kfs.len() - 1) {
        let cur = &kfs[i];
        let next = &kfs[i + 1];
        if frame < next.time {
            if cur.hold {
                return cur.start.clone();
            }
            let span = next.time - cur.time;
            if span <= 1e-9 {
                return cur.start.clone();
            }
            let t = ((frame - cur.time) / span).clamp(0.0, 1.0) as f32;
            let end_val = cur.end.as_ref().unwrap_or(&next.start);
            return lerp(&cur.start, end_val, cur, t);
        }
    }
    last.end.as_ref().unwrap_or(&last.start).clone()
}

/// Fill rule (`r`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FillRule {
    /// Non-zero winding rule (`r == 1`).
    #[default]
    NonZero,
    /// Even-odd rule (`r == 2`).
    EvenOdd,
}

/// Stroke line cap (`lc`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LineCap {
    /// Butt cap (`lc == 1`).
    #[default]
    Butt,
    /// Round cap (`lc == 2`).
    Round,
    /// Projecting square cap (`lc == 3`).
    Square,
}

/// Stroke line join (`lj`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LineJoin {
    /// Miter join (`lj == 1`).
    #[default]
    Miter,
    /// Round join (`lj == 2`).
    Round,
    /// Bevel join (`lj == 3`).
    Bevel,
}

/// Path winding direction (`d`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    /// Clockwise (default, `d == 1`).
    #[default]
    Clockwise,
    /// Counter-clockwise (`d == 3`).
    CounterClockwise,
}

/// Gradient type (`t`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GradientType {
    /// Linear gradient (`t == 1`).
    #[default]
    Linear,
    /// Radial gradient (`t == 2`).
    Radial,
}

/// Polystar shape variant (`sy`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PolystarType {
    /// Star with inner and outer vertices (`sy == 1`).
    #[default]
    Star,
    /// Regular polygon (`sy == 2`).
    Polygon,
}

/// Trim paths combination mode (`m`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TrimMode {
    /// Trim each subpath simultaneously (`m == 1`).
    #[default]
    Simultaneous,
    /// Trim across all subpaths combined as one length (`m == 2`).
    Individual,
}

/// Repeater compositing order (`m`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RepeaterComposite {
    /// Each subsequent copy is painted below the previous (`m == 1`).
    #[default]
    Below,
    /// Each subsequent copy is painted above the previous (`m == 2`).
    Above,
}

/// Stroke dash entry (`d[]`).
#[derive(Debug, Clone)]
pub struct DashItem {
    /// Kind: `'d'` (dash), `'g'` (gap), `'o'` (offset).
    pub kind: char,
    /// Animated length or offset (`v`).
    pub value: AnimatedScalar,
}

/// Gradient definition shared by gradient fills and gradient strokes.
#[derive(Debug, Clone)]
pub struct GradientData {
    /// Linear or radial (`t`).
    pub gradient_type: GradientType,
    /// Start point (`s`).
    pub start: AnimatedVec2,
    /// End point (`e`).
    pub end: AnimatedVec2,
    /// Number of RGB color stops (`g.p`).
    pub stop_count: usize,
    /// Flat stop array (`g.k`).
    pub colors: AnimatedGradient,
    /// Radial gradient highlight length percentage (`h`).
    pub highlight_length: AnimatedScalar,
    /// Radial gradient highlight angle degrees (`a`).
    pub highlight_angle: AnimatedScalar,
}

/// Shape item inside a `ShapeLayer` or `ShapeGroup`.
#[derive(Debug, Clone)]
pub enum ShapeItem {
    /// Shape group (`ty == "gr"`).
    Group(ShapeGroup),
    /// Rectangle (`ty == "rc"`).
    Rect(RectShape),
    /// Ellipse (`ty == "el"`).
    Ellipse(EllipseShape),
    /// Freeform bezier path (`ty == "sh"`).
    Path(PathShape),
    /// Star or regular polygon (`ty == "sr"`).
    Polystar(PolystarShape),
    /// Solid fill (`ty == "fl"`).
    Fill(FillShape),
    /// Solid stroke (`ty == "st"`).
    Stroke(StrokeShape),
    /// Gradient fill (`ty == "gf"`).
    GradientFill(GradientFillShape),
    /// Gradient stroke (`ty == "gs"`).
    GradientStroke(GradientStrokeShape),
    /// Trim paths modifier (`ty == "tm"`).
    TrimPaths(TrimPathsShape),
    /// Repeater modifier (`ty == "rp"`).
    Repeater(RepeaterShape),
    /// Group transform (`ty == "tr"`).
    Transform(TransformProp),
}

/// Group of shapes (`gr`).
#[derive(Debug, Clone)]
pub struct ShapeGroup {
    /// Group name (`nm`).
    pub name: String,
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Child shape items (`it`).
    pub items: Vec<ShapeItem>,
}

/// Rectangle generator (`rc`).
#[derive(Debug, Clone)]
pub struct RectShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Winding direction (`d`).
    pub direction: Direction,
    /// Center position (`p`).
    pub position: AnimatedVec2,
    /// Width and height (`s`).
    pub size: AnimatedVec2,
    /// Corner radius (`r`).
    pub roundness: AnimatedScalar,
}

/// Ellipse generator (`el`).
#[derive(Debug, Clone)]
pub struct EllipseShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Winding direction (`d`).
    pub direction: Direction,
    /// Center position (`p`).
    pub position: AnimatedVec2,
    /// Width and height (`s`).
    pub size: AnimatedVec2,
}

/// Custom bezier path generator (`sh`).
#[derive(Debug, Clone)]
pub struct PathShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Winding direction (`d`).
    pub direction: Direction,
    /// Animated bezier vertices and tangents (`ks`).
    pub path: AnimatedPath,
}

/// Star or polygon generator (`sr`).
#[derive(Debug, Clone)]
pub struct PolystarShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Winding direction (`d`).
    pub direction: Direction,
    /// Star or polygon (`sy`).
    pub star_type: PolystarType,
    /// Number of points (`pt`).
    pub points: AnimatedScalar,
    /// Center position (`p`).
    pub position: AnimatedVec2,
    /// Rotation in degrees (`r`).
    pub rotation: AnimatedScalar,
    /// Inner radius (`ir`, stars only).
    pub inner_radius: AnimatedScalar,
    /// Outer radius (`or`).
    pub outer_radius: AnimatedScalar,
    /// Inner roundness percentage (`is`, stars only).
    pub inner_roundness: AnimatedScalar,
    /// Outer roundness percentage (`os`).
    pub outer_roundness: AnimatedScalar,
}

/// Solid fill (`fl`).
#[derive(Debug, Clone)]
pub struct FillShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Fill color (`c`).
    pub color: AnimatedColor,
    /// Opacity 0..100 (`o`).
    pub opacity: AnimatedScalar,
    /// Fill rule (`r`).
    pub fill_rule: FillRule,
}

/// Solid stroke (`st`).
#[derive(Debug, Clone)]
pub struct StrokeShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Stroke color (`c`).
    pub color: AnimatedColor,
    /// Opacity 0..100 (`o`).
    pub opacity: AnimatedScalar,
    /// Stroke width (`w`).
    pub width: AnimatedScalar,
    /// Line cap (`lc`).
    pub line_cap: LineCap,
    /// Line join (`lj`).
    pub line_join: LineJoin,
    /// Miter limit (`ml`).
    pub miter_limit: f32,
    /// Optional dash pattern (`d`).
    pub dashes: Vec<DashItem>,
}

/// Gradient fill (`gf`).
#[derive(Debug, Clone)]
pub struct GradientFillShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Gradient parameters.
    pub gradient: GradientData,
    /// Opacity 0..100 (`o`).
    pub opacity: AnimatedScalar,
    /// Fill rule (`r`).
    pub fill_rule: FillRule,
}

/// Gradient stroke (`gs`).
#[derive(Debug, Clone)]
pub struct GradientStrokeShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Gradient parameters.
    pub gradient: GradientData,
    /// Opacity 0..100 (`o`).
    pub opacity: AnimatedScalar,
    /// Stroke width (`w`).
    pub width: AnimatedScalar,
    /// Line cap (`lc`).
    pub line_cap: LineCap,
    /// Line join (`lj`).
    pub line_join: LineJoin,
    /// Miter limit (`ml`).
    pub miter_limit: f32,
    /// Optional dash pattern (`d`).
    pub dashes: Vec<DashItem>,
}

/// Trim paths modifier (`tm`).
#[derive(Debug, Clone)]
pub struct TrimPathsShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Start percentage 0..100 (`s`).
    pub start: AnimatedScalar,
    /// End percentage 0..100 (`e`).
    pub end: AnimatedScalar,
    /// Offset in degrees (`o`).
    pub offset: AnimatedScalar,
    /// Trim mode (`m`).
    pub mode: TrimMode,
}

/// Repeater modifier (`rp`).
#[derive(Debug, Clone)]
pub struct RepeaterShape {
    /// Hidden flag (`hd`).
    pub hidden: bool,
    /// Copy count (`c`).
    pub copies: AnimatedScalar,
    /// Copy offset (`o`).
    pub offset: AnimatedScalar,
    /// Composite order (`m`).
    pub composite: RepeaterComposite,
    /// Transform applied incrementally per copy (`tr`).
    pub transform: TransformProp,
    /// Start copy opacity 0..100 (`tr.so`).
    pub start_opacity: AnimatedScalar,
    /// End copy opacity 0..100 (`tr.eo`).
    pub end_opacity: AnimatedScalar,
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn get_f64(v: Option<&Value>) -> Option<f64> {
    let val = v?;
    if let Some(n) = val.as_f64() {
        return n.is_finite().then_some(n);
    }
    if let Some(arr) = val.as_array() {
        return arr
            .first()
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite());
    }
    None
}

fn get_f32(v: Option<&Value>) -> Option<f32> {
    get_f64(v).map(|n| n as f32).filter(|n| n.is_finite())
}

fn get_bool(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

fn parse_marker(v: &Value) -> Option<Marker> {
    let obj = v.as_object()?;
    let comment = obj
        .get("cm")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let time = get_f64(obj.get("tm")).unwrap_or(0.0);
    let duration = get_f64(obj.get("dr")).unwrap_or(0.0);
    Some(Marker {
        comment,
        time,
        duration,
    })
}

fn parse_precomp_asset(v: &Value) -> Option<PrecompAsset> {
    let obj = v.as_object()?;
    let id = obj.get("id").and_then(Value::as_str)?.to_string();
    let layers_arr = obj.get("layers").and_then(Value::as_array)?;
    let width = get_f64(obj.get("w"))
        .filter(|&n| n > 0.0)
        .map(|n| n.round() as u32);
    let height = get_f64(obj.get("h"))
        .filter(|&n| n > 0.0)
        .map(|n| n.round() as u32);
    let frame_rate = get_f64(obj.get("fr")).filter(|&n| n > 0.0);
    Some(PrecompAsset {
        id,
        width,
        height,
        frame_rate,
        layers: parse_layers(layers_arr),
    })
}

fn parse_layers(arr: &[Value]) -> Vec<Layer> {
    arr.iter()
        .take(MAX_LAYERS)
        .filter_map(parse_layer)
        .collect()
}

fn parse_layer(v: &Value) -> Option<Layer> {
    let obj = v.as_object()?;
    let ty = obj.get("ty").and_then(Value::as_i64).unwrap_or(-1);
    let index = obj.get("ind").and_then(Value::as_i64);
    let name = obj
        .get("nm")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parent = obj.get("parent").and_then(Value::as_i64);
    let hidden = get_bool(obj.get("hd"));
    let in_point = get_f64(obj.get("ip")).unwrap_or(0.0);
    let out_point = get_f64(obj.get("op")).unwrap_or(0.0);
    let start_time = get_f64(obj.get("st")).unwrap_or(0.0);
    let stretch_raw = get_f64(obj.get("sr")).unwrap_or(1.0);
    let stretch = if stretch_raw.abs() < 1e-9 {
        1.0
    } else {
        stretch_raw
    };
    let transform = obj.get("ks").map(parse_transform).unwrap_or_default();
    let auto_orient = obj.get("ao").and_then(Value::as_i64).unwrap_or(0) == 1;
    let blend_mode = parse_blend_mode(obj.get("bm").and_then(Value::as_i64).unwrap_or(0));
    let matte_type = match obj.get("tt").and_then(Value::as_i64) {
        Some(1) => Some(MatteType::Alpha),
        Some(2) => Some(MatteType::AlphaInverted),
        Some(3) => Some(MatteType::Luma),
        Some(4) => Some(MatteType::LumaInverted),
        _ => None,
    };
    let matte_parent = obj.get("tp").and_then(Value::as_i64);
    let is_matte_source = obj.get("td").and_then(Value::as_i64).unwrap_or(0) != 0;

    let masks = obj
        .get("masksProperties")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().take(256).filter_map(parse_mask).collect())
        .unwrap_or_default();

    let kind = match ty {
        0 => {
            let ref_id = obj
                .get("refId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let width = get_f32(obj.get("w")).unwrap_or(0.0);
            let height = get_f32(obj.get("h")).unwrap_or(0.0);
            let time_remap = obj.get("tm").map(|v| parse_animated_scalar(v, 0.0));
            LayerKind::Precomp {
                ref_id,
                width,
                height,
                time_remap,
            }
        }
        1 => {
            let width = get_f32(obj.get("sw")).unwrap_or(0.0);
            let height = get_f32(obj.get("sh")).unwrap_or(0.0);
            let color = obj
                .get("sc")
                .and_then(Value::as_str)
                .and_then(parse_hex_color)
                .unwrap_or(Color::BLACK);
            LayerKind::Solid {
                width,
                height,
                color,
            }
        }
        3 => LayerKind::Null,
        4 => {
            let shapes = obj
                .get("shapes")
                .and_then(Value::as_array)
                .map(|arr| parse_shapes(arr, 0))
                .unwrap_or_default();
            LayerKind::Shape { shapes }
        }
        _ => LayerKind::Unsupported,
    };

    Some(Layer {
        index,
        name,
        parent,
        hidden,
        in_point,
        out_point,
        start_time,
        stretch,
        transform,
        auto_orient,
        blend_mode,
        matte_type,
        matte_parent,
        is_matte_source,
        masks,
        kind,
    })
}

fn parse_blend_mode(bm: i64) -> BlendMode {
    match bm {
        1 => BlendMode::Multiply,
        2 => BlendMode::Screen,
        3 => BlendMode::Overlay,
        4 => BlendMode::Darken,
        5 => BlendMode::Lighten,
        6 => BlendMode::ColorDodge,
        7 => BlendMode::ColorBurn,
        8 => BlendMode::HardLight,
        9 => BlendMode::SoftLight,
        10 => BlendMode::Difference,
        11 => BlendMode::Exclusion,
        12 => BlendMode::Hue,
        13 => BlendMode::Saturation,
        14 => BlendMode::Color,
        15 => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

fn parse_mask(v: &Value) -> Option<Mask> {
    let obj = v.as_object()?;
    let mode = match obj.get("mode").and_then(Value::as_str).unwrap_or("a") {
        "n" => MaskMode::None,
        "a" => MaskMode::Add,
        "s" => MaskMode::Subtract,
        "i" => MaskMode::Intersect,
        "f" => MaskMode::Difference,
        _ => MaskMode::Add,
    };
    let inverted = get_bool(obj.get("inv"));
    let path = obj
        .get("pt")
        .map(parse_animated_path)
        .unwrap_or_else(|| AnimatedPath::Static(LottiePathData::default()));
    let opacity = obj
        .get("o")
        .map(|v| parse_animated_scalar(v, 100.0))
        .unwrap_or(AnimatedScalar::Static(100.0));
    let expansion = obj
        .get("x")
        .map(|v| parse_animated_scalar(v, 0.0))
        .unwrap_or(AnimatedScalar::Static(0.0));
    Some(Mask {
        mode,
        inverted,
        path,
        opacity,
        expansion,
    })
}

fn parse_hex_color(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if hex.len() == 6 {
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(Color::from_rgb_u32(v))
    } else if hex.len() == 3 {
        let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
        let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
        let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
        Some(Color::rgb(r, g, b))
    } else if hex.len() == 8 {
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(Color::rgba(
            ((v >> 24) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        ))
    } else {
        None
    }
}

fn parse_transform(v: &Value) -> TransformProp {
    let Some(obj) = v.as_object() else {
        return TransformProp::default();
    };
    let anchor = obj
        .get("a")
        .map(|val| parse_animated_vec2(val, [0.0, 0.0]))
        .unwrap_or(AnimatedVec2::Static([0.0, 0.0]));
    let position = obj
        .get("p")
        .map(parse_animated_position)
        .unwrap_or(AnimatedPosition::Combined(AnimatedVec2::Static([0.0, 0.0])));
    let scale = obj
        .get("s")
        .map(|val| parse_animated_vec2(val, [100.0, 100.0]))
        .unwrap_or(AnimatedVec2::Static([100.0, 100.0]));
    let rotation = obj
        .get("r")
        .or_else(|| obj.get("rz"))
        .map(|val| parse_animated_scalar(val, 0.0))
        .unwrap_or(AnimatedScalar::Static(0.0));
    let opacity = obj
        .get("o")
        .map(|val| parse_animated_scalar(val, 100.0))
        .unwrap_or(AnimatedScalar::Static(100.0));
    let skew = obj
        .get("sk")
        .map(|val| parse_animated_scalar(val, 0.0))
        .unwrap_or(AnimatedScalar::Static(0.0));
    let skew_axis = obj
        .get("sa")
        .map(|val| parse_animated_scalar(val, 0.0))
        .unwrap_or(AnimatedScalar::Static(0.0));

    TransformProp {
        anchor,
        position,
        scale,
        rotation,
        opacity,
        skew,
        skew_axis,
    }
}

fn parse_animated_position(v: &Value) -> AnimatedPosition {
    if let Some(obj) = v.as_object()
        && get_bool(obj.get("s"))
    {
        let x = obj
            .get("x")
            .map(|val| parse_animated_scalar(val, 0.0))
            .unwrap_or(AnimatedScalar::Static(0.0));
        let y = obj
            .get("y")
            .map(|val| parse_animated_scalar(val, 0.0))
            .unwrap_or(AnimatedScalar::Static(0.0));
        return AnimatedPosition::Separated { x, y };
    }
    AnimatedPosition::Combined(parse_animated_vec2(v, [0.0, 0.0]))
}

fn is_keyframed(obj: &serde_json::Map<String, Value>) -> bool {
    let a = obj.get("a").and_then(Value::as_i64);
    if a == Some(1) {
        return true;
    }
    // Some producers omit "a" or set it inaccurately; check if "k" is an array of keyframe objects.
    if let Some(arr) = obj.get("k").and_then(Value::as_array)
        && let Some(first) = arr.first()
        && first.is_object()
        && first.get("t").is_some()
    {
        return true;
    }
    false
}

fn parse_easing(obj: &serde_json::Map<String, Value>) -> EasingHandles {
    let mut out_x = Vec::new();
    let mut out_y = Vec::new();
    let mut in_x = Vec::new();
    let mut in_y = Vec::new();

    if let Some(o) = obj.get("o").and_then(Value::as_object) {
        collect_f32s(o.get("x"), &mut out_x);
        collect_f32s(o.get("y"), &mut out_y);
    }
    if let Some(i) = obj.get("i").and_then(Value::as_object) {
        collect_f32s(i.get("x"), &mut in_x);
        collect_f32s(i.get("y"), &mut in_y);
    }
    if out_x.is_empty() && out_y.is_empty() && in_x.is_empty() && in_y.is_empty() {
        return EasingHandles::linear();
    }
    EasingHandles {
        out_x,
        out_y,
        in_x,
        in_y,
    }
}

fn collect_f32s(v: Option<&Value>, out: &mut Vec<f32>) {
    match v {
        Some(Value::Number(n)) => {
            if let Some(f) = n.as_f64()
                && f.is_finite()
            {
                out.push(f as f32);
            }
        }
        Some(Value::Array(arr)) => {
            for item in arr.iter().take(16) {
                if let Some(f) = item.as_f64()
                    && f.is_finite()
                {
                    out.push(f as f32);
                }
            }
        }
        _ => {}
    }
}

fn parse_vec2_val(v: &Value, default: [f32; 2]) -> [f32; 2] {
    if let Some(arr) = v.as_array() {
        let x = arr
            .first()
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .map_or(default[0], |n| n as f32);
        let y = arr
            .get(1)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .map_or(default[1], |n| n as f32);
        [x, y]
    } else if let Some(n) = v.as_f64().filter(|n| n.is_finite()) {
        [n as f32, n as f32]
    } else {
        default
    }
}

fn parse_opt_vec2(v: Option<&Value>) -> Option<[f32; 2]> {
    let arr = v?.as_array()?;
    let x = arr
        .first()?
        .as_f64()?
        .is_finite()
        .then_some(arr[0].as_f64()? as f32)?;
    let y = arr
        .get(1)?
        .as_f64()?
        .is_finite()
        .then_some(arr[1].as_f64()? as f32)?;
    Some([x, y])
}

fn parse_animated_scalar(v: &Value, default: f32) -> AnimatedScalar {
    if let Some(f) = v.as_f64().filter(|n| n.is_finite()) {
        return AnimatedScalar::Static(f as f32);
    }
    let Some(obj) = v.as_object() else {
        return AnimatedScalar::Static(default);
    };
    let Some(k) = obj.get("k") else {
        return AnimatedScalar::Static(default);
    };
    if is_keyframed(obj)
        && let Some(arr) = k.as_array()
    {
        let mut kfs = Vec::new();
        for item in arr.iter().take(MAX_KEYFRAMES) {
            let Some(kobj) = item.as_object() else {
                continue;
            };
            let time = get_f64(kobj.get("t")).unwrap_or(0.0);
            let start = match kobj.get("s") {
                Some(sv) => get_f32(Some(sv)).unwrap_or(default),
                None => kfs
                    .last()
                    .and_then(|prev: &Keyframe<f32>| prev.end.or(Some(prev.start)))
                    .unwrap_or(default),
            };
            let end = kobj.get("e").and_then(|ev| get_f32(Some(ev)));
            let hold = get_bool(kobj.get("h"));
            let easing = parse_easing(kobj);
            kfs.push(Keyframe {
                time,
                start,
                end,
                hold,
                easing,
                spatial_out: None,
                spatial_in: None,
            });
        }
        if !kfs.is_empty() {
            return AnimatedScalar::Keyframed(kfs);
        }
    }
    AnimatedScalar::Static(get_f32(Some(k)).unwrap_or(default))
}

fn parse_animated_vec2(v: &Value, default: [f32; 2]) -> AnimatedVec2 {
    let Some(obj) = v.as_object() else {
        return AnimatedVec2::Static(parse_vec2_val(v, default));
    };
    let Some(k) = obj.get("k") else {
        return AnimatedVec2::Static(default);
    };
    if is_keyframed(obj)
        && let Some(arr) = k.as_array()
    {
        let mut kfs = Vec::new();
        for item in arr.iter().take(MAX_KEYFRAMES) {
            let Some(kobj) = item.as_object() else {
                continue;
            };
            let time = get_f64(kobj.get("t")).unwrap_or(0.0);
            let start = match kobj.get("s") {
                Some(sv) => parse_vec2_val(sv, default),
                None => kfs
                    .last()
                    .and_then(|prev: &Keyframe<[f32; 2]>| prev.end.or(Some(prev.start)))
                    .unwrap_or(default),
            };
            let end = kobj.get("e").map(|ev| parse_vec2_val(ev, start));
            let hold = get_bool(kobj.get("h"));
            let easing = parse_easing(kobj);
            let spatial_out = parse_opt_vec2(kobj.get("to"));
            let spatial_in = parse_opt_vec2(kobj.get("ti"));
            kfs.push(Keyframe {
                time,
                start,
                end,
                hold,
                easing,
                spatial_out,
                spatial_in,
            });
        }
        if !kfs.is_empty() {
            return AnimatedVec2::Keyframed(kfs);
        }
    }
    AnimatedVec2::Static(parse_vec2_val(k, default))
}

fn parse_color_val(v: &Value) -> [f32; 4] {
    let Some(arr) = v.as_array() else {
        return [0.0, 0.0, 0.0, 1.0];
    };
    let mut r = arr.first().and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let mut g = arr.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let mut b = arr.get(2).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let mut a = arr.get(3).and_then(Value::as_f64).unwrap_or(1.0) as f32;

    if !r.is_finite() {
        r = 0.0;
    }
    if !g.is_finite() {
        g = 0.0;
    }
    if !b.is_finite() {
        b = 0.0;
    }
    if !a.is_finite() {
        a = 1.0;
    }

    // Legacy Lottie files occasionally store colors in 0..255 instead of 0..1.
    if r > 1.0 || g > 1.0 || b > 1.0 {
        r /= 255.0;
        g /= 255.0;
        b /= 255.0;
        if a > 1.0 {
            a /= 255.0;
        }
    }
    [
        r.clamp(0.0, 1.0),
        g.clamp(0.0, 1.0),
        b.clamp(0.0, 1.0),
        a.clamp(0.0, 1.0),
    ]
}

fn parse_animated_color(v: &Value) -> AnimatedColor {
    let Some(obj) = v.as_object() else {
        return AnimatedColor::Static(parse_color_val(v));
    };
    let Some(k) = obj.get("k") else {
        return AnimatedColor::Static([0.0, 0.0, 0.0, 1.0]);
    };
    if is_keyframed(obj)
        && let Some(arr) = k.as_array()
    {
        let mut kfs = Vec::new();
        for item in arr.iter().take(MAX_KEYFRAMES) {
            let Some(kobj) = item.as_object() else {
                continue;
            };
            let time = get_f64(kobj.get("t")).unwrap_or(0.0);
            let start = match kobj.get("s") {
                Some(sv) => parse_color_val(sv),
                None => kfs
                    .last()
                    .and_then(|prev: &Keyframe<[f32; 4]>| prev.end.or(Some(prev.start)))
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]),
            };
            let end = kobj.get("e").map(parse_color_val);
            let hold = get_bool(kobj.get("h"));
            let easing = parse_easing(kobj);
            kfs.push(Keyframe {
                time,
                start,
                end,
                hold,
                easing,
                spatial_out: None,
                spatial_in: None,
            });
        }
        if !kfs.is_empty() {
            return AnimatedColor::Keyframed(kfs);
        }
    }
    AnimatedColor::Static(parse_color_val(k))
}

fn parse_point_list(v: Option<&Value>) -> Vec<[f32; 2]> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .take(MAX_VERTICES)
        .map(|pt| parse_vec2_val(pt, [0.0, 0.0]))
        .collect()
}

fn parse_path_data(v: &Value) -> LottiePathData {
    // Keyframes sometimes wrap the path object in a 1-element array `[ { v, i, o, c } ]`.
    let target = if let Some(arr) = v.as_array() {
        arr.first().unwrap_or(v)
    } else {
        v
    };
    let Some(obj) = target.as_object() else {
        return LottiePathData::default();
    };
    LottiePathData {
        vertices: parse_point_list(obj.get("v")),
        in_tangents: parse_point_list(obj.get("i")),
        out_tangents: parse_point_list(obj.get("o")),
        closed: get_bool(obj.get("c")),
    }
}

fn parse_animated_path(v: &Value) -> AnimatedPath {
    let Some(obj) = v.as_object() else {
        return AnimatedPath::Static(LottiePathData::default());
    };
    let Some(k) = obj.get("k") else {
        return AnimatedPath::Static(LottiePathData::default());
    };
    if is_keyframed(obj)
        && let Some(arr) = k.as_array()
    {
        let mut kfs: Vec<Keyframe<LottiePathData>> = Vec::new();
        for item in arr.iter().take(MAX_KEYFRAMES) {
            let Some(kobj) = item.as_object() else {
                continue;
            };
            let time = get_f64(kobj.get("t")).unwrap_or(0.0);
            let start = match kobj.get("s") {
                Some(sv) => parse_path_data(sv),
                None => kfs
                    .last()
                    .map(|prev| prev.end.as_ref().unwrap_or(&prev.start).clone())
                    .unwrap_or_default(),
            };
            let end = kobj.get("e").map(parse_path_data);
            let hold = get_bool(kobj.get("h"));
            let easing = parse_easing(kobj);
            kfs.push(Keyframe {
                time,
                start,
                end,
                hold,
                easing,
                spatial_out: None,
                spatial_in: None,
            });
        }
        if !kfs.is_empty() {
            return AnimatedPath::Keyframed(kfs);
        }
    }
    AnimatedPath::Static(parse_path_data(k))
}

fn parse_f32_vec(v: &Value) -> Vec<f32> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .take(4096)
        .filter_map(|x| x.as_f64().filter(|n| n.is_finite()).map(|n| n as f32))
        .collect()
}

fn parse_animated_gradient(v: &Value) -> AnimatedGradient {
    let Some(obj) = v.as_object() else {
        return AnimatedGradient::Static(Vec::new());
    };
    let Some(k) = obj.get("k") else {
        return AnimatedGradient::Static(Vec::new());
    };
    if is_keyframed(obj)
        && let Some(arr) = k.as_array()
    {
        let mut kfs: Vec<Keyframe<Vec<f32>>> = Vec::new();
        for item in arr.iter().take(MAX_KEYFRAMES) {
            let Some(kobj) = item.as_object() else {
                continue;
            };
            let time = get_f64(kobj.get("t")).unwrap_or(0.0);
            let start = match kobj.get("s") {
                Some(sv) => parse_f32_vec(sv),
                None => kfs
                    .last()
                    .map(|prev| prev.end.as_ref().unwrap_or(&prev.start).clone())
                    .unwrap_or_default(),
            };
            let end = kobj.get("e").map(parse_f32_vec);
            let hold = get_bool(kobj.get("h"));
            let easing = parse_easing(kobj);
            kfs.push(Keyframe {
                time,
                start,
                end,
                hold,
                easing,
                spatial_out: None,
                spatial_in: None,
            });
        }
        if !kfs.is_empty() {
            return AnimatedGradient::Keyframed(kfs);
        }
    }
    AnimatedGradient::Static(parse_f32_vec(k))
}

fn parse_direction(v: Option<&Value>) -> Direction {
    match v.and_then(Value::as_i64) {
        Some(3) => Direction::CounterClockwise,
        _ => Direction::Clockwise,
    }
}

fn parse_fill_rule(v: Option<&Value>) -> FillRule {
    match v.and_then(Value::as_i64) {
        Some(2) => FillRule::EvenOdd,
        _ => FillRule::NonZero,
    }
}

fn parse_line_cap(v: Option<&Value>) -> LineCap {
    match v.and_then(Value::as_i64) {
        Some(2) => LineCap::Round,
        Some(3) => LineCap::Square,
        _ => LineCap::Butt,
    }
}

fn parse_line_join(v: Option<&Value>) -> LineJoin {
    match v.and_then(Value::as_i64) {
        Some(2) => LineJoin::Round,
        Some(3) => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}

fn parse_dashes(v: Option<&Value>) -> Vec<DashItem> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr.iter().take(64) {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let kind = obj
            .get("n")
            .and_then(Value::as_str)
            .and_then(|s| s.chars().next())
            .unwrap_or('d');
        let value = obj
            .get("v")
            .map(|val| parse_animated_scalar(val, 0.0))
            .unwrap_or(AnimatedScalar::Static(0.0));
        out.push(DashItem { kind, value });
    }
    out
}

fn parse_gradient_data(obj: &serde_json::Map<String, Value>) -> GradientData {
    let gradient_type = match obj.get("t").and_then(Value::as_i64) {
        Some(2) => GradientType::Radial,
        _ => GradientType::Linear,
    };
    let start = obj
        .get("s")
        .map(|v| parse_animated_vec2(v, [0.0, 0.0]))
        .unwrap_or(AnimatedVec2::Static([0.0, 0.0]));
    let end = obj
        .get("e")
        .map(|v| parse_animated_vec2(v, [0.0, 0.0]))
        .unwrap_or(AnimatedVec2::Static([0.0, 0.0]));
    let (stop_count, colors) = if let Some(g) = obj.get("g").and_then(Value::as_object) {
        let p = g.get("p").and_then(Value::as_u64).unwrap_or(0).min(1024) as usize;
        let k = g
            .get("k")
            .map(parse_animated_gradient)
            .unwrap_or_else(|| AnimatedGradient::Static(Vec::new()));
        (p, k)
    } else {
        (0, AnimatedGradient::Static(Vec::new()))
    };
    let highlight_length = obj
        .get("h")
        .map(|v| parse_animated_scalar(v, 0.0))
        .unwrap_or(AnimatedScalar::Static(0.0));
    let highlight_angle = obj
        .get("a")
        .map(|v| parse_animated_scalar(v, 0.0))
        .unwrap_or(AnimatedScalar::Static(0.0));

    GradientData {
        gradient_type,
        start,
        end,
        stop_count,
        colors,
        highlight_length,
        highlight_angle,
    }
}

fn parse_shapes(arr: &[Value], depth: usize) -> Vec<ShapeItem> {
    if depth >= MAX_PARSE_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::new();
    for v in arr.iter().take(4096) {
        if let Some(item) = parse_shape(v, depth) {
            out.push(item);
        }
    }
    out
}

fn parse_shape(v: &Value, depth: usize) -> Option<ShapeItem> {
    let obj = v.as_object()?;
    let ty = obj.get("ty").and_then(Value::as_str)?;
    let hidden = get_bool(obj.get("hd"));

    match ty {
        "gr" => {
            let name = obj
                .get("nm")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let items = obj
                .get("it")
                .and_then(Value::as_array)
                .map(|arr| parse_shapes(arr, depth + 1))
                .unwrap_or_default();
            Some(ShapeItem::Group(ShapeGroup {
                name,
                hidden,
                items,
            }))
        }
        "rc" => Some(ShapeItem::Rect(RectShape {
            hidden,
            direction: parse_direction(obj.get("d")),
            position: obj
                .get("p")
                .map(|val| parse_animated_vec2(val, [0.0, 0.0]))
                .unwrap_or(AnimatedVec2::Static([0.0, 0.0])),
            size: obj
                .get("s")
                .map(|val| parse_animated_vec2(val, [0.0, 0.0]))
                .unwrap_or(AnimatedVec2::Static([0.0, 0.0])),
            roundness: obj
                .get("r")
                .map(|val| parse_animated_scalar(val, 0.0))
                .unwrap_or(AnimatedScalar::Static(0.0)),
        })),
        "el" => Some(ShapeItem::Ellipse(EllipseShape {
            hidden,
            direction: parse_direction(obj.get("d")),
            position: obj
                .get("p")
                .map(|val| parse_animated_vec2(val, [0.0, 0.0]))
                .unwrap_or(AnimatedVec2::Static([0.0, 0.0])),
            size: obj
                .get("s")
                .map(|val| parse_animated_vec2(val, [0.0, 0.0]))
                .unwrap_or(AnimatedVec2::Static([0.0, 0.0])),
        })),
        "sh" => Some(ShapeItem::Path(PathShape {
            hidden,
            direction: parse_direction(obj.get("d")),
            path: obj
                .get("ks")
                .map(parse_animated_path)
                .unwrap_or_else(|| AnimatedPath::Static(LottiePathData::default())),
        })),
        "sr" => {
            let star_type = match obj.get("sy").and_then(Value::as_i64) {
                Some(2) => PolystarType::Polygon,
                _ => PolystarType::Star,
            };
            Some(ShapeItem::Polystar(PolystarShape {
                hidden,
                direction: parse_direction(obj.get("d")),
                star_type,
                points: obj
                    .get("pt")
                    .map(|val| parse_animated_scalar(val, 5.0))
                    .unwrap_or(AnimatedScalar::Static(5.0)),
                position: obj
                    .get("p")
                    .map(|val| parse_animated_vec2(val, [0.0, 0.0]))
                    .unwrap_or(AnimatedVec2::Static([0.0, 0.0])),
                rotation: obj
                    .get("r")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
                inner_radius: obj
                    .get("ir")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
                outer_radius: obj
                    .get("or")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
                inner_roundness: obj
                    .get("is")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
                outer_roundness: obj
                    .get("os")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
            }))
        }
        "fl" => Some(ShapeItem::Fill(FillShape {
            hidden,
            color: obj
                .get("c")
                .map(parse_animated_color)
                .unwrap_or(AnimatedColor::Static([0.0, 0.0, 0.0, 1.0])),
            opacity: obj
                .get("o")
                .map(|val| parse_animated_scalar(val, 100.0))
                .unwrap_or(AnimatedScalar::Static(100.0)),
            fill_rule: parse_fill_rule(obj.get("r")),
        })),
        "st" => Some(ShapeItem::Stroke(StrokeShape {
            hidden,
            color: obj
                .get("c")
                .map(parse_animated_color)
                .unwrap_or(AnimatedColor::Static([0.0, 0.0, 0.0, 1.0])),
            opacity: obj
                .get("o")
                .map(|val| parse_animated_scalar(val, 100.0))
                .unwrap_or(AnimatedScalar::Static(100.0)),
            width: obj
                .get("w")
                .map(|val| parse_animated_scalar(val, 1.0))
                .unwrap_or(AnimatedScalar::Static(1.0)),
            line_cap: parse_line_cap(obj.get("lc")),
            line_join: parse_line_join(obj.get("lj")),
            miter_limit: get_f32(obj.get("ml")).unwrap_or(4.0).max(1.0),
            dashes: parse_dashes(obj.get("d")),
        })),
        "gf" => Some(ShapeItem::GradientFill(GradientFillShape {
            hidden,
            gradient: parse_gradient_data(obj),
            opacity: obj
                .get("o")
                .map(|val| parse_animated_scalar(val, 100.0))
                .unwrap_or(AnimatedScalar::Static(100.0)),
            fill_rule: parse_fill_rule(obj.get("r")),
        })),
        "gs" => Some(ShapeItem::GradientStroke(GradientStrokeShape {
            hidden,
            gradient: parse_gradient_data(obj),
            opacity: obj
                .get("o")
                .map(|val| parse_animated_scalar(val, 100.0))
                .unwrap_or(AnimatedScalar::Static(100.0)),
            width: obj
                .get("w")
                .map(|val| parse_animated_scalar(val, 1.0))
                .unwrap_or(AnimatedScalar::Static(1.0)),
            line_cap: parse_line_cap(obj.get("lc")),
            line_join: parse_line_join(obj.get("lj")),
            miter_limit: get_f32(obj.get("ml")).unwrap_or(4.0).max(1.0),
            dashes: parse_dashes(obj.get("d")),
        })),
        "tm" => {
            let mode = match obj.get("m").and_then(Value::as_i64) {
                Some(2) => TrimMode::Individual,
                _ => TrimMode::Simultaneous,
            };
            Some(ShapeItem::TrimPaths(TrimPathsShape {
                hidden,
                start: obj
                    .get("s")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
                end: obj
                    .get("e")
                    .map(|val| parse_animated_scalar(val, 100.0))
                    .unwrap_or(AnimatedScalar::Static(100.0)),
                offset: obj
                    .get("o")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
                mode,
            }))
        }
        "rp" => {
            let composite = match obj.get("m").and_then(Value::as_i64) {
                Some(2) => RepeaterComposite::Above,
                _ => RepeaterComposite::Below,
            };
            let tr_val = obj.get("tr");
            let transform = tr_val.map(parse_transform).unwrap_or_default();
            let tr_obj = tr_val.and_then(Value::as_object);
            let start_opacity = tr_obj
                .and_then(|o| o.get("so"))
                .map(|val| parse_animated_scalar(val, 100.0))
                .unwrap_or(AnimatedScalar::Static(100.0));
            let end_opacity = tr_obj
                .and_then(|o| o.get("eo"))
                .map(|val| parse_animated_scalar(val, 100.0))
                .unwrap_or(AnimatedScalar::Static(100.0));
            Some(ShapeItem::Repeater(RepeaterShape {
                hidden,
                copies: obj
                    .get("c")
                    .map(|val| parse_animated_scalar(val, 1.0))
                    .unwrap_or(AnimatedScalar::Static(1.0)),
                offset: obj
                    .get("o")
                    .map(|val| parse_animated_scalar(val, 0.0))
                    .unwrap_or(AnimatedScalar::Static(0.0)),
                composite,
                transform,
                start_opacity,
                end_opacity,
            }))
        }
        "tr" => Some(ShapeItem::Transform(parse_transform(v))),
        _ => None,
    }
}
