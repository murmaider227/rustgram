//! Point detail popup data model, formatting, and layout geometry.
//!
//! Port of `Telegram/SourceFiles/statistics/widgets/point_details_widget.{h,cpp}`
//! and popup placement from `Telegram/SourceFiles/statistics/chart_widget.cpp`.

use crate::anim::interpolation_ratio;
use crate::chart::{Limits, ONE_DAY_MS, RectF, StatisticalChart, StatisticalCurrency};
use crate::color::Color;
use crate::filter::LinesFilterController;
use crate::format::{
    CreditsAmount, CreditsType, credits_to_usd, format_count_decimal,
    format_credits_amount_decimal, format_week, lang_detailed_day_month,
};
use crate::views::stack_linear::pie_parts_percentage_by_indices;

/// Popup padding around the inner rounded card (`st::statisticsDetailsPopupPadding`).
pub const POPUP_PADDING: f64 = 6.0;
/// Space between lines inside the popup (`st::statisticsDetailsPopupMidLineSpace`).
pub const POPUP_MID_LINE_SPACE: f64 = 4.0;
/// Bottom margin inside the popup card (`st::statisticsDetailsPopupMargins.bottom()`).
pub const POPUP_MARGIN_BOTTOM: f64 = 11.0;

/// One row inside a [`PointDetailsData`] popup (`PointDetailsWidget::Line`).
#[derive(Debug, Clone, PartialEq)]
pub struct PointDetailRow {
    /// Line ID (or `line.id * 100` for currency amount rows when `currency_rate != 0`).
    pub id: i32,
    /// Series display name (or `"TON"`/`"Stars"`/`"USD"` for revenue charts).
    pub name: String,
    /// Formatted value string (decimal count, exact currency amount, or `"≈$..."`).
    pub value: String,
    /// Optional rounded percentage label (e.g. `"42%"`) when `chart.has_percentages` is true.
    pub percentage: Option<String>,
    /// Value text color.
    pub value_color: Color,
    /// Current opacity (synced with [`LinesFilterController`]).
    pub alpha: f64,
}

/// Complete popup payload for a selected X index (`PointDetailsWidget`).
#[derive(Debug, Clone, PartialEq)]
pub struct PointDetailsData {
    /// Selected X index in `chart.x`.
    pub x_index: usize,
    /// Formatted header string (hour, week range, or detailed weekday + date).
    pub header: String,
    /// Ordered detail rows.
    pub lines: Vec<PointDetailRow>,
    /// Whether at least one series has a strictly positive value at `x_index`.
    pub has_positive_values: bool,
    /// Whether clicking the popup triggers zoom (`zoom_enabled && has_positive_values`).
    pub clickable: bool,
    /// Overall popup fade opacity in `0.0..=1.0`.
    pub alpha: f64,
}

impl PointDetailsData {
    /// Builds the point-detail popup data for `x_index` in `chart_data`.
    ///
    /// Port of `PointDetailsWidget::setXIndex` (`point_details_widget.cpp:302-389`).
    #[must_use]
    pub fn build(
        chart_data: &StatisticalChart,
        x_index: usize,
        zoom_enabled: bool,
        lines_filter: Option<&LinesFilterController>,
    ) -> Option<Self> {
        if chart_data.x.is_empty() {
            return None;
        }
        let x_index = x_index.min(chart_data.x.len() - 1);
        let timestamp = chart_data.x[x_index];
        let header = if timestamp < ONE_DAY_MS {
            chart_data.get_day_string(x_index).to_string()
        } else if chart_data.week_format {
            format_week(timestamp)
        } else {
            lang_detailed_day_month((timestamp / 1000.0) as i64)
        };

        let parts = if chart_data.has_percentages {
            Some(
                pie_parts_percentage_by_indices(
                    chart_data,
                    None,
                    Limits::new(x_index as f64, x_index as f64),
                )
                .parts,
            )
        } else {
            None
        };

        let is_credits = chart_data.currency == StatisticalCurrency::Credits;
        let mut lines = Vec::with_capacity(
            chart_data.lines.len()
                * if chart_data.currency_rate != 0.0 {
                    2
                } else {
                    1
                },
        );
        let mut has_positive_values = false;

        for (i, data_line) in chart_data.lines.iter().enumerate() {
            let Some(&raw_value) = data_line.y.get(x_index) else {
                continue;
            };
            let line_alpha = lines_filter.map_or(1.0, |f| f.alpha(data_line.id));
            has_positive_values |= raw_value > 0;

            let percentage = parts
                .as_ref()
                .and_then(|p| p.get(i))
                .map(|part| part.percentage_text.clone());

            if chart_data.currency_rate != 0.0 {
                let amount = if is_credits {
                    CreditsAmount::from_whole(raw_value, CreditsType::Stars)
                } else {
                    CreditsAmount::from_nanoton(raw_value)
                };
                lines.push(PointDetailRow {
                    id: data_line.id * 100,
                    name: if is_credits {
                        "Stars".to_string()
                    } else {
                        "TON".to_string()
                    },
                    value: format_credits_amount_decimal(amount),
                    percentage: None,
                    value_color: data_line.color,
                    alpha: line_alpha,
                });
                lines.push(PointDetailRow {
                    id: data_line.id,
                    name: "USD".to_string(),
                    value: credits_to_usd(amount, chart_data.currency_rate, 0),
                    percentage,
                    value_color: data_line.color,
                    alpha: line_alpha,
                });
            } else {
                lines.push(PointDetailRow {
                    id: data_line.id,
                    name: data_line.name.clone(),
                    value: format_count_decimal(raw_value),
                    percentage,
                    value_color: data_line.color,
                    alpha: line_alpha,
                });
            }
        }

        Some(Self {
            x_index,
            header,
            lines,
            has_positive_values,
            clickable: zoom_enabled && has_positive_values,
            alpha: 1.0,
        })
    }

    /// Computes the vertical Y offset for row `index`, matching `PointDetailsWidget::lineYAt`.
    #[must_use]
    pub fn line_y_at(
        &self,
        index: usize,
        text_top: f64,
        header_height: f64,
        line_height: f64,
    ) -> f64 {
        let mut lines_height = 0.0;
        for i in 0..index {
            let alpha = self.lines.get(i).map_or(1.0, |l| l.alpha);
            lines_height += alpha * (line_height + POPUP_MID_LINE_SPACE);
        }
        text_top + header_height + (POPUP_MARGIN_BOTTOM / 2.0).floor() + lines_height.ceil()
    }
}

/// Computes the horizontal X position for the details popup so it stays within `chart_area`.
///
/// Port of popup placement in `ChartWidget::setupDetails` (`chart_widget.cpp:1207-1225`).
#[must_use]
pub fn compute_popup_x(
    chart_data: &StatisticalChart,
    x_index: usize,
    x_percentage_limits: Limits,
    chart_rect: RectF,
    chart_area: RectF,
    popup_width: f64,
) -> f64 {
    let x_pct = chart_data.x_percentage.get(x_index).copied().unwrap_or(0.0);
    let current_x = chart_rect.width
        * interpolation_ratio(x_percentage_limits.min, x_percentage_limits.max, x_pct);
    let widget_area = popup_width + POPUP_PADDING;
    let x_left = current_x - widget_area;
    let raw_x = if x_left >= 0.0 {
        x_left
    } else if (current_x + widget_area - chart_area.width) > 0.0 {
        0.0
    } else {
        current_x
    };
    let max_x = (chart_area.right() - widget_area).max(chart_area.x);
    raw_x.clamp(chart_area.x, max_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_percentage_and_standard_point_details() {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000],
                ["y0", 750, 300],
                ["y1", 250, 700]
            ],
            "names": { "y0": "English", "y1": "Spanish" },
            "colors": { "y0": "BLUE#327fe5", "y1": "GREEN#61c752" },
            "percentage": true
        }"#;
        let chart = StatisticalChart::from_json_str(json).unwrap();
        let details = PointDetailsData::build(&chart, 0, true, None).unwrap();
        assert_eq!(details.header, "Wed, 15 Nov 2023");
        assert!(details.clickable);
        assert_eq!(details.lines.len(), 2);
        assert_eq!(details.lines[0].name, "English");
        assert_eq!(details.lines[0].value, "750");
        assert_eq!(details.lines[0].percentage.as_deref(), Some("75%"));
        assert_eq!(details.lines[1].percentage.as_deref(), Some("25%"));
    }

    #[test]
    fn builds_ton_revenue_dual_rows() {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000],
                ["y0", 2500000000, 0]
            ],
            "names": { "y0": "Revenue" },
            "colors": { "y0": "BLUE#327fe5" },
            "yTickFormatter": "statsFormat('TON')"
        }"#;
        let mut chart = StatisticalChart::from_json_str(json).unwrap();
        chart.currency_rate = 4.0;

        let details = PointDetailsData::build(&chart, 0, true, None).unwrap();
        assert_eq!(details.lines.len(), 2);
        assert_eq!(details.lines[0].name, "TON");
        assert_eq!(details.lines[0].value, "2.5");
        assert_eq!(details.lines[1].name, "USD");
        assert_eq!(details.lines[1].value, "\u{2248}$10.00");

        let zero_day = PointDetailsData::build(&chart, 1, true, None).unwrap();
        assert!(!zero_day.has_positive_values);
        assert!(!zero_day.clickable);
    }
}
