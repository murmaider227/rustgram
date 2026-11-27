//! Horizontal (date) and vertical (value) chart rulers and caption generation.
//!
//! Port of:
//! - `Telegram/SourceFiles/statistics/chart_rulers_data.{h,cpp}`
//! - `Telegram/SourceFiles/statistics/view/chart_rulers_view.{h,cpp}`
//! - Bottom date ruler calculation (`updateBottomDates`, `PaintBottomLine`) from
//!   `Telegram/SourceFiles/statistics/chart_widget.cpp`.

use crate::chart::{ChartViewType, Limits, StatisticalChart, StatisticalCurrency};
use crate::color::{ChartPalette, Color};
use crate::filter::LinesFilterController;
use crate::format::{
    ONE_STAR_IN_NANO, format_count_decimal, format_count_to_short, format_exact_count_decimal,
    to_usd,
};
use crate::segment_tree::ChartValue;

const MIN_LINES: ChartValue = 2;
const MAX_LINES: ChartValue = 6;
const STEP_DIVISOR: f64 = 5.0;

/// Rounds `max_value` up to a clean multiple of 10 when `max_value / 5` is not a multiple of 10.
///
/// Port of `Round` in `statistics/chart_rulers_data.cpp`.
#[must_use]
pub fn round_ruler_max(max_value: ChartValue) -> ChartValue {
    let k = (max_value as f64 / STEP_DIVISOR) as ChartValue;
    if k % 10 == 0 {
        max_value
    } else {
        (max_value / 10).saturating_add(1).saturating_mul(10)
    }
}

/// Formats an integer ruler label (`>= 10,000` shortened, else decimal).
#[must_use]
pub fn format_ruler_value(absolute_value: ChartValue) -> String {
    const TOO_MUCH: ChartValue = 10_000;
    if absolute_value >= TOO_MUCH {
        format_count_to_short(absolute_value, false).string
    } else {
        format_count_decimal(absolute_value)
    }
}

/// Formats a floating-point currency ruler label (`FormatF` in `chart_rulers_view.cpp`).
#[must_use]
pub fn format_ruler_float(absolute_value: f64) -> String {
    const TOO_MUCH: f64 = 10_000.0;
    const TOO_SMALL: f64 = 1e-9;
    if absolute_value.abs() <= TOO_SMALL {
        "0".to_string()
    } else if absolute_value >= TOO_MUCH {
        format_count_to_short(absolute_value as i64, false).string
    } else {
        format_exact_count_decimal(absolute_value)
    }
}

/// One horizontal gridline and its left/right captions (`ChartRulersData::Line`).
#[derive(Debug, Clone, PartialEq)]
pub struct RulerLine {
    /// Absolute Y value represented by this gridline.
    pub absolute_value: f64,
    /// Relative vertical coordinate in `0.0..=1.0` (`0.0` = top, `1.0` = bottom).
    pub relative_value: f64,
    /// Primary (typically left-side) caption text.
    pub caption: String,
    /// Secondary (typically right-side or scaled) caption text.
    pub scaled_line_caption: String,
    /// Estimated pixel width of the right-side caption.
    pub right_caption_width: f64,
}

/// A set of horizontal Y-axis gridlines (`Statistic::ChartRulersData`).
#[derive(Debug, Clone)]
pub struct ChartRulersData {
    /// Gridlines ordered from bottom (`i = 0`) to top (`i = n - 1`).
    pub lines: Vec<RulerLine>,
    /// Current fade opacity in `0.0..=1.0`.
    pub alpha: f64,
    /// Snapshot of `alpha` taken when a newer ruler set was pushed.
    pub fixed_alpha: f64,
}

impl ChartRulersData {
    /// Computes horizontal ruler lines for `[new_min_height, new_max_height]`.
    ///
    /// Port of `ChartRulersData::ChartRulersData` (`statistics/chart_rulers_data.cpp:35-116`).
    #[must_use]
    pub fn new(
        new_max_height: ChartValue,
        mut new_min_height: ChartValue,
        use_min_height: bool,
        right_ratio: f64,
        left_custom_caption: Option<&dyn Fn(f64) -> String>,
        right_custom_caption: Option<&dyn Fn(f64) -> String>,
    ) -> Self {
        let mut lines = Vec::new();

        if !use_min_height {
            let v = if new_max_height > 100 {
                round_ruler_max(new_max_height)
            } else {
                new_max_height
            };
            let step = ((v as f64 / STEP_DIVISOR).ceil() as ChartValue).max(1);
            let n = if v < MAX_LINES {
                (v + 1).max(2)
            } else if v / 2 < MAX_LINES {
                let base = v / 2 + 1;
                if v % 2 != 0 { base + 1 } else { base }
            } else {
                MAX_LINES
            };

            lines.resize(
                n as usize,
                RulerLine {
                    absolute_value: 0.0,
                    relative_value: 0.0,
                    caption: String::new(),
                    scaled_line_caption: String::new(),
                    right_caption_width: 0.0,
                },
            );
            for (i, line) in lines.iter_mut().enumerate().take(n as usize).skip(1) {
                let abs_val = (i as ChartValue).saturating_mul(step);
                line.absolute_value = abs_val as f64;
                line.caption = format_count_to_short(abs_val, false).string;
            }
        } else {
            let diff = new_max_height.saturating_sub(new_min_height);
            let (n, step): (ChartValue, f64) = if diff == 0 {
                new_min_height -= 1;
                (MAX_LINES / 2, 1.0)
            } else if diff < MAX_LINES {
                ((diff + 1).max(MIN_LINES), 1.0)
            } else if diff / 2 < MAX_LINES {
                (diff / 2 + diff % 2 + 1, 2.0)
            } else {
                let s = diff as f64 / STEP_DIVISOR;
                if s <= 0.0 {
                    (diff.saturating_add(1).max(MIN_LINES), 1.0)
                } else {
                    (6, s)
                }
            };

            let diff_absolute_value = (((n - 1) as f64) * step) as ChartValue;
            let skip_float_values = right_ratio > 0.0 && (step / right_ratio) < 1.0;

            lines.reserve(n as usize);
            for i in 0..(n as usize) {
                let value = ((i as f64) * step) as ChartValue;
                let abs_int = new_min_height.saturating_add(value);
                let abs_f = abs_int as f64;
                let relative_value = if diff_absolute_value == 0 {
                    1.0
                } else {
                    1.0 - (value as f64) / (diff_absolute_value as f64)
                };
                let caption = match left_custom_caption {
                    Some(f) => f(abs_f),
                    None => format_ruler_value(abs_int),
                };
                let scaled_line_caption = if right_ratio > 0.0 || right_custom_caption.is_some() {
                    if let Some(f) = right_custom_caption {
                        f(abs_f)
                    } else {
                        let v = (new_min_height as f64 + (i as f64) * step) / right_ratio;
                        if !skip_float_values || (v - (v as ChartValue as f64)).abs() < 0.01 {
                            format_ruler_value(v as ChartValue)
                        } else {
                            String::new()
                        }
                    }
                } else {
                    String::new()
                };

                lines.push(RulerLine {
                    absolute_value: abs_f,
                    relative_value,
                    caption,
                    scaled_line_caption,
                    right_caption_width: 0.0,
                });
            }
        }

        Self {
            lines,
            alpha: 0.0,
            fixed_alpha: 1.0,
        }
    }

    /// Recomputes each line's `relative_value` for an animated `[new_min_height, new_max_height]`.
    ///
    /// Port of `ChartRulersData::computeRelative`.
    pub fn compute_relative(&mut self, new_max_height: f64, new_min_height: f64) {
        let span = new_max_height - new_min_height;
        if span == 0.0 {
            return;
        }
        for line in &mut self.lines {
            line.relative_value = 1.0 - ((line.absolute_value - new_min_height) / span);
        }
    }
}

/// Custom caption mode for currency-denominated revenue charts.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CurrencyCaptionMode {
    None,
    Ton { rate: f64 },
    Credits { rate: f64 },
}

/// Manages stacked, cross-fading vertical Y-axis rulers (`Statistic::ChartRulersView`).
#[derive(Debug, Clone)]
pub struct ChartRulersView {
    is_double: bool,
    left_color: Color,
    right_color: Color,
    left_line_id: i32,
    right_line_id: i32,
    currency_mode: CurrencyCaptionMode,
    rulers: Vec<ChartRulersData>,
    scaled_line_ratio: f64,
    is_left_line_scaled: bool,
}

impl Default for ChartRulersView {
    fn default() -> Self {
        Self::new()
    }
}

impl ChartRulersView {
    /// Creates an empty rulers view.
    #[must_use]
    pub fn new() -> Self {
        Self {
            is_double: false,
            left_color: ChartPalette::light().window_sub_text_fg,
            right_color: ChartPalette::light().window_sub_text_fg,
            left_line_id: 0,
            right_line_id: 0,
            currency_mode: CurrencyCaptionMode::None,
            rulers: Vec::new(),
            scaled_line_ratio: 0.0,
            is_left_line_scaled: false,
        }
    }

    /// Configures dual-axis ratios, currency formatters, and colors from `chart_data`.
    ///
    /// Port of `ChartRulersView::setChartData`.
    pub fn set_chart_data(
        &mut self,
        chart_data: &StatisticalChart,
        view_type: ChartViewType,
        palette: &ChartPalette,
    ) {
        self.rulers.clear();
        self.is_double =
            (view_type == ChartViewType::DoubleLinear) || chart_data.currency_rate != 0.0;
        self.left_color = palette.window_sub_text_fg;
        self.right_color = palette.window_sub_text_fg;
        self.left_line_id = 0;
        self.right_line_id = 0;
        self.scaled_line_ratio = 0.0;
        self.is_left_line_scaled = false;

        self.currency_mode = if chart_data.currency_rate != 0.0 {
            if chart_data.currency == StatisticalCurrency::Ton {
                CurrencyCaptionMode::Ton {
                    rate: chart_data.currency_rate,
                }
            } else {
                CurrencyCaptionMode::Credits {
                    rate: chart_data.currency_rate,
                }
            }
        } else {
            CurrencyCaptionMode::None
        };

        if self.is_double && chart_data.lines.len() == 2 {
            self.left_color = chart_data.lines[0].color;
            self.right_color = chart_data.lines[1].color;
            self.left_line_id = chart_data.lines[0].id;
            self.right_line_id = chart_data.lines[1].id;

            let first_max = chart_data.lines[0].max_value;
            let second_max = chart_data.lines[1].max_value;
            if first_max > second_max {
                self.is_left_line_scaled = false;
                self.scaled_line_ratio = if second_max != 0 {
                    first_max as f64 / second_max as f64
                } else {
                    1.0
                };
            } else {
                self.is_left_line_scaled = true;
                self.scaled_line_ratio = if first_max != 0 {
                    second_max as f64 / first_max as f64
                } else {
                    1.0
                };
            }
        }
    }

    /// Returns whether this rulers view displays dual left/right captions.
    #[must_use]
    pub const fn is_double(&self) -> bool {
        self.is_double
    }

    /// Returns whether the left axis carries the scaled line captions.
    #[must_use]
    pub const fn is_left_line_scaled(&self) -> bool {
        self.is_left_line_scaled
    }

    /// Returns the scaling ratio between the two axes (0.0 for single-axis charts).
    #[must_use]
    pub const fn scaled_line_ratio(&self) -> f64 {
        self.scaled_line_ratio
    }

    /// Returns `(left_color, right_color)`.
    #[must_use]
    pub const fn axis_colors(&self) -> (Color, Color) {
        (self.left_color, self.right_color)
    }

    /// Returns `(left_line_id, right_line_id)` for double-linear filter alpha lookup.
    #[must_use]
    pub const fn axis_line_ids(&self) -> (i32, i32) {
        (self.left_line_id, self.right_line_id)
    }

    /// Returns whether a custom right-side currency caption is active.
    #[must_use]
    pub const fn has_currency_captions(&self) -> bool {
        !matches!(self.currency_mode, CurrencyCaptionMode::None)
    }

    /// Returns all active ruler layers (oldest to newest).
    #[must_use]
    pub fn rulers(&self) -> &[ChartRulersData] {
        &self.rulers
    }

    /// Resolves the left and optional right caption strings for a given [`RulerLine`].
    #[must_use]
    pub fn line_captions<'a>(
        &self,
        line: &'a RulerLine,
        lines_filter: Option<&LinesFilterController>,
    ) -> (&'a str, f64, Option<(&'a str, f64)>) {
        let has_double_filter = self.is_double && self.left_line_id != 0 && lines_filter.is_some();
        let left_alpha = if has_double_filter {
            lines_filter.map_or(1.0, |f| f.alpha(self.left_line_id))
        } else {
            1.0
        };
        let left_text = if !self.is_double {
            line.caption.as_str()
        } else if self.is_left_line_scaled {
            line.scaled_line_caption.as_str()
        } else {
            line.caption.as_str()
        };

        let right = if has_double_filter || self.has_currency_captions() {
            let right_alpha = if has_double_filter {
                lines_filter.map_or(1.0, |f| f.alpha(self.right_line_id))
            } else {
                1.0
            };
            let right_text = if self.is_left_line_scaled {
                line.caption.as_str()
            } else {
                line.scaled_line_caption.as_str()
            };
            Some((right_text, right_alpha))
        } else {
            None
        };

        (left_text, left_alpha, right)
    }

    /// Adds a new ruler layer for `new_height`.
    ///
    /// Port of `ChartRulersView::add` (`chart_rulers_view.cpp:177-203`).
    pub fn add(&mut self, new_height: Limits, animated: bool) {
        let ratio = if self.is_double {
            self.scaled_line_ratio
        } else {
            0.0
        };

        let mut new_data = match self.currency_mode {
            CurrencyCaptionMode::None => ChartRulersData::new(
                new_height.max as ChartValue,
                new_height.min as ChartValue,
                true,
                ratio,
                None,
                None,
            ),
            CurrencyCaptionMode::Ton { rate } => {
                let left_fn = |v: f64| format_ruler_float(v / (ONE_STAR_IN_NANO as f64));
                let right_fn = |v: f64| to_usd(v / (ONE_STAR_IN_NANO as f64), rate, 0);
                ChartRulersData::new(
                    new_height.max as ChartValue,
                    new_height.min as ChartValue,
                    true,
                    ratio,
                    Some(&left_fn),
                    Some(&right_fn),
                )
            }
            CurrencyCaptionMode::Credits { rate } => {
                let left_fn = format_ruler_float;
                let right_fn = |v: f64| to_usd(v, rate, 0);
                ChartRulersData::new(
                    new_height.max as ChartValue,
                    new_height.min as ChartValue,
                    true,
                    ratio,
                    Some(&left_fn),
                    Some(&right_fn),
                )
            }
        };

        if self.is_double {
            const APPROX_CHAR_WIDTH: f64 = 6.0;
            for line in &mut new_data.lines {
                let text = if self.is_left_line_scaled {
                    &line.caption
                } else {
                    &line.scaled_line_caption
                };
                line.right_caption_width = (text.chars().count() as f64) * APPROX_CHAR_WIDTH;
            }
        }

        if !animated {
            self.rulers.clear();
        }
        for ruler in &mut self.rulers {
            ruler.fixed_alpha = ruler.alpha;
        }
        self.rulers.push(new_data);
        if !animated && let Some(last) = self.rulers.last_mut() {
            last.alpha = 1.0;
        }
    }

    /// Recomputes relative gridline positions for all active ruler layers.
    pub fn compute_relative(&mut self, new_max_height: f64, new_min_height: f64) {
        for ruler in &mut self.rulers {
            ruler.compute_relative(new_max_height, new_min_height);
        }
    }

    /// Cross-fades ruler layers using `value` in `0.0..=1.0` and prunes faded-out layers.
    ///
    /// Port of `ChartRulersView::setAlpha`.
    pub fn set_alpha(&mut self, value: f64) {
        if self.rulers.is_empty() {
            return;
        }
        for ruler in &mut self.rulers {
            ruler.alpha = ruler.fixed_alpha * (1.0 - value);
        }
        if let Some(last) = self.rulers.last_mut() {
            last.alpha = value;
        }
        if value == 1.0 {
            while self.rulers.len() > 1 && self.rulers[0].alpha == 0.0 {
                self.rulers.remove(0);
            }
        }
    }
}

/// Bottom horizontal date caption layer state (`ChartWidget::BottomCaptionLineData`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BottomCaptionLineData {
    /// Index stride between displayed X labels (power of two).
    pub step: i32,
    /// Upper hysteresis threshold before increasing `step`.
    pub step_max: i32,
    /// Lower hysteresis threshold before decreasing `step`.
    pub step_min: i32,
    /// Fast-fade threshold.
    pub step_min_fast: i32,
    /// Raw unrounded step estimate.
    pub step_raw: i32,
    /// Current opacity in `0.0..=1.0`.
    pub alpha: f64,
    /// Snapshot opacity captured when a newer caption layer is pushed.
    pub fixed_alpha: f64,
}

/// A positioned X-axis date label ready for drawing.
#[derive(Debug, Clone, PartialEq)]
pub struct BottomCaptionLabel {
    /// Center X coordinate in chart area space.
    pub x_center: f64,
    /// Left X edge of the label box.
    pub x_left: f64,
    /// Label box width (`day_string_max_width`).
    pub width: f64,
    /// Label text (from `chart.get_day_string(i)`).
    pub text: String,
    /// Combined edge-fade and layer-transition opacity in `0.0..=1.0`.
    pub alpha: f64,
}

/// State managing bottom horizontal X-axis date labels (`ChartWidget::_bottomLine`).
#[derive(Debug, Clone, Default)]
pub struct BottomDatesState {
    /// Currently active step configuration.
    pub current: BottomCaptionLineData,
    /// Active cross-fading date layers (at most 2).
    pub dates: Vec<BottomCaptionLineData>,
    /// Virtual full width of the chart (`chart_width / (x_max - x_min)`).
    pub chart_full_width: i32,
    /// Index alignment offset for label spacing.
    pub caption_indices_offset: i32,
}

impl BottomDatesState {
    /// Updates `chart_full_width` for viewport width `w` and X percentage range `x_limits`.
    pub fn update_chart_full_width(&mut self, w: i32, x_limits: Limits) {
        let span = x_limits.span();
        self.chart_full_width = if span == 0.0 {
            0
        } else {
            (f64::from(w) / span) as i32
        };
    }

    /// Recomputes step hysteresis and pushes a new date layer when the stride changes.
    ///
    /// Returns `true` if a cross-fade animation should be started.
    ///
    /// Port of `ChartWidget::updateBottomDates` (`chart_widget.cpp:1015-1082`).
    pub fn update(&mut self, chart_data: &StatisticalChart, chart_area_width: i32) -> bool {
        if chart_data.is_empty()
            || self.chart_full_width == 0
            || chart_data.one_day_percentage == 0.0
        {
            return false;
        }
        let d = f64::from(self.chart_full_width) * chart_data.one_day_percentage;
        if d <= 0.0 {
            return false;
        }
        let k = f64::from(chart_area_width) / d;
        let step_raw = (k / 6.0) as i32;

        let by = (f64::from(chart_area_width) / (chart_data.x.len() as f64)) as i32;
        self.caption_indices_offset = chart_data.day_string_max_width / by.max(1);

        let is_current_null = self.current.step_min_fast == 0 && self.dates.is_empty();
        if !is_current_null && step_raw < self.current.step_max && step_raw > self.current.step_min
        {
            return false;
        }

        let step = highest_one_bit(step_raw.max(0) as u32) << 1;
        if !is_current_null && self.current.step == step {
            return false;
        }

        const STEP_RATIO: f64 = 0.1;
        const FAST_STEP_OFFSET: i32 = 4;
        let step_max = (f64::from(step) + f64::from(step) * STEP_RATIO) as i32;
        let step_min = (f64::from(step) - f64::from(step) * STEP_RATIO) as i32;
        let step_min_fast = step_min - FAST_STEP_OFFSET;

        let data = BottomCaptionLineData {
            step,
            step_max,
            step_min,
            step_min_fast,
            step_raw,
            alpha: 1.0,
            fixed_alpha: 0.0,
        };

        if is_current_null {
            self.current = data;
            self.dates.push(data);
            return false;
        }

        self.current = data;
        for date in &mut self.dates {
            date.fixed_alpha = date.alpha;
        }
        self.dates.push(data);
        if self.dates.len() > 2 {
            self.dates.remove(0);
        }
        true
    }

    /// Computes all visible X-axis date labels and their edge-faded alphas.
    ///
    /// Port of `PaintBottomLine` in `statistics/chart_widget.cpp:92-168`.
    #[must_use]
    pub fn compute_labels(
        &self,
        chart_data: &StatisticalChart,
        x_percentage_limits: Limits,
        chart_width: i32,
    ) -> Vec<BottomCaptionLabel> {
        let mut labels = Vec::new();
        if chart_data.x.len() < 2 || self.dates.is_empty() {
            return labels;
        }
        let start_x_index = chart_data.find_start_index(x_percentage_limits.min) as i32;
        let end_x_index =
            chart_data.find_end_index(start_x_index as usize, x_percentage_limits.max) as i32;

        let caption_max_width = f64::from(chart_data.day_string_max_width.max(1));
        let edge_alpha_size = (caption_max_width / 4.0).max(1.0);
        let full_width = f64::from(self.chart_full_width);
        let chart_w_f = f64::from(chart_width);
        let offset = full_width * x_percentage_limits.min;
        let x_front = chart_data.x[0];
        let x_span = chart_data.x[chart_data.x.len() - 1] - x_front;
        if x_span == 0.0 {
            return labels;
        }

        const FAST_ALPHA_SPEED: f64 = 0.85;
        let last_step_min_fast = self.dates.last().map_or(0, |d| d.step_min_fast);

        for (k, date) in self.dates.iter().enumerate() {
            let is_last = k + 1 == self.dates.len();
            let result_alpha = date.alpha;
            let step = date.step.max(1);

            let mut start = start_x_index - self.caption_indices_offset;
            while start.rem_euclid(step) != 0 {
                start -= 1;
            }
            let mut end = end_x_index - self.caption_indices_offset;
            let max_idx = (chart_data.x.len() - 1) as i32;
            while end.rem_euclid(step) != 0 || end < max_idx {
                end += 1;
            }

            start += self.caption_indices_offset;
            end += self.caption_indices_offset;

            let has_fast_alpha = date.step_raw < last_step_min_fast;
            let fast_alpha = if is_last {
                1.0
            } else {
                (result_alpha - FAST_ALPHA_SPEED).max(0.0)
            };

            let mut i = start;
            while i < end {
                if i >= 0 && (i as usize) < chart_data.x.len() - 1 {
                    let x_pct = (chart_data.x[i as usize] - x_front) / x_span;
                    let x_point = x_pct * full_width - offset;
                    let rx = x_point - caption_max_width / 2.0;
                    let r_right = rx + caption_max_width;
                    let edge_alpha = if rx < 0.0 {
                        (1.0 + rx / edge_alpha_size).max(0.0)
                    } else if r_right > chart_w_f {
                        (1.0 + (chart_w_f - r_right) / edge_alpha_size).max(0.0)
                    } else {
                        1.0
                    };
                    let alpha = edge_alpha
                        * if has_fast_alpha {
                            fast_alpha
                        } else {
                            result_alpha
                        };
                    if alpha > 0.0 {
                        labels.push(BottomCaptionLabel {
                            x_center: x_point,
                            x_left: rx,
                            width: caption_max_width,
                            text: chart_data.get_day_string(i as usize).to_string(),
                            alpha,
                        });
                    }
                }
                i += step;
            }
        }
        labels
    }
}

/// Returns the highest set bit of `v` as an `i32`, or `0` if `v == 0`.
///
/// Port of `highestOneBit` in `statistics/chart_widget.cpp:1033-1043`.
#[must_use]
pub fn highest_one_bit(mut v: u32) -> i32 {
    if v == 0 {
        return 0;
    }
    let mut r = 1u32;
    loop {
        v >>= 1;
        if v == 0 {
            break;
        }
        r *= 2;
    }
    r as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::ONE_DAY_MS;

    #[test]
    fn rulers_data_with_min_height_known_answers() {
        // diff = 0 -> min decremented by 1, n = 3, step = 1
        let d0 = ChartRulersData::new(50, 50, true, 0.0, None, None);
        assert_eq!(d0.lines.len(), 3);
        assert_eq!(d0.lines[0].absolute_value, 49.0);
        assert_eq!(d0.lines[1].absolute_value, 50.0);
        assert_eq!(d0.lines[2].absolute_value, 51.0);
        assert_eq!(d0.lines[0].relative_value, 1.0);
        assert_eq!(d0.lines[2].relative_value, 0.0);

        // diff = 4 (< 6) -> n = 5, step = 1
        let d1 = ChartRulersData::new(14, 10, true, 0.0, None, None);
        assert_eq!(d1.lines.len(), 5);
        assert_eq!(d1.lines[0].caption, "10");
        assert_eq!(d1.lines[4].caption, "14");

        // diff = 100 -> n = 6, step = 20
        let d2 = ChartRulersData::new(100, 0, true, 0.0, None, None);
        assert_eq!(d2.lines.len(), 6);
        assert_eq!(d2.lines[0].caption, "0");
        assert_eq!(d2.lines[1].caption, "20");
        assert_eq!(d2.lines[5].caption, "100");
        assert!((d2.lines[0].relative_value - 1.0).abs() < 1e-12);
        assert!((d2.lines[5].relative_value - 0.0).abs() < 1e-12);
    }

    #[test]
    fn rulers_data_double_axis_scaled_captions() {
        // Left max 1000, right max 100 -> ratio 10.0
        let d = ChartRulersData::new(1000, 0, true, 10.0, None, None);
        assert_eq!(d.lines.len(), 6);
        assert_eq!(d.lines[0].caption, "0");
        assert_eq!(d.lines[0].scaled_line_caption, "0");
        assert_eq!(d.lines[5].caption, "1,000");
        assert_eq!(d.lines[5].scaled_line_caption, "100");
    }

    #[test]
    fn rulers_view_ton_currency_captions() {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000],
                ["y0", 1000000000, 5000000000]
            ],
            "yTickFormatter": "statsFormat('TON')"
        }"#;
        let mut chart = StatisticalChart::from_json_str(json).unwrap();
        chart.currency_rate = 5.0;

        let mut view = ChartRulersView::new();
        view.set_chart_data(&chart, ChartViewType::StackBar, &ChartPalette::light());
        view.add(Limits::new(0.0, 5_000_000_000.0), false);
        assert_eq!(view.rulers().len(), 1);
        assert_eq!(view.rulers()[0].lines.last().unwrap().caption, "5");
        assert_eq!(
            view.rulers()[0].lines.last().unwrap().scaled_line_caption,
            "\u{2248}$25.00"
        );
    }

    #[test]
    fn highest_one_bit_and_bottom_dates() {
        assert_eq!(highest_one_bit(0), 0);
        assert_eq!(highest_one_bit(1), 1);
        assert_eq!(highest_one_bit(5), 4);
        assert_eq!(highest_one_bit(8), 8);
        assert_eq!(highest_one_bit(15), 8);

        let mut x = Vec::new();
        let mut y = Vec::new();
        for i in 0..30 {
            x.push(1_700_006_400_000.0 + (i as f64) * ONE_DAY_MS);
            y.push(i as i64 * 10);
        }
        let mut chart = StatisticalChart {
            x,
            time_step: ONE_DAY_MS,
            ..Default::default()
        };
        chart.lines.push(crate::chart::ChartLine {
            y,
            segment_tree: crate::segment_tree::SegmentTree::default(),
            id: 1,
            id_string: "y0".into(),
            name: "Views".into(),
            max_value: 290,
            min_value: 0,
            color_key: String::new(),
            color: Color::BLACK,
            color_dark: Color::BLACK,
            is_hidden_on_start: false,
        });
        chart.measure();

        let mut state = BottomDatesState::default();
        state.update_chart_full_width(400, Limits::new(0.0, 1.0));
        state.update(&chart, 400);
        let labels = state.compute_labels(&chart, Limits::new(0.0, 1.0), 400);
        assert!(!labels.is_empty());
        assert!(labels.iter().all(|l| l.alpha > 0.0 && l.alpha <= 1.0));
    }
}
