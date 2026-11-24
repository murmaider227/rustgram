//! Chart data model, JSON deserialization, and range lookup.
//!
//! Port of:
//! - `Telegram/SourceFiles/statistics/statistics_common.h`
//! - `Telegram/SourceFiles/statistics/statistics_data_deserialize.{h,cpp}`
//! - `Telegram/SourceFiles/data/data_statistics_chart.{h,cpp}`

use serde_json::Value;
use thiserror::Error;

use crate::color::{ChartPalette, Color};
use crate::format::{CREDITS_CURRENCY, EM_DASH, UtcDateTime, lang_day_month};
use crate::segment_tree::{ChartValue, SegmentTree};

/// Opacity applied to horizontal and vertical ruler hairlines (`kRulerLineAlpha`).
pub const RULER_LINE_ALPHA: f64 = 0.06;

/// Milliseconds in one day (`3600 * 24 * 1000`).
pub const ONE_DAY_MS: f64 = (3600 * 24 * 1000) as f64;

/// Inclusive `[min, max]` floating-point range (`Statistic::Limits`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Limits {
    /// Lower bound.
    pub min: f64,
    /// Upper bound.
    pub max: f64,
}

impl Limits {
    /// Creates a new [`Limits`] range.
    #[must_use]
    pub const fn new(min: f64, max: f64) -> Self {
        Self { min, max }
    }

    /// Returns `max - min`.
    #[must_use]
    pub fn span(self) -> f64 {
        self.max - self.min
    }
}

/// 2D floating-point point.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PointF {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
}

impl PointF {
    /// Creates a new point.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Axis-aligned rectangle in floating-point coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    /// Left edge X coordinate.
    pub x: f64,
    /// Top edge Y coordinate.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl RectF {
    /// Creates a new rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns the right edge (`x + width`).
    #[must_use]
    pub fn right(self) -> f64 {
        self.x + self.width
    }

    /// Returns the bottom edge (`y + height`).
    #[must_use]
    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    /// Returns the center point.
    #[must_use]
    pub fn center(self) -> PointF {
        PointF::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// Visual representation type for a [`StatisticalChart`] (`Statistic::ChartViewType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartViewType {
    /// Single-axis multi-line chart.
    Linear,
    /// Step-line bar chart (tdesktop `BarChartView(false)`).
    Bar,
    /// Stacked vertical bar chart (tdesktop `BarChartView(true)`).
    StackBar,
    /// Dual Y-axis line chart with independent scaling ratios.
    DoubleLinear,
    /// Percentage stacked area chart, zooming locally into a pie chart.
    StackLinear,
}

/// Currency unit carried by revenue charts (`Data::StatisticalCurrency`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StatisticalCurrency {
    /// Plain integer counts.
    #[default]
    None,
    /// Toncoin (values in nanoton, `10^-9` TON).
    Ton,
    /// Telegram Stars (`XTR`).
    Credits,
}

/// Errors returned when deserializing a Telegram chart JSON payload.
#[derive(Debug, Error)]
pub enum ParseError {
    /// Input was not valid JSON.
    #[error("invalid stats graph json: {0}")]
    Json(#[from] serde_json::Error),
    /// Top-level JSON value was not an object.
    #[error("stats graph json root is not an object")]
    RootNotObject,
    /// `columns` array was missing or empty.
    #[error("stats graph json has an empty or missing columns list")]
    EmptyColumns,
    /// A column inside `columns` was not a non-empty array.
    #[error("stats graph json contains an empty or non-array column")]
    EmptyColumn,
    /// The `x` column was missing or a `y` column had a different length than `x`.
    #[error("stats graph json has mismatched or missing x/y column lengths")]
    MismatchedColumns,
}

/// A single data series (line / bar / slice) inside a [`StatisticalChart`].
#[derive(Debug, Clone)]
pub struct ChartLine {
    /// Y-axis sample values aligned with [`StatisticalChart::x`].
    pub y: Vec<ChartValue>,
    /// Segment tree over `y` for O(log n) range min/max queries.
    pub segment_tree: SegmentTree,
    /// 1-based numeric series identifier assigned during parsing.
    pub id: i32,
    /// Original JSON column key (e.g. `"y0"`).
    pub id_string: String,
    /// Display name (hyphens normalized to em-dash, matching tdesktop).
    pub name: String,
    /// Maximum value in `y`.
    pub max_value: ChartValue,
    /// Minimum value in `y`.
    pub min_value: ChartValue,
    /// Optional theme color key parsed before `#` (e.g. `"GREEN"`).
    pub color_key: String,
    /// Resolved line color.
    pub color: Color,
    /// Optional dark-mode color.
    pub color_dark: Color,
    /// Whether the series is listed in the JSON `hidden` array.
    pub is_hidden_on_start: bool,
}

/// Parsed Telegram statistics chart (`Data::StatisticalChart`).
#[derive(Debug, Clone)]
pub struct StatisticalChart {
    /// X-axis timestamps (milliseconds since epoch) or hour indices.
    pub x: Vec<f64>,
    /// Normalized X positions in `0.0..=1.0`.
    pub x_percentage: Vec<f64>,
    /// Precomputed X-axis tick labels indexed by `(x[i] - x[0]) / time_step`.
    pub days_lookup: Vec<String>,
    /// All Y series in declaration order.
    pub lines: Vec<ChartLine>,
    /// Default footer zoom range in X indices (`defaultZoomXIndex`).
    pub default_zoom_x_index: Limits,
    /// Maximum Y value across all lines.
    pub max_value: ChartValue,
    /// Minimum Y value across all lines.
    pub min_value: ChartValue,
    /// Fraction of the X span covered by one `time_step`.
    pub one_day_percentage: f64,
    /// Step between consecutive X points (at least `1.0`).
    pub time_step: f64,
    /// Whether the footer range selector is hidden (`subchart.show == false`).
    pub is_footer_hidden: bool,
    /// Whether point details and chart values represent percentages (`percentage == true`).
    pub has_percentages: bool,
    /// Whether tooltip dates represent 7-day weeks (`xTooltipFormatter` contains `'week'`).
    pub week_format: bool,
    /// Currency mode inferred from `yTickFormatter`.
    pub currency: StatisticalCurrency,
    /// USD conversion rate (for TON/Stars revenue charts; `0.0` when unused).
    pub currency_rate: f64,
    /// Estimated maximum pixel width of an X-axis day label (used for bottom caption spacing).
    pub day_string_max_width: i32,
    /// Optional chart view type hint inferred from JSON `types`/`stacked`/`percentage`/`y_scaled`.
    pub inferred_type: Option<ChartViewType>,
}

impl Default for StatisticalChart {
    fn default() -> Self {
        Self {
            x: Vec::new(),
            x_percentage: Vec::new(),
            days_lookup: Vec::new(),
            lines: Vec::new(),
            default_zoom_x_index: Limits::default(),
            max_value: 0,
            min_value: ChartValue::MAX,
            one_day_percentage: 0.0,
            time_step: 1.0,
            is_footer_hidden: false,
            has_percentages: false,
            week_format: false,
            currency: StatisticalCurrency::None,
            currency_rate: 0.0,
            day_string_max_width: 0,
            inferred_type: None,
        }
    }
}

/// Wrapper representing a `StatsGraph` TL response (`Data::StatisticalGraph`).
#[derive(Debug, Clone, Default)]
pub struct StatisticalGraph {
    /// Parsed chart data (empty if async or error).
    pub chart: StatisticalChart,
    /// Token passed to `stats.loadAsyncGraph` for lazy loading or zooming.
    pub zoom_token: String,
    /// Error message when `statsGraphError` is returned.
    pub error: String,
}

impl StatisticalChart {
    /// Parses a Telegram `StatsGraph` JSON document into a measured [`StatisticalChart`].
    ///
    /// Port of `Statistic::StatisticalChartFromJSON` (`statistics_data_deserialize.cpp`).
    pub fn from_json(json: &[u8]) -> Result<Self, ParseError> {
        let value: Value = serde_json::from_slice(json)?;
        let Some(root) = value.as_object() else {
            return Err(ParseError::RootNotObject);
        };
        let Some(columns) = root.get("columns").and_then(Value::as_array) else {
            return Err(ParseError::EmptyColumns);
        };
        if columns.is_empty() {
            return Err(ParseError::EmptyColumns);
        }

        let hidden_lines: Vec<String> = root
            .get("hidden")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let mut result = Self::default();

        if let Some(tick_format) = root.get("yTickFormatter").and_then(Value::as_str) {
            if tick_format.contains("TON") {
                result.currency = StatisticalCurrency::Ton;
            } else if tick_format.contains(CREDITS_CURRENCY) {
                result.currency = StatisticalCurrency::Credits;
            }
        }

        let mut column_id_count: i32 = 0;
        for column in columns {
            let Some(array) = column.as_array() else {
                return Err(ParseError::EmptyColumn);
            };
            if array.is_empty() {
                return Err(ParseError::EmptyColumn);
            }
            let column_id = array[0].as_str().unwrap_or_default();
            if column_id == "x" {
                let length = array.len() - 1;
                result.x.reserve(length);
                for item in &array[1..] {
                    result.x.push(item.as_f64().unwrap_or(0.0));
                }
            } else {
                column_id_count += 1;
                let length = array.len() - 1;
                let mut y = Vec::with_capacity(length);
                let mut max_val: ChartValue = 0;
                let mut min_val: ChartValue = ChartValue::MAX;
                for item in &array[1..] {
                    let val = item.as_f64().unwrap_or(0.0).round() as ChartValue;
                    y.push(val);
                    if val > max_val {
                        max_val = val;
                    }
                    if val < min_val {
                        min_val = val;
                    }
                }
                result.lines.push(ChartLine {
                    y,
                    segment_tree: SegmentTree::default(),
                    id: column_id_count,
                    id_string: column_id.to_owned(),
                    name: String::new(),
                    max_value: max_val,
                    min_value: min_val,
                    color_key: String::new(),
                    color: Color::BLACK,
                    color_dark: Color::BLACK,
                    is_hidden_on_start: hidden_lines.iter().any(|h| h == column_id),
                });
            }
        }

        if result.x.empty_or_mismatched(&result.lines) {
            return Err(ParseError::MismatchedColumns);
        }

        if result.x.len() > 1 {
            result.time_step = (result.x[1] - result.x[0]).max(1.0);
        } else {
            result.time_step = ONE_DAY_MS;
        }
        result.measure();

        if result.max_value == result.min_value {
            if result.min_value != 0 {
                result.min_value = 0;
            } else {
                result.max_value = 1;
            }
        }

        // subchart.show and subchart.defaultZoom
        let mut default_min: i32 = 0;
        let mut default_max: i32 = (result.x.len() - 1) as i32;
        if let Some(subchart) = root.get("subchart").and_then(Value::as_object) {
            if let Some(show) = subchart.get("show").and_then(Value::as_bool) {
                result.is_footer_hidden = !show;
            }
            if let Some(zoom_arr) = subchart.get("defaultZoom").and_then(Value::as_array)
                && !zoom_arr.is_empty()
            {
                let min_v = zoom_arr
                    .first()
                    .and_then(Value::as_f64)
                    .unwrap_or(result.x[0]);
                let max_v = zoom_arr
                    .last()
                    .and_then(Value::as_f64)
                    .unwrap_or(*result.x.last().unwrap_or(&0.0));
                for (i, &xv) in result.x.iter().enumerate() {
                    if xv == min_v {
                        default_min = i as i32;
                    }
                    if xv == max_v {
                        default_max = i as i32;
                    }
                }
            }
        }
        result.default_zoom_x_index = Limits::new(
            f64::from(default_min.min(default_max)),
            f64::from(default_min.max(default_max)),
        );

        if let Some(pct) = root.get("percentage").and_then(Value::as_bool) {
            result.has_percentages = pct;
        }

        if let Some(fmt) = root.get("xTooltipFormatter").and_then(Value::as_str) {
            result.week_format = fmt.contains("'week'");
        }

        // Parse colors (`(.*)(#.*)`) and names (`'-'` -> em-dash).
        let colors = root.get("colors").and_then(Value::as_object);
        let names = root.get("names").and_then(Value::as_object);

        for line in &mut result.lines {
            if let Some(raw_color) = colors
                .and_then(|c| c.get(&line.id_string))
                .and_then(Value::as_str)
                && let Some(hash_idx) = raw_color.rfind('#')
            {
                line.color_key = raw_color[..hash_idx].to_owned();
                if let Some(parsed) = Color::from_hex(&raw_color[hash_idx..]) {
                    line.color = parsed;
                }
            }
            if let Some(raw_name) = names
                .and_then(|n| n.get(&line.id_string))
                .and_then(Value::as_str)
            {
                line.name = raw_name.replace('-', &EM_DASH.to_string());
            }
        }

        result.inferred_type = infer_chart_view_type(root);

        Ok(result)
    }

    /// Parses a UTF-8 JSON string into a [`StatisticalChart`].
    pub fn from_json_str(json: &str) -> Result<Self, ParseError> {
        Self::from_json(json.as_bytes())
    }

    /// Returns `true` if the chart has no lines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Resolves named `color_key` values (e.g. `"GREEN"`, `"BLUE"`) using `palette`.
    ///
    /// Port of `FillLineColorsByKey` in `statistics/chart_widget.cpp`.
    pub fn apply_palette(&mut self, palette: &ChartPalette) {
        for line in &mut self.lines {
            if let Some(resolved) = palette.line_color_by_key(&line.color_key) {
                line.color = resolved;
            }
        }
    }

    /// Computes `x_percentage`, `min_value`/`max_value`, per-line [`SegmentTree`]s,
    /// `days_lookup`, `day_string_max_width`, and `one_day_percentage`.
    ///
    /// Port of `Data::StatisticalChart::measure` (`data_statistics_chart.cpp:19-74`).
    pub fn measure(&mut self) {
        if self.x.is_empty() {
            return;
        }
        let n = self.x.len();
        let start = self.x[0];
        let end = self.x[n - 1];
        let span = end - start;

        self.x_percentage.clear();
        self.x_percentage.resize(n, 0.0);
        if n == 1 || span == 0.0 {
            self.x_percentage[0] = 1.0;
        } else {
            for i in 0..n {
                self.x_percentage[i] = (self.x[i] - start) / span;
            }
        }

        for line in &mut self.lines {
            if line.max_value > self.max_value {
                self.max_value = line.max_value;
            }
            if line.min_value < self.min_value {
                self.min_value = line.min_value;
            }
            line.segment_tree = SegmentTree::new(line.y.clone());
        }

        self.days_lookup.clear();
        let safe_step = self.time_step.max(1.0);
        let date_count = (((end - start) / safe_step) as usize).saturating_add(10);
        self.days_lookup.reserve(date_count);

        // Approximate 10px font glyph advance (6px per character), matching
        // `st::statisticsDetailsBottomCaptionStyle.font` width estimates.
        const APPROX_CHAR_WIDTH: i32 = 6;
        let mut max_width = 0i32;

        for i in 0..date_count {
            let r = ((start + (i as f64) * safe_step) / 1000.0) as i64;
            let label = if safe_step == 1.0 {
                format!("{i:02}:00")
            } else if safe_step < ONE_DAY_MS {
                let dt = UtcDateTime::from_unix_seconds(r);
                format!("{:02}:{:02}", dt.hour, dt.minute)
            } else {
                lang_day_month(r)
            };
            let w = (label.chars().count() as i32) * APPROX_CHAR_WIDTH;
            if w > max_width {
                max_width = w;
            }
            self.days_lookup.push(label);
        }
        self.day_string_max_width = max_width;

        self.one_day_percentage = if span == 0.0 { 1.0 } else { safe_step / span };
    }

    /// Returns the precomputed X-axis label for point index `i`.
    ///
    /// Port of `Data::StatisticalChart::getDayString`.
    #[must_use]
    pub fn get_day_string(&self, i: usize) -> &str {
        if self.x.is_empty() || self.days_lookup.is_empty() || i >= self.x.len() {
            return "";
        }
        let idx = ((self.x[i] - self.x[0]) / self.time_step.max(1.0)) as usize;
        self.days_lookup.get(idx).map(String::as_str).unwrap_or("")
    }

    /// Binary search for the first X index at or after normalized position `v`.
    ///
    /// Port of `Data::StatisticalChart::findStartIndex` (`data_statistics_chart.cpp:80-108`).
    #[must_use]
    pub fn find_start_index(&self, v: f64) -> usize {
        if v == 0.0 {
            return 0;
        }
        let n = self.x_percentage.len();
        if n < 2 {
            return 0;
        }
        let mut left: isize = 0;
        let mut right: isize = (n - 1) as isize;

        while left <= right {
            let middle = (right + left) >> 1;
            let mid_val = self.x_percentage[middle as usize];
            if v < mid_val && (middle == 0 || v > self.x_percentage[(middle - 1) as usize]) {
                return middle as usize;
            }
            if v == mid_val {
                return middle as usize;
            }
            if v < mid_val {
                right = middle - 1;
            } else {
                left = middle + 1;
            }
        }
        left.clamp(0, (n - 1) as isize) as usize
    }

    /// Binary search for the last X index at or before normalized position `v`, starting from `left`.
    ///
    /// Port of `Data::StatisticalChart::findEndIndex` (`data_statistics_chart.cpp:110-134`).
    #[must_use]
    pub fn find_end_index(&self, left: usize, v: f64) -> usize {
        let n = self.x_percentage.len();
        if n == 0 {
            return 0;
        }
        if v == 1.0 {
            return n - 1;
        }
        let was_left = left as isize;
        let mut left = left as isize;
        let mut right = (n - 1) as isize;

        while left <= right {
            let middle = (right + left) >> 1;
            let mid_val = self.x_percentage[middle as usize];
            if v > mid_val
                && (middle == (n - 1) as isize || v < self.x_percentage[(middle + 1) as usize])
            {
                return middle as usize;
            }
            if v == mid_val {
                return middle as usize;
            }
            if v < mid_val {
                right = middle - 1;
            } else {
                left = middle + 1;
            }
        }
        was_left.max(right).clamp(0, (n - 1) as isize) as usize
    }

    /// Returns the default zoom X-percentage limits derived from `default_zoom_x_index`.
    #[must_use]
    pub fn default_zoom_x_percentage(&self) -> Limits {
        if self.x_percentage.is_empty() {
            return Limits::new(0.0, 1.0);
        }
        let max_idx = self.x_percentage.len() - 1;
        let min_i = (self.default_zoom_x_index.min as usize).min(max_idx);
        let max_i = (self.default_zoom_x_index.max as usize).min(max_idx);
        Limits::new(self.x_percentage[min_i], self.x_percentage[max_i])
    }
}

trait SliceValidation {
    fn empty_or_mismatched(&self, lines: &[ChartLine]) -> bool;
}

impl SliceValidation for Vec<f64> {
    fn empty_or_mismatched(&self, lines: &[ChartLine]) -> bool {
        self.is_empty() || lines.iter().any(|l| l.y.len() != self.len())
    }
}

fn infer_chart_view_type(root: &serde_json::Map<String, Value>) -> Option<ChartViewType> {
    let percentage = root
        .get("percentage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let stacked = root
        .get("stacked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let y_scaled = root
        .get("y_scaled")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if percentage {
        return Some(ChartViewType::StackLinear);
    }
    if y_scaled {
        return Some(ChartViewType::DoubleLinear);
    }

    let types = root.get("types").and_then(Value::as_object);
    if let Some(types) = types {
        let has_area = types.values().any(|v| v.as_str() == Some("area"));
        if has_area {
            return Some(ChartViewType::StackLinear);
        }
        let has_bar = types.values().any(|v| v.as_str() == Some("bar"));
        let has_step = types.values().any(|v| v.as_str() == Some("step"));
        if has_bar {
            return Some(if stacked {
                ChartViewType::StackBar
            } else {
                ChartViewType::Bar
            });
        }
        if has_step {
            return Some(ChartViewType::Bar);
        }
        let has_line = types.values().any(|v| v.as_str() == Some("line"));
        if has_line {
            return Some(ChartViewType::Linear);
        }
    }

    if stacked {
        Some(ChartViewType::StackBar)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_JSON: &str = r#"{
        "columns": [
            ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000, 1700352000000],
            ["y0", 100, 150, 120, 200, 180],
            ["y1", 10, 25, 15, 30, 20]
        ],
        "types": { "x": "x", "y0": "line", "y1": "line" },
        "names": { "y0": "Joined", "y1": "Left-out" },
        "colors": { "y0": "GREEN#61c752", "y1": "RED#e05356" },
        "hidden": ["y1"],
        "subchart": {
            "show": true,
            "defaultZoom": [1700092800000, 1700352000000]
        },
        "percentage": false,
        "yTickFormatter": "statsFormat('TON')",
        "xTooltipFormatter": "statsTooltipFormat('week')"
    }"#;

    #[test]
    fn parses_telegram_chart_json_faithfully() {
        let mut chart = StatisticalChart::from_json_str(SAMPLE_JSON).unwrap();
        assert_eq!(chart.x.len(), 5);
        assert_eq!(chart.lines.len(), 2);
        assert_eq!(chart.currency, StatisticalCurrency::Ton);
        assert!(chart.week_format);
        assert!(!chart.is_footer_hidden);
        assert!(!chart.has_percentages);
        assert_eq!(chart.default_zoom_x_index, Limits::new(1.0, 4.0));
        assert_eq!(chart.min_value, 10);
        assert_eq!(chart.max_value, 200);
        assert_eq!(chart.time_step, ONE_DAY_MS);
        assert!((chart.one_day_percentage - 0.25).abs() < 1e-12);

        assert_eq!(chart.lines[0].id, 1);
        assert_eq!(chart.lines[0].name, "Joined");
        assert_eq!(chart.lines[0].color_key, "GREEN");
        assert_eq!(chart.lines[0].color, Color::rgb(0x61, 0xc7, 0x52));
        assert!(!chart.lines[0].is_hidden_on_start);

        assert_eq!(chart.lines[1].id, 2);
        assert_eq!(chart.lines[1].name, "Left\u{2014}out");
        assert_eq!(chart.lines[1].color_key, "RED");
        assert!(chart.lines[1].is_hidden_on_start);

        assert_eq!(chart.get_day_string(0), "15 Nov");
        assert_eq!(chart.get_day_string(4), "19 Nov");

        chart.apply_palette(&ChartPalette::light());
        assert_eq!(chart.lines[0].color, ChartPalette::light().line_green);
    }

    #[test]
    fn find_start_and_end_indices() {
        let chart = StatisticalChart::from_json_str(SAMPLE_JSON).unwrap();
        // x_percentage = [0.0, 0.25, 0.5, 0.75, 1.0]
        assert_eq!(chart.find_start_index(0.0), 0);
        assert_eq!(chart.find_start_index(0.1), 1);
        assert_eq!(chart.find_start_index(0.25), 1);
        assert_eq!(chart.find_start_index(0.6), 3);
        assert_eq!(chart.find_end_index(0, 1.0), 4);
        assert_eq!(chart.find_end_index(0, 0.75), 3);
        assert_eq!(chart.find_end_index(1, 0.6), 2);
    }

    #[test]
    fn flat_zero_chart_bumps_max_value_to_one() {
        let json = r#"{
            "columns": [
                ["x", 0, 1, 2],
                ["y0", 0, 0, 0]
            ]
        }"#;
        let chart = StatisticalChart::from_json_str(json).unwrap();
        assert_eq!(chart.min_value, 0);
        assert_eq!(chart.max_value, 1);
        assert_eq!(chart.get_day_string(0), "00:00");
        assert_eq!(chart.get_day_string(2), "02:00");
    }

    #[test]
    fn rejects_invalid_or_mismatched_json() {
        assert!(StatisticalChart::from_json_str("not json").is_err());
        assert!(StatisticalChart::from_json_str("{}").is_err());
        assert!(StatisticalChart::from_json_str(r#"{"columns": []}"#).is_err());
        assert!(
            StatisticalChart::from_json_str(r#"{"columns": [["x", 1, 2], ["y0", 1]]}"#).is_err()
        );
    }
}
