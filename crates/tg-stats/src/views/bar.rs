//! Bar (step-line) and stacked vertical bar (`StackBar`) chart geometry and height limits.
//!
//! Port of `Telegram/SourceFiles/statistics/view/bar_chart_view.{h,cpp}`.

use crate::chart::{Limits, PointF, RectF, StatisticalChart};
use crate::filter::LinesFilterController;
use crate::segment_tree::{ChartValue, SegmentTree};
use crate::views::{
    DoubleLineRatios, HeightLimits, PaintContext, compute_left_start_and_step,
    default_height_limits,
};

/// Dimming opacity applied to non-selected columns in `StackBar` mode (`kSelectedAlpha`).
pub const BAR_SELECTED_DIM_ALPHA: f64 = 0.5;

/// Computed geometry for one series in a [`BarChartView`].
#[derive(Debug, Clone, PartialEq)]
pub struct BarSeriesGeometry {
    /// Series ID.
    pub line_id: i32,
    /// Solid bar rectangles (populated for `StackBar`).
    pub rects: Vec<RectF>,
    /// Step polyline vertices (populated for non-stacked `Bar`).
    pub step_points: Vec<PointF>,
    /// Highlighted rectangle at the selected X index (populated for `StackBar`).
    pub selected_rect: Option<RectF>,
}

/// Bar (`is_stack = false`) and stacked bar (`is_stack = true`) chart view.
///
/// Port of `Statistic::BarChartView`.
#[derive(Debug, Clone)]
pub struct BarChartView {
    is_stack: bool,
    cached_line_ratios: DoubleLineRatios,
    last_painted_x_indices: Limits,
    last_selected_x_index: Option<usize>,
    last_selected_x_progress: f64,
}

impl BarChartView {
    /// Creates a new bar chart view (`is_stack = true` for `StackBar`, `false` for `Bar`).
    #[must_use]
    pub const fn new(is_stack: bool) -> Self {
        Self {
            is_stack,
            cached_line_ratios: DoubleLineRatios::new(false),
            last_painted_x_indices: Limits::new(0.0, 0.0),
            last_selected_x_index: None,
            last_selected_x_progress: 0.0,
        }
    }

    /// Returns whether this view renders stacked bars (`true`) or step-line bars (`false`).
    #[must_use]
    pub const fn is_stack(&self) -> bool {
        self.is_stack
    }

    /// Updates the selected X index and fade progress (`0.0..=1.0`).
    pub fn set_selected(&mut self, x_index: Option<usize>, progress: f64) {
        self.last_selected_x_index = x_index;
        self.last_selected_x_progress = progress;
    }

    /// Computes vertical height limits.
    ///
    /// For `StackBar`, sums all enabled lines at every X point and builds a [`SegmentTree`]
    /// to query the visible maximum (minimum is always `0`, max clamped to at least `1`).
    /// For non-stack `Bar`, delegates to [`default_height_limits`].
    ///
    /// Port of `BarChartView::heightLimits` (`bar_chart_view.cpp:241-284`).
    #[must_use]
    pub fn height_limits(
        &mut self,
        chart_data: &StatisticalChart,
        lines_filter: &LinesFilterController,
        x_indices: Limits,
    ) -> HeightLimits {
        if !self.is_stack {
            if !self.cached_line_ratios.is_initialized() {
                self.cached_line_ratios.init(chart_data);
            }
            return default_height_limits(
                self.cached_line_ratios,
                lines_filter,
                chart_data,
                x_indices,
            );
        }

        let n = chart_data.x.len();
        let mut y_sum = Vec::with_capacity(n);
        let mut max_value_full: ChartValue = 0;

        for i in 0..n {
            let mut sum: ChartValue = 0;
            for line in &chart_data.lines {
                if lines_filter.is_enabled(line.id) {
                    sum = sum.saturating_add(line.y[i]);
                }
            }
            y_sum.push(sum);
            max_value_full = max_value_full.max(sum);
        }

        let tree = SegmentTree::new(y_sum);
        let ranged_max = tree
            .r_max_q(x_indices.min as i32, x_indices.max as i32)
            .max(1);

        HeightLimits {
            full: Limits::new(0.0, max_value_full as f64),
            ranged: Limits::new(0.0, ranged_max as f64),
        }
    }

    /// Computes bar rectangles (for `StackBar`) or step polylines (for `Bar`) across `c`.
    ///
    /// Port of `BarChartView::paint` and `BarChartView::paintChartAndSelected`.
    #[must_use]
    pub fn compute_geometry(
        &mut self,
        c: &PaintContext<'_>,
        lines_filter: &LinesFilterController,
    ) -> Vec<BarSeriesGeometry> {
        if c.chart_data.x_percentage.is_empty() || c.height_limits.span() == 0.0 {
            return Vec::new();
        }
        const OFFSET: f64 = 2.0;
        let local_start = (c.x_indices.min - OFFSET).max(0.0) as usize;
        let local_end =
            (c.x_indices.max + OFFSET).min((c.chart_data.x_percentage.len() - 1) as f64) as usize;
        self.last_painted_x_indices = Limits::new(local_start as f64, local_end as f64);

        let left_and_step =
            compute_left_start_and_step(c.chart_data, c.x_percentage_limits, c.rect, local_start);
        let left_start = left_and_step.start;
        let w = left_and_step.step;
        let y_span = c.height_limits.span();

        let count = local_end.saturating_sub(local_start) + 1;
        let mut bottoms = vec![0.0f64; count];
        let has_selected = self.is_stack && !c.footer && self.last_selected_x_index.is_some();
        let selected_idx = self.last_selected_x_index;

        let mut result = Vec::with_capacity(c.chart_data.lines.len());

        for line in &c.chart_data.lines {
            let alpha = lines_filter.alpha(line.id);
            let mut rects = Vec::new();
            let mut step_points = Vec::new();
            let mut selected_rect = None;

            for (x, &y_val) in line
                .y
                .iter()
                .enumerate()
                .take(local_end + 1)
                .skip(local_start)
            {
                if y_val <= 0 && self.is_stack {
                    continue;
                }
                let y_pct = ((y_val as f64) - c.height_limits.min) / y_span;
                let y_point = y_pct * c.rect.height * alpha;
                let bottom_idx = x - local_start;
                let col_x = left_start + ((x - local_start) as f64) * w;
                let col_y = c.rect.bottom() - bottoms[bottom_idx] - y_point;
                let column = RectF::new(col_x, col_y, w, y_point);

                if has_selected && selected_idx == Some(x) && y_point > 0.0 {
                    selected_rect = Some(column);
                }

                if self.is_stack {
                    rects.push(column);
                    bottoms[bottom_idx] += y_point;
                } else {
                    step_points.push(PointF::new(column.x, column.y));
                    if x == local_end {
                        step_points.push(PointF::new(c.rect.right(), column.y));
                    } else {
                        step_points.push(PointF::new(column.right(), column.y));
                    }
                }
            }

            result.push(BarSeriesGeometry {
                line_id: line.id,
                rects,
                step_points,
                selected_rect,
            });
        }

        result
    }

    /// Computes selected-point marker coordinates for non-stack `Bar` charts.
    #[must_use]
    pub fn selected_bar_points(
        &self,
        c: &PaintContext<'_>,
        selected_x_index: usize,
    ) -> Vec<(i32, PointF)> {
        if selected_x_index >= c.chart_data.x_percentage.len() || c.height_limits.span() == 0.0 {
            return Vec::new();
        }
        let local_start = self.last_painted_x_indices.min as usize;
        let ls =
            compute_left_start_and_step(c.chart_data, c.x_percentage_limits, c.rect, local_start);
        let y_span = c.height_limits.span();

        let mut out = Vec::with_capacity(c.chart_data.lines.len());
        for line in &c.chart_data.lines {
            let y_pct = ((line.y[selected_x_index] as f64) - c.height_limits.min) / y_span;
            let y_point = c.rect.y + (1.0 - y_pct) * c.rect.height;
            let col_left = ls.start + ((selected_x_index as f64) - (local_start as f64)) * ls.step;
            let x_point = col_left + ls.step / 2.0;
            out.push((line.id, PointF::new(x_point, y_point)));
        }
        out
    }

    /// Finds the bar X index covering horizontal coordinate `x_pos`.
    ///
    /// Port of `BarChartView::findXIndexByPosition` (`bar_chart_view.cpp:215-239`).
    #[must_use]
    pub fn find_x_index_by_position(
        &mut self,
        chart_data: &StatisticalChart,
        x_percentage_limits: Limits,
        rect: RectF,
        x_pos: f64,
    ) -> Option<usize> {
        if chart_data.x_percentage.is_empty() || x_pos < rect.x || x_pos > rect.right() {
            self.last_selected_x_index = None;
            return None;
        }
        let local_start = self.last_painted_x_indices.min as usize;
        let local_end =
            (self.last_painted_x_indices.max as usize).min(chart_data.x_percentage.len() - 1);
        let ls = compute_left_start_and_step(chart_data, x_percentage_limits, rect, local_start);

        for x in local_start..=local_end {
            let left = ls.start + ((x - local_start) as f64) * ls.step;
            if x_pos >= left && x_pos < left + ls.step {
                self.last_selected_x_index = Some(x);
                return Some(x);
            }
        }
        self.last_selected_x_index = None;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_bar_sums_only_enabled_lines() {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000],
                ["y0", 10, 20, 30, 40],
                ["y1", 5, 15, 25, 35]
            ],
            "stacked": true
        }"#;
        let chart = StatisticalChart::from_json_str(json).unwrap();
        let mut filter = LinesFilterController::new();
        let mut view = BarChartView::new(true);

        // Sums: [15, 35, 55, 75]
        let lim = view.height_limits(&chart, &filter, Limits::new(0.0, 2.0));
        assert_eq!(lim.full, Limits::new(0.0, 75.0));
        assert_eq!(lim.ranged, Limits::new(0.0, 55.0));

        // Disable y1 -> sums become [10, 20, 30, 40]
        filter.set_enabled(2, false, 1);
        filter.tick(1.0);
        let lim2 = view.height_limits(&chart, &filter, Limits::new(0.0, 2.0));
        assert_eq!(lim2.full, Limits::new(0.0, 40.0));
        assert_eq!(lim2.ranged, Limits::new(0.0, 30.0));
    }

    #[test]
    fn stack_bar_geometry_stacks_columns_bottom_to_top() {
        let json = r#"{
            "columns": [
                ["x", 0, 1, 2],
                ["y0", 25, 50, 75],
                ["y1", 75, 50, 25]
            ],
            "stacked": true
        }"#;
        let chart = StatisticalChart::from_json_str(json).unwrap();
        let filter = LinesFilterController::new();
        let mut view = BarChartView::new(true);
        let ctx = PaintContext {
            chart_data: &chart,
            x_indices: Limits::new(0.0, 2.0),
            x_percentage_limits: Limits::new(0.0, 1.0),
            height_limits: Limits::new(0.0, 100.0),
            rect: RectF::new(0.0, 0.0, 300.0, 100.0),
            footer: false,
        };
        let geom = view.compute_geometry(&ctx, &filter);
        assert_eq!(geom.len(), 2);
        // Column 0: y0 height 25 (y=75..100), y1 height 75 (y=0..75).
        assert!((geom[0].rects[0].height - 25.0).abs() < 1e-9);
        assert!((geom[0].rects[0].y - 75.0).abs() < 1e-9);
        assert!((geom[1].rects[0].height - 75.0).abs() < 1e-9);
        assert!((geom[1].rects[0].y - 0.0).abs() < 1e-9);
    }
}
