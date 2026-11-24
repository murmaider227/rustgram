//! Telegram statistics charts, math, overview models, and `tiny-skia` software renderer.
//!
//! Port of `Telegram/SourceFiles/statistics/` and the statistics/boosts/earn data &
//! overview formatting logic from `Telegram/SourceFiles/data/` and
//! `Telegram/SourceFiles/info/statistics/` + `Telegram/SourceFiles/info/channel_statistics/`.

pub mod anim;
pub mod chart;
pub mod color;
pub mod controller;
pub mod details;
pub mod filter;
pub mod footer;
pub mod format;
pub mod overview;
pub mod render;
pub mod rulers;
pub mod segment_tree;
pub mod views;
pub mod xlsx;

pub use anim::{AnimValue, ease_in_cubic, ease_out_circ, ease_out_cubic, interpolate_f, linear};
pub use chart::{
    ChartLine, ChartViewType, Limits, ONE_DAY_MS, ParseError, PointF, RULER_LINE_ALPHA, RectF,
    StatisticalChart, StatisticalCurrency, StatisticalGraph,
};
pub use color::{ChartPalette, Color};
pub use controller::{ChartAnimationController, ZoomRequest, ZoomedChartState};
pub use details::{PointDetailRow, PointDetailsData, compute_popup_x};
pub use filter::LinesFilterController;
pub use footer::{CenterAnimationTarget, DragArea, FooterRangeSelector};
pub use format::{
    CreditsAmount, CreditsType, ONE_STAR_IN_NANO, ShortenedCount, UtcDateTime, credits_to_usd,
    format_count_decimal, format_count_to_short, format_credits_amount_decimal,
    format_exact_count_decimal, format_week, header_subtitle, lang_day_month, lang_day_month_year,
    lang_detailed_day_month, major_part_credits, major_part_nanoton, minor_part_credits,
    minor_part_nanoton, to_usd,
};
pub use overview::{
    AdministratorActionsInfo, Boost, BoostPrepaidGiveaway, BoostStatus, BoostsListSlice,
    BoostsOffsetToken, BoostsOverview, ChannelStatistics, CreditsEarnStatistics, DeltaTrend,
    EarnHistoryEntry, EarnHistorySlice, EarnOverviewRow, EarnStatistics, GiftCodeLink, InviterInfo,
    MessageInteractionInfo, MessageSenderInfo, MessageStatistics, OverviewBlock, OverviewCard,
    OverviewDelta, StatisticalValue, StatisticsLists, StoryStatistics, SupergroupStatistics,
    boosts_overview_block, channel_overview_blocks, earn_chart_view_type, earn_overview_rows,
    format_notifications_percentage, format_value_delta, message_overview_block,
    supergroup_overview_block,
};
pub use render::{RenderChartType, RenderError, RenderOptions, RenderedChart, render_chart};
pub use rulers::{
    BottomCaptionLabel, BottomCaptionLineData, BottomDatesState, ChartRulersData, ChartRulersView,
    RulerLine, format_ruler_float, format_ruler_value, highest_one_bit, round_ruler_max,
};
pub use segment_tree::{ChartValue, SegmentTree};
pub use views::{
    BarChartView, ChangingPiePartController, ChartView, DoubleLineRatios, HeightLimits,
    LeftStartAndStep, LinearChartView, LocalZoomArgs, LocalZoomResult, LocalZoomType, PaintContext,
    PiePart, PiePartController, PiePartData, StackLinearChartView, compute_left_start_and_step,
    default_height_limits, find_additional_zoomed_out_x_indices,
    find_stack_x_indices_from_raw_x_percentages, pie_parts_percentage,
    pie_parts_percentage_by_indices,
};
pub use xlsx::{
    AnyStatistics, Cell, CellType, Sheet, XlsxError, serialize_xlsx, sheet_from_chart,
    sheets_from_statistics,
};
