//! Software RGBA chart renderer built on `tiny-skia`.
//!
//! Renders every Telegram statistics chart type (`Linear`, `DoubleLinear`, `Bar`,
//! `StackBar`, `StackLinear`, and zoomed `Pie`) with horizontal rulers, dual-axis
//! captions, bottom date labels, footer range selector preview, and point/bar/slice
//! selection highlights into an RGBA [`tiny_skia::Pixmap`] (exportable to PNG).

use std::f64::consts::PI;

use thiserror::Error;
use tiny_skia::{
    FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use crate::chart::{ChartViewType, Limits, RULER_LINE_ALPHA, RectF, StatisticalChart};
use crate::color::{ChartPalette, Color};
use crate::filter::LinesFilterController;
use crate::footer::{FOOTER_ARROW_HEIGHT, FOOTER_HEIGHT, FOOTER_SIDE_RADIUS, FooterRangeSelector};
use crate::format::header_subtitle;
use crate::rulers::{BottomDatesState, ChartRulersView};
use crate::views::bar::BAR_SELECTED_DIM_ALPHA;
use crate::views::stack_linear::PIE_CIRCLE_SIZE_RATIO;
use crate::views::{ChartView, PaintContext};

/// Chart rendering mode (extends [`ChartViewType`] with explicit zoomed `Pie` rendering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderChartType {
    /// Single-axis multi-line chart.
    Linear,
    /// Dual Y-axis scaled multi-line chart.
    DoubleLinear,
    /// Step-line bar chart (`BarChartView(false)`).
    Bar,
    /// Stacked vertical bar chart (`BarChartView(true)`).
    StackBar,
    /// Percentage stacked area chart (`StackLinear`).
    StackLinear,
    /// Zoomed `StackLinear` rendered as a pie chart.
    Pie,
}

impl From<ChartViewType> for RenderChartType {
    fn from(value: ChartViewType) -> Self {
        match value {
            ChartViewType::Linear => Self::Linear,
            ChartViewType::DoubleLinear => Self::DoubleLinear,
            ChartViewType::Bar => Self::Bar,
            ChartViewType::StackBar => Self::StackBar,
            ChartViewType::StackLinear => Self::StackLinear,
        }
    }
}

impl RenderChartType {
    /// Converts to the underlying [`ChartViewType`].
    #[must_use]
    pub const fn view_type(self) -> ChartViewType {
        match self {
            Self::Linear => ChartViewType::Linear,
            Self::DoubleLinear => ChartViewType::DoubleLinear,
            Self::Bar => ChartViewType::Bar,
            Self::StackBar => ChartViewType::StackBar,
            Self::StackLinear | Self::Pie => ChartViewType::StackLinear,
        }
    }
}

/// Errors returned by chart rendering or PNG encoding.
#[derive(Debug, Error)]
pub enum RenderError {
    /// Requested output dimensions were invalid (`width == 0` or `height == 0`).
    #[error("invalid render dimensions: {width}x{height}")]
    InvalidDimensions {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// Chart had no data to draw.
    #[error("chart has no data points or series")]
    EmptyChart,
    /// PNG encoding failed.
    #[error("failed to encode PNG: {0}")]
    Png(String),
}

/// Configuration for rendering a chart to an RGBA image.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Image width in pixels (default `640`).
    pub width: u32,
    /// Image height in pixels (default `360`).
    pub height: u32,
    /// Chart type to render.
    pub chart_type: RenderChartType,
    /// Optional title drawn in the top header.
    pub title: Option<String>,
    /// Optional explicit X percentage range (defaults to `chart.default_zoom_x_percentage()`).
    pub x_percentage_limits: Option<Limits>,
    /// Optional selected X index (draws highlight guide/dots or highlights stacked bar column).
    pub selected_x_index: Option<usize>,
    /// Optional selected slice line ID when rendering `RenderChartType::Pie`.
    pub selected_pie_line_id: Option<i32>,
    /// Whether to draw the footer range selector preview (respects `chart.is_footer_hidden`).
    pub show_footer: bool,
    /// Color palette (defaults to Telegram Desktop light palette).
    pub palette: ChartPalette,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            chart_type: RenderChartType::Linear,
            title: None,
            x_percentage_limits: None,
            selected_x_index: None,
            selected_pie_line_id: None,
            show_footer: true,
            palette: ChartPalette::light(),
        }
    }
}

/// Rendered RGBA image buffer.
#[derive(Debug, Clone)]
pub struct RenderedChart {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Premultiplied RGBA pixels (`width * height * 4` bytes, straight from `tiny-skia`).
    pub rgba: Vec<u8>,
    /// PNG bytes (lazily encoded via [`RenderedChart::encode_png`]).
    pixmap: Pixmap,
}

impl RenderedChart {
    /// Encodes the rendered image as a PNG byte vector.
    pub fn encode_png(&self) -> Result<Vec<u8>, RenderError> {
        self.pixmap
            .encode_png()
            .map_err(|e| RenderError::Png(e.to_string()))
    }

    /// Saves the rendered image to `path` as a PNG file.
    pub fn save_png(&self, path: impl AsRef<std::path::Path>) -> Result<(), RenderError> {
        self.pixmap
            .save_png(path)
            .map_err(|e| RenderError::Png(e.to_string()))
    }
}

/// Renders `chart` into an RGBA image according to `options` and `lines_filter`.
pub fn render_chart(
    chart: &StatisticalChart,
    lines_filter: &LinesFilterController,
    options: &RenderOptions,
) -> Result<RenderedChart, RenderError> {
    if options.width == 0 || options.height == 0 {
        return Err(RenderError::InvalidDimensions {
            width: options.width,
            height: options.height,
        });
    }
    if chart.is_empty() || chart.x.is_empty() {
        return Err(RenderError::EmptyChart);
    }

    let mut pixmap =
        Pixmap::new(options.width, options.height).ok_or(RenderError::InvalidDimensions {
            width: options.width,
            height: options.height,
        })?;
    pixmap.fill(options.palette.box_bg.to_skia());

    let w = f64::from(options.width);
    let h = f64::from(options.height);
    let pad_x = 16.0;
    let inner_w = (w - pad_x * 2.0).max(1.0);

    let x_pct = options
        .x_percentage_limits
        .unwrap_or_else(|| chart.default_zoom_x_percentage());
    let x_pct = if x_pct.span() <= 0.0 {
        Limits::new(0.0, 1.0)
    } else {
        x_pct
    };
    let start_idx = chart.find_start_index(x_pct.min);
    let end_idx = chart.find_end_index(start_idx, x_pct.max);
    let x_indices = Limits::new(start_idx as f64, end_idx as f64);

    // Header layout
    let header_top = 12.0;
    let title_text = options.title.as_deref().unwrap_or("");
    let subtitle_text = header_subtitle(&chart.x, chart.week_format, start_idx, end_idx);
    let has_header = !title_text.is_empty() || !subtitle_text.is_empty();
    let header_height = if has_header { 36.0 } else { 8.0 };

    if !title_text.is_empty() {
        draw_text(
            &mut pixmap,
            title_text,
            pad_x as i32,
            header_top as i32,
            2,
            options.palette.window_active_text_fg,
        );
    }
    if !subtitle_text.is_empty() {
        let sub_y = if title_text.is_empty() {
            header_top as i32
        } else {
            (header_top + 18.0) as i32
        };
        draw_text(
            &mut pixmap,
            &subtitle_text,
            pad_x as i32,
            sub_y,
            1,
            options.palette.window_sub_text_fg,
        );
    }

    let show_footer = options.show_footer && !chart.is_footer_hidden && h >= 140.0;
    let footer_area_h = if show_footer {
        FOOTER_HEIGHT + 16.0
    } else {
        8.0
    };
    let bottom_caption_h = if options.chart_type == RenderChartType::Pie {
        8.0
    } else {
        22.0
    };

    let chart_top = header_top + header_height + 12.0;
    let chart_bottom = (h - footer_area_h - bottom_caption_h).max(chart_top + 20.0);
    let chart_rect = RectF::new(pad_x, chart_top, inner_w, chart_bottom - chart_top);

    let view_type = options.chart_type.view_type();
    let mut view = ChartView::create(view_type);
    let height_limits = view.height_limits(chart, lines_filter, x_indices);

    let mut rulers = ChartRulersView::new();
    rulers.set_chart_data(chart, view_type, &options.palette);
    rulers.add(height_limits.ranged, false);

    let paint_ctx = PaintContext {
        chart_data: chart,
        x_indices,
        x_percentage_limits: x_pct,
        height_limits: height_limits.ranged,
        rect: chart_rect,
        footer: false,
    };

    let is_pie = options.chart_type == RenderChartType::Pie;
    let rulers_above = options.chart_type == RenderChartType::StackBar;

    if !is_pie && !rulers_above {
        draw_rulers(&mut pixmap, &rulers, chart_rect, &options.palette);
    }

    match options.chart_type {
        RenderChartType::Linear | RenderChartType::DoubleLinear => {
            if let ChartView::Linear(ref mut lv) = view {
                draw_linear(&mut pixmap, lv, &paint_ctx, lines_filter, 2.0);
                if let Some(sel) = options.selected_x_index {
                    draw_linear_selection(
                        &mut pixmap,
                        lv,
                        &paint_ctx,
                        lines_filter,
                        sel,
                        &options.palette,
                    );
                }
            }
        }
        RenderChartType::Bar | RenderChartType::StackBar => {
            if let ChartView::Bar(ref mut bv) = view {
                bv.set_selected(options.selected_x_index, 1.0);
                draw_bar(&mut pixmap, bv, &paint_ctx, lines_filter, &options.palette);
            }
        }
        RenderChartType::StackLinear => {
            if let ChartView::StackLinear(ref mut sv) = view {
                draw_stack_linear(&mut pixmap, sv, &paint_ctx, lines_filter, &options.palette);
                if let Some(sel) = options.selected_x_index {
                    draw_vertical_guide(&mut pixmap, &paint_ctx, sel, &options.palette);
                }
            }
        }
        RenderChartType::Pie => {
            if let ChartView::StackLinear(ref mut sv) = view {
                if let Some(sel_id) = options.selected_pie_line_id {
                    sv.pie_part_controller.set(Some(sel_id), 0.0);
                }
                draw_pie(
                    &mut pixmap,
                    sv,
                    chart,
                    lines_filter,
                    x_indices,
                    chart_rect,
                    &options.palette,
                );
            }
        }
    }

    if !is_pie {
        if rulers_above {
            draw_rulers(&mut pixmap, &rulers, chart_rect, &options.palette);
        }
        draw_ruler_captions(
            &mut pixmap,
            &rulers,
            lines_filter,
            chart_rect,
            &options.palette,
        );
        // Bottom baseline hairline
        fill_rect(
            &mut pixmap,
            RectF::new(chart_rect.x, chart_rect.bottom(), chart_rect.width, 1.0),
            options
                .palette
                .box_text_fg
                .with_alpha_factor(RULER_LINE_ALPHA),
        );

        // Bottom X date labels
        let mut dates_state = BottomDatesState::default();
        dates_state.update_chart_full_width(chart_rect.width as i32, x_pct);
        dates_state.update(chart, chart_rect.width as i32);
        let labels = dates_state.compute_labels(chart, x_pct, chart_rect.width as i32);
        let label_y = (chart_rect.bottom() + 6.0) as i32;
        for lbl in labels {
            let color = options
                .palette
                .window_sub_text_fg
                .with_alpha_factor(lbl.alpha);
            let text_w = measure_text_width(&lbl.text, 1) as f64;
            let draw_x = (chart_rect.x + lbl.x_center - text_w / 2.0) as i32;
            draw_text(&mut pixmap, &lbl.text, draw_x, label_y, 1, color);
        }
    }

    // Footer miniature preview + range handles
    if show_footer {
        let footer_rect = RectF::new(pad_x, h - FOOTER_HEIGHT - 8.0, inner_w, FOOTER_HEIGHT);
        draw_footer(
            &mut pixmap,
            chart,
            lines_filter,
            &mut view,
            options.chart_type,
            height_limits.full,
            x_pct,
            footer_rect,
            &options.palette,
        );
    }

    let rgba = pixmap.data().to_vec();
    Ok(RenderedChart {
        width: options.width,
        height: options.height,
        rgba,
        pixmap,
    })
}

fn fill_rect(pixmap: &mut Pixmap, rect: RectF, color: Color) {
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.width.is_finite()
        || !rect.height.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
        || color.a == 0
    {
        return;
    }
    let Some(r) = Rect::from_xywh(
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
    ) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(color.to_skia());
    paint.anti_alias = false;
    pixmap.fill_rect(r, &paint, Transform::identity(), None);
}

fn fill_rounded_rect(pixmap: &mut Pixmap, rect: RectF, radius: f64, color: Color) {
    if rect.width <= 0.0 || rect.height <= 0.0 || color.a == 0 {
        return;
    }
    let r = radius.min(rect.width / 2.0).min(rect.height / 2.0) as f32;
    let x0 = rect.x as f32;
    let y0 = rect.y as f32;
    let x1 = rect.right() as f32;
    let y1 = rect.bottom() as f32;
    let k = 0.552_284_8 * r;

    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();

    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn draw_rulers(pixmap: &mut Pixmap, rulers: &ChartRulersView, rect: RectF, palette: &ChartPalette) {
    for ruler in rulers.rulers() {
        let color = palette
            .box_text_fg
            .with_alpha_factor(ruler.alpha * RULER_LINE_ALPHA);
        for line in &ruler.lines {
            let y = rect.y + rect.height * line.relative_value;
            fill_rect(pixmap, RectF::new(rect.x, y, rect.width, 1.0), color);
        }
    }
}

fn draw_ruler_captions(
    pixmap: &mut Pixmap,
    rulers: &ChartRulersView,
    lines_filter: &LinesFilterController,
    rect: RectF,
    palette: &ChartPalette,
) {
    let (left_color, right_color) = rulers.axis_colors();
    for ruler in rulers.rulers() {
        for line in &ruler.lines {
            let y = (rect.y + rect.height * line.relative_value - 11.0) as i32;
            let (left_text, left_alpha, right) = rulers.line_captions(line, Some(lines_filter));
            if !left_text.is_empty() && left_alpha > 0.0 {
                let c = if rulers.is_double() && rulers.axis_line_ids().0 != 0 {
                    left_color
                } else {
                    palette.window_sub_text_fg
                }
                .with_alpha_factor(ruler.alpha * left_alpha);
                draw_text(pixmap, left_text, rect.x as i32, y, 1, c);
            }
            if let Some((right_text, right_alpha)) = right
                && !right_text.is_empty()
                && right_alpha > 0.0
            {
                let tw = measure_text_width(right_text, 1);
                let rx = (rect.right() as i32) - tw;
                let c = right_color.with_alpha_factor(ruler.alpha * right_alpha);
                draw_text(pixmap, right_text, rx, y, 1, c);
            }
        }
    }
}

fn make_clip_mask(pixmap: &Pixmap, rect: RectF) -> Option<Mask> {
    let mut mask = Mask::new(pixmap.width(), pixmap.height())?;
    let r = Rect::from_xywh(
        rect.x as f32,
        rect.y as f32,
        rect.width.max(1.0) as f32,
        rect.height.max(1.0) as f32,
    )?;
    let path = PathBuilder::from_rect(r);
    mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
    Some(mask)
}

fn build_polyline(points: &[crate::chart::PointF]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut last: Option<(f32, f32)> = None;
    let mut count = 0usize;
    for p in points {
        if !p.x.is_finite() || !p.y.is_finite() {
            continue;
        }
        let x = p.x as f32;
        let y = p.y as f32;
        if let Some((lx, ly)) = last {
            if (x - lx).abs() < 1e-4 && (y - ly).abs() < 1e-4 {
                continue;
            }
            pb.line_to(x, y);
        } else {
            pb.move_to(x, y);
        }
        last = Some((x, y));
        count += 1;
    }
    if count >= 2 { pb.finish() } else { None }
}

fn draw_linear(
    pixmap: &mut Pixmap,
    view: &mut crate::views::LinearChartView,
    ctx: &PaintContext<'_>,
    lines_filter: &LinesFilterController,
    stroke_width: f32,
) {
    let stroke = Stroke {
        width: stroke_width.max(1.25),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let mask = make_clip_mask(pixmap, ctx.rect);

    for i in 0..ctx.chart_data.lines.len() {
        let line = &ctx.chart_data.lines[i];
        let alpha = lines_filter.alpha(line.id);
        if alpha <= 0.0 {
            continue;
        }
        let points = view.compute_line_points(i, ctx);
        if let Some(path) = build_polyline(&points) {
            let mut paint = Paint::default();
            paint.set_color(line.color.with_alpha_factor(alpha).to_skia());
            paint.anti_alias = true;
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), mask.as_ref());
        }
    }
}

fn draw_linear_selection(
    pixmap: &mut Pixmap,
    view: &mut crate::views::LinearChartView,
    ctx: &PaintContext<'_>,
    lines_filter: &LinesFilterController,
    selected_x_index: usize,
    palette: &ChartPalette,
) {
    let pts = view.selected_points(ctx, selected_x_index);
    if pts.is_empty() {
        return;
    }
    let guide_x = pts[0].1.x;
    fill_rect(
        pixmap,
        RectF::new(guide_x - 0.5, ctx.rect.y, 1.0, ctx.rect.height),
        palette
            .box_text_fg
            .with_alpha_factor(RULER_LINE_ALPHA * 3.0),
    );
    let stroke = Stroke {
        width: 2.0,
        ..Stroke::default()
    };
    for (line, (_, pt)) in ctx.chart_data.lines.iter().zip(pts.iter()) {
        let alpha = lines_filter.alpha(line.id);
        if alpha <= 0.0 {
            continue;
        }
        if let Some(circle) = PathBuilder::from_circle(pt.x as f32, pt.y as f32, 4.5) {
            let mut fill = Paint::default();
            fill.set_color(palette.box_bg.with_alpha_factor(alpha).to_skia());
            fill.anti_alias = true;
            pixmap.fill_path(
                &circle,
                &fill,
                FillRule::Winding,
                Transform::identity(),
                None,
            );

            let mut edge = Paint::default();
            edge.set_color(line.color.with_alpha_factor(alpha).to_skia());
            edge.anti_alias = true;
            pixmap.stroke_path(&circle, &edge, &stroke, Transform::identity(), None);
        }
    }
}

fn draw_vertical_guide(
    pixmap: &mut Pixmap,
    ctx: &PaintContext<'_>,
    selected_x_index: usize,
    palette: &ChartPalette,
) {
    let Some(&xp) = ctx.chart_data.x_percentage.get(selected_x_index) else {
        return;
    };
    let span = ctx.x_percentage_limits.span();
    if span == 0.0 {
        return;
    }
    let x = ctx.rect.x + ctx.rect.width * ((xp - ctx.x_percentage_limits.min) / span);
    fill_rect(
        pixmap,
        RectF::new(x - 0.5, ctx.rect.y, 1.0, ctx.rect.height),
        palette
            .box_text_fg
            .with_alpha_factor(RULER_LINE_ALPHA * 3.0),
    );
}

fn draw_bar(
    pixmap: &mut Pixmap,
    view: &mut crate::views::BarChartView,
    ctx: &PaintContext<'_>,
    lines_filter: &LinesFilterController,
    palette: &ChartPalette,
) {
    let geom = view.compute_geometry(ctx, lines_filter);
    let has_selection = geom.iter().any(|g| g.selected_rect.is_some());
    let base_alpha = if view.is_stack() && has_selection && !ctx.footer {
        BAR_SELECTED_DIM_ALPHA
    } else {
        1.0
    };

    let stroke = Stroke {
        width: if ctx.footer { 1.25 } else { 2.0 },
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let mask = make_clip_mask(pixmap, ctx.rect);

    for (line, series) in ctx.chart_data.lines.iter().zip(geom.iter()) {
        if view.is_stack() {
            let color = line.color.with_alpha_factor(base_alpha);
            for r in &series.rects {
                let clipped = clip_rect(*r, ctx.rect);
                fill_rect(pixmap, clipped, color);
            }
        } else if series.step_points.len() >= 2 {
            let alpha = lines_filter.alpha(line.id);
            if alpha <= 0.0 {
                continue;
            }
            if let Some(path) = build_polyline(&series.step_points) {
                let mut paint = Paint::default();
                paint.set_color(line.color.with_alpha_factor(alpha).to_skia());
                paint.anti_alias = true;
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), mask.as_ref());
            }
        }
    }

    // Highlight selected stacked column at full opacity.
    if view.is_stack() && has_selection && !ctx.footer {
        for (line, series) in ctx.chart_data.lines.iter().zip(geom.iter()) {
            if let Some(sel_rect) = series.selected_rect {
                let clipped = clip_rect(sel_rect, ctx.rect);
                fill_rect(pixmap, clipped, line.color);
            }
        }
    } else if !view.is_stack() && !ctx.footer {
        // Non-stack bar selected dots if applicable.
        let _ = palette;
    }
}

fn draw_stack_linear(
    pixmap: &mut Pixmap,
    view: &mut crate::views::StackLinearChartView,
    ctx: &PaintContext<'_>,
    lines_filter: &LinesFilterController,
    palette: &ChartPalette,
) {
    fill_rect(pixmap, ctx.rect, palette.box_divider_bg);
    let polygons = view.compute_area_polygons(ctx, lines_filter);
    for poly in polygons {
        let Some(line) = ctx.chart_data.lines.iter().find(|l| l.id == poly.line_id) else {
            continue;
        };
        if poly.points.len() < 3 {
            continue;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(
            poly.points[0].x.clamp(ctx.rect.x, ctx.rect.right()) as f32,
            poly.points[0].y.clamp(ctx.rect.y, ctx.rect.bottom()) as f32,
        );
        for p in &poly.points[1..] {
            pb.line_to(
                p.x.clamp(ctx.rect.x, ctx.rect.right()) as f32,
                p.y.clamp(ctx.rect.y, ctx.rect.bottom()) as f32,
            );
        }
        pb.close();
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(line.color.to_skia());
            paint.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

fn draw_pie(
    pixmap: &mut Pixmap,
    view: &mut crate::views::StackLinearChartView,
    chart_data: &StatisticalChart,
    lines_filter: &LinesFilterController,
    x_indices: Limits,
    rect: RectF,
    palette: &ChartPalette,
) {
    let (_parts_data, slices) = view.compute_pie_layout(
        chart_data,
        lines_filter,
        x_indices,
        rect,
        crate::views::stack_linear::PIE_ANIMATION_DURATION_MS,
    );
    let center = rect.center();
    let radius = ((rect.width / 2.0) * PIE_CIRCLE_SIZE_RATIO).min(rect.height / 2.0 - 8.0);
    if radius <= 1.0 {
        return;
    }

    for slice in &slices {
        if slice.sweep_angle_deg.abs() < 1e-4 {
            continue;
        }
        let Some(line) = chart_data.lines.iter().find(|l| l.id == slice.line_id) else {
            continue;
        };
        let cx = center.x + slice.offset.x;
        let cy = center.y + slice.offset.y;

        let path = build_pie_wedge(cx, cy, radius, slice.start_angle_deg, slice.sweep_angle_deg);
        if let Some(path) = path {
            let mut paint = Paint::default();
            paint.set_color(line.color.to_skia());
            paint.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    for slice in &slices {
        if let Some(lbl_center) = slice.label_center {
            let scale = if slice.percentage > 0.25 { 2 } else { 1 };
            let tw = measure_text_width(&slice.percentage_text, scale) as f64;
            let th = (7 * scale) as f64;
            let alpha = lines_filter.alpha(slice.line_id);
            draw_text(
                pixmap,
                &slice.percentage_text,
                (lbl_center.x - tw / 2.0) as i32,
                (lbl_center.y - th / 2.0) as i32,
                scale,
                palette.premium_button_fg.with_alpha_factor(alpha),
            );
        }
    }
}

fn build_pie_wedge(
    cx: f64,
    cy: f64,
    radius: f64,
    start_deg: f64,
    sweep_deg: f64,
) -> Option<tiny_skia::Path> {
    if sweep_deg.abs() >= 359.99 {
        return PathBuilder::from_circle(cx as f32, cy as f32, radius as f32);
    }
    let steps = ((sweep_deg.abs() / 3.0).ceil() as usize).max(4);
    let mut pb = PathBuilder::new();
    pb.move_to(cx as f32, cy as f32);
    for i in 0..=steps {
        let t = (i as f64) / (steps as f64);
        let rad = (start_deg + sweep_deg * t) * PI / 180.0;
        let px = cx + radius * rad.cos();
        let py = cy + radius * rad.sin();
        pb.line_to(px as f32, py as f32);
    }
    pb.close();
    pb.finish()
}

#[allow(clippy::too_many_arguments)]
fn draw_footer(
    pixmap: &mut Pixmap,
    chart: &StatisticalChart,
    lines_filter: &LinesFilterController,
    view: &mut ChartView,
    chart_type: RenderChartType,
    full_height_limits: Limits,
    x_pct: Limits,
    footer_rect: RectF,
    palette: &ChartPalette,
) {
    let inner = RectF::new(
        footer_rect.x,
        footer_rect.y + 1.0,
        footer_rect.width,
        (footer_rect.height - 2.0).max(1.0),
    );
    fill_rounded_rect(pixmap, inner, FOOTER_SIDE_RADIUS, palette.box_bg);

    let full_ctx = PaintContext {
        chart_data: chart,
        x_indices: Limits::new(0.0, chart.x.len().saturating_sub(1) as f64),
        x_percentage_limits: Limits::new(0.0, 1.0),
        height_limits: full_height_limits,
        rect: inner,
        footer: true,
    };

    match chart_type {
        RenderChartType::Linear | RenderChartType::DoubleLinear => {
            if let ChartView::Linear(lv) = view {
                draw_linear(pixmap, lv, &full_ctx, lines_filter, 1.25);
            }
        }
        RenderChartType::Bar | RenderChartType::StackBar => {
            if let ChartView::Bar(bv) = view {
                draw_bar(pixmap, bv, &full_ctx, lines_filter, palette);
            }
        }
        RenderChartType::StackLinear | RenderChartType::Pie => {
            if let ChartView::StackLinear(sv) = view {
                draw_stack_linear(pixmap, sv, &full_ctx, lines_filter, palette);
            }
        }
    }

    let selector = FooterRangeSelector::with_width(footer_rect.width, x_pct);
    let left_s = selector.left_side();
    let right_s = selector.right_side();

    // Inactive dimmed wings outside the selection handles
    let left_wing = RectF::new(footer_rect.x, inner.y, left_s.min, inner.height);
    let right_wing = RectF::new(
        footer_rect.x + right_s.max,
        inner.y,
        (footer_rect.width - right_s.max).max(0.0),
        inner.height,
    );
    fill_rect(pixmap, left_wing, palette.chart_inactive);
    fill_rect(pixmap, right_wing, palette.chart_inactive);

    // Active top/bottom border strips and left/right handles
    let handle_left = RectF::new(
        footer_rect.x + left_s.min,
        footer_rect.y,
        left_s.span(),
        footer_rect.height,
    );
    let handle_right = RectF::new(
        footer_rect.x + right_s.min,
        footer_rect.y,
        right_s.span(),
        footer_rect.height,
    );
    fill_rounded_rect(pixmap, handle_left, 3.0, palette.chart_active);
    fill_rounded_rect(pixmap, handle_right, 3.0, palette.chart_active);

    let mid_x = footer_rect.x + left_s.max;
    let mid_w = (right_s.min - left_s.max).max(0.0);
    fill_rect(
        pixmap,
        RectF::new(mid_x, footer_rect.y, mid_w, 2.0),
        palette.chart_active,
    );
    fill_rect(
        pixmap,
        RectF::new(mid_x, footer_rect.bottom() - 2.0, mid_w, 2.0),
        palette.chart_active,
    );

    // Handle grip bars (`_sidePen`)
    let grip_top = footer_rect.y + (footer_rect.height - FOOTER_ARROW_HEIGHT) / 2.0;
    let left_grip_x = handle_left.x + handle_left.width / 2.0 - 1.0;
    let right_grip_x = handle_right.x + handle_right.width / 2.0 - 1.0;
    fill_rect(
        pixmap,
        RectF::new(left_grip_x, grip_top, 2.0, FOOTER_ARROW_HEIGHT),
        palette.premium_button_fg,
    );
    fill_rect(
        pixmap,
        RectF::new(right_grip_x, grip_top, 2.0, FOOTER_ARROW_HEIGHT),
        palette.premium_button_fg,
    );
}

fn clip_rect(r: RectF, bounds: RectF) -> RectF {
    let x0 = r.x.max(bounds.x).floor();
    let y0 = r.y.max(bounds.y).floor();
    let x1 = r.right().min(bounds.right()).ceil();
    let y1 = r.bottom().min(bounds.bottom()).ceil();
    RectF::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// Measures width in pixels of a string rendered with the embedded 5x7 bitmap font.
fn measure_text_width(text: &str, scale: i32) -> i32 {
    let count = text.chars().count() as i32;
    if count == 0 {
        0
    } else {
        count * 6 * scale - scale
    }
}

/// Draws crisp ASCII/symbol labels with an embedded 5x7 bitmap font (no external font files).
fn draw_text(pixmap: &mut Pixmap, text: &str, x: i32, y: i32, scale: i32, color: Color) {
    if color.a == 0 || scale <= 0 {
        return;
    }
    let mut cursor_x = x;
    for ch in text.chars() {
        let glyph = glyph_5x7(ch);
        for (row, &bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if (bits >> (4 - col)) & 1 == 1 {
                    fill_rect(
                        pixmap,
                        RectF::new(
                            f64::from(cursor_x + col * scale),
                            f64::from(y + (row as i32) * scale),
                            f64::from(scale),
                            f64::from(scale),
                        ),
                        color,
                    );
                }
            }
        }
        cursor_x += 6 * scale;
    }
}

/// Minimal 5x7 bitmap glyph lookup for digits, letters, and statistics punctuation.
fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '-' | '\u{2014}' | '\u{2212}' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '$' => [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04],
        '\u{2248}' => [0x00, 0x0A, 0x15, 0x00, 0x0A, 0x15, 0x00],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        _ => [0x00; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chart() -> StatisticalChart {
        let json = r#"{
            "columns": [
                ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000, 1700352000000, 1700438400000],
                ["y0", 100, 180, 140, 220, 260, 310],
                ["y1", 20, 35, 25, 40, 30, 50]
            ],
            "names": { "y0": "Views", "y1": "Shares" },
            "colors": { "y0": "BLUE#327fe5", "y1": "GREEN#61c752" }
        }"#;
        let mut chart = StatisticalChart::from_json_str(json).unwrap();
        chart.apply_palette(&ChartPalette::light());
        chart
    }

    #[test]
    fn renders_all_six_chart_types_to_non_blank_png() {
        let chart = sample_chart();
        let filter = LinesFilterController::new();
        let bg = ChartPalette::light().box_bg;

        for kind in [
            RenderChartType::Linear,
            RenderChartType::DoubleLinear,
            RenderChartType::Bar,
            RenderChartType::StackBar,
            RenderChartType::StackLinear,
            RenderChartType::Pie,
        ] {
            let rendered = render_chart(
                &chart,
                &filter,
                &RenderOptions {
                    width: 480,
                    height: 300,
                    chart_type: kind,
                    title: Some(format!("{kind:?}")),
                    selected_x_index: Some(2),
                    selected_pie_line_id: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();

            assert_eq!(rendered.rgba.len(), 480 * 300 * 4);
            let non_bg_pixels = rendered
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[0] != bg.r || px[1] != bg.g || px[2] != bg.b)
                .count();
            assert!(
                non_bg_pixels > 500,
                "expected {kind:?} chart to draw visible pixels, got {non_bg_pixels}"
            );

            let png = rendered.encode_png().unwrap();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
    }
}
