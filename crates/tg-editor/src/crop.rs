//! Headless crop interaction math (handles, aspect ratio locking, dragging, canvas expansion).
//!
//! Port of `editor/editor_crop.h` and `editor/editor_crop.cpp`.

use crate::common::{EditorData, PhotoModifications, RoundedCornersLevel};
use crate::geometry::{
    Affine2D, Margins, PointF, Rect, RectF, Size, SizeF, flip_size_f_by_rotation,
    normalize_angle_90,
};

/// Crop handle visual/hit size in logical pixels (`photoEditorCropPointSize`).
pub const CROP_POINT_SIZE: i32 = 10;

/// Minimum crop rectangle side in widget pixels (`photoEditorCropMinSize`).
pub const CROP_MIN_SIZE: i32 = 20;

/// Maximum expanded canvas dimension ratio (`kMaxCanvasRatio`).
pub const MAX_CANVAS_RATIO: f64 = 2.0;

/// Bitmask of active crop edges (`Qt::Edges` equivalents in `editor_crop.cpp`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CropEdge(u8);

impl CropEdge {
    /// No edge hit.
    pub const NONE: Self = Self(0);
    /// Top edge.
    pub const TOP: Self = Self(1 << 0);
    /// Left edge.
    pub const LEFT: Self = Self(1 << 1);
    /// Right edge.
    pub const RIGHT: Self = Self(1 << 2);
    /// Bottom edge.
    pub const BOTTOM: Self = Self(1 << 3);
    /// Top-left corner handle.
    pub const TOP_LEFT: Self = Self(Self::TOP.0 | Self::LEFT.0);
    /// Top-right corner handle.
    pub const TOP_RIGHT: Self = Self(Self::TOP.0 | Self::RIGHT.0);
    /// Bottom-left corner handle.
    pub const BOTTOM_LEFT: Self = Self(Self::BOTTOM.0 | Self::LEFT.0);
    /// Bottom-right corner handle.
    pub const BOTTOM_RIGHT: Self = Self(Self::BOTTOM.0 | Self::RIGHT.0);
    /// Interior drag (move entire crop rect).
    pub const ALL: Self = Self(Self::TOP.0 | Self::LEFT.0 | Self::RIGHT.0 | Self::BOTTOM.0);

    /// Returns true if no edge is set.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Returns true if all edges in `other` are set on `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Cursor shape corresponding to a hovered/dragged crop handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CropCursor {
    /// Default pointer.
    Default,
    /// Move interior (`cur_sizeall`).
    SizeAll,
    /// Horizontal resize (`cur_sizehor`).
    SizeHor,
    /// Vertical resize (`cur_sizever`).
    SizeVer,
    /// Top-left / bottom-right diagonal resize (`cur_sizefdiag`).
    SizeFDiag,
    /// Top-right / bottom-left diagonal resize (`cur_sizebdiag`).
    SizeBDiag,
}

/// Standard aspect ratio presets from `photo_editor_controls.cpp`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropPreset {
    /// Freeform aspect ratio (`0.0`).
    Free,
    /// Original image aspect ratio (`width / height`).
    Original,
    /// Square `1:1` (also used for circle/userpic crops).
    Square,
    /// Landscape `3:2`.
    Ratio3x2,
    /// Landscape `16:9`.
    Ratio16x9,
    /// Portrait `3:4`.
    Ratio3x4,
    /// Portrait `9:16`.
    Ratio9x16,
}

impl CropPreset {
    /// Returns the numeric aspect ratio (`width / height`), or `0.0` for [`CropPreset::Free`].
    #[must_use]
    pub fn ratio(self, image_size: Size) -> f64 {
        match self {
            Self::Free => 0.0,
            Self::Original => {
                if image_size.height > 0 {
                    image_size.width as f64 / image_size.height as f64
                } else {
                    1.0
                }
            }
            Self::Square => 1.0,
            Self::Ratio3x2 => 3.0 / 2.0,
            Self::Ratio16x9 => 16.0 / 9.0,
            Self::Ratio3x4 => 3.0 / 4.0,
            Self::Ratio9x16 => 9.0 / 16.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DragBorders {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

#[derive(Debug, Clone, Copy, Default)]
struct DownState {
    rect: RectF,
    edge: CropEdge,
    point: (i32, i32),
    crop_ratio: f64,
    bounds: RectF,
    borders: DragBorders,
}

fn rect_edges_int(r: RectF) -> (i32, i32, i32, i32) {
    (
        r.left() as i32,
        r.top() as i32,
        (r.left() + r.width) as i32,
        (r.top() + r.height) as i32,
    )
}

fn point_of_edge(e: CropEdge, r: RectF) -> (i32, i32) {
    if e == CropEdge::TOP_LEFT {
        (r.x as i32, r.y as i32)
    } else if e == CropEdge::TOP_RIGHT {
        ((r.x + r.width) as i32, r.y as i32)
    } else if e == CropEdge::BOTTOM_LEFT {
        (r.x as i32, (r.y + r.height) as i32)
    } else if e == CropEdge::BOTTOM_RIGHT {
        ((r.x + r.width) as i32, (r.y + r.height) as i32)
    } else if e == CropEdge::LEFT {
        (r.x as i32, (r.y + r.height / 2.0) as i32)
    } else if e == CropEdge::RIGHT {
        ((r.x + r.width) as i32, (r.y + r.height / 2.0) as i32)
    } else if e == CropEdge::TOP {
        ((r.x + r.width / 2.0) as i32, r.y as i32)
    } else if e == CropEdge::BOTTOM {
        ((r.x + r.width / 2.0) as i32, (r.y + r.height) as i32)
    } else {
        (0, 0)
    }
}

/// Computes the initial centered crop rect fitting `inner` aspect inside `outer`
/// (`OriginalCrop` in `editor_crop.cpp`).
#[must_use]
pub fn original_crop(outer: Size, inner: Size) -> RectF {
    let size = inner.scaled_keep_aspect(outer);
    RectF::new(
        ((outer.width - size.width) / 2) as f64,
        ((outer.height - size.height) / 2) as f64,
        size.width as f64,
        size.height as f64,
    )
}

/// Clamps `crop` to fit inside `inner`, shifting its position if needed (`AdjustCropToInner`).
pub fn adjust_crop_to_inner(crop: &mut RectF, inner: RectF) -> bool {
    if inner.is_empty() {
        return false;
    }
    let was = *crop;
    crop.width = crop.width.min(inner.width);
    crop.height = crop.height.min(inner.height);
    if crop.left() < inner.left() {
        crop.x = inner.left();
    }
    if crop.top() < inner.top() {
        crop.y = inner.top();
    }
    if crop.right() > inner.right() {
        crop.x = inner.right() - crop.width;
    }
    if crop.bottom() > inner.bottom() {
        crop.y = inner.bottom() - crop.height;
    }
    *crop != was
}

/// Pure, UI-independent crop interaction state machine (`Editor::Crop`).
#[derive(Debug, Clone)]
pub struct CropController {
    point_size: i32,
    point_size_half: f64,
    min_size: i32,
    inner_margins: Margins,
    image_size: Size,
    data: EditorData,
    geometry: Rect,
    edges: Vec<(CropEdge, RectF)>,
    ratio_w: f64,
    ratio_h: f64,
    crop_paint: RectF,
    crop_original: RectF,
    inner_rect: RectF,
    down: Option<DownState>,
    angle: i32,
    flipped: bool,
    keep_aspect_ratio: bool,
    expansion_allowed: bool,
    corners_level: RoundedCornersLevel,
}

impl CropController {
    /// Creates a new crop controller with tdesktop's default handle sizes
    /// (`point_size = 10`, `min_size = 20`).
    #[must_use]
    pub fn new(modifications: &PhotoModifications, image_size: Size, data: EditorData) -> Self {
        Self::with_metrics(
            modifications,
            image_size,
            data,
            CROP_POINT_SIZE,
            CROP_MIN_SIZE,
        )
    }

    /// Creates a new crop controller with custom handle/minimum sizes.
    #[must_use]
    pub fn with_metrics(
        modifications: &PhotoModifications,
        image_size: Size,
        data: EditorData,
        point_size: i32,
        min_size: i32,
    ) -> Self {
        let point_size_half = point_size as f64 / 2.0;
        let half_margin = point_size_half.round() as i32;
        let inner_margins = Margins::uniform(half_margin);

        let crop_original = if let Some(crop) = modifications.crop.filter(|c| c.is_valid()) {
            crop.to_f64()
        } else if let Some(exact) = data.exact_size.filter(|s| !s.is_empty()) {
            original_crop(image_size, exact)
        } else {
            Rect::from_size(image_size).to_f64()
        };

        let keep_aspect_ratio = data.keep_aspect_ratio;
        let expansion_allowed = !data.fixed_crop;

        Self {
            point_size,
            point_size_half,
            min_size,
            inner_margins,
            image_size,
            data,
            geometry: Rect::default(),
            edges: Vec::with_capacity(8),
            ratio_w: 1.0,
            ratio_h: 1.0,
            crop_paint: crop_original,
            crop_original,
            inner_rect: Rect::from_size(image_size).to_f64(),
            down: None,
            angle: normalize_angle_90(modifications.angle),
            flipped: modifications.flipped,
            keep_aspect_ratio,
            expansion_allowed,
            corners_level: modifications.corners_level,
        }
    }

    /// Returns the margins reserved around the crop viewport for handles (`Crop::cropMargins`).
    #[must_use]
    pub const fn crop_margins(&self) -> Margins {
        self.inner_margins
    }

    /// Returns the editor configuration.
    #[must_use]
    pub const fn editor_data(&self) -> &EditorData {
        &self.data
    }

    /// Returns whether aspect ratio locking is currently enabled.
    #[must_use]
    pub const fn keep_aspect_ratio(&self) -> bool {
        self.keep_aspect_ratio
    }

    /// Returns the current rounded corners level.
    #[must_use]
    pub const fn corners_level(&self) -> RoundedCornersLevel {
        self.corners_level
    }

    /// Sets the rounded corners level.
    pub fn set_corners_level(&mut self, level: RoundedCornersLevel) {
        self.corners_level = level;
    }

    /// Enables or disables canvas expansion beyond original image edges.
    pub fn set_expansion_allowed(&mut self, allowed: bool) {
        self.expansion_allowed = allowed;
    }

    /// Returns the current crop rectangle in widget/paint coordinates.
    #[must_use]
    pub const fn crop_paint_f(&self) -> RectF {
        self.crop_paint
    }

    /// Returns the integer paint rectangle (`Crop::paintRect`).
    #[must_use]
    pub fn paint_rect(&self) -> Rect {
        self.crop_paint.to_rect()
    }

    /// Returns the current crop rectangle in original image coordinates (`Crop::cropRect`).
    #[must_use]
    pub fn crop_rect(&self) -> Rect {
        self.crop_original.to_rect()
    }

    /// Returns the crop rectangle to save in `PhotoModifications`, or `None` when the crop
    /// covers the exact unmodified image (`Crop::saveCropRect`).
    #[must_use]
    pub fn save_crop_rect(&self) -> Option<Rect> {
        let saved = self.crop_original.to_rect();
        if saved.x != 0 || saved.y != 0 || saved.size() != self.image_size {
            Some(saved)
        } else {
            None
        }
    }

    /// Returns the current handle hit-test rectangles.
    #[must_use]
    pub fn edge_rects(&self) -> &[(CropEdge, RectF)] {
        &self.edges
    }

    /// Returns whether a drag operation is currently active.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.down.is_some()
    }

    /// Updates viewport transform and maps `_cropOriginal` into widget coordinates
    /// (`Crop::applyTransform`).
    pub fn apply_transform(
        &mut self,
        geometry: Rect,
        image_position: PointF,
        angle: i32,
        flipped: bool,
        scaled_image_size: SizeF,
    ) {
        if geometry.is_empty() || self.image_size.is_empty() {
            return;
        }
        let angle = normalize_angle_90(angle);
        self.geometry = geometry;
        self.inner_rect = RectF::new(
            image_position.x,
            image_position.y,
            flip_size_f_by_rotation(scaled_image_size, angle).width,
            flip_size_f_by_rotation(scaled_image_size, angle).height,
        );
        self.ratio_w = scaled_image_size.width / self.image_size.width as f64;
        self.ratio_h = scaled_image_size.height / self.image_size.height as f64;
        self.flipped = flipped;
        self.angle = angle;

        let crop_holder = RectF::new(0.0, 0.0, scaled_image_size.width, scaled_image_size.height);
        let center = crop_holder.center();

        // Exact port of QTransform().translate(c).scale(flip, 1).rotate(angle).translate(-c)
        let matrix = Affine2D::identity()
            .qt_translate(center.x, center.y)
            .qt_scale(if flipped { -1.0 } else { 1.0 }, 1.0)
            .qt_rotate(angle as f64)
            .qt_translate(-center.x, -center.y);

        let holder_rotated = matrix.map_rect(crop_holder);

        let mut crop_paint = matrix
            .qt_scale(self.ratio_w, self.ratio_h)
            .map_rect(self.crop_original)
            .translated(
                image_position.x - holder_rotated.x,
                image_position.y - holder_rotated.y,
            );

        let min = self.min_size as f64;
        if crop_paint.width < min || crop_paint.height < min {
            let bounds = self.inner_rect.united(crop_paint);
            crop_paint.width = min.min(bounds.width).max(crop_paint.width);
            crop_paint.height = min.min(bounds.height).max(crop_paint.height);
            adjust_crop_to_inner(&mut crop_paint, bounds);
        }
        self.set_crop_paint(crop_paint);
    }

    /// Hit-tests `point` against the corner handles, edge handles (when unlocked),
    /// and crop interior (`Crop::mouseState`).
    #[must_use]
    pub fn hit_test(&self, point: PointF) -> CropEdge {
        for &(edge, rect) in &self.edges {
            if rect.contains_point(point) {
                return edge;
            }
        }
        if self.crop_paint.contains_point(point) {
            return CropEdge::ALL;
        }
        CropEdge::NONE
    }

    /// Maps a [`CropEdge`] to its corresponding resize/move cursor.
    #[must_use]
    pub const fn cursor_for_edge(edge: CropEdge) -> CropCursor {
        match edge {
            CropEdge::TOP_LEFT | CropEdge::BOTTOM_RIGHT => CropCursor::SizeFDiag,
            CropEdge::TOP_RIGHT | CropEdge::BOTTOM_LEFT => CropCursor::SizeBDiag,
            CropEdge::LEFT | CropEdge::RIGHT => CropCursor::SizeHor,
            CropEdge::TOP | CropEdge::BOTTOM => CropCursor::SizeVer,
            CropEdge::ALL => CropCursor::SizeAll,
            _ => CropCursor::Default,
        }
    }

    /// Begins a crop or move drag at `point` (`Crop::mousePressEvent` + `computeDownState`).
    ///
    /// Returns the hit [`CropEdge`] (`CropEdge::NONE` if outside the crop handles and interior).
    pub fn begin_drag(&mut self, point: PointF, expanding_ctrl_held: bool) -> CropEdge {
        let edge = self.hit_test(point);
        if edge.is_none() {
            self.down = None;
            return CropEdge::NONE;
        }
        let expanding = self.expansion_allowed && expanding_ctrl_held;
        let crop = self.crop_paint;
        let bounds = if expanding && edge != CropEdge::ALL {
            self.expansion_bounds()
        } else {
            self.inner_rect.united(crop)
        };
        let (i_left, i_top, i_right, i_bottom) = rect_edges_int(bounds);
        let (c_left, c_top, c_right, c_bottom) = rect_edges_int(crop);
        let edge_pt = point_of_edge(edge, crop);

        // Ratio in paint coordinates (matches rotated/unrotated orientation of the active crop).
        let crop_ratio = if crop.height > 0.0 {
            crop.width / crop.height
        } else {
            1.0
        };

        let mut borders = DragBorders {
            left: i_left - c_left,
            right: i_right - c_right,
            top: i_top - c_top,
            bottom: i_bottom - c_bottom,
        };

        if self.keep_aspect_ratio && edge != CropEdge::ALL {
            let has_left = edge.contains(CropEdge::LEFT);
            let has_top = edge.contains(CropEdge::TOP);
            let x_sign = if has_left { -1 } else { 1 };
            let y_sign = if has_top { -1 } else { 1 };
            let x_side = if has_left {
                borders.left
            } else {
                borders.right
            };
            let y_side = if has_top { borders.top } else { borders.bottom };
            let x_room = ((x_sign * x_side) as f64).min((y_sign * y_side) as f64 * crop_ratio);
            let new_x = (x_sign as f64 * x_room) as i32;
            let new_y = (y_sign as f64 * x_room / crop_ratio) as i32;
            if has_left {
                borders.left = new_x;
            } else {
                borders.right = new_x;
            }
            if has_top {
                borders.top = new_y;
            } else {
                borders.bottom = new_y;
            }
        }

        self.down = Some(DownState {
            rect: crop,
            edge,
            point: (
                point.x.round() as i32 - edge_pt.0,
                point.y.round() as i32 - edge_pt.1,
            ),
            crop_ratio,
            bounds,
            borders,
        });
        edge
    }

    /// Updates the active drag to `point` (`Crop::mouseMoveEvent`).
    pub fn update_drag(&mut self, point: PointF) {
        let Some(down) = self.down else {
            return;
        };
        let pos = (point.x.round() as i32, point.y.round() as i32);
        if down.edge == CropEdge::ALL {
            self.perform_move(down, pos);
        } else {
            self.perform_crop(down, pos);
        }
    }

    /// Finishes the active drag and converts paint crop back to original image coordinates
    /// (`Crop::finishDrag`).
    ///
    /// Returns `true` if `save_crop_rect()` changed.
    pub fn end_drag(&mut self) -> bool {
        if self.down.is_none() {
            return false;
        }
        self.down = None;
        let was = self.save_crop_rect();
        self.convert_crop_paint_to_original();
        self.save_crop_rect() != was
    }

    /// Locks the crop to `ratio` (`width / height`), or unlocks freeform cropping when `ratio <= 0.0`
    /// (`Crop::setAspectRatio`).
    ///
    /// Returns `true` if `save_crop_rect()` changed.
    pub fn set_aspect_ratio(&mut self, ratio: f64) -> bool {
        let free = ratio <= 0.0;
        self.keep_aspect_ratio = !free;

        if !free {
            let bounds = self.inner_rect.united(self.crop_paint);
            let max_w = bounds.width;
            let max_h = bounds.height;
            let mut new_w = max_w;
            let mut new_h = max_w / ratio;
            if new_h > max_h {
                new_h = max_h;
                new_w = max_h * ratio;
            }
            let center = self.crop_paint.center();
            let mut adjusted =
                RectF::new(center.x - new_w / 2.0, center.y - new_h / 2.0, new_w, new_h);
            adjust_crop_to_inner(&mut adjusted, bounds);
            self.set_crop_paint(adjusted);
            let was = self.save_crop_rect();
            self.convert_crop_paint_to_original();
            self.save_crop_rect() != was
        } else {
            self.update_edges();
            false
        }
    }

    /// Computes expansion bounds when Ctrl is held (`Crop::expansionBounds`).
    #[must_use]
    pub fn expansion_bounds(&self) -> RectF {
        let united = self.inner_rect.united(self.crop_paint);
        let max_w = self.inner_rect.width * MAX_CANVAS_RATIO;
        let max_h = self.inner_rect.height * MAX_CANVAS_RATIO;
        let limit = RectF::from_edges(
            (united.right() - max_w).min(united.left()),
            (united.bottom() - max_h).min(united.top()),
            (united.left() + max_w).max(united.right()),
            (united.top() + max_h).max(united.bottom()),
        );
        let viewport = self.geometry.shrink(self.inner_margins).to_f64();
        viewport.intersected(limit)
    }

    fn set_crop_paint(&mut self, rect: RectF) {
        self.crop_paint = rect;
        self.update_edges();
    }

    fn update_edges(&mut self) {
        self.edges.clear();
        let s = self.point_size as f64;
        let h = self.point_size_half;
        let r = self.crop_paint;
        let side_w = (r.width - s).max(0.0);
        let side_h = (r.height - s).max(0.0);

        for corner in [
            CropEdge::TOP_LEFT,
            CropEdge::TOP_RIGHT,
            CropEdge::BOTTOM_LEFT,
            CropEdge::BOTTOM_RIGHT,
        ] {
            let (px, py) = point_of_edge(corner, r);
            self.edges
                .push((corner, RectF::new(px as f64 - h, py as f64 - h, s, s)));
        }

        if !self.keep_aspect_ratio {
            self.edges.push((
                CropEdge::LEFT,
                RectF::new(r.left() - h, r.top() + h, s, side_h),
            ));
            self.edges.push((
                CropEdge::RIGHT,
                RectF::new(r.right() - h, r.top() + h, s, side_h),
            ));
            self.edges.push((
                CropEdge::TOP,
                RectF::new(r.left() + h, r.top() - h, side_w, s),
            ));
            self.edges.push((
                CropEdge::BOTTOM,
                RectF::new(r.left() + h, r.bottom() - h, side_w, s),
            ));
        }
    }

    fn perform_crop(&mut self, down: DownState, pos: (i32, i32)) {
        let crop = down.rect;
        let edge = down.edge;
        let has_left = edge.contains(CropEdge::LEFT);
        let has_top = edge.contains(CropEdge::TOP);
        let has_right = edge.contains(CropEdge::RIGHT);
        let has_bottom = edge.contains(CropEdge::BOTTOM);

        let edge_pt = point_of_edge(edge, crop);
        let mut dx = pos.0 - edge_pt.0 - down.point.0;
        let mut dy = pos.1 - edge_pt.1 - down.point.1;

        let x_factor = if has_left { 1 } else { -1 };
        let y_factor = if has_top { 1 } else { -1 };
        let crop_ratio = down.crop_ratio;

        if self.keep_aspect_ratio {
            let diff_sign = x_factor * y_factor;
            if (crop_ratio - 1.0).abs() > f64::EPSILON {
                dy = ((1.0 / crop_ratio) * dx as f64 * diff_sign as f64) as i32;
            } else if (dx * x_factor) < (dy * y_factor) {
                dy = dx * diff_sign;
            } else {
                dx = dy * diff_sign;
            }
        }

        let min_size = self.min_size as f64;
        let min_w = if self.keep_aspect_ratio && crop_ratio > 1.0 {
            min_size * crop_ratio
        } else {
            min_size
        };
        let min_h = if self.keep_aspect_ratio && crop_ratio < 1.0 {
            min_size / crop_ratio
        } else {
            min_size
        };

        let x_inward = (crop.width - min_w).min(if has_left {
            self.inner_rect.right() - min_w - crop.left()
        } else {
            crop.right() - min_w - self.inner_rect.left()
        });
        let y_inward = (crop.height - min_h).min(if has_top {
            self.inner_rect.bottom() - min_h - crop.top()
        } else {
            crop.bottom() - min_h - self.inner_rect.top()
        });
        let x_min = x_factor * (x_inward.max(0.0) as i32);
        let y_min = y_factor * (y_inward.max(0.0) as i32);

        let (x_lo, x_hi) = if has_left {
            (down.borders.left, x_min)
        } else {
            (x_min, down.borders.right)
        };
        let (y_lo, y_hi) = if has_top {
            (down.borders.top, y_min)
        } else {
            (y_min, down.borders.bottom)
        };

        let clamped_x = clamp_safe(dx, x_lo, x_hi);
        let clamped_y = clamp_safe(dy, y_lo, y_hi);

        let left = crop.left() + if has_left { clamped_x as f64 } else { 0.0 };
        let top = crop.top() + if has_top { clamped_y as f64 } else { 0.0 };
        let right = crop.right() + if has_right { clamped_x as f64 } else { 0.0 };
        let bottom = crop.bottom() + if has_bottom { clamped_y as f64 } else { 0.0 };

        let b = down.bounds;
        let result = RectF::from_edges(
            left.max(b.left()),
            top.max(b.top()),
            right.min(b.right()),
            bottom.min(b.bottom()),
        );
        self.set_crop_paint(result);
    }

    fn perform_move(&mut self, down: DownState, pos: (i32, i32)) {
        let diff_x = clamp_safe(pos.0 - down.point.0, down.borders.left, down.borders.right);
        let diff_y = clamp_safe(pos.1 - down.point.1, down.borders.top, down.borders.bottom);
        let mut result = down.rect.translated(diff_x as f64, diff_y as f64);
        adjust_crop_to_inner(&mut result, down.bounds);
        self.set_crop_paint(result);
    }

    fn convert_crop_paint_to_original(&mut self) {
        if self.ratio_w <= 0.0 || self.ratio_h <= 0.0 {
            return;
        }
        let rotated_size = flip_size_f_by_rotation(self.image_size.to_f64(), self.angle);
        let crop_holder = Affine2D::from_scale(self.ratio_w, self.ratio_h).map_rect(RectF::new(
            0.0,
            0.0,
            rotated_size.width,
            rotated_size.height,
        ));
        let center = crop_holder.center();

        // Exact inverse matrix from Crop::convertCropPaintToOriginal.
        let matrix = Affine2D::identity()
            .qt_translate(center.x, center.y)
            .qt_rotate(-self.angle as f64)
            .qt_scale(
                (if self.flipped { -1.0 } else { 1.0 }) / self.ratio_w,
                1.0 / self.ratio_h,
            )
            .qt_translate(-center.x, -center.y);

        let holder_rotated = matrix.map_rect(crop_holder);

        self.crop_original = matrix
            .map_rect(
                self.crop_paint
                    .translated(-self.inner_rect.x, -self.inner_rect.y),
            )
            .translated(-holder_rotated.x, -holder_rotated.y);
    }
}

fn clamp_safe(val: i32, lo: i32, hi: i32) -> i32 {
    if lo <= hi { val.clamp(lo, hi) } else { lo }
}
