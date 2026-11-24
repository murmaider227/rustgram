//! Non-visual state machine and layout geometry for the floating round-video mini-player.
//!
//! Port of `media/player/media_player_float.h` and `media/player/media_player_float.cpp`
//! (`Float::outRatio`, `FloatController::{checkCurrent, toggle, checkVisibility,
//! getPosition, getHiddenPosition, getSide, updateColumnCorner, finishDrag}`).

use tg_data::FullMsgId;

/// 2D integer point in parent-window coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

impl Point {
    /// Creates a new point.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Manhattan distance (`|x1 - x2| + |y1 - y2|`), matching Qt's `QPoint::manhattanLength()`.
    #[must_use]
    pub const fn manhattan_distance(self, other: Self) -> i32 {
        (self.x - other.x).abs() + (self.y - other.y).abs()
    }
}

/// 2D integer size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl Size {
    /// Creates a new size.
    #[must_use]
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }
}

/// Axis-aligned rectangle in parent coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left coordinate.
    pub x: i32,
    /// Top coordinate.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

impl Rect {
    /// Creates a new rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Center point of the rectangle.
    #[must_use]
    pub const fn center(self) -> Point {
        Point::new(self.x + self.width / 2, self.y + self.height / 2)
    }
}

/// Window column in Telegram Desktop's multi-column layout (`Window::Column`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FloatColumn {
    /// Left (dialogs) column.
    First,
    /// Main (chat history) column — default in `media_player_float.cpp:303`.
    #[default]
    Second,
    /// Right (profile/info) column.
    Third,
}

/// Anchored corner of a section (`RectPart` corners in `media_player_float.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FloatCorner {
    /// Top-left corner.
    TopLeft,
    /// Top-right corner — default in `media_player_float.cpp:304`.
    #[default]
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
}

impl FloatCorner {
    /// Returns `true` for bottom corners.
    #[must_use]
    pub const fn is_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::BottomRight)
    }

    /// Returns `true` for right corners.
    #[must_use]
    pub const fn is_right(self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }
}

/// Edge from which the float slides in/out (`RectPart` edges in `media_player_float.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FloatSide {
    /// Left edge.
    Left,
    /// Top edge.
    Top,
    /// Right edge — default in `media_player_float.cpp:302`.
    #[default]
    Right,
    /// Bottom edge.
    Bottom,
}

/// Available layout area for a single window column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionArea {
    /// Column identifier.
    pub column: FloatColumn,
    /// Available bounds in parent coordinates.
    pub available_rect: Rect,
}

/// State of an active floating round-video mini-player item.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatItemState {
    /// Message ID of the currently attached round video message (`None` if detached).
    pub item: Option<FullMsgId>,
    /// Whether the underlying streamed video has a ready frame.
    pub ready: bool,
    /// Hidden because the original message bubble is visible in chat history.
    pub hidden_by_history: bool,
    /// Hidden by widget callback (e.g. no frame yet or detached).
    pub hidden_by_widget: bool,
    /// Hidden because the user dragged it off-screen to dismiss.
    pub hidden_by_drag: bool,
    /// Computed visibility (`!hidden_by_history && !hidden_by_widget && ready`).
    pub visible: bool,
    /// Current opacity in `[0.0, 1.0]`.
    pub opacity: f64,
    /// Edge used for slide-in/out animation.
    pub animation_side: FloatSide,
    /// Currently anchored column.
    pub column: FloatColumn,
    /// Currently anchored corner.
    pub corner: FloatCorner,
    /// Current widget top-left position in parent coordinates.
    pub position: Point,
    /// Widget size (square: `2 * margin + float_size`).
    pub size: Size,
    /// Whether a drag gesture is currently active.
    pub dragging: bool,
    /// Starting position of the current snap/dismiss animation.
    pub drag_from: Point,
}

/// Action emitted by [`FloatController::finish_drag`] when a drag completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragOutcome {
    /// `true` if the player was dragged past the 50% boundary and dismissed.
    pub closed: bool,
    /// Closed message ID if `closed` is `true`.
    pub closed_item: Option<FullMsgId>,
    /// Newly persisted column preference.
    pub column: FloatColumn,
    /// Newly persisted corner preference.
    pub corner: FloatCorner,
    /// Target resting (or hidden) position in parent coordinates.
    pub target_position: Point,
}

/// Computes what fraction (`0.0..=1.0`) of `widget_rect` remains inside `parent_rect`.
///
/// Exact port of `Float::outRatio` (`media_player_float.cpp:151-167`).
/// If `< 0.5` on mouse release, the floating player is closed/dismissed.
#[must_use]
pub fn compute_out_ratio(widget_rect: Rect, parent_rect: Rect) -> f64 {
    if widget_rect.width <= 0 || widget_rect.height <= 0 {
        return 0.0;
    }
    let mut min_ratio = 1.0f64;
    if widget_rect.x < parent_rect.x {
        min_ratio = min_ratio
            .min(1.0 - f64::from(parent_rect.x - widget_rect.x) / f64::from(widget_rect.width));
    }
    if widget_rect.y < parent_rect.y {
        min_ratio = min_ratio
            .min(1.0 - f64::from(parent_rect.y - widget_rect.y) / f64::from(widget_rect.height));
    }
    let right_overflow = (widget_rect.x + widget_rect.width) - (parent_rect.x + parent_rect.width);
    if right_overflow > 0 {
        min_ratio = min_ratio.min(1.0 - f64::from(right_overflow) / f64::from(widget_rect.width));
    }
    let bottom_overflow =
        (widget_rect.y + widget_rect.height) - (parent_rect.y + parent_rect.height);
    if bottom_overflow > 0 {
        min_ratio = min_ratio.min(1.0 - f64::from(bottom_overflow) / f64::from(widget_rect.height));
    }
    min_ratio.clamp(0.0, 1.0)
}

/// Nearest parent edge for a given widget center (`FloatController::getSide`).
#[must_use]
pub fn nearest_side(center: Point, parent_size: Size) -> FloatSide {
    let left = center.x.abs();
    let right = (parent_size.width - center.x).abs();
    let top = center.y.abs();
    let bottom = (parent_size.height - center.y).abs();
    if left < right && left < top && left < bottom {
        FloatSide::Left
    } else if right < top && right < bottom {
        FloatSide::Right
    } else if top < bottom {
        FloatSide::Top
    } else {
        FloatSide::Bottom
    }
}

/// Off-screen resting coordinate for slide-out animations (`FloatController::getHiddenPosition`).
#[must_use]
pub fn hidden_position(position: Point, size: Size, parent_size: Size, side: FloatSide) -> Point {
    match side {
        FloatSide::Left => Point::new(-size.width, position.y),
        FloatSide::Top => Point::new(position.x, -size.height),
        FloatSide::Right => Point::new(parent_size.width, position.y),
        FloatSide::Bottom => Point::new(position.x, parent_size.height),
    }
}

/// Anchored corner coordinate inside a section rectangle (`FloatController::getPosition`).
#[must_use]
pub fn corner_position(section_rect: Rect, widget_size: Size, corner: FloatCorner) -> Point {
    let mut x = section_rect.x;
    let mut y = section_rect.y;
    if corner.is_bottom() {
        y += section_rect.height - widget_size.height;
    }
    if corner.is_right() {
        x += section_rect.width - widget_size.width;
    }
    Point::new(x, y)
}

/// Non-visual controller for the floating round-video mini-player.
///
/// Port of `Media::Player::FloatController` (`media_player_float.cpp:317-665`).
#[derive(Debug, Clone)]
pub struct FloatController {
    parent_size: Size,
    widget_size: Size,
    preferred_column: FloatColumn,
    preferred_corner: FloatCorner,
    sections: Vec<SectionArea>,
    current: Option<FloatItemState>,
}

impl FloatController {
    /// Creates a new [`FloatController`] with parent bounds and widget size.
    #[must_use]
    pub fn new(parent_size: Size, widget_size: Size) -> Self {
        Self {
            parent_size,
            widget_size,
            preferred_column: FloatColumn::Second,
            preferred_corner: FloatCorner::TopRight,
            sections: vec![SectionArea {
                column: FloatColumn::Second,
                available_rect: Rect::new(0, 0, parent_size.width, parent_size.height),
            }],
            current: None,
        }
    }

    /// Updates parent window dimensions and available column rects.
    pub fn set_layout(&mut self, parent_size: Size, sections: Vec<SectionArea>) {
        self.parent_size = parent_size;
        self.sections = sections;
        self.update_position();
    }

    /// Returns the preferred column.
    #[must_use]
    pub const fn preferred_column(&self) -> FloatColumn {
        self.preferred_column
    }

    /// Returns the preferred corner.
    #[must_use]
    pub const fn preferred_corner(&self) -> FloatCorner {
        self.preferred_corner
    }

    /// Sets initial persisted column/corner preferences.
    pub fn set_preferences(&mut self, column: FloatColumn, corner: FloatCorner) {
        self.preferred_column = column;
        self.preferred_corner = corner;
        if let Some(item) = &mut self.current {
            item.column = column;
            item.corner = corner;
        }
        self.update_position();
    }

    /// Returns the current floating item state, if any.
    #[must_use]
    pub fn current(&self) -> Option<&FloatItemState> {
        self.current.as_ref()
    }

    /// Attaches a playing round video message (or detaches when `item` is `None`).
    ///
    /// Port of `FloatController::checkCurrent` (`media_player_float.cpp:382-409`).
    pub fn set_current_round_video(&mut self, item: Option<FullMsgId>) {
        if let Some(cur) = &self.current
            && cur.item == item
            && item.is_some()
        {
            return;
        }
        let Some(full_id) = item else {
            self.detach();
            return;
        };

        let pos = self.section_position(self.preferred_column, self.preferred_corner);
        self.current = Some(FloatItemState {
            item: Some(full_id),
            ready: false,
            hidden_by_history: false,
            hidden_by_widget: false,
            hidden_by_drag: false,
            visible: false,
            opacity: 0.0,
            animation_side: FloatSide::Right,
            column: self.preferred_column,
            corner: self.preferred_corner,
            position: pos,
            size: self.widget_size,
            dragging: false,
            drag_from: pos,
        });
    }

    /// Marks the active floating player as detached (`Float::detach`).
    pub fn detach(&mut self) {
        if let Some(cur) = &mut self.current {
            cur.item = None;
            cur.hidden_by_widget = true;
            cur.visible = false;
            cur.opacity = 0.0;
        }
    }

    /// Updates readiness and whether the underlying chat item is currently visible
    /// in the history viewport (`FloatController::checkVisibility` + `toggle`).
    pub fn update_visibility(&mut self, ready: bool, item_visible_in_history: bool) {
        let parent_size = self.parent_size;
        let target_pos = self
            .current
            .as_ref()
            .map(|c| self.section_position(c.column, c.corner))
            .unwrap_or_default();

        if let Some(cur) = &mut self.current {
            cur.ready = ready;
            cur.hidden_by_history = item_visible_in_history;
            let visible = !cur.hidden_by_history && !cur.hidden_by_widget && cur.ready;
            if cur.visible != visible {
                cur.visible = visible;
                cur.dragging = false;
                if !cur.hidden_by_drag {
                    let final_rect =
                        Rect::new(target_pos.x, target_pos.y, cur.size.width, cur.size.height);
                    cur.animation_side = nearest_side(final_rect.center(), parent_size);
                }
            }
            cur.opacity = if cur.visible { 1.0 } else { 0.0 };
            if !cur.dragging {
                cur.position = if cur.visible {
                    target_pos
                } else {
                    hidden_position(target_pos, cur.size, parent_size, cur.animation_side)
                };
            }
        }
    }

    /// Moves the floating player during an active drag gesture and updates its
    /// opacity according to [`compute_out_ratio`].
    pub fn drag_to(&mut self, new_position: Point) {
        let parent_rect = Rect::new(0, 0, self.parent_size.width, self.parent_size.height);
        if let Some(cur) = &mut self.current {
            cur.dragging = true;
            cur.position = new_position;
            let widget_rect = Rect::new(
                cur.position.x,
                cur.position.y,
                cur.size.width,
                cur.size.height,
            );
            cur.opacity = compute_out_ratio(widget_rect, parent_rect);
        }
    }

    /// Completes a drag gesture: snaps to the nearest section corner by Manhattan
    /// distance, or dismisses if `out_ratio < 0.5` (`FloatController::finishDrag`).
    pub fn finish_drag(&mut self) -> Option<DragOutcome> {
        let parent_rect = Rect::new(0, 0, self.parent_size.width, self.parent_size.height);
        let cur = self.current.as_ref()?;
        let widget_rect = Rect::new(
            cur.position.x,
            cur.position.y,
            cur.size.width,
            cur.size.height,
        );
        let closed = compute_out_ratio(widget_rect, parent_rect) < 0.5;
        let center = widget_rect.center();

        // Nearest corner across all sections (`updateColumnCorner`, lines 594-635).
        let (column, corner) = self.find_nearest_corner(center, cur.size);
        self.preferred_column = column;
        self.preferred_corner = corner;

        let target = if closed {
            let side = nearest_side(center, self.parent_size);
            hidden_position(cur.position, cur.size, self.parent_size, side)
        } else {
            self.section_position(column, corner)
        };

        let closed_item = if closed { cur.item } else { None };
        if let Some(cur) = &mut self.current {
            cur.drag_from = cur.position;
            cur.dragging = false;
            cur.column = column;
            cur.corner = corner;
            cur.position = target;
            if closed {
                cur.hidden_by_drag = true;
                cur.animation_side = nearest_side(center, self.parent_size);
                cur.item = None;
                cur.visible = false;
                cur.opacity = 0.0;
            } else {
                cur.opacity = if cur.visible { 1.0 } else { 0.0 };
            }
        }

        Some(DragOutcome {
            closed,
            closed_item,
            column,
            corner,
            target_position: target,
        })
    }

    fn find_nearest_corner(&self, center: Point, size: Size) -> (FloatColumn, FloatCorner) {
        let mut best_dist = i32::MAX;
        let mut best_column = self.preferred_column;
        let mut best_corner = self.preferred_corner;

        for section in &self.sections {
            let r = section.available_rect;
            let left = r.x + size.width / 2;
            let right = r.x + r.width - size.width / 2;
            let top = r.y + size.height / 2;
            let bottom = r.y + r.height - size.height / 2;

            let candidates = [
                (Point::new(left, top), FloatCorner::TopLeft),
                (Point::new(right, top), FloatCorner::TopRight),
                (Point::new(left, bottom), FloatCorner::BottomLeft),
                (Point::new(right, bottom), FloatCorner::BottomRight),
            ];
            for (pt, corner) in candidates {
                let dist = pt.manhattan_distance(center);
                if dist < best_dist {
                    best_dist = dist;
                    best_column = section.column;
                    best_corner = corner;
                }
            }
        }

        (best_column, best_corner)
    }

    fn section_position(&self, column: FloatColumn, corner: FloatCorner) -> Point {
        let rect = self
            .sections
            .iter()
            .find(|s| s.column == column)
            .or_else(|| self.sections.first())
            .map(|s| s.available_rect)
            .unwrap_or_else(|| Rect::new(0, 0, self.parent_size.width, self.parent_size.height));
        corner_position(rect, self.widget_size, corner)
    }

    fn update_position(&mut self) {
        let Some(cur) = &self.current else {
            return;
        };
        if cur.dragging {
            return;
        }
        let target = self.section_position(cur.column, cur.corner);
        let parent_size = self.parent_size;
        if let Some(cur) = &mut self.current {
            cur.position = if cur.visible {
                target
            } else {
                hidden_position(target, cur.size, parent_size, cur.animation_side)
            };
        }
    }
}
