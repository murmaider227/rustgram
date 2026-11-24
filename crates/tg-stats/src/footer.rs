//! Footer range selector geometry and handle-dragging math.
//!
//! Port of `ChartWidget::Footer` from `Telegram/SourceFiles/statistics/chart_widget.cpp`
//! and style metrics from `Telegram/SourceFiles/statistics/statistics.style`.

use crate::chart::Limits;

/// Width of each draggable left/right handle in logical pixels (`st::statisticsChartFooterSideWidth`).
pub const FOOTER_SIDE_WIDTH: f64 = 10.0;

/// Minimum clear pixel gap between the left and right handles (`st::statisticsChartFooterBetweenSide`).
pub const FOOTER_BETWEEN_SIDE: f64 = 5.0;

/// Standard height of the footer selector widget (`st::statisticsChartFooterHeight`).
pub const FOOTER_HEIGHT: f64 = 42.0;

/// Corner radius of the active footer selection frame (`st::statisticsChartFooterSideRadius`).
pub const FOOTER_SIDE_RADIUS: f64 = 6.0;

/// Vertical grip bar height inside each handle (`st::statisticsChartFooterArrowHeight`).
pub const FOOTER_ARROW_HEIGHT: f64 = 10.0;

/// Active drag target within the footer range selector (`ChartWidget::Footer::DragArea`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DragArea {
    /// Outside the selection window (triggers click-to-center on release).
    #[default]
    None,
    /// Inside the window between handles (pans the selection keeping its span).
    Middle,
    /// Left resize handle.
    Left,
    /// Right resize handle.
    Right,
}

/// Optional click-to-center animation request returned on mouse release outside the selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CenterAnimationTarget {
    /// Current center X coordinate of the selection window.
    pub start_x: f64,
    /// Clicked target X coordinate.
    pub finish_x: f64,
    /// Whether the window should shift leftward.
    pub to_left: bool,
    /// Offset from `start_x` to `_leftSide.min`.
    pub diff_between_start_and_left: f64,
}

/// Pure state & geometry model of the footer range selector (`ChartWidget::Footer`).
#[derive(Debug, Clone)]
pub struct FooterRangeSelector {
    side_width: f64,
    between_side: f64,
    width: f64,
    width_between_sides: f64,
    left_side: Limits,
    right_side: Limits,
    drag_area: DragArea,
    press_start_x: f64,
    diff_between_start_and_side: f64,
    dragged_after_press: bool,
}

impl Default for FooterRangeSelector {
    fn default() -> Self {
        Self::new(FOOTER_SIDE_WIDTH, FOOTER_BETWEEN_SIDE)
    }
}

impl FooterRangeSelector {
    /// Creates a footer range selector with explicit handle width and minimum gap.
    #[must_use]
    pub fn new(side_width: f64, between_side: f64) -> Self {
        Self {
            side_width,
            between_side,
            width: 0.0,
            width_between_sides: 0.0,
            left_side: Limits::default(),
            right_side: Limits::default(),
            drag_area: DragArea::None,
            press_start_x: 0.0,
            diff_between_start_and_side: 0.0,
            dragged_after_press: false,
        }
    }

    /// Creates a selector already sized to `total_width` with initial `x_limits` (`0.0..=1.0`).
    #[must_use]
    pub fn with_width(total_width: f64, x_limits: Limits) -> Self {
        let mut s = Self::default();
        s.resize(total_width);
        s.set_x_percentage_limits(x_limits);
        s
    }

    /// Resizes the footer to `new_width`, preserving the normalized percentage window.
    ///
    /// Port of `ChartWidget::Footer::resizeGetHeight` (`chart_widget.cpp:367-384`).
    pub fn resize(&mut self, new_width: f64) {
        if new_width <= 0.0 {
            return;
        }
        let was = self.x_percentage_limits();
        self.width = new_width - self.side_width;
        self.width_between_sides = new_width - self.side_width * 2.0;
        if self.width_between_sides > 0.0 && was.max > 0.0 {
            self.set_x_percentage_limits(was);
        } else if self.width_between_sides > 0.0 && was.min == was.max {
            self.set_x_percentage_limits(Limits::new(0.0, 1.0));
        }
    }

    /// Returns the left handle horizontal pixel bounds `[min, max]`.
    #[must_use]
    pub const fn left_side(&self) -> Limits {
        self.left_side
    }

    /// Returns the right handle horizontal pixel bounds `[min, max]`.
    #[must_use]
    pub const fn right_side(&self) -> Limits {
        self.right_side
    }

    /// Returns the current drag area.
    #[must_use]
    pub const fn drag_area(&self) -> DragArea {
        self.drag_area
    }

    /// Returns the minimum allowed percentage span `(between_side / width_between_sides)`.
    #[must_use]
    pub fn min_percentage_range(&self) -> f64 {
        if self.width_between_sides <= 0.0 {
            0.0
        } else {
            self.between_side / self.width_between_sides
        }
    }

    /// Returns the current selection window in normalized `0.0..=1.0` coordinates.
    ///
    /// Port of `ChartWidget::Footer::xPercentageLimits`.
    #[must_use]
    pub fn x_percentage_limits(&self) -> Limits {
        if self.width_between_sides <= 0.0 {
            return Limits::new(0.0, 0.0);
        }
        Limits::new(
            self.left_side.min / self.width_between_sides,
            (self.right_side.min - self.side_width) / self.width_between_sides,
        )
    }

    /// Sets the selection window from normalized `0.0..=1.0` coordinates, enforcing minimum range.
    ///
    /// Port of `ChartWidget::Footer::setXPercentageLimits`.
    pub fn set_x_percentage_limits(&mut self, x_limits: Limits) {
        let left = x_limits.min * self.width_between_sides;
        let right = x_limits.max * self.width_between_sides + self.side_width;
        self.move_side(true, left);
        self.move_side(false, right);
    }

    /// Moves either the left (`left == true`) or right (`left == false`) handle to pixel `x`,
    /// clamped to `[0, width]` and preserving `between_side`.
    ///
    /// Port of `ChartWidget::Footer::moveSide` (`chart_widget.cpp:417-433`).
    pub fn move_side(&mut self, left: bool, x: f64) {
        let w = self.side_width;
        let mid = self.between_side;
        if self.width < (2.0 * w + mid) {
            return;
        }
        if left {
            let right_limit = self.right_side.min - w - mid;
            let upper = if right_limit <= 0.0 {
                self.width_between_sides
            } else {
                right_limit
            };
            let min = x.clamp(0.0, upper.max(0.0));
            self.left_side = Limits::new(min, min + w);
        } else {
            let lower = self.left_side.max + mid;
            let min = x.clamp(lower, self.width.max(lower));
            self.right_side = Limits::new(min, min + w);
        }
    }

    /// Translates both handles together while keeping their pixel spacing constant.
    ///
    /// Port of `ChartWidget::Footer::moveCenter` (`chart_widget.cpp:400-415`).
    pub fn move_center(
        &mut self,
        is_direction_to_left: bool,
        x: f64,
        diff_between_start_and_left: f64,
    ) {
        let result_x = x - diff_between_start_and_left;
        let diff_between_sides = (self.right_side.min - self.left_side.min).max(self.between_side);
        if is_direction_to_left {
            self.move_side(true, result_x);
            self.move_side(false, self.left_side.min + diff_between_sides);
        } else {
            self.move_side(false, result_x + diff_between_sides);
            self.move_side(true, self.right_side.min - diff_between_sides);
        }
    }

    /// Hit-tests pixel `pos_x` against left handle, right handle, middle window, or outside.
    #[must_use]
    pub fn hit_test(&self, pos_x: f64) -> DragArea {
        let is_left = pos_x >= self.left_side.min && pos_x <= self.left_side.max;
        let is_right = !is_left && pos_x >= self.right_side.min && pos_x <= self.right_side.max;
        if is_left {
            DragArea::Left
        } else if is_right {
            DragArea::Right
        } else if pos_x < self.left_side.min || pos_x > self.right_side.max {
            DragArea::None
        } else {
            DragArea::Middle
        }
    }

    /// Processes a mouse press at horizontal coordinate `pos_x`.
    pub fn on_mouse_press(&mut self, pos_x: f64) {
        self.press_start_x = pos_x;
        self.dragged_after_press = false;
        self.drag_area = self.hit_test(pos_x);
        self.diff_between_start_and_side = match self.drag_area {
            DragArea::Right => self.press_start_x - self.right_side.min,
            _ => self.press_start_x - self.left_side.min,
        };
    }

    /// Processes a mouse drag move to `pos_x`, returning the updated percentage limits if changed.
    pub fn on_mouse_move(&mut self, pos_x: f64) -> Option<Limits> {
        self.dragged_after_press = true;
        if self.drag_area == DragArea::None {
            return None;
        }
        let result_x = pos_x - self.diff_between_start_and_side;
        match self.drag_area {
            DragArea::Right => self.move_side(false, result_x),
            DragArea::Left => self.move_side(true, result_x),
            DragArea::Middle => {
                let to_left = (result_x - self.left_side.min) <= 0.0;
                self.move_center(to_left, pos_x, self.diff_between_start_and_side);
            }
            DragArea::None => return None,
        }
        Some(self.x_percentage_limits())
    }

    /// Processes a mouse release at `pos_x`.
    ///
    /// If the user clicked outside the selection without dragging, returns a
    /// [`CenterAnimationTarget`] describing the animated pan to center on `pos_x`.
    pub fn on_mouse_release(&mut self, pos_x: f64) -> Option<CenterAnimationTarget> {
        if self.drag_area == DragArea::None && !self.dragged_after_press {
            let start_x = self.left_side.min + (self.right_side.max - self.left_side.min) / 2.0;
            let finish_x = pos_x;
            let to_left = finish_x <= start_x;
            let diff_between_start_and_left = start_x - self.left_side.min;
            Some(CenterAnimationTarget {
                start_x,
                finish_x,
                to_left,
                diff_between_start_and_left,
            })
        } else {
            self.drag_area = DragArea::None;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_enforces_minimum_range_and_preserves_width_on_drag() {
        // Total width 220 -> side_width 10 -> width = 210, width_between_sides = 200.
        let mut footer = FooterRangeSelector::with_width(220.0, Limits::new(0.25, 0.75));
        let l = footer.x_percentage_limits();
        assert!((l.min - 0.25).abs() < 1e-12);
        assert!((l.max - 0.75).abs() < 1e-12);
        assert_eq!(footer.left_side(), Limits::new(50.0, 60.0));
        assert_eq!(footer.right_side(), Limits::new(160.0, 170.0));

        // Drag left handle way past right handle -> clamped to right.min - w - mid = 145.
        footer.on_mouse_press(55.0);
        assert_eq!(footer.drag_area(), DragArea::Left);
        footer.on_mouse_move(200.0);
        assert_eq!(footer.left_side(), Limits::new(145.0, 155.0));
        assert!(footer.x_percentage_limits().span() >= footer.min_percentage_range() - 1e-12);
        footer.on_mouse_release(200.0);

        // Reset to [0.25, 0.75] and pan middle right by 20px.
        footer.set_x_percentage_limits(Limits::new(0.25, 0.75));
        footer.on_mouse_press(100.0);
        assert_eq!(footer.drag_area(), DragArea::Middle);
        let moved = footer.on_mouse_move(120.0).unwrap();
        assert!((moved.min - 0.35).abs() < 1e-12);
        assert!((moved.max - 0.85).abs() < 1e-12);
        footer.on_mouse_release(120.0);

        // Click outside without drag returns center animation target.
        footer.on_mouse_press(10.0);
        assert_eq!(footer.drag_area(), DragArea::None);
        let anim = footer.on_mouse_release(10.0).unwrap();
        assert!(anim.to_left);
        assert_eq!(anim.finish_x, 10.0);
    }

    #[test]
    fn footer_resize_preserves_percentage_limits() {
        let mut footer = FooterRangeSelector::with_width(220.0, Limits::new(0.2, 0.6));
        footer.resize(420.0);
        let l = footer.x_percentage_limits();
        assert!((l.min - 0.2).abs() < 1e-12);
        assert!((l.max - 0.6).abs() < 1e-12);
    }
}
