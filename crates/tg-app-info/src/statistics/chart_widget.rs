//! Interactive `egui`-painted Telegram statistics chart widget.
//!
//! Port origin:
//! - `Telegram/SourceFiles/statistics/chart_widget.{h,cpp}`
//! - `Telegram/SourceFiles/statistics/widgets/chart_header_widget.{h,cpp}`
//! - `Telegram/SourceFiles/statistics/widgets/chart_lines_filter_widget.{h,cpp}`
//! - `Telegram/SourceFiles/statistics/widgets/point_details_widget.{h,cpp}`
//! - `Telegram/SourceFiles/ui/effects/shake_animation.{h,cpp}`

use std::f64::consts::PI;

use egui::epaint::{Mesh, PathShape, Vertex};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_stats::anim::sine_in_out;
use tg_stats::views::bar::BAR_SELECTED_DIM_ALPHA;
use tg_stats::views::stack_linear::PIE_CIRCLE_SIZE_RATIO;
use tg_stats::{
    AnimValue, BottomDatesState, CenterAnimationTarget, ChartAnimationController, ChartPalette,
    ChartRulersView, ChartView, ChartViewType, Color, DragArea, FooterRangeSelector, Limits,
    LinesFilterController, LocalZoomArgs, LocalZoomType, PaintContext, PointDetailsData, PointF,
    RULER_LINE_ALPHA, RectF, StatisticalChart, StatisticalGraph, ZoomedChartState, compute_popup_x,
    ease_out_circ, find_stack_x_indices_from_raw_x_percentages, format_count_decimal,
    header_subtitle, interpolate_f,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::statistics::backend::StatsBackend;
use crate::statistics::{paint_stats_icon, strings};

/// Main chart canvas height in logical pixels (`st::statisticsChartHeight`).
pub const STATISTICS_CHART_HEIGHT: f32 = st::statistics_chart_height.0 as f32;
/// Chart header height when subtitle is visible (`st::statisticsChartHeaderHeight`).
pub const STATISTICS_HEADER_HEIGHT: f32 = st::statistics_chart_header_height.0 as f32;
/// Bottom date caption strip height (`st::statisticsChartBottomCaptionHeight`).
pub const STATISTICS_BOTTOM_CAPTION_HEIGHT: f32 =
    st::statistics_chart_bottom_caption_height.0 as f32;
/// Footer preview height (`st::statisticsChartFooterHeight`).
pub const STATISTICS_FOOTER_HEIGHT: f32 = st::statistics_chart_footer_height.0 as f32;
/// Footer handle width (`st::statisticsChartFooterSideWidth`).
pub const STATISTICS_FOOTER_SIDE_WIDTH: f32 = st::statistics_chart_footer_side_width.0 as f32;
/// Footer handle vertical grip height (`st::statisticsChartFooterArrowHeight`).
pub const STATISTICS_FOOTER_ARROW_HEIGHT: f32 = st::statistics_chart_footer_arrow_height.0 as f32;
/// Stroke width for chart polylines and checkbox borders (`st::statisticsChartLineWidth`).
pub const STATISTICS_LINE_WIDTH: f32 = st::statistics_chart_line_width.0 as f32;
/// Selected point dot radius (`st::statisticsDetailsDotRadius`).
pub const STATISTICS_DOT_RADIUS: f32 = st::statistics_details_dot_radius.0 as f32;
/// Shake animation amplitude (`st::shakeShift`).
pub const SHAKE_SHIFT_PX: f64 = 4.0;
/// Shake and filter-toggle animation duration in ms (`st::shakeDuration`).
pub const SHAKE_DURATION_MS: f64 = 300.0;
/// Details popup fade animation duration in ms (`chart_widget.cpp:1245`).
pub const POPUP_FADE_DURATION_MS: f64 = 200.0;
/// Footer click-to-center animation duration in ms (`st::slideWrapDuration`).
pub const FOOTER_CENTER_DURATION_MS: f64 = 200.0;
/// Local pie-zoom animation duration in ms (`kFooterZoomDuration`).
pub const LOCAL_ZOOM_DURATION_MS: f64 = 400.0;

/// Converts a `tg_stats::Color` to `egui::Color32`.
#[must_use]
pub fn to_color32(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Converts a `tg_stats::Color` to `egui::Color32` with multiplied alpha factor.
#[must_use]
pub fn to_color32_alpha(c: Color, alpha_factor: f64) -> Color32 {
    to_color32(c.with_alpha_factor(alpha_factor))
}

/// Linearly interpolates two `Color32` values in sRGBA space (`anim::color`).
#[must_use]
pub fn lerp_color32(from: Color32, to: Color32, t: f64) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| -> u8 {
        (f64::from(a) + (f64::from(b) - f64::from(a)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
        mix(from.a(), to.a()),
    )
}

/// Exact port of `Ui::DefaultShakeCallback` (`ui/effects/shake_animation.cpp`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ShakeAnimation {
    started_at_ms: Option<i64>,
    current_shift: f32,
}

impl ShakeAnimation {
    /// Starts a horizontal shake at `now_ms` if not already running.
    pub fn start(&mut self, now_ms: i64) {
        if self.started_at_ms.is_none() {
            self.started_at_ms = Some(now_ms);
        }
    }

    /// Returns whether the shake is currently animating.
    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.started_at_ms.is_some()
    }

    /// Current horizontal pixel shift.
    #[must_use]
    pub const fn shift_px(&self) -> f32 {
        self.current_shift
    }

    /// Evaluates the 6-segment tdesktop shake waveform at normalized `progress` in `0.0..=1.0`.
    #[must_use]
    pub fn evaluate_shift(progress: f64) -> f64 {
        const SHIFT_PROGRESS: f64 = 6.0;
        const SEGMENTS_COUNT: i32 = 5;
        let p = progress.clamp(0.0, 1.0);
        let full_progress = p * SHIFT_PROGRESS;
        let segment = (full_progress.floor() as i32).clamp(0, SEGMENTS_COUNT);
        let part = full_progress - f64::from(segment);
        let from = match segment {
            0 => 0.0,
            1 | 3 | 5 => 1.0,
            _ => -1.0,
        };
        let to = match segment {
            0 | 2 | 4 => 1.0,
            1 | 3 => -1.0,
            _ => 0.0,
        };
        let shift = from * (1.0 - part) + to * part;
        (shift * SHAKE_SHIFT_PX).round()
    }

    /// Advances the shake animation to `now_ms`, returning `true` while still active.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        let Some(started) = self.started_at_ms else {
            self.current_shift = 0.0;
            return false;
        };
        let progress = ((now_ms - started).max(0) as f64) / SHAKE_DURATION_MS;
        if progress >= 1.0 {
            self.started_at_ms = None;
            self.current_shift = 0.0;
            false
        } else {
            self.current_shift = Self::evaluate_shift(progress) as f32;
            true
        }
    }
}

/// Interactive state for one filter pill chip (`ChartLinesFilterWidget::FlatCheckbox`).
#[derive(Debug, Clone)]
pub struct FilterChipState {
    /// Series line ID (`ChartLine::id`).
    pub id: i32,
    /// Display name.
    pub name: String,
    /// Active series color.
    pub color: Color,
    /// Logical checked state.
    pub checked: bool,
    /// Animated check progress (`0.0` unchecked .. `1.0` checked).
    anim: AnimValue,
    anim_started_at_ms: Option<i64>,
    /// Horizontal refusal shake when attempting to uncheck the last active chip.
    pub shake: ShakeAnimation,
}

impl FilterChipState {
    /// Creates a new filter chip with initial `checked` state.
    #[must_use]
    pub fn new(id: i32, name: String, color: Color, checked: bool) -> Self {
        let initial = if checked { 1.0 } else { 0.0 };
        Self {
            id,
            name,
            color,
            checked,
            anim: AnimValue::constant(initial),
            anim_started_at_ms: None,
            shake: ShakeAnimation::default(),
        }
    }

    /// Starts a toggle transition to `checked` at `now_ms`.
    pub fn set_checked(&mut self, checked: bool, animated: bool, now_ms: i64) {
        if self.checked == checked {
            return;
        }
        self.checked = checked;
        let target = if checked { 1.0 } else { 0.0 };
        if animated {
            self.anim.start(target);
            self.anim_started_at_ms = Some(now_ms);
        } else {
            self.anim = AnimValue::constant(target);
            self.anim_started_at_ms = None;
        }
    }

    /// Current animated progress in `0.0..=1.0`.
    #[must_use]
    pub fn progress(&self) -> f64 {
        self.anim.current().clamp(0.0, 1.0)
    }

    /// Advances toggle and shake animations to `now_ms`. Returns `true` if still animating.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        let mut active = self.shake.tick(now_ms);
        if let Some(started) = self.anim_started_at_ms {
            let dt = (((now_ms - started).max(0) as f64) / SHAKE_DURATION_MS).clamp(0.0, 1.0);
            self.anim.update(dt, tg_stats::linear);
            if dt >= 1.0 {
                self.anim.finish();
                self.anim_started_at_ms = None;
            } else {
                active = true;
            }
        }
        active
    }
}

/// Fade & selection state for the floating point details popup.
#[derive(Debug, Clone, Default)]
pub struct PopupAnimState {
    /// Selected X index in the chart.
    pub selected_x_index: Option<usize>,
    /// Cached formatted details for `selected_x_index`.
    pub details: Option<PointDetailsData>,
    /// Current opacity in `0.0..=1.0`.
    pub alpha: f64,
    /// `true` when fading out toward dismissal.
    hiding: bool,
    started_at_ms: Option<i64>,
}

impl PopupAnimState {
    /// Selects `x_index` and fades the popup in (or updates content immediately if visible).
    pub fn show_at(
        &mut self,
        chart: &StatisticalChart,
        x_index: usize,
        zoom_enabled: bool,
        filter: &LinesFilterController,
        now_ms: i64,
    ) {
        let was_hidden = self.selected_x_index.is_none() || self.hiding;
        self.selected_x_index = Some(x_index);
        self.details = PointDetailsData::build(chart, x_index, zoom_enabled, Some(filter));
        self.hiding = false;
        if was_hidden {
            self.started_at_ms = Some(now_ms);
            if self.alpha <= 0.0 {
                self.alpha = 0.01;
            }
        } else {
            self.alpha = 1.0;
            self.started_at_ms = None;
        }
    }

    /// Immediately populates popup state at full opacity (useful for tests & screenshots).
    pub fn set_pinned(
        &mut self,
        chart: &StatisticalChart,
        x_index: usize,
        zoom_enabled: bool,
        filter: &LinesFilterController,
    ) {
        self.selected_x_index = Some(x_index);
        self.details = PointDetailsData::build(chart, x_index, zoom_enabled, Some(filter));
        self.hiding = false;
        self.alpha = 1.0;
        self.started_at_ms = None;
    }

    /// Starts fading the popup out at `now_ms`.
    pub fn start_hide(&mut self, now_ms: i64) {
        if self.selected_x_index.is_some() && !self.hiding {
            self.hiding = true;
            self.started_at_ms = Some(now_ms);
        }
    }

    /// Immediately clears selection and hides the popup.
    pub fn clear(&mut self) {
        self.selected_x_index = None;
        self.details = None;
        self.alpha = 0.0;
        self.hiding = false;
        self.started_at_ms = None;
    }

    /// Advances fade animation. Returns `true` if still animating.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        let Some(started) = self.started_at_ms else {
            return false;
        };
        let dt = (((now_ms - started).max(0) as f64) / POPUP_FADE_DURATION_MS).clamp(0.0, 1.0);
        self.alpha = if self.hiding { 1.0 - dt } else { dt };
        if let Some(ref mut d) = self.details {
            d.alpha = self.alpha;
        }
        if dt >= 1.0 {
            self.started_at_ms = None;
            if self.hiding {
                self.clear();
            }
            false
        } else {
            true
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FooterCenterAnim {
    target: CenterAnimationTarget,
    started_at_ms: i64,
}

#[derive(Debug, Clone, Copy)]
struct LocalZoomAnim {
    zooming_in: bool,
    started_at_ms: i64,
    x_index: usize,
    was_footer_limits: Limits,
    exit_start_limits: Limits,
    limit_indices: Limits,
    settled_in_pie: bool,
}

/// Remote zoomed-in detail sub-chart (`ChartWidget::_zoomedChartWidget`).
#[derive(Debug, Clone)]
pub struct ZoomedSubChart {
    /// Detail state including subtitle formatted from parent X.
    pub meta: ZoomedChartState,
    /// Interactive child chart widget for the zoomed detail data.
    pub widget: ChartWidgetState,
}

/// Complete interactive state of a Telegram statistics chart widget (`Statistic::ChartWidget`).
#[derive(Debug, Clone)]
pub struct ChartWidgetState {
    /// Header title (e.g. `"Followers"`, `"Views by source"`).
    pub title: String,
    /// Current subtitle (date range or zoomed day).
    pub subtitle: String,
    /// Parsed chart data.
    pub chart: StatisticalChart,
    /// Chart rendering type.
    pub view_type: ChartViewType,
    /// Async zoom token (`""` if not remotely zoomable).
    pub zoom_token: String,
    /// Polymorphic view math state.
    pub view: ChartView,
    /// 60 fps X/Y/footer/ruler/filter animation controller.
    pub anim: ChartAnimationController,
    /// Stacked cross-fading Y-axis rulers.
    pub rulers: ChartRulersView,
    /// Bottom horizontal date caption state.
    pub bottom_dates: BottomDatesState,
    /// Footer draggable range selector.
    pub footer: FooterRangeSelector,
    /// Per-line visibility filter and alpha transitions.
    pub filter: LinesFilterController,
    /// Interactive filter pill chips.
    pub chips: Vec<FilterChipState>,
    /// Floating point details popup state.
    pub popup: PopupAnimState,
    /// Active click-to-center footer pan animation.
    footer_center_anim: Option<FooterCenterAnim>,
    /// Active or settled local pie zoom state (for `StackLinear`).
    local_zoom: Option<LocalZoomAnim>,
    /// Active remote day-zoom sub-chart.
    remote_zoomed: Option<Box<ZoomedSubChart>>,
    /// Non-fatal error banner from a failed zoom request.
    pub zoom_error: Option<String>,
    /// Last measured widget width (for resize tracking).
    last_width: f32,
    /// Cached chart rect from the last paint pass.
    last_chart_rect: RectF,
    /// Hovered pie slice line ID when locally zoomed into Pie mode.
    hovered_pie_line_id: Option<i32>,
}

impl ChartWidgetState {
    /// Initializes a chart widget from a [`StatisticalGraph`] and [`ChartViewType`].
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        graph: StatisticalGraph,
        view_type: ChartViewType,
        palette: &ChartPalette,
    ) -> Self {
        let mut chart = graph.chart;
        chart.apply_palette(palette);

        let mut view = ChartView::create(view_type);
        let mut filter = LinesFilterController::new();
        let mut chips = Vec::with_capacity(chart.lines.len());
        for line in &chart.lines {
            if line.is_hidden_on_start {
                filter.set_enabled(line.id, false, 1);
            }
            chips.push(FilterChipState::new(
                line.id,
                line.name.clone(),
                line.color,
                !line.is_hidden_on_start,
            ));
        }
        filter.tick(1.0);

        let default_zoom = chart.default_zoom_x_percentage();
        let default_zoom = if default_zoom.span() <= 0.0 {
            Limits::new(0.0, 1.0)
        } else {
            default_zoom
        };

        let footer = FooterRangeSelector::with_width(480.0, default_zoom);
        let mut anim = ChartAnimationController::new();
        let mut rulers = ChartRulersView::new();
        let mut bottom_dates = BottomDatesState::default();
        let subtitle = if !chart.is_empty() && !chart.x.is_empty() {
            anim.set_x_percentage_limits(&chart, default_zoom, &mut view, &filter, 0);
            anim.finish();

            rulers.set_chart_data(&chart, view_type, palette);
            rulers.add(anim.final_height_limits(), false);

            bottom_dates.update_chart_full_width(480, default_zoom);
            bottom_dates.update(&chart, 480);

            let indices = anim.current_x_indices();
            header_subtitle(
                &chart.x,
                chart.week_format,
                indices.min as usize,
                indices.max as usize,
            )
        } else {
            String::new()
        };

        Self {
            title: title.into(),
            subtitle,
            chart,
            view_type,
            zoom_token: graph.zoom_token,
            view,
            anim,
            rulers,
            bottom_dates,
            footer,
            filter,
            chips,
            popup: PopupAnimState::default(),
            footer_center_anim: None,
            local_zoom: None,
            remote_zoomed: None,
            zoom_error: if graph.error.is_empty() {
                None
            } else {
                Some(graph.error)
            },
            last_width: 480.0,
            last_chart_rect: RectF::new(0.0, 0.0, 480.0, f64::from(STATISTICS_CHART_HEIGHT)),
            hovered_pie_line_id: None,
        }
    }

    /// Re-applies theme colors from `palette` (e.g. on Day/Night switch).
    pub fn apply_palette(&mut self, palette: &ChartPalette) {
        self.chart.apply_palette(palette);
        for (chip, line) in self.chips.iter_mut().zip(self.chart.lines.iter()) {
            chip.color = line.color;
        }
        let limits = self.anim.final_height_limits();
        self.rulers
            .set_chart_data(&self.chart, self.view_type, palette);
        self.rulers.add(limits, false);
        if let Some(ref mut zoomed) = self.remote_zoomed {
            zoomed.widget.apply_palette(palette);
        }
    }

    /// Returns `true` if this chart supports either local pie zoom or remote day zoom.
    #[must_use]
    pub fn is_zoomable(&self) -> bool {
        self.has_local_zoom() || !self.zoom_token.is_empty()
    }

    /// Returns `true` when this chart supports local zoom into pie mode (`StackLinear`).
    #[must_use]
    pub fn has_local_zoom(&self) -> bool {
        matches!(self.view, ChartView::StackLinear(_)) && !self.chart.is_footer_hidden
    }

    /// Returns `true` if currently displaying a remote zoomed day chart or local pie zoom.
    #[must_use]
    pub fn is_zoomed(&self) -> bool {
        self.remote_zoomed.is_some() || self.local_zoom.is_some()
    }

    /// Returns a reference to the active remote zoomed sub-chart, if any.
    #[must_use]
    pub fn remote_zoomed(&self) -> Option<&ZoomedSubChart> {
        self.remote_zoomed.as_deref()
    }

    /// Returns a mutable reference to the active remote zoomed sub-chart, if any.
    pub fn remote_zoomed_mut(&mut self) -> Option<&mut ZoomedSubChart> {
        self.remote_zoomed.as_deref_mut()
    }

    /// Sets the hovered pie slice line ID (used when locally zoomed into Pie mode).
    pub fn set_hovered_pie_line(&mut self, line_id: Option<i32>, now_ms: f64) {
        self.hovered_pie_line_id = line_id;
        if let ChartView::StackLinear(ref mut sv) = self.view {
            sv.pie_part_controller.set(line_id, now_ms);
        }
    }

    /// Returns current local zoom progress (`0.0` area .. `1.0` pie).
    #[must_use]
    pub fn local_zoom_progress(&self) -> f64 {
        match &self.view {
            ChartView::StackLinear(sv) => sv.zoom_progress(),
            _ => 0.0,
        }
    }

    /// Toggles line `line_id` visibility. Returns `false` and starts chip shake if it is the last enabled line.
    pub fn toggle_line(&mut self, line_id: i32, now_ms: i64) -> bool {
        let all_ids: Vec<i32> = self.chart.lines.iter().map(|l| l.id).collect();
        let effective_now = now_ms.max(1);
        if !self.filter.try_toggle(line_id, &all_ids, effective_now) {
            if let Some(chip) = self.chips.iter_mut().find(|c| c.id == line_id) {
                chip.shake.start(effective_now);
            }
            return false;
        }
        let enabled = self.filter.is_enabled(line_id);
        if let Some(chip) = self.chips.iter_mut().find(|c| c.id == line_id) {
            chip.set_checked(enabled, true, effective_now);
        }
        let add_ruler = self.anim.set_x_percentage_limits(
            &self.chart,
            self.anim.current_x_limits(),
            &mut self.view,
            &self.filter,
            effective_now,
        );
        if add_ruler {
            self.rulers.add(self.anim.final_height_limits(), true);
        }
        if let Some(sel) = self.popup.selected_x_index {
            self.popup.details =
                PointDetailsData::build(&self.chart, sel, self.is_zoomable(), Some(&self.filter));
        }
        true
    }

    /// Updates the visible X percentage range from the footer selector.
    pub fn apply_x_percentage_limits(&mut self, limits: Limits, now_ms: i64) {
        if let Some(lz) = self.local_zoom
            && lz.settled_in_pie
        {
            let range_idx =
                find_stack_x_indices_from_raw_x_percentages(&self.chart, limits, lz.limit_indices);
            self.subtitle = header_subtitle(
                &self.chart.x,
                self.chart.week_format,
                range_idx.min as usize,
                range_idx.max as usize,
            );
            return;
        }

        let add_ruler = self.anim.set_x_percentage_limits(
            &self.chart,
            limits,
            &mut self.view,
            &self.filter,
            now_ms,
        );
        if add_ruler {
            self.rulers.add(self.anim.final_height_limits(), true);
        }
        let w = self.last_width.max(1.0) as i32;
        self.bottom_dates.update_chart_full_width(w, limits);
        if self.bottom_dates.update(&self.chart, w) {
            self.anim.restart_bottom_line_alpha(now_ms);
        }
        let indices = self.anim.current_x_indices();
        self.subtitle = header_subtitle(
            &self.chart.x,
            self.chart.week_format,
            indices.min as usize,
            indices.max as usize,
        );
        self.popup.clear();
    }

    /// Triggers either local pie zoom (`StackLinear`) or remote day zoom through `backend`.
    pub fn trigger_zoom(
        &mut self,
        x_index: usize,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
    ) -> Result<(), String> {
        if x_index >= self.chart.x.len() {
            return Err(strings::ZOOM_INDEX_OUT_OF_BOUNDS.to_owned());
        }
        self.popup.clear();
        self.zoom_error = None;

        if self.has_local_zoom() {
            let was_footer = self.footer.x_percentage_limits();
            let _ = self.view.maybe_local_zoom(LocalZoomArgs {
                chart_data: &self.chart,
                kind: LocalZoomType::Prepare,
                progress: 0.0,
                x_index,
            });
            let res = self.view.maybe_local_zoom(LocalZoomArgs {
                chart_data: &self.chart,
                kind: LocalZoomType::Process,
                progress: 0.0,
                x_index,
            });
            self.local_zoom = Some(LocalZoomAnim {
                zooming_in: true,
                started_at_ms: now_ms,
                x_index,
                was_footer_limits: was_footer,
                exit_start_limits: was_footer,
                limit_indices: res.limit_indices,
                settled_in_pie: false,
            });
            self.subtitle =
                header_subtitle(&self.chart.x, self.chart.week_format, x_index, x_index);
            return Ok(());
        }

        if self.zoom_token.is_empty() {
            return Err(strings::ZOOM_NO_TOKEN.to_owned());
        }
        let x_ts = self.chart.x[x_index];
        let graph = backend.load_zoom_graph(&self.zoom_token, x_ts)?;
        if !graph.error.is_empty() {
            self.zoom_error = Some(graph.error.clone());
            return Err(graph.error);
        }
        if graph.chart.is_empty() {
            let msg = strings::ZOOM_EMPTY_CHART.to_owned();
            self.zoom_error = Some(msg.clone());
            return Err(msg);
        }

        let meta = ZoomedChartState::from_loaded(
            &self.chart,
            &self.title,
            x_ts,
            graph.chart.clone(),
            self.view_type,
        );
        let mut child = ChartWidgetState::new(
            meta.title.clone(),
            StatisticalGraph {
                chart: graph.chart,
                zoom_token: String::new(),
                error: String::new(),
            },
            self.view_type,
            palette,
        );
        child.subtitle = meta.subtitle.clone();
        self.remote_zoomed = Some(Box::new(ZoomedSubChart {
            meta,
            widget: child,
        }));
        Ok(())
    }

    /// Exits remote zoom or animates back from local pie zoom.
    pub fn zoom_out(&mut self, now_ms: i64) {
        if self.remote_zoomed.is_some() {
            self.remote_zoomed = None;
            return;
        }
        if let Some(ref mut lz) = self.local_zoom {
            lz.zooming_in = false;
            lz.settled_in_pie = false;
            lz.started_at_ms = now_ms;
            lz.exit_start_limits = self.footer.x_percentage_limits();
            self.hovered_pie_line_id = None;
        }
    }

    /// Advances all animations to `now_ms`. Returns `true` if another 60 fps frame is needed.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        if let Some(ref mut zoomed) = self.remote_zoomed {
            return zoomed.widget.tick(now_ms);
        }

        let mut animating = false;

        if self.anim.is_animating() {
            self.anim.tick(
                now_ms,
                &mut self.rulers,
                &mut self.bottom_dates.dates,
                &mut self.filter,
            );
            animating |= self.anim.is_animating();
        }

        for chip in &mut self.chips {
            animating |= chip.tick(now_ms);
        }

        if let Some(sel) = self.popup.selected_x_index
            && !self.filter.is_finished()
        {
            self.popup.details =
                PointDetailsData::build(&self.chart, sel, self.is_zoomable(), Some(&self.filter));
        }
        animating |= self.popup.tick(now_ms);

        // Footer click-to-center animation (`sine_in_out` over 200ms).
        if let Some(center_anim) = self.footer_center_anim {
            let dt = (((now_ms - center_anim.started_at_ms).max(0) as f64)
                / FOOTER_CENTER_DURATION_MS)
                .clamp(0.0, 1.0);
            let value = center_anim.target.start_x
                + sine_in_out(center_anim.target.finish_x - center_anim.target.start_x, dt);
            self.footer.move_center(
                center_anim.target.to_left,
                value,
                center_anim.target.diff_between_start_and_left,
            );
            let new_limits = self.footer.x_percentage_limits();
            self.apply_x_percentage_limits(new_limits, now_ms);
            if dt >= 1.0 {
                self.footer_center_anim = None;
            } else {
                animating = true;
            }
        }

        // Local pie-zoom animation (`ease_out_circ` over 400ms).
        if let Some(mut lz) = self.local_zoom {
            if !lz.settled_in_pie {
                let raw_dt = (((now_ms - lz.started_at_ms).max(0) as f64) / LOCAL_ZOOM_DURATION_MS)
                    .clamp(0.0, 1.0);
                if lz.zooming_in {
                    let eased = ease_out_circ(1.0, raw_dt).clamp(0.0, 1.0);
                    let res = self.view.maybe_local_zoom(LocalZoomArgs {
                        chart_data: &self.chart,
                        kind: LocalZoomType::Process,
                        progress: eased,
                        x_index: lz.x_index,
                    });
                    lz.limit_indices = res.limit_indices;
                    let target_footer = Limits::new(
                        interpolate_f(lz.was_footer_limits.min, res.range.min, eased),
                        interpolate_f(lz.was_footer_limits.max, res.range.max, eased),
                    );
                    self.footer.set_x_percentage_limits(target_footer);
                    if raw_dt >= 1.0 {
                        lz.settled_in_pie = true;
                        self.local_zoom = Some(lz);
                    } else {
                        self.local_zoom = Some(lz);
                        animating = true;
                    }
                } else {
                    let eased = (1.0 - ease_out_circ(1.0, raw_dt)).clamp(0.0, 1.0);
                    let _ = self.view.maybe_local_zoom(LocalZoomArgs {
                        chart_data: &self.chart,
                        kind: LocalZoomType::SkipCalculation,
                        progress: eased,
                        x_index: lz.x_index,
                    });
                    let target_footer = Limits::new(
                        interpolate_f(lz.was_footer_limits.min, lz.exit_start_limits.min, eased),
                        interpolate_f(lz.was_footer_limits.max, lz.exit_start_limits.max, eased),
                    );
                    self.footer.set_x_percentage_limits(target_footer);
                    if raw_dt >= 1.0 {
                        self.local_zoom = None;
                        self.apply_x_percentage_limits(lz.was_footer_limits, now_ms);
                    } else {
                        self.local_zoom = Some(lz);
                        animating = true;
                    }
                }
            } else if let ChartView::StackLinear(ref sv) = self.view
                && !sv.pie_part_controller.is_finished(now_ms as f64)
            {
                animating = true;
            }
        }

        animating
    }

    /// Renders the full interactive chart widget using native `egui` shapes.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
    ) {
        if self.tick(now_ms) {
            ui.ctx().request_repaint();
        }

        // If remotely zoomed into a day detail chart, delegate rendering with a Zoom Out button.
        if self.remote_zoomed.is_some() {
            let mut exit_zoom = false;
            if let Some(ref mut zoomed) = self.remote_zoomed {
                zoomed
                    .widget
                    .show_inner(ui, backend, palette, now_ms, true, &mut exit_zoom);
            }
            if exit_zoom {
                self.zoom_out(now_ms);
                ui.ctx().request_repaint();
            }
            return;
        }

        let is_local_zoomed = self.local_zoom.is_some();
        let mut exit_local_zoom = false;
        self.show_inner(
            ui,
            backend,
            palette,
            now_ms,
            is_local_zoomed,
            &mut exit_local_zoom,
        );
        if exit_local_zoom {
            self.zoom_out(now_ms);
            ui.ctx().request_repaint();
        }
    }

    fn show_inner(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
        show_zoom_out: bool,
        zoom_out_clicked: &mut bool,
    ) {
        let avail_w = ui.available_width().max(200.0);
        if (avail_w - self.last_width).abs() > 1.0 {
            self.last_width = avail_w;
            self.footer.resize(f64::from(avail_w));
            let cur_x = self.anim.current_x_limits();
            self.bottom_dates
                .update_chart_full_width(avail_w as i32, cur_x);
            self.bottom_dates.update(&self.chart, avail_w as i32);
        }

        ui.vertical(|ui| {
            // 1. Header (`ChartWidget::Header` + optional `"Zoom Out"` pill button)
            self.paint_header(ui, avail_w, palette, show_zoom_out, zoom_out_clicked);
            ui.add_space(6.0);

            // 2. Main interactive chart canvas + rulers + bottom captions + details popup
            let zoom_request_idx = self.paint_chart_area(ui, avail_w, palette, now_ms);
            if let Some(x_idx) = zoom_request_idx {
                let _ = self.trigger_zoom(x_idx, backend, palette, now_ms);
                ui.ctx().request_repaint();
            }

            if let Some(ref err) = self.zoom_error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(11.5)
                        .color(to_color32(palette.delta_negative)),
                );
            }

            // 3. Footer range selector with miniature preview & draggable handles
            if !self.chart.is_footer_hidden {
                ui.add_space(8.0);
                self.paint_footer(ui, avail_w, palette, now_ms);
            }

            // 4. Multi-line filter chips with toggle animation & last-line shake
            if self.chips.len() > 1 {
                ui.add_space(10.0);
                self.paint_filter_chips(ui, avail_w, palette, now_ms);
            }
        });
    }

    fn paint_header(
        &self,
        ui: &mut Ui,
        avail_w: f32,
        palette: &ChartPalette,
        show_zoom_out: bool,
        zoom_out_clicked: &mut bool,
    ) {
        let header_h = if self.subtitle.is_empty() {
            22.0
        } else {
            STATISTICS_HEADER_HEIGHT
        };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, header_h), Sense::hover());
        let painter = ui.painter();

        let title_font = font_id_from_style(st::statistics_header_title_text_style.font);
        let dates_font = font_id_from_style(st::statistics_header_dates_text_style.font);
        painter.text(
            Pos2::new(rect.left(), rect.top()),
            Align2::LEFT_TOP,
            &self.title,
            title_font,
            to_color32(palette.window_active_text_fg),
        );
        if !self.subtitle.is_empty() {
            painter.text(
                Pos2::new(rect.left(), rect.bottom() - 14.0),
                Align2::LEFT_TOP,
                &self.subtitle,
                dates_font,
                to_color32(palette.window_sub_text_fg),
            );
        }

        if show_zoom_out {
            let zoom_out_label = tr::lng_stats_zoom_out();
            let btn_w = (zoom_out_label.len() as f32 * 7.2 + 22.0).max(78.0);
            let btn_h = 22.0;
            let btn_rect = Rect::from_min_size(
                Pos2::new(rect.right() - btn_w, rect.center().y - btn_h / 2.0),
                Vec2::new(btn_w, btn_h),
            );
            let resp = ui.interact(
                btn_rect,
                ui.scope_id().with(("zoom_out_btn", &self.title)),
                Sense::click(),
            );
            let hovered = resp.hovered();
            let bg = if hovered {
                to_color32_alpha(palette.window_active_text_fg, 0.16)
            } else {
                to_color32_alpha(palette.window_active_text_fg, 0.08)
            };
            painter.rect_filled(btn_rect, CornerRadius::same(11), bg);
            painter.text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                zoom_out_label,
                FontId::proportional(11.5),
                to_color32(palette.window_active_text_fg),
            );
            if hovered {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                *zoom_out_clicked = true;
            }
        }
    }

    fn paint_chart_area(
        &mut self,
        ui: &mut Ui,
        avail_w: f32,
        palette: &ChartPalette,
        now_ms: i64,
    ) -> Option<usize> {
        let is_pie_mode = self.local_zoom.is_some() && self.local_zoom_progress() > 0.5;
        let total_h = STATISTICS_CHART_HEIGHT
            + if is_pie_mode {
                4.0
            } else {
                STATISTICS_BOTTOM_CAPTION_HEIGHT + 4.0
            };

        let (area_rect, response) =
            ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::click_and_drag());
        let chart_egui_rect =
            Rect::from_min_size(area_rect.min, Vec2::new(avail_w, STATISTICS_CHART_HEIGHT));
        let chart_rect = RectF::new(
            f64::from(chart_egui_rect.left()),
            f64::from(chart_egui_rect.top()),
            f64::from(chart_egui_rect.width()),
            f64::from(chart_egui_rect.height()),
        );
        self.last_chart_rect = chart_rect;

        let x_pct = self.anim.current_x_limits();
        let x_indices = self.anim.current_x_indices();
        let h_limits = self.anim.current_height_limits();
        let paint_ctx = PaintContext {
            chart_data: &self.chart,
            x_indices,
            x_percentage_limits: x_pct,
            height_limits: h_limits,
            rect: chart_rect,
            footer: false,
        };

        // Pre-compute popup rectangle so clicks on the popup take priority over chart drag/hover.
        let popup_rect_opt = if !is_pie_mode {
            self.popup.details.as_ref().map(|details| {
                compute_popup_egui_rect(
                    ui,
                    details,
                    &self.chart,
                    self.anim.final_x_limits(),
                    chart_rect,
                    area_rect,
                )
            })
        } else {
            None
        };

        // Handle pointer interaction on popup or chart area.
        let mut zoom_triggered_idx: Option<usize> = None;
        let pointer_pos = response
            .interact_pointer_pos()
            .or_else(|| response.hover_pos());
        let pointer_in_popup = pointer_pos
            .zip(popup_rect_opt)
            .is_some_and(|(p, pr)| pr.contains(p));

        if pointer_in_popup {
            if let Some(ref details) = self.popup.details
                && details.clickable
            {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                if response.clicked() {
                    zoom_triggered_idx = Some(details.x_index);
                }
            }
        } else if is_pie_mode {
            let pie_pt = pointer_pos
                .filter(|p| chart_egui_rect.contains(*p))
                .map(|p| PointF::new(f64::from(p.x), f64::from(p.y)));
            if let ChartView::StackLinear(ref mut sv) = self.view {
                let hit = sv.handle_pie_mouse_move(&self.chart, chart_rect, pie_pt, now_ms as f64);
                if hit != self.hovered_pie_line_id {
                    self.hovered_pie_line_id = hit;
                    ui.ctx().request_repaint();
                }
            }
        } else if !self.anim.is_animating() {
            if let Some(pos) = pointer_pos
                && chart_egui_rect.contains(pos)
            {
                let nearest = self.view.find_x_index_by_position(
                    &self.chart,
                    self.anim.final_x_limits(),
                    chart_rect,
                    f64::from(pos.x),
                );
                if let Some(idx) = nearest {
                    let was_same = self.popup.selected_x_index == Some(idx);
                    if response.clicked() && was_same {
                        self.popup.start_hide(now_ms);
                    } else if !was_same || self.popup.hiding {
                        self.popup.show_at(
                            &self.chart,
                            idx,
                            self.is_zoomable(),
                            &self.filter,
                            now_ms,
                        );
                    }
                    ui.ctx().request_repaint();
                }
            } else if !response.hovered() && !pointer_in_popup && response.clicked_elsewhere() {
                self.popup.start_hide(now_ms);
            }
        }

        // Paint layers in exact tdesktop Z-order.
        let painter = ui.painter();
        let clipped = painter.with_clip_rect(chart_egui_rect);
        let dot_clipped = painter.with_clip_rect(chart_egui_rect.expand2(Vec2::new(4.0, 8.0)));
        let rulers_above = self.view_type == ChartViewType::StackBar;

        if !is_pie_mode && !rulers_above {
            paint_rulers_egui(&clipped, &self.rulers, chart_rect, palette);
        }

        match self.view_type {
            ChartViewType::Linear | ChartViewType::DoubleLinear => {
                if let ChartView::Linear(ref mut lv) = self.view {
                    paint_linear_egui(
                        &clipped,
                        lv,
                        &paint_ctx,
                        &self.filter,
                        STATISTICS_LINE_WIDTH,
                    );
                    if let Some(sel) = self.popup.selected_x_index
                        && self.popup.alpha > 0.01
                    {
                        paint_linear_selection_egui(
                            &dot_clipped,
                            lv,
                            &paint_ctx,
                            &self.filter,
                            sel,
                            self.popup.alpha,
                            palette,
                        );
                    }
                }
            }
            ChartViewType::Bar | ChartViewType::StackBar => {
                if let ChartView::Bar(ref mut bv) = self.view {
                    bv.set_selected(self.popup.selected_x_index, self.popup.alpha);
                    paint_bar_egui(
                        &clipped,
                        bv,
                        &paint_ctx,
                        &self.filter,
                        self.popup.selected_x_index,
                        self.popup.alpha,
                        palette,
                    );
                }
            }
            ChartViewType::StackLinear => {
                if let ChartView::StackLinear(ref mut sv) = self.view {
                    if is_pie_mode {
                        let pie_indices = self.local_zoom.map_or(x_indices, |lz| {
                            if lz.settled_in_pie {
                                find_stack_x_indices_from_raw_x_percentages(
                                    &self.chart,
                                    self.footer.x_percentage_limits(),
                                    lz.limit_indices,
                                )
                            } else {
                                Limits::new(lz.x_index as f64, lz.x_index as f64)
                            }
                        });
                        paint_pie_egui(
                            &clipped,
                            sv,
                            &self.chart,
                            &self.filter,
                            pie_indices,
                            chart_rect,
                            palette,
                            now_ms as f64,
                            self.hovered_pie_line_id,
                        );
                    } else {
                        paint_stack_linear_egui(&clipped, sv, &paint_ctx, &self.filter);
                        if let Some(sel) = self.popup.selected_x_index
                            && self.popup.alpha > 0.01
                        {
                            paint_vertical_guide_egui(
                                &clipped,
                                &paint_ctx,
                                sel,
                                self.popup.alpha,
                                palette,
                            );
                        }
                    }
                }
            }
        }

        if !is_pie_mode {
            if rulers_above {
                paint_rulers_egui(&clipped, &self.rulers, chart_rect, palette);
            }
            paint_ruler_captions_egui(&clipped, &self.rulers, &self.filter, chart_rect, palette);

            // Bottom baseline hairline
            painter.line_segment(
                [
                    Pos2::new(chart_egui_rect.left(), chart_egui_rect.bottom()),
                    Pos2::new(chart_egui_rect.right(), chart_egui_rect.bottom()),
                ],
                Stroke::new(
                    1.0,
                    to_color32_alpha(palette.box_text_fg, RULER_LINE_ALPHA * 1.5),
                ),
            );

            // Bottom horizontal X-axis date labels
            let labels = self.bottom_dates.compute_labels(
                &self.chart,
                x_pct,
                chart_egui_rect.width() as i32,
            );
            let label_y = chart_egui_rect.bottom() + 4.0;
            for lbl in labels {
                let draw_x = chart_egui_rect.left() + lbl.x_center as f32;
                if draw_x < chart_egui_rect.left() - 12.0 || draw_x > chart_egui_rect.right() + 12.0
                {
                    continue;
                }
                painter.text(
                    Pos2::new(draw_x, label_y),
                    Align2::CENTER_TOP,
                    &lbl.text,
                    FontId::proportional(10.5),
                    to_color32_alpha(palette.window_sub_text_fg, lbl.alpha),
                );
            }

            // Floating Point Details Popup card
            if let (Some(details), Some(popup_rect)) = (&self.popup.details, popup_rect_opt)
                && self.popup.alpha > 0.01
            {
                paint_point_details_popup_egui(
                    painter,
                    details,
                    popup_rect,
                    self.popup.alpha,
                    palette,
                );
            }
        }

        zoom_triggered_idx
    }

    fn paint_footer(&mut self, ui: &mut Ui, avail_w: f32, palette: &ChartPalette, now_ms: i64) {
        let (footer_egui_rect, response) = ui.allocate_exact_size(
            Vec2::new(avail_w, STATISTICS_FOOTER_HEIGHT),
            Sense::click_and_drag(),
        );

        // Pointer handling mapped directly onto `FooterRangeSelector`.
        if let Some(pos) = response.hover_pos() {
            let local_x = f64::from(pos.x - footer_egui_rect.left());
            match self.footer.hit_test(local_x) {
                DragArea::Left | DragArea::Right => {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                DragArea::Middle => {
                    ui.ctx().set_cursor_icon(if response.dragged() {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::Grab
                    });
                }
                DragArea::None => {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
            }
        }

        if response.is_pointer_button_down_on()
            && self.footer_center_anim.is_none()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let local_x = f64::from(pos.x - footer_egui_rect.left());
            if response.drag_started() || ui.input(|i| i.pointer.primary_pressed()) {
                self.footer.on_mouse_press(local_x);
            } else if response.dragged()
                && let Some(new_limits) = self.footer.on_mouse_move(local_x)
            {
                self.apply_x_percentage_limits(new_limits, now_ms);
                ui.ctx().request_repaint();
            }
        }
        if (response.drag_stopped() || response.clicked())
            && let Some(pos) = response
                .interact_pointer_pos()
                .or_else(|| ui.input(|i| i.pointer.latest_pos()))
        {
            let local_x = f64::from(pos.x - footer_egui_rect.left());
            if let Some(target) = self.footer.on_mouse_release(local_x) {
                self.footer_center_anim = Some(FooterCenterAnim {
                    target,
                    started_at_ms: now_ms,
                });
                ui.ctx().request_repaint();
            }
        }

        // Paint miniature full-range chart inside rounded inner rect.
        let painter = ui.painter();
        let inner_egui_rect = footer_egui_rect.shrink2(Vec2::new(0.0, 1.0));
        let inner_rect = RectF::new(
            f64::from(inner_egui_rect.left()),
            f64::from(inner_egui_rect.top()),
            f64::from(inner_egui_rect.width()),
            f64::from(inner_egui_rect.height()),
        );

        painter.rect_filled(
            inner_egui_rect,
            CornerRadius::same(6),
            to_color32(palette.box_bg),
        );

        let full_x_indices = Limits::new(0.0, self.chart.x.len().saturating_sub(1) as f64);
        let footer_h = self.anim.current_footer_height_limits();
        let footer_h = if footer_h.span() <= 0.0 {
            self.anim.current_height_limits()
        } else {
            footer_h
        };
        let footer_ctx = PaintContext {
            chart_data: &self.chart,
            x_indices: full_x_indices,
            x_percentage_limits: Limits::new(0.0, 1.0),
            height_limits: footer_h,
            rect: inner_rect,
            footer: true,
        };

        let clipped = painter.with_clip_rect(inner_egui_rect);
        match self.view_type {
            ChartViewType::Linear | ChartViewType::DoubleLinear => {
                if let ChartView::Linear(ref mut lv) = self.view {
                    paint_linear_egui(&clipped, lv, &footer_ctx, &self.filter, 1.25);
                }
            }
            ChartViewType::Bar | ChartViewType::StackBar => {
                if let ChartView::Bar(ref mut bv) = self.view {
                    paint_bar_egui(&clipped, bv, &footer_ctx, &self.filter, None, 0.0, palette);
                }
            }
            ChartViewType::StackLinear => {
                if let ChartView::StackLinear(ref mut sv) = self.view {
                    paint_stack_linear_egui(&clipped, sv, &footer_ctx, &self.filter);
                }
            }
        }

        // Dimmed inactive left & right curtains.
        let ls = self.footer.left_side();
        let rs = self.footer.right_side();
        let left_min_x = footer_egui_rect.left() + ls.min as f32;
        let left_max_x = footer_egui_rect.left() + ls.max as f32;
        let right_min_x = footer_egui_rect.left() + rs.min as f32;
        let right_max_x = footer_egui_rect.left() + rs.max as f32;
        let inactive_col = to_color32(palette.chart_inactive);
        let active_col = to_color32(palette.chart_active);

        if left_min_x > inner_egui_rect.left() {
            painter.rect_filled(
                Rect::from_min_max(
                    inner_egui_rect.min,
                    Pos2::new(left_min_x + 4.0, inner_egui_rect.bottom()),
                ),
                CornerRadius {
                    nw: 6,
                    sw: 6,
                    ne: 0,
                    se: 0,
                },
                inactive_col,
            );
        }
        if right_max_x < inner_egui_rect.right() {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(right_max_x - 4.0, inner_egui_rect.top()),
                    inner_egui_rect.max,
                ),
                CornerRadius {
                    nw: 0,
                    sw: 0,
                    ne: 6,
                    se: 6,
                },
                inactive_col,
            );
        }

        // Active frame: left handle, right handle, top rail, bottom rail.
        let left_handle_rect = Rect::from_min_max(
            Pos2::new(left_min_x, footer_egui_rect.top()),
            Pos2::new(left_max_x, footer_egui_rect.bottom()),
        );
        let right_handle_rect = Rect::from_min_max(
            Pos2::new(right_min_x, footer_egui_rect.top()),
            Pos2::new(right_max_x, footer_egui_rect.bottom()),
        );
        painter.rect_filled(
            left_handle_rect,
            CornerRadius {
                nw: 6,
                sw: 6,
                ne: 0,
                se: 0,
            },
            active_col,
        );
        painter.rect_filled(
            right_handle_rect,
            CornerRadius {
                nw: 0,
                sw: 0,
                ne: 6,
                se: 6,
            },
            active_col,
        );
        if right_min_x > left_max_x {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(left_max_x, footer_egui_rect.top()),
                    Pos2::new(right_min_x, footer_egui_rect.top() + 2.0),
                ),
                CornerRadius::ZERO,
                active_col,
            );
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(left_max_x, footer_egui_rect.bottom() - 2.0),
                    Pos2::new(right_min_x, footer_egui_rect.bottom()),
                ),
                CornerRadius::ZERO,
                active_col,
            );
        }

        // Centered vertical grip bars inside both handles.
        let grip_half_h = STATISTICS_FOOTER_ARROW_HEIGHT / 2.0;
        let cy = footer_egui_rect.center().y;
        let grip_stroke = Stroke::new(2.0, to_color32(palette.premium_button_fg));
        painter.line_segment(
            [
                Pos2::new(left_handle_rect.center().x, cy - grip_half_h),
                Pos2::new(left_handle_rect.center().x, cy + grip_half_h),
            ],
            grip_stroke,
        );
        painter.line_segment(
            [
                Pos2::new(right_handle_rect.center().x, cy - grip_half_h),
                Pos2::new(right_handle_rect.center().x, cy + grip_half_h),
            ],
            grip_stroke,
        );
    }

    fn paint_filter_chips(
        &mut self,
        ui: &mut Ui,
        avail_w: f32,
        palette: &ChartPalette,
        now_ms: i64,
    ) {
        let mut clicked_id: Option<i32> = None;
        let chip_font = FontId::proportional(12.0);
        let chip_h = 26.0;
        let gap_x = 8.0;
        let gap_y = 6.0;

        // Pre-measure chip widths to wrap across rows just like `ChartLinesFilterWidget::resizeToWidth`.
        let mut chip_sizes: Vec<f32> = Vec::with_capacity(self.chips.len());
        for chip in &self.chips {
            let galley = ui.fonts_mut(|f| {
                f.layout_no_wrap(chip.name.clone(), chip_font.clone(), Color32::WHITE)
            });
            let natural_w = (galley.size().x + 34.0).min(avail_w);
            chip_sizes.push(natural_w);
        }

        let mut rows: Vec<Vec<(usize, f32)>> = Vec::new();
        let mut current_row: Vec<(usize, f32)> = Vec::new();
        let mut row_w = 0.0f32;
        for (idx, w) in chip_sizes.into_iter().enumerate() {
            let next_w = if current_row.is_empty() {
                w
            } else {
                row_w + gap_x + w
            };
            if !current_row.is_empty() && next_w > avail_w {
                rows.push(std::mem::take(&mut current_row));
                current_row.push((idx, w));
                row_w = w;
            } else {
                current_row.push((idx, w));
                row_w = next_w;
            }
        }
        if !current_row.is_empty() {
            rows.push(current_row);
        }

        let inactive_text = to_color32(palette.premium_button_fg);
        let inactive_fill = to_color32(palette.box_bg);

        for (r_idx, row) in rows.iter().enumerate() {
            if r_idx > 0 {
                ui.add_space(gap_y);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap_x;
                for &(chip_idx, chip_w) in row {
                    let chip = &self.chips[chip_idx];
                    let (base_rect, resp) =
                        ui.allocate_exact_size(Vec2::new(chip_w, chip_h), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        clicked_id = Some(chip.id);
                    }

                    let shifted_rect = base_rect.translate(Vec2::new(chip.shake.shift_px(), 0.0));
                    let progress = chip.progress();
                    const CHECK_PART_PROGRESS: f64 = 0.5;
                    let check_progress = progress / CHECK_PART_PROGRESS;

                    let active_col = to_color32(chip.color);
                    let text_col = if progress <= CHECK_PART_PROGRESS {
                        lerp_color32(active_col, inactive_text, check_progress)
                    } else {
                        inactive_text
                    };
                    let fill_col = if progress <= CHECK_PART_PROGRESS {
                        lerp_color32(inactive_fill, active_col, check_progress)
                    } else {
                        active_col
                    };

                    let radius = (shifted_rect.height() / 2.0).round() as u8;
                    let painter = ui.painter();
                    painter.rect(
                        shifted_rect,
                        CornerRadius::same(radius),
                        fill_col,
                        Stroke::new(STATISTICS_LINE_WIDTH, active_col),
                        egui::StrokeKind::Inside,
                    );

                    // Text shifts smoothly from centered (unchecked) to right of checkmark (checked).
                    let galley = ui.fonts_mut(|f| {
                        f.layout_no_wrap(chip.name.clone(), chip_font.clone(), text_col)
                    });
                    let checked_text_x = shifted_rect.left() + 24.0;
                    let unchecked_text_x = shifted_rect.center().x - galley.size().x / 2.0;
                    let text_x = interpolate_f(
                        f64::from(unchecked_text_x),
                        f64::from(checked_text_x),
                        progress,
                    ) as f32;
                    let text_y = shifted_rect.center().y - galley.size().y / 2.0;
                    painter.galley(Pos2::new(text_x, text_y), galley, text_col);

                    // Animated 2-segment checkmark (`progress > 0.5`).
                    if progress > CHECK_PART_PROGRESS {
                        let bounce = (check_progress - 1.0).clamp(0.0, 1.0) as f32;
                        let cw = 3.0;
                        let origin =
                            Pos2::new(shifted_rect.left() + 12.0, shifted_rect.center().y + 3.0);
                        let p1 = Pos2::new(origin.x - cw * bounce, origin.y - cw * bounce);
                        let p2 =
                            Pos2::new(origin.x + cw * 2.0 * bounce, origin.y - cw * 2.0 * bounce);
                        let check_stroke = Stroke::new(1.8, text_col);
                        painter.line_segment([origin, p1], check_stroke);
                        painter.line_segment([origin, p2], check_stroke);
                    }
                }
            });
        }

        if let Some(id) = clicked_id {
            let _ = self.toggle_line(id, now_ms);
            ui.ctx().request_repaint();
        }
    }
}

// ============================================================================
// Native egui painting routines (no tiny-skia preview renderer)
// ============================================================================

fn paint_rulers_egui(
    painter: &egui::Painter,
    rulers: &ChartRulersView,
    rect: RectF,
    palette: &ChartPalette,
) {
    for ruler in rulers.rulers() {
        let color = to_color32_alpha(palette.box_text_fg, ruler.alpha * RULER_LINE_ALPHA);
        for line in &ruler.lines {
            let y = (rect.y + rect.height * line.relative_value) as f32;
            painter.line_segment(
                [
                    Pos2::new(rect.x as f32, y),
                    Pos2::new(rect.right() as f32, y),
                ],
                Stroke::new(1.0, color),
            );
        }
    }
}

fn paint_ruler_captions_egui(
    painter: &egui::Painter,
    rulers: &ChartRulersView,
    filter: &LinesFilterController,
    rect: RectF,
    palette: &ChartPalette,
) {
    let (left_color, right_color) = rulers.axis_colors();
    let font = FontId::proportional(10.5);
    for ruler in rulers.rulers() {
        for line in &ruler.lines {
            let y = (rect.y + rect.height * line.relative_value - 13.0) as f32;
            let (left_text, left_alpha, right) = rulers.line_captions(line, Some(filter));
            if !left_text.is_empty() && left_alpha > 0.01 {
                let base_col = if rulers.is_double() && rulers.axis_line_ids().0 != 0 {
                    left_color
                } else {
                    palette.window_sub_text_fg
                };
                painter.text(
                    Pos2::new(rect.x as f32, y),
                    Align2::LEFT_TOP,
                    left_text,
                    font.clone(),
                    to_color32_alpha(base_col, ruler.alpha * left_alpha),
                );
            }
            if let Some((right_text, right_alpha)) = right
                && !right_text.is_empty()
                && right_alpha > 0.01
            {
                painter.text(
                    Pos2::new(rect.right() as f32, y),
                    Align2::RIGHT_TOP,
                    right_text,
                    font.clone(),
                    to_color32_alpha(right_color, ruler.alpha * right_alpha),
                );
            }
        }
    }
}

fn paint_linear_egui(
    painter: &egui::Painter,
    lv: &mut tg_stats::LinearChartView,
    ctx: &PaintContext<'_>,
    filter: &LinesFilterController,
    stroke_width: f32,
) {
    for (i, line) in ctx.chart_data.lines.iter().enumerate() {
        let alpha = filter.alpha(line.id);
        if alpha <= 0.01 {
            continue;
        }
        let pts: Vec<Pos2> = lv
            .compute_line_points(i, ctx)
            .into_iter()
            .map(|p| Pos2::new(p.x as f32, p.y as f32))
            .collect();
        if pts.len() >= 2 {
            let color = to_color32_alpha(line.color, alpha);
            painter.add(Shape::Path(PathShape::line(
                pts,
                Stroke::new(stroke_width, color),
            )));
        }
    }
}

fn paint_linear_selection_egui(
    painter: &egui::Painter,
    lv: &mut tg_stats::LinearChartView,
    ctx: &PaintContext<'_>,
    filter: &LinesFilterController,
    selected_x_index: usize,
    popup_alpha: f64,
    palette: &ChartPalette,
) {
    paint_vertical_guide_egui(painter, ctx, selected_x_index, popup_alpha, palette);
    let points = lv.selected_points(ctx, selected_x_index);
    for (line_id, pt) in points {
        let alpha = filter.alpha(line_id) * popup_alpha;
        if alpha <= 0.01 {
            continue;
        }
        if let Some(line) = ctx.chart_data.lines.iter().find(|l| l.id == line_id) {
            let center = Pos2::new(pt.x as f32, pt.y as f32);
            painter.circle(
                center,
                STATISTICS_DOT_RADIUS,
                to_color32_alpha(palette.box_bg, alpha),
                Stroke::new(STATISTICS_LINE_WIDTH, to_color32_alpha(line.color, alpha)),
            );
        }
    }
}

fn paint_vertical_guide_egui(
    painter: &egui::Painter,
    ctx: &PaintContext<'_>,
    selected_x_index: usize,
    popup_alpha: f64,
    palette: &ChartPalette,
) {
    let x_span = ctx.x_percentage_limits.span();
    if x_span == 0.0 || selected_x_index >= ctx.chart_data.x_percentage.len() {
        return;
    }
    let x = (ctx.rect.x
        + ctx.rect.width
            * ((ctx.chart_data.x_percentage[selected_x_index] - ctx.x_percentage_limits.min)
                / x_span)) as f32;
    let color = to_color32_alpha(palette.box_text_fg, 0.14 * popup_alpha);
    painter.line_segment(
        [
            Pos2::new(x, ctx.rect.y as f32),
            Pos2::new(x, ctx.rect.bottom() as f32),
        ],
        Stroke::new(1.0, color),
    );
}

fn paint_bar_egui(
    painter: &egui::Painter,
    bv: &mut tg_stats::BarChartView,
    ctx: &PaintContext<'_>,
    filter: &LinesFilterController,
    selected_x_index: Option<usize>,
    popup_alpha: f64,
    palette: &ChartPalette,
) {
    let geom = bv.compute_geometry(ctx, filter);
    let has_selection =
        bv.is_stack() && !ctx.footer && selected_x_index.is_some() && popup_alpha > 0.01;
    let base_dim = if has_selection {
        interpolate_f(1.0, BAR_SELECTED_DIM_ALPHA, popup_alpha)
    } else {
        1.0
    };

    for series in &geom {
        let Some(line) = ctx.chart_data.lines.iter().find(|l| l.id == series.line_id) else {
            continue;
        };
        let line_alpha = filter.alpha(line.id);
        if line_alpha <= 0.01 {
            continue;
        }
        if bv.is_stack() {
            let fill = to_color32_alpha(line.color, line_alpha * base_dim);
            for r in &series.rects {
                let egui_r = Rect::from_min_size(
                    Pos2::new(r.x as f32, r.y as f32),
                    Vec2::new((r.width as f32).max(1.0), r.height as f32),
                );
                painter.rect_filled(egui_r, CornerRadius::ZERO, fill);
            }
            if let Some(sel_r) = series.selected_rect {
                let highlight = to_color32_alpha(line.color, line_alpha);
                let egui_r = Rect::from_min_size(
                    Pos2::new(sel_r.x as f32, sel_r.y as f32),
                    Vec2::new((sel_r.width as f32).max(1.0), sel_r.height as f32),
                );
                painter.rect_filled(egui_r, CornerRadius::ZERO, highlight);
            }
        } else {
            let pts: Vec<Pos2> = series
                .step_points
                .iter()
                .map(|p| Pos2::new(p.x as f32, p.y as f32))
                .collect();
            if pts.len() >= 2 {
                let stroke_w = if ctx.footer {
                    1.25
                } else {
                    STATISTICS_LINE_WIDTH
                };
                painter.add(Shape::Path(PathShape::line(
                    pts,
                    Stroke::new(stroke_w, to_color32_alpha(line.color, line_alpha)),
                )));
            }
        }
    }

    if !bv.is_stack()
        && !ctx.footer
        && let Some(sel) = selected_x_index
        && popup_alpha > 0.01
    {
        let pts = bv.selected_bar_points(ctx, sel);
        if let Some((_, first)) = pts.first() {
            let x = first.x as f32;
            painter.line_segment(
                [
                    Pos2::new(x, ctx.rect.y as f32),
                    Pos2::new(x, ctx.rect.bottom() as f32),
                ],
                Stroke::new(
                    1.0,
                    to_color32_alpha(palette.box_text_fg, 0.14 * popup_alpha),
                ),
            );
        }
        for (line_id, pt) in pts {
            let alpha = filter.alpha(line_id) * popup_alpha;
            if alpha <= 0.01 {
                continue;
            }
            if let Some(line) = ctx.chart_data.lines.iter().find(|l| l.id == line_id) {
                painter.circle(
                    Pos2::new(pt.x as f32, pt.y as f32),
                    STATISTICS_DOT_RADIUS,
                    to_color32_alpha(palette.box_bg, alpha),
                    Stroke::new(STATISTICS_LINE_WIDTH, to_color32_alpha(line.color, alpha)),
                );
            }
        }
    }
}

/// Paints percentage stacked area bands using clean quad-strip `egui::Mesh` triangles
/// (handles arbitrary non-convex upper/lower curves accurately without ear-clipping artifacts).
fn paint_stack_linear_egui(
    painter: &egui::Painter,
    sv: &mut tg_stats::StackLinearChartView,
    ctx: &PaintContext<'_>,
    filter: &LinesFilterController,
) {
    let polygons = sv.compute_area_polygons(ctx, filter);
    let bottom_y = ctx.rect.bottom() as f32;

    for poly in polygons {
        // `compute_area_polygons` returns `[bottom_left, top_0, top_1, ..., top_k, bottom_right]`.
        if poly.points.len() < 4 {
            continue;
        }
        let Some(line) = ctx.chart_data.lines.iter().find(|l| l.id == poly.line_id) else {
            continue;
        };
        let color = to_color32(line.color);
        let top_pts = &poly.points[1..poly.points.len() - 1];
        if top_pts.len() < 2 {
            continue;
        }

        let mut mesh = Mesh::default();
        for win in top_pts.windows(2) {
            let p0 = win[0];
            let p1 = win[1];
            let base = mesh.vertices.len() as u32;
            mesh.vertices.push(Vertex {
                pos: Pos2::new(p0.x as f32, p0.y as f32),
                uv: Pos2::ZERO,
                color,
            });
            mesh.vertices.push(Vertex {
                pos: Pos2::new(p1.x as f32, p1.y as f32),
                uv: Pos2::ZERO,
                color,
            });
            mesh.vertices.push(Vertex {
                pos: Pos2::new(p1.x as f32, bottom_y),
                uv: Pos2::ZERO,
                color,
            });
            mesh.vertices.push(Vertex {
                pos: Pos2::new(p0.x as f32, bottom_y),
                uv: Pos2::ZERO,
                color,
            });
            mesh.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        painter.add(Shape::mesh(mesh));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_pie_egui(
    painter: &egui::Painter,
    sv: &mut tg_stats::StackLinearChartView,
    chart: &StatisticalChart,
    filter: &LinesFilterController,
    x_indices: Limits,
    rect: RectF,
    palette: &ChartPalette,
    now_ms: f64,
    hovered_line_id: Option<i32>,
) {
    let (_parts_data, slices) = sv.compute_pie_layout(chart, filter, x_indices, rect, now_ms);
    let center = rect.center();
    let radius =
        ((rect.width / 2.0) * PIE_CIRCLE_SIZE_RATIO).min((rect.height / 2.0 - 8.0).max(1.0));

    for slice in &slices {
        if slice.sweep_angle_deg.abs() < 0.05 {
            continue;
        }
        let Some(line) = chart.lines.iter().find(|l| l.id == slice.line_id) else {
            continue;
        };
        let color = to_color32(line.color);
        let cx = (center.x + slice.offset.x) as f32;
        let cy = (center.y + slice.offset.y) as f32;
        let steps = ((slice.sweep_angle_deg.abs() / 5.0).ceil() as usize).max(2);

        let mut mesh = Mesh::default();
        mesh.vertices.push(Vertex {
            pos: Pos2::new(cx, cy),
            uv: Pos2::ZERO,
            color,
        });
        for s in 0..=steps {
            let t = (s as f64) / (steps as f64);
            let deg = slice.start_angle_deg + slice.sweep_angle_deg * t;
            let rad = deg * PI / 180.0;
            mesh.vertices.push(Vertex {
                pos: Pos2::new(
                    cx + (radius * rad.cos()) as f32,
                    cy + (radius * rad.sin()) as f32,
                ),
                uv: Pos2::ZERO,
                color,
            });
            if s > 0 {
                mesh.indices
                    .extend_from_slice(&[0, s as u32, (s + 1) as u32]);
            }
        }
        painter.add(Shape::mesh(mesh));

        if let Some(label_pos) = slice.label_center {
            let font_size = (16.0 * slice.label_scale as f32).clamp(10.0, 18.0);
            painter.text(
                Pos2::new(label_pos.x as f32, label_pos.y as f32),
                Align2::CENTER_CENTER,
                &slice.percentage_text,
                FontId::proportional(font_size),
                to_color32(palette.premium_button_fg),
            );
        }
    }

    // Floating hovered slice summary badge (`PaintDetails` in `point_details_widget.cpp`).
    if let Some(sel_id) = hovered_line_id
        && let Some(line) = chart.lines.iter().find(|l| l.id == sel_id)
    {
        let min_i = (x_indices.min.round().max(0.0) as usize).min(chart.x.len().saturating_sub(1));
        let max_i = (x_indices.max.round().max(0.0) as usize).min(chart.x.len().saturating_sub(1));
        let sum: i64 = if min_i <= max_i && !line.y.is_empty() {
            line.y[min_i..=max_i.min(line.y.len() - 1)].iter().sum()
        } else {
            0
        };
        let value_text = format_count_decimal(sum);
        let badge_w = ((line.name.len() + value_text.len()) as f32 * 7.2 + 32.0).max(120.0);
        let badge_h = 30.0;
        let badge_rect = Rect::from_min_size(
            Pos2::new(rect.right() as f32 - badge_w - 4.0, rect.y as f32 + 4.0),
            Vec2::new(badge_w, badge_h),
        );
        paint_card_shadow(painter, badge_rect, 1.0);
        painter.rect_filled(
            badge_rect,
            CornerRadius::same(6),
            to_color32(palette.box_bg),
        );
        painter.text(
            Pos2::new(badge_rect.left() + 10.0, badge_rect.center().y),
            Align2::LEFT_CENTER,
            &line.name,
            FontId::proportional(12.0),
            to_color32(palette.box_text_fg),
        );
        painter.text(
            Pos2::new(badge_rect.right() - 10.0, badge_rect.center().y),
            Align2::RIGHT_CENTER,
            &value_text,
            FontId::proportional(12.0),
            to_color32(line.color),
        );
    }
}

fn compute_popup_egui_rect(
    ui: &Ui,
    details: &PointDetailsData,
    chart: &StatisticalChart,
    x_limits: Limits,
    chart_rect: RectF,
    area_rect: Rect,
) -> Rect {
    let header_font = FontId::proportional(12.0);
    let body_font = FontId::proportional(12.0);

    let mut max_left_w = ui.fonts_mut(|f| {
        f.layout_no_wrap(details.header.clone(), header_font.clone(), Color32::WHITE)
            .size()
            .x
    }) + if details.clickable { 16.0 } else { 0.0 };
    let mut max_right_w = 0.0f32;
    let mut visible_rows = 0usize;

    for row in &details.lines {
        if row.alpha <= 0.01 {
            continue;
        }
        visible_rows += 1;
        let pct_prefix = row
            .percentage
            .as_deref()
            .map(|p| format!("{p}  "))
            .unwrap_or_default();
        let left_str = format!("{pct_prefix}{}", row.name);
        let lw = ui.fonts_mut(|f| {
            f.layout_no_wrap(left_str, body_font.clone(), Color32::WHITE)
                .size()
                .x
        });
        let rw = ui.fonts_mut(|f| {
            f.layout_no_wrap(row.value.clone(), body_font.clone(), Color32::WHITE)
                .size()
                .x
        });
        max_left_w = max_left_w.max(lw);
        max_right_w = max_right_w.max(rw);
    }

    let popup_w = (max_left_w + max_right_w + 36.0).clamp(136.0, 270.0);
    let row_h = 17.0;
    let popup_h = 18.0 + 18.0 + (visible_rows as f32) * row_h + 6.0;

    let area_rect_f = RectF::new(
        f64::from(area_rect.left()),
        f64::from(area_rect.top()),
        f64::from(area_rect.width()),
        f64::from(area_rect.height()),
    );
    let x = compute_popup_x(
        chart,
        details.x_index,
        x_limits,
        chart_rect,
        area_rect_f,
        f64::from(popup_w),
    ) as f32;
    let clamped_x = x.clamp(
        area_rect.left() + 2.0,
        (area_rect.right() - popup_w - 2.0).max(area_rect.left() + 2.0),
    );
    Rect::from_min_size(
        Pos2::new(clamped_x, area_rect.top() + 4.0),
        Vec2::new(popup_w, popup_h),
    )
}

fn paint_card_shadow(painter: &egui::Painter, rect: Rect, alpha: f64) {
    let s1 = Color32::from_rgba_unmultiplied(0, 0, 0, (18.0 * alpha) as u8);
    let s2 = Color32::from_rgba_unmultiplied(0, 0, 0, (10.0 * alpha) as u8);
    painter.rect_filled(
        rect.expand2(Vec2::new(2.0, 2.0))
            .translate(Vec2::new(0.0, 2.0)),
        CornerRadius::same(8),
        s2,
    );
    painter.rect_filled(
        rect.expand2(Vec2::new(1.0, 1.0))
            .translate(Vec2::new(0.0, 1.0)),
        CornerRadius::same(7),
        s1,
    );
}

fn paint_point_details_popup_egui(
    painter: &egui::Painter,
    details: &PointDetailsData,
    rect: Rect,
    alpha: f64,
    palette: &ChartPalette,
) {
    paint_card_shadow(painter, rect, alpha);
    painter.rect(
        rect,
        CornerRadius::same(6),
        to_color32_alpha(palette.box_bg, alpha),
        Stroke::new(1.0, to_color32_alpha(palette.box_text_fg, 0.08 * alpha)),
        egui::StrokeKind::Inside,
    );

    let pad_x = 11.0;
    let mut y = rect.top() + 8.0;

    // Header date + optional zoom chevron (`>`)
    painter.text(
        Pos2::new(rect.left() + pad_x, y),
        Align2::LEFT_TOP,
        &details.header,
        font_id_from_style(st::statistics_details_popup_header_style.font),
        to_color32_alpha(palette.box_text_fg, alpha),
    );
    if details.clickable {
        let arrow_rect = Rect::from_center_size(
            Pos2::new(rect.right() - pad_x - 4.0, y + 7.0),
            Vec2::splat(12.0),
        );
        paint_stats_icon(
            painter,
            arrow_rect,
            st::menu_icon_submenu_arrow,
            palette,
            Some(to_color32_alpha(palette.window_sub_text_fg, alpha)),
        );
    }

    y += 19.0;
    let row_font = font_id_from_style(st::statistics_details_popup_style.font);
    for row in &details.lines {
        let row_alpha = row.alpha * alpha;
        if row_alpha <= 0.01 {
            continue;
        }
        let mut name_x = rect.left() + pad_x;
        if let Some(ref pct) = row.percentage {
            painter.text(
                Pos2::new(name_x, y),
                Align2::LEFT_TOP,
                pct,
                row_font.clone(),
                to_color32_alpha(palette.box_text_fg, row_alpha),
            );
            name_x += 32.0;
        }
        painter.text(
            Pos2::new(name_x, y),
            Align2::LEFT_TOP,
            &row.name,
            row_font.clone(),
            to_color32_alpha(palette.box_text_fg, row_alpha),
        );
        painter.text(
            Pos2::new(rect.right() - pad_x, y),
            Align2::RIGHT_TOP,
            &row.value,
            row_font.clone(),
            to_color32_alpha(row.value_color, row_alpha),
        );
        y += 17.0;
    }
}
