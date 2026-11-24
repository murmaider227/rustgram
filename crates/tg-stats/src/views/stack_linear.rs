//! Percentage stacked-area (`StackLinear`) and zoomed pie chart math & state.
//!
//! Port of:
//! - `Telegram/SourceFiles/statistics/view/stack_linear_chart_common.{h,cpp}`
//! - `Telegram/SourceFiles/statistics/view/stack_linear_chart_view.{h,cpp}`

use std::collections::BTreeMap;
use std::f64::consts::PI;

use crate::anim::{AnimValue, ease_out_cubic, interpolate_f, interpolation_ratio};
use crate::chart::{Limits, PointF, RectF, StatisticalChart};
use crate::filter::LinesFilterController;
use crate::views::{
    HeightLimits, LocalZoomArgs, LocalZoomResult, LocalZoomType, PaintContext,
    find_stack_x_indices_from_raw_x_percentages,
};

/// Diameter ratio of the zoomed pie circle relative to `rect.width / 2` (`kCircleSizeRatio`).
pub const PIE_CIRCLE_SIZE_RATIO: f64 = 0.42;

/// Minimum text scale ratio for small pie slices (`kMinTextScaleRatio`).
pub const PIE_MIN_TEXT_SCALE_RATIO: f64 = 0.3;

/// Angle offset aligning `-180°` to the top vertical axis (`kPieAngleOffset`).
pub const PIE_ANGLE_OFFSET: f64 = 90.0;

/// Minimum slice share (`> 3.9%`) required to draw a percentage label inside a pie slice.
pub const PIE_MIN_LABEL_PERCENTAGE: f64 = 0.039;

/// Radial explosion offset in logical pixels for a hovered/selected pie slice (`st::statisticsPieChartPartOffset`).
pub const PIE_PART_OFFSET_PX: f64 = 8.0;

/// Animation duration in ms for pie slice selection and morphing (`st::slideWrapDuration`).
pub const PIE_ANIMATION_DURATION_MS: f64 = 200.0;

/// Single slice inside a [`PiePartData`] (`PiePartData::Part`).
#[derive(Debug, Clone, PartialEq)]
pub struct PiePart {
    /// Rounded fraction in `0.0..=1.0` (e.g. `0.42`).
    pub rounded_percentage: f64,
    /// Cumulative end angle in degrees (`-180.0..=180.0`).
    pub stacked_angle: f64,
    /// Formatted percentage string (e.g. `"42%"`).
    pub percentage_text: String,
}

/// Complete pie breakdown across all lines (`Statistic::PiePartData`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PiePartData {
    /// Per-line slice data aligned with `chart.lines`.
    pub parts: Vec<PiePart>,
    /// `true` if a single slice accounts for 100% (disables radial slice explosion).
    pub pie_has_single_part: bool,
}

/// Computes pie percentages, cumulative angles (`-180°..=180°`), and labels from per-line sums.
///
/// Port of `Statistic::PiePartsPercentage` (`stack_linear_chart_common.cpp:16-74`).
#[must_use]
pub fn pie_parts_percentage(sums: &[f64], total_sum: f64, round: bool) -> PiePartData {
    let mut result = PiePartData {
        parts: Vec::with_capacity(sums.len()),
        pie_has_single_part: false,
    };

    let mut stacked_percentage = 0.0;
    let mut sum_perc_diffs = 0.0;
    let mut max_perc_diff = 0.0;
    let mut min_perc_diff = 0.0;
    let mut max_perc_diff_index: Option<usize> = None;
    let mut min_perc_diff_index: Option<usize> = None;
    let mut rounded_percentages_sum = 0.0;

    for (k, &s) in sums.iter().enumerate() {
        let raw_percentage = if total_sum != 0.0 { s / total_sum } else { 0.0 };
        let rounded = if round {
            0.01 * (raw_percentage * 100.0).round()
        } else {
            raw_percentage
        };
        rounded_percentages_sum += rounded;
        let diff = raw_percentage - rounded;
        sum_perc_diffs += diff;
        let diff_abs = diff.abs();
        if max_perc_diff < diff_abs {
            max_perc_diff = diff_abs;
            max_perc_diff_index = Some(k);
        }
        if min_perc_diff < diff_abs {
            min_perc_diff = diff_abs;
            min_perc_diff_index = Some(k);
        }

        stacked_percentage += rounded;
        let pct_int = (rounded * 100.0).round() as i64;
        result.parts.push(PiePart {
            rounded_percentage: rounded,
            stacked_angle: stacked_percentage * 360.0 - 180.0,
            percentage_text: format!("{pct_int}%"),
        });
        if (rounded - 1.0).abs() < 1e-9 {
            result.pie_has_single_part = true;
        }
    }

    if round {
        let adjust_index = if rounded_percentages_sum > 1.0 {
            max_perc_diff_index
        } else {
            min_perc_diff_index
        };
        if let Some(index) = adjust_index {
            result.parts[index].rounded_percentage += sum_perc_diffs;
            let pct_int = (result.parts[index].rounded_percentage * 100.0).round() as i64;
            result.parts[index].percentage_text = format!("{pct_int}%");
            let angle_shrink = sum_perc_diffs * 360.0;
            for part in &mut result.parts {
                part.stacked_angle += angle_shrink;
            }
        }
    }

    result
}

/// Aggregates line values over `[x_indices.min, x_indices.max]` (weighted by filter alpha)
/// and computes rounded [`PiePartData`].
///
/// Port of `Statistic::PiePartsPercentageByIndices` (`stack_linear_chart_common.cpp:76-97`).
#[must_use]
pub fn pie_parts_percentage_by_indices(
    chart_data: &StatisticalChart,
    lines_filter: Option<&LinesFilterController>,
    x_indices: Limits,
) -> PiePartData {
    let mut sums = Vec::with_capacity(chart_data.lines.len());
    let mut total_sum = 0.0;

    let min_i = (x_indices.min.round().max(0.0) as usize).min(chart_data.x.len().saturating_sub(1));
    let max_i = (x_indices.max.round().max(0.0) as usize).min(chart_data.x.len().saturating_sub(1));

    for line in &chart_data.lines {
        let mut sum: f64 = 0.0;
        if !line.y.is_empty() && min_i <= max_i {
            let clamped_max = max_i.min(line.y.len() - 1);
            for &y in &line.y[min_i..=clamped_max] {
                sum += y as f64;
            }
        }
        if let Some(filter) = lines_filter {
            sum *= filter.alpha(line.id);
        }
        total_sum += sum;
        sums.push(sum);
    }

    pie_parts_percentage(&sums, total_sum, true)
}

/// Expands visible X indices by at least 1 sample on each side so the area fills the viewport edges.
///
/// Port of `FindAdditionalZoomedOutXIndices` (`stack_linear_chart_view.cpp:49-86`).
#[must_use]
pub fn find_additional_zoomed_out_x_indices(
    chart_data: &StatisticalChart,
    x_indices: Limits,
    x_percentage_limits: Limits,
) -> Limits {
    let xp = &chart_data.x_percentage;
    if xp.is_empty() {
        return Limits::default();
    }
    let last_index = (xp.len() - 1) as i32;

    let mut left_i = ((x_indices.min as i32) - 1).max(0);
    let left_result = if xp[left_i as usize] > x_percentage_limits.min {
        loop {
            left_i -= 1;
            if left_i < 0 {
                break 0.0;
            }
            if xp[left_i as usize] <= x_percentage_limits.min {
                break f64::from(left_i);
            }
        }
    } else {
        f64::from(left_i)
    };

    let mut right_i = ((x_indices.max as i32) + 1).min(last_index);
    let right_result = if xp[right_i as usize] < x_percentage_limits.max {
        loop {
            right_i += 1;
            if right_i > last_index {
                break f64::from(last_index);
            }
            if xp[right_i as usize] >= x_percentage_limits.max {
                break f64::from(right_i);
            }
        }
    } else {
        f64::from(right_i)
    };

    Limits::new(left_result, right_result)
}

/// Tracks hover/selection radial explosion animation for pie slices (`PiePartController`).
#[derive(Debug, Clone, Default)]
pub struct PiePartController {
    started_at: BTreeMap<i32, f64>,
    selected: Option<i32>,
}

impl PiePartController {
    /// Selects slice `id` (or `None` to deselect) at timestamp `now_ms`.
    /// Returns `true` if the selection changed.
    pub fn set(&mut self, id: Option<i32>, now_ms: f64) -> bool {
        if self.selected != id {
            if let Some(prev) = self.selected {
                self.update_entry(prev, now_ms);
            }
            self.selected = id;
            if let Some(next) = self.selected {
                self.update_entry(next, now_ms);
            }
            true
        } else {
            false
        }
    }

    fn update_entry(&mut self, id: i32, now_ms: f64) {
        let was = self.started_at.get(&id).copied().unwrap_or(0.0);
        let p = (now_ms - was) / PIE_ANIMATION_DURATION_MS;
        let progress = if p > 0.0 && p < 1.0 { 1.0 - p } else { 0.0 };
        self.started_at
            .insert(id, now_ms - PIE_ANIMATION_DURATION_MS * progress);
    }

    /// Returns the animation progress in `0.0..=1.0` for slice `id` at `now_ms`.
    #[must_use]
    pub fn progress(&self, id: i32, now_ms: f64) -> f64 {
        let Some(&at) = self.started_at.get(&id) else {
            return 0.0;
        };
        let show = self.selected == Some(id);
        let p = ((now_ms - at) / PIE_ANIMATION_DURATION_MS).clamp(0.0, 1.0);
        if show { p } else { 1.0 - p }.clamp(0.0, 1.0)
    }

    /// Returns the radial translation vector for slice `id` along bisector `angle_deg`.
    #[must_use]
    pub fn offset(&self, id: i32, angle_deg: f64, now_ms: f64) -> PointF {
        let offset = PIE_PART_OFFSET_PX * self.progress(id, now_ms);
        let rad = angle_deg * PI / 180.0;
        PointF::new(rad.cos() * offset, rad.sin() * offset)
    }

    /// Returns the currently selected slice line ID.
    #[must_use]
    pub const fn selected(&self) -> Option<i32> {
        self.selected
    }

    /// Returns `true` when no slice offset animation is in flight at `now_ms`.
    #[must_use]
    pub fn is_finished(&self, now_ms: f64) -> bool {
        self.started_at.keys().all(|&id| {
            let p = self.progress(id, now_ms);
            p <= 0.0 || p >= 1.0
        })
    }
}

/// Morphs pie slice percentages smoothly when the footer range changes in zoomed pie mode.
///
/// Port of `StackLinearChartView::ChangingPiePartController`.
#[derive(Debug, Clone)]
pub struct ChangingPiePartController {
    started_at: f64,
    anim_values: Vec<AnimValue>,
    current: PiePartData,
    is_finished: bool,
}

impl Default for ChangingPiePartController {
    fn default() -> Self {
        Self {
            started_at: 0.0,
            anim_values: Vec::new(),
            current: PiePartData::default(),
            is_finished: true,
        }
    }
}

impl ChangingPiePartController {
    /// Starts an eased transition from `was` to `now_parts` at timestamp `now_ms`.
    pub fn set_parts(&mut self, was: &[PiePart], now_parts: &[PiePart], now_ms: f64) {
        if self.anim_values.len() != was.len() {
            self.anim_values = was
                .iter()
                .zip(now_parts.iter())
                .map(|(w, n)| AnimValue::new(w.rounded_percentage, n.rounded_percentage))
                .collect();
        } else {
            for (anim, n) in self.anim_values.iter_mut().zip(now_parts.iter()) {
                *anim = AnimValue::new(anim.current(), n.rounded_percentage);
            }
        }
        self.started_at = now_ms;
        self.is_finished = false;
    }

    /// Advances the morph animation to `now_ms`.
    pub fn update(&mut self, now_ms: f64) {
        let progress = ((now_ms - self.started_at) / PIE_ANIMATION_DURATION_MS).clamp(0.0, 1.0);
        let mut total_sum = 0.0;
        let mut finished = true;
        let mut values = Vec::with_capacity(self.anim_values.len());
        for anim in &mut self.anim_values {
            anim.update(progress, ease_out_cubic);
            if finished && !anim.is_finished() {
                finished = false;
            }
            let v = anim.current();
            values.push(v);
            total_sum += v;
        }
        self.is_finished = finished;
        self.current = pie_parts_percentage(&values, total_sum, false);
    }

    /// Returns the interpolated pie parts.
    #[must_use]
    pub fn current(&self) -> &PiePartData {
        &self.current
    }

    /// Returns `true` when the morph animation has settled.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.is_finished
    }
}

/// Geometry for a single rendered pie slice, including label placement.
#[derive(Debug, Clone, PartialEq)]
pub struct PieSliceLayout {
    /// Series ID.
    pub line_id: i32,
    /// Start angle in degrees (`previous + 90°`).
    pub start_angle_deg: f64,
    /// Sweep angle in degrees (`now - previous`).
    pub sweep_angle_deg: f64,
    /// Rounded percentage in `0.0..=1.0`.
    pub percentage: f64,
    /// Percentage label text (e.g. `"34%"`).
    pub percentage_text: String,
    /// Center of the label text in canvas coordinates (present if `percentage > 0.039`).
    pub label_center: Option<PointF>,
    /// Font scale factor for the label text.
    pub label_scale: f64,
    /// Radial slice explosion offset in pixels.
    pub offset: PointF,
}

/// Polygon band for one series in percentage stacked-area mode.
#[derive(Debug, Clone, PartialEq)]
pub struct StackAreaPolygon {
    /// Series ID.
    pub line_id: i32,
    /// Closed polygon vertices in canvas coordinates.
    pub points: Vec<PointF>,
}

/// Percentage stacked-area chart view with local zoom to a pie chart (`Statistic::StackLinearChartView`).
#[derive(Debug, Clone, Default)]
pub struct StackLinearChartView {
    progress: f64,
    zoomed_out_x_indices: Limits,
    zoomed_out_x_indices_additional: Limits,
    zoomed_out_x_percentage: Limits,
    zoomed_in_limit: Limits,
    zoomed_in_limit_x_indices: Limits,
    zoomed_in_range: Limits,
    zoomed_in_range_x_indices: Limits,
    text_parts: Vec<PiePart>,
    pie_has_single_part: bool,
    /// Hover/selection explosion state for zoomed pie slices.
    pub pie_part_controller: PiePartController,
    /// Slice morphing state when dragging the zoomed footer.
    pub changing_pie_controller: ChangingPiePartController,
}

impl StackLinearChartView {
    /// Creates a new stacked-area / pie chart view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current local-zoom transition progress (`0.0` = area, `1.0` = pie).
    #[must_use]
    pub const fn zoom_progress(&self) -> f64 {
        self.progress
    }

    /// Returns fixed `0..=100` height limits (`StackLinearChartView::heightLimits`).
    #[must_use]
    pub fn height_limits(&self) -> HeightLimits {
        const MAX_STACK_LINEAR: f64 = 100.0;
        HeightLimits {
            full: Limits::new(0.0, MAX_STACK_LINEAR),
            ranged: Limits::new(0.0, MAX_STACK_LINEAR),
        }
    }

    /// Computes stacked percentage area polygons for each visible line (`progress == 0.0`).
    ///
    /// Port of `StackLinearChartView::paintChartOrZoomAnimation` (`stack_linear_chart_view.cpp:310-525`).
    #[must_use]
    pub fn compute_area_polygons(
        &mut self,
        c: &PaintContext<'_>,
        lines_filter: &LinesFilterController,
    ) -> Vec<StackAreaPolygon> {
        if c.chart_data.x_percentage.is_empty() || c.chart_data.lines.is_empty() {
            return Vec::new();
        }
        if !c.footer && self.progress == 0.0 {
            self.zoomed_out_x_indices = c.x_indices;
            self.zoomed_out_x_percentage = c.x_percentage_limits;
            self.zoomed_out_x_indices_additional = find_additional_zoomed_out_x_indices(
                c.chart_data,
                c.x_indices,
                c.x_percentage_limits,
            );
        }

        let (local_start, local_end) = if c.footer {
            (0usize, c.chart_data.x_percentage.len() - 1)
        } else {
            (
                self.zoomed_out_x_indices_additional.min as usize,
                self.zoomed_out_x_indices_additional.max as usize,
            )
        };
        let x_limits = if c.footer {
            Limits::new(
                c.chart_data.x_percentage[local_start],
                c.chart_data.x_percentage[local_end],
            )
        } else {
            self.zoomed_out_x_percentage
        };
        let x_span = x_limits.span();
        if x_span == 0.0 {
            return Vec::new();
        }

        let line_count = c.chart_data.lines.len();
        let mut polygons: Vec<Vec<PointF>> = vec![Vec::new(); line_count];

        for i in local_start..=local_end {
            let x_point =
                c.rect.x + c.rect.width * ((c.chart_data.x_percentage[i] - x_limits.min) / x_span);
            let mut sum = 0.0;
            let mut drawing_lines_count = 0usize;
            for line in &c.chart_data.lines {
                let alpha = lines_filter.alpha(line.id);
                if alpha <= 0.0 {
                    continue;
                }
                if line.y[i] > 0 {
                    sum += (line.y[i] as f64) * alpha;
                    drawing_lines_count += 1;
                }
            }

            let mut stack_offset = 0.0;
            for (k, line) in c.chart_data.lines.iter().enumerate() {
                let alpha = lines_filter.alpha(line.id);
                if alpha <= 0.0 {
                    continue;
                }
                let y_pct = if drawing_lines_count == 1 {
                    if line.y[i] != 0 { alpha } else { 0.0 }
                } else if sum != 0.0 {
                    (line.y[i] as f64) * alpha / sum
                } else {
                    0.0
                };
                let h = y_pct * c.rect.height;
                let y_point = c.rect.bottom() - h - stack_offset;

                if i == local_start {
                    polygons[k].push(PointF::new(c.rect.x, c.rect.bottom()));
                }
                polygons[k].push(PointF::new(x_point, y_point));
                if i == local_end {
                    polygons[k].push(PointF::new(c.rect.right(), c.rect.bottom()));
                }
                stack_offset += h;
            }
        }

        // Paint order in tdesktop is reverse (last line drawn first, underlying earlier lines).
        c.chart_data
            .lines
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(k, line)| {
                if polygons[k].len() < 3 {
                    None
                } else {
                    Some(StackAreaPolygon {
                        line_id: line.id,
                        points: std::mem::take(&mut polygons[k]),
                    })
                }
            })
            .collect()
    }

    /// Computes pie slice layout (angles, offsets, and label centers) for `x_indices` inside `rect`.
    ///
    /// Port of `StackLinearChartView::paintZoomed` & `paintPieText`.
    #[must_use]
    pub fn compute_pie_layout(
        &mut self,
        chart_data: &StatisticalChart,
        lines_filter: &LinesFilterController,
        x_indices: Limits,
        rect: RectF,
        now_ms: f64,
    ) -> (PiePartData, Vec<PieSliceLayout>) {
        let parts_data = pie_parts_percentage_by_indices(chart_data, Some(lines_filter), x_indices);
        self.text_parts = parts_data.parts.clone();
        self.pie_has_single_part = parts_data.pie_has_single_part;

        let center = rect.center();
        let side =
            ((rect.width / 2.0) * PIE_CIRCLE_SIZE_RATIO).min((rect.height / 2.0 - 8.0).max(1.0));
        let font_height = 20.0;
        let max_scale = side / (font_height * 2.0);
        let min_scale = max_scale * PIE_MIN_TEXT_SCALE_RATIO;
        let skip_translation = self.pie_has_single_part;

        let mut slices = Vec::with_capacity(chart_data.lines.len());
        for (k, line) in chart_data.lines.iter().enumerate() {
            let previous = if k > 0 {
                parts_data.parts[k - 1].stacked_angle
            } else {
                -180.0
            };
            let now = parts_data.parts[k].stacked_angle;
            let pct = parts_data.parts[k].rounded_percentage;
            let sweep = now - previous;
            let text_angle = if sweep == 0.0 {
                0.0
            } else {
                (previous + PIE_ANGLE_OFFSET) + sweep / 2.0
            };
            let offset = if skip_translation {
                PointF::default()
            } else {
                self.pie_part_controller.offset(line.id, text_angle, now_ms)
            };

            let scale = if (max_scale - min_scale).abs() < 1e-12 {
                0.0
            } else {
                min_scale + pct * (max_scale - min_scale)
            };

            let label_center = if pct > PIE_MIN_LABEL_PERCENTAGE {
                let r_text = side * (1.0 - pct).max(0.0).sqrt();
                let rad = text_angle * PI / 180.0;
                let text_w = (parts_data.parts[k].percentage_text.len() as f64) * 11.0;
                let shift = text_w / 2.0;
                let clamped_r = (r_text - shift * (1.0 - scale).max(0.0)).clamp(0.0, side * 0.72);
                let base_x = center.x + clamped_r * rad.cos();
                let base_y = center.y + clamped_r * rad.sin();
                Some(PointF::new(base_x + offset.x, base_y + offset.y))
            } else {
                None
            };

            slices.push(PieSliceLayout {
                line_id: line.id,
                start_angle_deg: previous + PIE_ANGLE_OFFSET,
                sweep_angle_deg: sweep,
                percentage: pct,
                percentage_text: parts_data.parts[k].percentage_text.clone(),
                label_center,
                label_scale: scale,
                offset,
            });
        }

        (parts_data, slices)
    }

    /// Hit-tests pointer coordinates `point` against the pie slices in `rect`, updating selection.
    ///
    /// Port of `StackLinearChartView::handleMouseMove` (`stack_linear_chart_view.cpp:825-853`).
    pub fn handle_pie_mouse_move(
        &mut self,
        chart_data: &StatisticalChart,
        rect: RectF,
        point: Option<PointF>,
        now_ms: f64,
    ) -> Option<i32> {
        let Some(p) = point else {
            self.pie_part_controller.set(None, now_ms);
            return None;
        };
        let center = rect.center();
        let theta = (center.y - p.y).atan2(center.x - p.x);
        let raw_angle = theta * (180.0 / PI) + 90.0;
        let angle = if raw_angle > 180.0 {
            raw_angle - 360.0
        } else {
            raw_angle
        };

        for (k, line) in chart_data.lines.iter().enumerate() {
            let previous = if k > 0 {
                self.text_parts
                    .get(k - 1)
                    .map_or(-180.0, |part| part.stacked_angle)
            } else {
                -180.0
            };
            let now = self
                .text_parts
                .get(k)
                .map_or(-180.0, |part| part.stacked_angle);
            if angle > previous && angle <= now {
                self.pie_part_controller.set(Some(line.id), now_ms);
                return Some(line.id);
            }
        }
        None
    }

    /// Finds the nearest X index for horizontal pointer position `x` inside `rect`.
    ///
    /// Port of `StackLinearChartView::findXIndexByPosition` (`stack_linear_chart_view.cpp:902-933`).
    #[must_use]
    pub fn find_x_index_by_position(
        &self,
        chart_data: &StatisticalChart,
        x_percentage_limits: Limits,
        rect: RectF,
        x: f64,
    ) -> Option<usize> {
        if self.progress == 1.0 || chart_data.x_percentage.is_empty() || rect.width <= 0.0 {
            return None;
        }
        if x < rect.x {
            return Some(0);
        }
        if x > rect.right() {
            return Some(chart_data.x_percentage.len() - 1);
        }
        let pointer_ratio = ((x - rect.x) / rect.width).clamp(0.0, 1.0);
        let limits = if self.zoomed_out_x_percentage.span() > 0.0 {
            self.zoomed_out_x_percentage
        } else {
            x_percentage_limits
        };
        let local_start = self.zoomed_out_x_indices.min as usize;
        let local_end = if self.zoomed_out_x_indices.max > 0.0 {
            (self.zoomed_out_x_indices.max as usize).min(chart_data.x_percentage.len() - 1)
        } else {
            chart_data.x_percentage.len() - 1
        };

        let raw_x_pct = interpolate_f(limits.min, limits.max, pointer_ratio);
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
        Some(nearest.clamp(local_start, local_end))
    }

    /// Handles local zoom into pie mode for an 8-day window centered around `args.x_index`.
    ///
    /// Port of `StackLinearChartView::maybeLocalZoom` (`stack_linear_chart_view.cpp:945-1008`).
    pub fn maybe_local_zoom(&mut self, args: LocalZoomArgs<'_>) -> LocalZoomResult {
        const LIMIT_LENGTH: usize = 8;
        const RANGE_LENGTH: usize = 0;
        const LEFT_SIDE: usize = LIMIT_LENGTH / 2 + RANGE_LENGTH; // 4
        const RIGHT_SIDE: usize = LIMIT_LENGTH / 2 + 1; // 5

        self.progress = args.progress;
        match args.kind {
            LocalZoomType::SkipCalculation => {
                return LocalZoomResult {
                    has_zoom: true,
                    limit_indices: self.zoomed_in_limit,
                    range: self.zoomed_in_range,
                };
            }
            LocalZoomType::CheckAvailability => {
                return LocalZoomResult {
                    has_zoom: true,
                    ..Default::default()
                };
            }
            LocalZoomType::SaveZoomFromFooter => {
                self.zoomed_in_range_x_indices = find_stack_x_indices_from_raw_x_percentages(
                    args.chart_data,
                    Limits::new(0.0, 1.0),
                    self.zoomed_in_limit_x_indices,
                );
                return LocalZoomResult {
                    has_zoom: true,
                    limit_indices: self.zoomed_in_limit_x_indices,
                    range: self.zoomed_in_range,
                };
            }
            LocalZoomType::Prepare | LocalZoomType::Process => {}
        }

        let xp = &args.chart_data.x_percentage;
        if xp.len() < 2 {
            return LocalZoomResult {
                has_zoom: true,
                ..Default::default()
            };
        }
        let back_index = xp.len() - 1;
        let x_index = args.x_index.min(back_index);
        let local_range_index = if x_index == back_index {
            back_index - RANGE_LENGTH
        } else {
            x_index
        };

        self.zoomed_in_range =
            Limits::new(xp[local_range_index], xp[local_range_index + RANGE_LENGTH]);
        self.zoomed_in_range_x_indices = Limits::new(
            local_range_index as f64,
            (local_range_index + RANGE_LENGTH) as f64,
        );

        self.zoomed_in_limit_x_indices = if x_index < LEFT_SIDE {
            Limits::new(0.0, (LEFT_SIDE + RIGHT_SIDE).min(back_index) as f64)
        } else if x_index > back_index.saturating_sub(RIGHT_SIDE + RANGE_LENGTH) {
            Limits::new(
                back_index.saturating_sub(LIMIT_LENGTH) as f64,
                back_index as f64,
            )
        } else {
            Limits::new((x_index - LEFT_SIDE) as f64, (x_index + RIGHT_SIDE) as f64)
        };

        let lim_min_i = (self.zoomed_in_limit_x_indices.min as usize).min(back_index);
        let lim_max_i = (self.zoomed_in_limit_x_indices.max as usize).min(back_index);
        self.zoomed_in_limit = Limits::new(
            interpolate_f(0.0, xp[lim_min_i], args.progress),
            interpolate_f(1.0, xp[lim_max_i], args.progress),
        );

        let neighbor = if x_index == back_index {
            local_range_index - 1
        } else {
            local_range_index + 1
        };
        let one_day = (xp[local_range_index] - xp[neighbor]).abs();
        let offset = if lim_max_i == back_index {
            -one_day
        } else {
            0.0
        };
        let result_range = Limits::new(
            interpolation_ratio(
                self.zoomed_in_limit.min,
                self.zoomed_in_limit.max,
                self.zoomed_in_range.min + one_day * 0.25 + offset,
            ),
            interpolation_ratio(
                self.zoomed_in_limit.min,
                self.zoomed_in_limit.max,
                self.zoomed_in_range.max + one_day * 0.75 + offset,
            ),
        );

        LocalZoomResult {
            has_zoom: true,
            limit_indices: self.zoomed_in_limit_x_indices,
            range: result_range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pie_parts_percentage_sums_to_100_and_angles_cover_360() {
        let data = pie_parts_percentage(&[1.0, 1.0, 1.0], 3.0, true);
        assert_eq!(data.parts.len(), 3);
        assert!(!data.pie_has_single_part);
        let total_pct: f64 = data.parts.iter().map(|p| p.rounded_percentage).sum();
        assert!((total_pct - 1.0).abs() < 1e-9);
        assert!((data.parts.last().unwrap().stacked_angle - 180.0).abs() < 1e-6);

        let single = pie_parts_percentage(&[0.0, 42.0, 0.0], 42.0, true);
        assert!(single.pie_has_single_part);
        assert_eq!(single.parts[1].percentage_text, "100%");
    }

    #[test]
    fn local_zoom_computes_8_day_window_around_index() {
        let mut cols_x = vec!["\"x\"".to_string()];
        let mut cols_y0 = vec!["\"y0\"".to_string()];
        let mut cols_y1 = vec!["\"y1\"".to_string()];
        for i in 0..20 {
            cols_x.push((1_700_006_400_000i64 + i * 86_400_000).to_string());
            cols_y0.push((60 + i).to_string());
            cols_y1.push((40 - i).to_string());
        }
        let json = format!(
            "{{\"columns\":[[{}],[{}],[{}]],\"percentage\":true}}",
            cols_x.join(","),
            cols_y0.join(","),
            cols_y1.join(",")
        );
        let chart = StatisticalChart::from_json_str(&json).unwrap();
        let mut view = StackLinearChartView::new();

        let res = view.maybe_local_zoom(LocalZoomArgs {
            chart_data: &chart,
            kind: LocalZoomType::Process,
            progress: 1.0,
            x_index: 10,
        });
        assert!(res.has_zoom);
        // x_index = 10 -> [10 - 4, 10 + 5] = [6, 15].
        assert_eq!(res.limit_indices, Limits::new(6.0, 15.0));
        assert!(res.range.min >= 0.0 && res.range.max <= 1.0 && res.range.min < res.range.max);
    }
}
