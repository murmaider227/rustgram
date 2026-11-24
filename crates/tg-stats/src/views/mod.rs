//! Chart view abstractions, dual-axis ratios, default height limits, and stacked-chart math.
//!
//! Port of:
//! - `Telegram/SourceFiles/statistics/view/abstract_chart_view.{h,cpp}`
//! - `Telegram/SourceFiles/statistics/view/chart_view_factory.{h,cpp}`
//! - `Telegram/SourceFiles/statistics/view/stack_chart_common.{h,cpp}`

pub mod bar;
pub mod linear;
pub mod stack_linear;

use crate::anim::interpolate_f;
use crate::chart::{ChartViewType, Limits, RectF, StatisticalChart};
use crate::filter::LinesFilterController;
use crate::segment_tree::ChartValue;

pub use bar::BarChartView;
pub use linear::LinearChartView;
pub use stack_linear::{
    ChangingPiePartController, PiePart, PiePartController, PiePartData, StackLinearChartView,
    find_additional_zoomed_out_x_indices, pie_parts_percentage, pie_parts_percentage_by_indices,
};

/// Full and visible-range vertical limits returned by [`ChartView::height_limits`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HeightLimits {
    /// Vertical limits across the entire X range (used for the footer preview).
    pub full: Limits,
    /// Vertical limits across the currently visible X range (used for the main chart).
    pub ranged: Limits,
}

/// Paint geometry parameters shared across all chart views (`Statistic::PaintContext`).
#[derive(Debug, Clone, Copy)]
pub struct PaintContext<'a> {
    /// Chart data being rendered.
    pub chart_data: &'a StatisticalChart,
    /// Visible X index range `[min, max]`.
    pub x_indices: Limits,
    /// Visible X percentage range `[min, max]` in `0.0..=1.0`.
    pub x_percentage_limits: Limits,
    /// Vertical value range `[min, max]`.
    pub height_limits: Limits,
    /// Destination rectangle in logical pixels.
    pub rect: RectF,
    /// Whether this pass renders the miniature footer chart.
    pub footer: bool,
}

/// Request kind for [`ChartView::maybe_local_zoom`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalZoomType {
    /// Prepare transition geometry before animating into zoomed pie mode.
    Prepare,
    /// Return cached zoom limits without recomputing from `x_index`.
    SkipCalculation,
    /// Check whether this chart view supports local zoom.
    CheckAvailability,
    /// Compute zoomed range and limit indices for a given `x_index` and `progress`.
    Process,
    /// Update zoomed range from footer changes.
    SaveZoomFromFooter,
}

/// Arguments to [`ChartView::maybe_local_zoom`] (`AbstractChartView::LocalZoomArgs`).
#[derive(Debug, Clone, Copy)]
pub struct LocalZoomArgs<'a> {
    /// Chart data.
    pub chart_data: &'a StatisticalChart,
    /// Operation to perform.
    pub kind: LocalZoomType,
    /// Animation progress in `0.0..=1.0`.
    pub progress: f64,
    /// Selected X index to zoom into.
    pub x_index: usize,
}

/// Result of [`ChartView::maybe_local_zoom`] (`AbstractChartView::LocalZoomResult`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LocalZoomResult {
    /// Whether local zoom is supported.
    pub has_zoom: bool,
    /// Outer X index window displayed in the zoomed footer.
    pub limit_indices: Limits,
    /// Target footer percentage range inside `limit_indices`.
    pub range: Limits,
}

/// Independent Y-axis scaling factors for two-line `DoubleLinear` charts (`DoubleLineRatios`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DoubleLineRatios {
    first: f64,
    second: f64,
}

impl DoubleLineRatios {
    /// Creates ratios: `(0.0, 0.0)` (uninitialized) when `is_double`, else `(1.0, 1.0)`.
    #[must_use]
    pub const fn new(is_double: bool) -> Self {
        let v = if is_double { 0.0 } else { 1.0 };
        Self {
            first: v,
            second: v,
        }
    }

    /// Returns `true` once initialized (`first > 0`).
    #[must_use]
    pub fn is_initialized(self) -> bool {
        self.first > 0.0
    }

    /// Computes exact scaling ratios from the two series maxima.
    ///
    /// Port of `DoubleLineRatios::init` (`abstract_chart_view.cpp:29-44`).
    pub fn init(&mut self, chart_data: &StatisticalChart) {
        if chart_data.lines.len() != 2 {
            self.first = 1.0;
            self.second = 1.0;
        } else {
            let first_max = chart_data.lines[0].max_value;
            let second_max = chart_data.lines[1].max_value;
            if first_max > second_max {
                self.first = 1.0;
                self.second = if second_max != 0 {
                    first_max as f64 / second_max as f64
                } else {
                    1.0
                };
            } else {
                self.first = if first_max != 0 {
                    second_max as f64 / first_max as f64
                } else {
                    1.0
                };
                self.second = 1.0;
            }
        }
    }

    /// Returns the scaling factor for `line_id` (`1` -> `first`, otherwise `second`).
    #[must_use]
    pub const fn ratio(self, line_id: i32) -> f64 {
        if line_id == 1 {
            self.first
        } else {
            self.second
        }
    }
}

/// Computes `HeightLimits` using each enabled line's [`crate::segment_tree::SegmentTree`]
/// and [`DoubleLineRatios`].
///
/// Port of `DefaultHeightLimits` (`abstract_chart_view.cpp:70-101`).
#[must_use]
pub fn default_height_limits(
    ratios: DoubleLineRatios,
    lines_filter: &LinesFilterController,
    chart_data: &StatisticalChart,
    x_indices: Limits,
) -> HeightLimits {
    let mut ranged_min = ChartValue::MAX;
    let mut ranged_max: ChartValue = 0;

    let mut min_value_full = ChartValue::MAX;
    let mut max_value_full: ChartValue = 0;

    let from = x_indices.min as i32;
    let to = x_indices.max as i32;

    for l in &chart_data.lines {
        if !lines_filter.is_enabled(l.id) {
            continue;
        }
        let r = ratios.ratio(l.id);
        let line_max = l.segment_tree.r_max_q(from, to);
        let line_min = l.segment_tree.r_min_q(from, to);
        ranged_max = ranged_max.max(((line_max as f64) * r) as ChartValue);
        ranged_min = ranged_min.min(((line_min as f64) * r) as ChartValue);

        max_value_full = max_value_full.max(((l.max_value as f64) * r) as ChartValue);
        min_value_full = min_value_full.min(((l.min_value as f64) * r) as ChartValue);
    }

    if ranged_max == ranged_min || ranged_min == ChartValue::MAX {
        ranged_max = chart_data.max_value;
        ranged_min = chart_data.min_value;
    }
    if min_value_full == ChartValue::MAX {
        min_value_full = chart_data.min_value;
        max_value_full = chart_data.max_value;
    }

    HeightLimits {
        full: Limits::new(min_value_full as f64, max_value_full as f64),
        ranged: Limits::new(ranged_min as f64, ranged_max as f64),
    }
}

/// Horizontal starting X coordinate and bar column width for bar/stack charts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeftStartAndStep {
    /// Left X coordinate of the first visible bar (`localStart`).
    pub start: f64,
    /// Width of each bar column in pixels.
    pub step: f64,
}

/// Computes bar starting X and column step width (`ComputeLeftStartAndStep`).
///
/// Port of `statistics/view/stack_chart_common.cpp:16-32`.
#[must_use]
pub fn compute_left_start_and_step(
    chart_data: &StatisticalChart,
    x_percentage_limits: Limits,
    rect: RectF,
    x_index_start: usize,
) -> LeftStartAndStep {
    let span = x_percentage_limits.span();
    if span == 0.0 || chart_data.x_percentage.is_empty() {
        return LeftStartAndStep {
            start: rect.x,
            step: rect.width,
        };
    }
    let full_width = rect.width / span;
    let offset = full_width * x_percentage_limits.min;
    let x1 = chart_data.x_percentage.get(1).copied().unwrap_or(1.0);
    let p = if chart_data.x_percentage.len() < 2 {
        1.0
    } else {
        x1 * full_width
    };
    let w = x1 * (full_width - p);
    let x_start_pct = chart_data
        .x_percentage
        .get(x_index_start)
        .copied()
        .unwrap_or(0.0);
    let left_start = rect.x + x_start_pct * (full_width - p) - offset;
    LeftStartAndStep {
        start: left_start,
        step: w,
    }
}

/// Maps normalized footer percentages inside a local-zoom window back to chart X indices.
///
/// Port of `FindStackXIndicesFromRawXPercentages` (`stack_chart_common.cpp:34-85`).
#[must_use]
pub fn find_stack_x_indices_from_raw_x_percentages(
    chart_data: &StatisticalChart,
    raw_x_percentage_limits: Limits,
    zoomed_in_limit_x_indices: Limits,
) -> Limits {
    let n = chart_data.x_percentage.len();
    if n == 0 {
        return Limits::default();
    }
    let min_idx = (zoomed_in_limit_x_indices.min as usize).min(n - 1);
    let max_idx = (zoomed_in_limit_x_indices.max as usize).min(n - 1);
    let zoom_limit = Limits::new(
        chart_data.x_percentage[min_idx],
        chart_data.x_percentage[max_idx],
    );

    let offset: isize = if zoom_limit.max == 1.0 { 0 } else { -1 };
    let right_shrink: f64 = if raw_x_percentage_limits.max == 1.0 {
        if zoom_limit.max == 1.0 { 0.0 } else { 1.0 }
    } else {
        0.0
    };

    let zoomed_in = Limits::new(
        interpolate_f(zoom_limit.min, zoom_limit.max, raw_x_percentage_limits.min),
        interpolate_f(zoom_limit.min, zoom_limit.max, raw_x_percentage_limits.max),
    );

    let mut min_it: isize = -1;
    let mut max_it: isize = n as isize;
    for (i, &xp) in chart_data.x_percentage.iter().enumerate() {
        if min_it < 0 && xp > zoomed_in.min {
            min_it = i as isize;
        }
        if max_it >= n as isize && xp > zoomed_in.max {
            max_it = i as isize;
        }
    }

    let upper = (zoomed_in_limit_x_indices.max - right_shrink).max(zoomed_in_limit_x_indices.min);
    Limits::new(
        ((min_it + offset) as f64).clamp(zoomed_in_limit_x_indices.min, upper),
        ((max_it + offset) as f64).clamp(zoomed_in_limit_x_indices.min, upper),
    )
}

/// Unified polymorphic chart view enum (`CreateChartView` factory port).
#[derive(Debug, Clone)]
pub enum ChartView {
    /// Single-axis or dual-axis linear chart.
    Linear(LinearChartView),
    /// Step-line bar chart or stacked vertical bar chart.
    Bar(BarChartView),
    /// Percentage stacked area chart with local zoom into pie chart.
    StackLinear(StackLinearChartView),
}

impl ChartView {
    /// Instantiates the appropriate chart view for `view_type`.
    ///
    /// Port of `Statistic::CreateChartView` (`chart_view_factory.cpp`).
    #[must_use]
    pub fn create(view_type: ChartViewType) -> Self {
        match view_type {
            ChartViewType::Linear => Self::Linear(LinearChartView::new(false)),
            ChartViewType::DoubleLinear => Self::Linear(LinearChartView::new(true)),
            ChartViewType::Bar => Self::Bar(BarChartView::new(false)),
            ChartViewType::StackBar => Self::Bar(BarChartView::new(true)),
            ChartViewType::StackLinear => Self::StackLinear(StackLinearChartView::new()),
        }
    }

    /// Computes vertical height limits for `x_indices`.
    #[must_use]
    pub fn height_limits(
        &mut self,
        chart_data: &StatisticalChart,
        lines_filter: &LinesFilterController,
        x_indices: Limits,
    ) -> HeightLimits {
        match self {
            Self::Linear(v) => v.height_limits(chart_data, lines_filter, x_indices),
            Self::Bar(v) => v.height_limits(chart_data, lines_filter, x_indices),
            Self::StackLinear(v) => v.height_limits(),
        }
    }

    /// Finds the nearest X index for horizontal pointer coordinate `x` in `rect`.
    #[must_use]
    pub fn find_x_index_by_position(
        &mut self,
        chart_data: &StatisticalChart,
        x_percentage_limits: Limits,
        rect: RectF,
        x: f64,
    ) -> Option<usize> {
        match self {
            Self::Linear(v) => v.find_x_index_by_position(chart_data, x_percentage_limits, rect, x),
            Self::Bar(v) => v.find_x_index_by_position(chart_data, x_percentage_limits, rect, x),
            Self::StackLinear(v) => {
                v.find_x_index_by_position(chart_data, x_percentage_limits, rect, x)
            }
        }
    }

    /// Executes local zoom operations (supported by [`ChartView::StackLinear`]).
    pub fn maybe_local_zoom(&mut self, args: LocalZoomArgs<'_>) -> LocalZoomResult {
        match self {
            Self::StackLinear(v) => v.maybe_local_zoom(args),
            _ => LocalZoomResult::default(),
        }
    }
}
