//! Chart animation controller and remote/local zoom session state.
//!
//! Port of `ChartWidget::ChartAnimationController`, `ChartWidget::setZoomedChartData`,
//! and `ChartWidget::processLocalZoom` in `Telegram/SourceFiles/statistics/chart_widget.cpp`.

use crate::anim::{AnimValue, ease_in_cubic, linear};
use crate::chart::{ChartViewType, Limits, StatisticalChart};
use crate::filter::LinesFilterController;
use crate::format::header_subtitle;
use crate::rulers::{BottomCaptionLineData, ChartRulersView};
use crate::views::ChartView;

/// Adaptive height animation speed constants from `chart_widget.cpp:599-605`.
const DT_HEIGHT_SPEED_1: f64 = 0.03 * 2.0;
const DT_HEIGHT_SPEED_2: f64 = 0.03 * 2.0;
const DT_HEIGHT_SPEED_3: f64 = 0.045 * 2.0;
const DT_HEIGHT_SPEED_FILTER: f64 = DT_HEIGHT_SPEED_1 / 1.2;
const DT_HEIGHT_SPEED_THRESHOLD_1: f64 = 0.7;
const DT_HEIGHT_SPEED_THRESHOLD_2: f64 = 0.1;
const DT_HEIGHT_INSTANT_THRESHOLD: f64 = 0.97;

/// X-range and bottom-caption alpha transition duration in milliseconds.
const X_EXPANDING_DURATION_MS: f64 = 200.0;
const ALPHA_EXPANDING_DURATION_MS: f64 = 200.0;

/// Target FPS assumed for per-frame speed scaling (`kIdealFPS`).
const IDEAL_FPS: f64 = 60.0;
/// Threshold below which animation steps double to reduce low-FPS lag (`kAcceptableFPS`).
const ACCEPTABLE_FPS: f64 = 30.0;

/// Drives smooth X-range, Y-axis height, footer height, ruler alpha, and line filter animations.
///
/// Port of `ChartWidget::ChartAnimationController` (`chart_widget.cpp:526-826`).
#[derive(Debug, Clone, Default)]
pub struct ChartAnimationController {
    animating: bool,
    animation_started_at: i64,
    last_user_interacted: i64,
    bottom_line_alpha_started_at: i64,

    anim_x_min: AnimValue,
    anim_x_max: AnimValue,
    anim_height_min: AnimValue,
    anim_height_max: AnimValue,
    anim_footer_height_min: AnimValue,
    anim_footer_height_max: AnimValue,
    anim_height_alpha: AnimValue,
    anim_bottom_line_alpha: AnimValue,

    final_height_limits: Limits,
    current_x_indices: Limits,

    dt_height_speed: f64,
    dt_height_current: Limits,
    dt_height_current_alpha: f64,
    previous_full_height_limits: Limits,

    last_ticked_at: i64,
    last_fps_slow: bool,
}

impl ChartAnimationController {
    /// Creates a new idle animation controller.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the target X percentage range and recomputes Y height limits & animation targets.
    ///
    /// Returns `true` if a new ruler layer should be pushed to [`ChartRulersView`].
    ///
    /// Port of `ChartAnimationController::setXPercentageLimits`.
    pub fn set_x_percentage_limits(
        &mut self,
        chart_data: &StatisticalChart,
        x_percentage_limits: Limits,
        chart_view: &mut ChartView,
        lines_filter: &LinesFilterController,
        now_ms: i64,
    ) -> bool {
        if self.anim_x_min.to() == x_percentage_limits.min
            && self.anim_x_max.to() == x_percentage_limits.max
            && lines_filter.is_finished()
        {
            return false;
        }
        self.start(now_ms);
        self.anim_x_min.start(x_percentage_limits.min);
        self.anim_x_max.start(x_percentage_limits.max);
        self.last_user_interacted = now_ms;

        let start_x_index = chart_data.find_start_index(self.anim_x_min.to());
        let end_x_index = chart_data.find_end_index(start_x_index, self.anim_x_max.to());
        self.current_x_indices = Limits::new(start_x_index as f64, end_x_index as f64);

        let height_limits =
            chart_view.height_limits(chart_data, lines_filter, self.current_x_indices);
        if height_limits.ranged.min == height_limits.ranged.max {
            return false;
        }
        self.previous_full_height_limits = self.final_height_limits;
        self.final_height_limits = height_limits.ranged;
        if self.previous_full_height_limits.max == 0.0 {
            self.previous_full_height_limits = self.final_height_limits;
        }

        if !lines_filter.is_finished() {
            self.anim_footer_height_min = AnimValue::new(
                self.anim_footer_height_min.current(),
                height_limits.full.min,
            );
            self.anim_footer_height_max = AnimValue::new(
                self.anim_footer_height_max.current(),
                height_limits.full.max,
            );
        } else if self.anim_footer_height_max.to() == 0.0 {
            self.anim_footer_height_min = AnimValue::new(0.0, height_limits.full.min);
            self.anim_footer_height_max = AnimValue::new(0.0, height_limits.full.max);
        }

        self.anim_height_min =
            AnimValue::new(self.anim_height_min.current(), self.final_height_limits.min);
        self.anim_height_max =
            AnimValue::new(self.anim_height_max.current(), self.final_height_limits.max);

        let prev_delta = self.previous_full_height_limits.span();
        let next_delta = self.final_height_limits.span();
        let mut k = if next_delta != 0.0 {
            prev_delta / next_delta
        } else {
            1.0
        };
        if k > 1.0 {
            k = 1.0 / k;
        }

        let mut add_ruler = false;
        if k < 1.0 {
            let start_alpha = if self.anim_height_alpha.is_finished() {
                0.0
            } else {
                self.anim_height_alpha.current()
            };
            self.anim_height_alpha = AnimValue::new(start_alpha, 1.0);
            self.dt_height_current_alpha = 0.0;
            add_ruler = true;
        }

        self.dt_height_speed = if !lines_filter.is_finished() {
            DT_HEIGHT_SPEED_FILTER
        } else if k > DT_HEIGHT_SPEED_THRESHOLD_1 {
            DT_HEIGHT_SPEED_1
        } else if k < DT_HEIGHT_SPEED_THRESHOLD_2 {
            DT_HEIGHT_SPEED_2
        } else {
            DT_HEIGHT_SPEED_3
        };
        if k < DT_HEIGHT_INSTANT_THRESHOLD {
            self.dt_height_current = Limits::new(0.0, 0.0);
        }

        add_ruler
    }

    /// Starts the animation loop at `now_ms` if not already animating.
    pub fn start(&mut self, now_ms: i64) {
        if !self.animating {
            self.animating = true;
            self.animation_started_at = now_ms;
        }
    }

    /// Immediately finishes all in-flight animations and jumps to target values.
    pub fn finish(&mut self) {
        self.animating = false;
        self.anim_x_min.finish();
        self.anim_x_max.finish();
        self.anim_height_min.finish();
        self.anim_height_max.finish();
        self.anim_footer_height_min.finish();
        self.anim_footer_height_max.finish();
        self.anim_height_alpha.finish();
        self.anim_bottom_line_alpha.finish();
        self.last_ticked_at = 0;
        self.last_fps_slow = false;
    }

    /// Starts a bottom-caption alpha cross-fade at `now_ms`.
    pub fn restart_bottom_line_alpha(&mut self, now_ms: i64) {
        self.bottom_line_alpha_started_at = now_ms;
        self.anim_bottom_line_alpha = AnimValue::new(0.0, 1.0);
        self.start(now_ms);
    }

    /// Advances all chart animations to timestamp `now_ms`.
    ///
    /// Port of `ChartAnimationController::tick` (`chart_widget.cpp:656-784`).
    pub fn tick(
        &mut self,
        now_ms: i64,
        rulers_view: &mut ChartRulersView,
        date_lines: &mut Vec<BottomCaptionLineData>,
        lines_filter: &mut LinesFilterController,
    ) {
        if !self.animating {
            return;
        }

        let current_fps = if self.last_ticked_at != 0 && now_ms > self.last_ticked_at {
            1000.0 / ((now_ms - self.last_ticked_at) as f64)
        } else {
            IDEAL_FPS
        };
        if !self.last_fps_slow {
            self.last_fps_slow = current_fps < ACCEPTABLE_FPS;
        }
        self.last_ticked_at = now_ms;

        let fps_k = (IDEAL_FPS / current_fps) * if self.last_fps_slow { 2.0 } else { 1.0 };
        let speed = self.dt_height_speed * fps_k;
        lines_filter.tick(speed);
        self.dt_height_current.min = (self.dt_height_current.min + speed).min(1.0);
        self.dt_height_current.max = (self.dt_height_current.max + speed).min(1.0);
        self.dt_height_current_alpha = (self.dt_height_current_alpha + speed).min(1.0);

        let dt_x = (((now_ms - self.animation_started_at).max(0) as f64) / X_EXPANDING_DURATION_MS)
            .min(1.0);
        let dt_bottom = (((now_ms - self.bottom_line_alpha_started_at).max(0) as f64)
            / ALPHA_EXPANDING_DURATION_MS)
            .min(1.0);

        let x_finished = self.anim_x_min.is_finished() && self.anim_x_max.is_finished();
        let y_finished = self.anim_height_min.is_finished() && self.anim_height_max.is_finished();
        let alpha_finished =
            self.anim_height_alpha.is_finished() && self.anim_height_max.is_finished();
        let bottom_finished = self.anim_bottom_line_alpha.is_finished();
        let footer_min_finished = self.anim_footer_height_min.is_finished();
        let footer_max_finished = self.anim_footer_height_max.is_finished();

        if x_finished
            && y_finished
            && alpha_finished
            && bottom_finished
            && footer_min_finished
            && footer_max_finished
            && lines_filter.is_finished()
            && self.final_height_limits.min == self.anim_height_min.to()
            && self.final_height_limits.max == self.anim_height_max.to()
        {
            self.animating = false;
            self.last_ticked_at = 0;
            self.last_fps_slow = false;
        }

        if x_finished {
            self.anim_x_min.finish();
            self.anim_x_max.finish();
        } else {
            self.anim_x_min.update(dt_x, linear);
            self.anim_x_max.update(dt_x, linear);
        }

        if bottom_finished {
            self.anim_bottom_line_alpha.finish();
            self.bottom_line_alpha_started_at = 0;
        } else {
            self.anim_bottom_line_alpha.update(dt_bottom, ease_in_cubic);
        }

        if !y_finished {
            self.anim_height_min
                .update(self.dt_height_current.min, ease_in_cubic);
            self.anim_height_max
                .update(self.dt_height_current.max, ease_in_cubic);
            rulers_view.compute_relative(
                self.anim_height_max.current(),
                self.anim_height_min.current(),
            );
        }

        if !footer_min_finished {
            self.anim_footer_height_min
                .update(self.dt_height_current.min, ease_in_cubic);
        }
        if !footer_max_finished {
            self.anim_footer_height_max
                .update(self.dt_height_current.max, ease_in_cubic);
        }

        if !alpha_finished {
            self.anim_height_alpha
                .update(self.dt_height_current_alpha, ease_in_cubic);
            rulers_view.set_alpha(self.anim_height_alpha.current());
        }

        if !bottom_finished {
            let value = self.anim_bottom_line_alpha.current();
            for date in date_lines.iter_mut() {
                date.alpha = (1.0 - value) * date.fixed_alpha;
            }
            if let Some(last) = date_lines.last_mut() {
                last.alpha = value;
            }
        } else if date_lines.len() > 1
            && let Some(last) = date_lines.pop()
        {
            date_lines.clear();
            date_lines.push(last);
        }
    }

    /// Current animated X percentage limits.
    #[must_use]
    pub const fn current_x_limits(&self) -> Limits {
        Limits::new(self.anim_x_min.current(), self.anim_x_max.current())
    }

    /// Current visible X index limits.
    #[must_use]
    pub const fn current_x_indices(&self) -> Limits {
        self.current_x_indices
    }

    /// Target X percentage limits.
    #[must_use]
    pub fn final_x_limits(&self) -> Limits {
        Limits::new(self.anim_x_min.to(), self.anim_x_max.to())
    }

    /// Current animated main chart vertical limits.
    #[must_use]
    pub const fn current_height_limits(&self) -> Limits {
        Limits::new(
            self.anim_height_min.current(),
            self.anim_height_max.current(),
        )
    }

    /// Current animated footer vertical limits.
    #[must_use]
    pub const fn current_footer_height_limits(&self) -> Limits {
        Limits::new(
            self.anim_footer_height_min.current(),
            self.anim_footer_height_max.current(),
        )
    }

    /// Target main chart vertical limits.
    #[must_use]
    pub const fn final_height_limits(&self) -> Limits {
        self.final_height_limits
    }

    /// Returns `true` if any chart animation is currently active.
    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.animating
    }

    /// Returns `true` if the footer vertical limits are animating.
    #[must_use]
    pub fn is_footer_animating(&self) -> bool {
        !self.anim_footer_height_min.is_finished() || !self.anim_footer_height_max.is_finished()
    }
}

/// Request emitted when the user clicks a point details popup on a zoomable chart.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoomRequest {
    /// Async zoom token from `StatsGraph` (empty for local pie zoom).
    pub zoom_token: String,
    /// X timestamp value (`chart.x[x_index]`) sent to `stats.loadAsyncGraph(token, x)`.
    pub x_timestamp: f64,
    /// X index in the parent chart.
    pub x_index: usize,
}

/// State of a zoomed-in detail chart loaded by `x` (`ChartWidget::setZoomedChartData`).
#[derive(Debug, Clone)]
pub struct ZoomedChartState {
    /// Detail chart loaded for the selected day/timestamp.
    pub chart: StatisticalChart,
    /// View type (inherits parent chart view type).
    pub view_type: ChartViewType,
    /// Header title inherited from the parent chart.
    pub title: String,
    /// Header subtitle formatted for the zoomed parent X timestamp.
    pub subtitle: String,
    /// Parent X timestamp that initiated the zoom.
    pub source_x: f64,
}

impl ZoomedChartState {
    /// Constructs zoomed chart state from a parent chart, its title, the clicked `x` timestamp,
    /// and the freshly loaded detail chart.
    ///
    /// Port of `ChartWidget::setZoomedChartData` (`chart_widget.cpp:1501-1551`).
    #[must_use]
    pub fn from_loaded(
        parent: &StatisticalChart,
        parent_title: &str,
        x: f64,
        zoomed_chart: StatisticalChart,
        view_type: ChartViewType,
    ) -> Self {
        let subtitle = parent
            .x
            .iter()
            .position(|&v| v == x)
            .map(|idx| header_subtitle(&parent.x, parent.week_format, idx, idx))
            .unwrap_or_default();
        Self {
            chart: zoomed_chart,
            view_type,
            title: parent_title.to_string(),
            subtitle,
            source_x: x,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ChartPalette;

    #[test]
    fn animation_controller_transitions_height_and_filter_to_targets() {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000],
                ["y0", 10, 20, 500, 1000],
                ["y1", 5, 10, 20, 30]
            ]
        }"#;
        let chart = StatisticalChart::from_json_str(json).unwrap();
        let mut view = ChartView::create(ChartViewType::Linear);
        let mut filter = LinesFilterController::new();
        let mut rulers = ChartRulersView::new();
        rulers.set_chart_data(&chart, ChartViewType::Linear, &ChartPalette::light());
        let mut dates = Vec::new();
        let mut ctrl = ChartAnimationController::new();

        // Initial settle on full range [0, 1].
        ctrl.set_x_percentage_limits(&chart, Limits::new(0.0, 1.0), &mut view, &filter, 0);
        ctrl.finish();
        rulers.add(ctrl.final_height_limits(), false);
        assert_eq!(ctrl.current_height_limits(), Limits::new(5.0, 1000.0));

        // Zoom into left half [0.0, 0.34] -> indices [0, 1] -> height [5, 20].
        let should_add =
            ctrl.set_x_percentage_limits(&chart, Limits::new(0.0, 0.34), &mut view, &filter, 100);
        assert!(should_add);
        rulers.add(ctrl.final_height_limits(), true);
        assert!(ctrl.is_animating());

        let mut t = 100;
        while ctrl.is_animating() && t < 5000 {
            t += 16;
            ctrl.tick(t, &mut rulers, &mut dates, &mut filter);
        }
        assert!(!ctrl.is_animating());
        assert_eq!(ctrl.current_height_limits(), Limits::new(5.0, 20.0));
        assert_eq!(rulers.rulers().len(), 1);
    }

    #[test]
    fn zoomed_chart_state_computes_subtitle_from_parent_x() {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000],
                ["y0", 10, 20]
            ]
        }"#;
        let parent = StatisticalChart::from_json_str(json).unwrap();
        let detail = StatisticalChart::from_json_str(json).unwrap();
        let zoomed = ZoomedChartState::from_loaded(
            &parent,
            "Growth",
            1_700_006_400_000.0,
            detail,
            ChartViewType::Linear,
        );
        assert_eq!(zoomed.title, "Growth");
        assert_eq!(zoomed.subtitle, "15 Nov 2023");
    }
}
