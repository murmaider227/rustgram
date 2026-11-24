//! Linear and dual-axis (`DoubleLinear`) chart geometry and hit-testing.
//!
//! Port of `Telegram/SourceFiles/statistics/view/linear_chart_view.{h,cpp}`.

use crate::anim::interpolate_f;
use crate::chart::{Limits, PointF, RectF, StatisticalChart};
use crate::filter::LinesFilterController;
use crate::views::{DoubleLineRatios, HeightLimits, PaintContext, default_height_limits};

/// Linear (single-axis or dual-axis) chart view (`Statistic::LinearChartView`).
#[derive(Debug, Clone)]
pub struct LinearChartView {
    cached_line_ratios: DoubleLineRatios,
}

impl LinearChartView {
    /// Creates a new linear chart view; pass `is_double = true` for `DoubleLinear`.
    #[must_use]
    pub const fn new(is_double: bool) -> Self {
        Self {
            cached_line_ratios: DoubleLineRatios::new(is_double),
        }
    }

    /// Returns the dual-axis line ratios (initializing them if needed).
    pub fn ratios(&mut self, chart_data: &StatisticalChart) -> DoubleLineRatios {
        if !self.cached_line_ratios.is_initialized() {
            self.cached_line_ratios.init(chart_data);
        }
        self.cached_line_ratios
    }

    /// Computes vertical full and visible-range height limits using segment trees.
    ///
    /// Port of `LinearChartView::heightLimits`.
    #[must_use]
    pub fn height_limits(
        &mut self,
        chart_data: &StatisticalChart,
        lines_filter: &LinesFilterController,
        x_indices: Limits,
    ) -> HeightLimits {
        let ratios = self.ratios(chart_data);
        default_height_limits(ratios, lines_filter, chart_data, x_indices)
    }

    /// Computes polyline vertices in `c.rect` coordinates for series `line_index`.
    ///
    /// Port of `PaintChartLine` (`linear_chart_view.cpp:21-55`).
    #[must_use]
    pub fn compute_line_points(&mut self, line_index: usize, c: &PaintContext<'_>) -> Vec<PointF> {
        let Some(line) = c.chart_data.lines.get(line_index) else {
            return Vec::new();
        };
        if c.chart_data.x_percentage.is_empty() {
            return Vec::new();
        }
        let ratios = self.ratios(c.chart_data);
        let ratio = ratios.ratio(line.id);

        let x_span = c.x_percentage_limits.span();
        let y_span = c.height_limits.span();
        if x_span == 0.0 || y_span == 0.0 {
            return Vec::new();
        }

        const OFFSET: f64 = 2.0;
        let local_start = (c.x_indices.min - OFFSET).max(0.0) as usize;
        let local_end =
            (c.x_indices.max + OFFSET).min((c.chart_data.x_percentage.len() - 1) as f64) as usize;

        let mut points = Vec::with_capacity(local_end.saturating_sub(local_start) + 1);
        for i in local_start..=local_end {
            if line.y[i] < 0 {
                continue;
            }
            let x_point = c.rect.x
                + c.rect.width
                    * ((c.chart_data.x_percentage[i] - c.x_percentage_limits.min) / x_span);
            let y_pct = ((line.y[i] as f64) * ratio - c.height_limits.min) / y_span;
            let y_point = c.rect.y + (1.0 - y_pct) * c.rect.height;
            points.push(PointF::new(x_point, y_point));
        }
        points
    }

    /// Computes the `(line_id, PointF)` dots for `selected_x_index`.
    ///
    /// Port of `LinearChartView::paintSelectedXIndex` coordinate calculation.
    #[must_use]
    pub fn selected_points(
        &mut self,
        c: &PaintContext<'_>,
        selected_x_index: usize,
    ) -> Vec<(i32, PointF)> {
        if selected_x_index >= c.chart_data.x_percentage.len() {
            return Vec::new();
        }
        let ratios = self.ratios(c.chart_data);
        let x_span = c.x_percentage_limits.span();
        let y_span = c.height_limits.span();
        if x_span == 0.0 || y_span == 0.0 {
            return Vec::new();
        }

        let x_point = c.rect.x
            + c.rect.width
                * ((c.chart_data.x_percentage[selected_x_index] - c.x_percentage_limits.min)
                    / x_span);
        let mut out = Vec::with_capacity(c.chart_data.lines.len());
        for line in &c.chart_data.lines {
            let r = ratios.ratio(line.id);
            let y_pct = ((line.y[selected_x_index] as f64) * r - c.height_limits.min) / y_span;
            let y_point = c.rect.y + (1.0 - y_pct) * c.rect.height;
            out.push((line.id, PointF::new(x_point, y_point)));
        }
        out
    }

    /// Finds the nearest X index for horizontal pointer position `x` inside `rect`.
    ///
    /// Port of `LinearChartView::findXIndexByPosition` (`linear_chart_view.cpp:180-211`).
    #[must_use]
    pub fn find_x_index_by_position(
        &self,
        chart_data: &StatisticalChart,
        x_percentage_limits: Limits,
        rect: RectF,
        x: f64,
    ) -> Option<usize> {
        if chart_data.x_percentage.is_empty() || rect.width <= 0.0 || x < rect.x || x > rect.right()
        {
            return None;
        }
        let pointer_ratio = ((x - rect.x) / rect.width).clamp(0.0, 1.0);
        let raw_x_pct = interpolate_f(
            x_percentage_limits.min,
            x_percentage_limits.max,
            pointer_ratio,
        );
        let n = chart_data.x_percentage.len();
        let idx = chart_data.x_percentage.partition_point(|&v| v < raw_x_pct);

        let nearest = if idx == 0 {
            0
        } else if idx >= n {
            n - 1
        } else {
            let left = raw_x_pct - chart_data.x_percentage[idx - 1];
            let right = chart_data.x_percentage[idx] - raw_x_pct;
            if right > left { idx - 1 } else { idx }
        };

        let clamped = if chart_data.x_percentage[nearest] > x_percentage_limits.max {
            nearest.saturating_sub(1)
        } else if chart_data.x_percentage[nearest] < x_percentage_limits.min {
            (nearest + 1).min(n - 1)
        } else {
            nearest
        };
        Some(clamped.min(n - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_linear_scales_smaller_series_to_larger() {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000, 1700179200000],
                ["y0", 1000, 2000, 1500],
                ["y1", 10, 20, 5]
            ],
            "y_scaled": true
        }"#;
        let chart = StatisticalChart::from_json_str(json).unwrap();
        let filter = LinesFilterController::new();
        let mut view = LinearChartView::new(true);

        let limits = view.height_limits(&chart, &filter, Limits::new(0.0, 2.0));
        // first_max = 2000, second_max = 20 -> ratio(1) = 1.0, ratio(2) = 100.0.
        // y1[2] = 5 -> scaled = 500, which becomes the global min!
        assert_eq!(limits.ranged, Limits::new(500.0, 2000.0));
        assert_eq!(limits.full, Limits::new(500.0, 2000.0));

        let ctx = PaintContext {
            chart_data: &chart,
            x_indices: Limits::new(0.0, 2.0),
            x_percentage_limits: Limits::new(0.0, 1.0),
            height_limits: limits.ranged,
            rect: RectF::new(0.0, 0.0, 200.0, 100.0),
            footer: false,
        };
        let pts0 = view.compute_line_points(0, &ctx);
        let pts1 = view.compute_line_points(1, &ctx);
        assert_eq!(pts0.len(), 3);
        assert_eq!(pts1.len(), 3);
        // At index 1, both lines reach max -> y = 0.0 (top of rect).
        assert!((pts0[1].y - 0.0).abs() < 1e-12);
        assert!((pts1[1].y - 0.0).abs() < 1e-12);
        // At index 2, line 1 is at min (500) -> y = 100.0 (bottom of rect).
        assert!((pts1[2].y - 100.0).abs() < 1e-12);
    }

    #[test]
    fn find_x_index_returns_nearest_visible_point() {
        let json = r#"{
            "columns": [
                ["x", 0, 100, 200, 300, 400],
                ["y0", 1, 2, 3, 4, 5]
            ]
        }"#;
        let chart = StatisticalChart::from_json_str(json).unwrap();
        let view = LinearChartView::new(false);
        let rect = RectF::new(10.0, 0.0, 400.0, 100.0);

        assert_eq!(
            view.find_x_index_by_position(&chart, Limits::new(0.0, 1.0), rect, 10.0),
            Some(0)
        );
        assert_eq!(
            view.find_x_index_by_position(&chart, Limits::new(0.0, 1.0), rect, 210.0),
            Some(2)
        );
        assert_eq!(
            view.find_x_index_by_position(&chart, Limits::new(0.0, 1.0), rect, 5.0),
            None
        );
    }
}
