//! Reusable `egui` formula painter widget for `tg_iv::markdown::MathLayoutEngine`.
//!
//! Port origin: `Telegram/SourceFiles/iv/markdown/iv_markdown_math_renderer.cpp`
//! and `Telegram/SourceFiles/iv/markdown/iv_markdown_microtex.cpp`.
//!
//! Paints positioned glyph runs ([`PositionedGlyphRun`]), filled rules ([`FormulaRule`]),
//! and stroked vector polylines ([`FormulaPolyline`]) aligned to the surrounding text
//! baseline. Reused across:
//! - the rich document editor (`webview::rich_editor`),
//! - Instant View pages (`webview::instant_view` / `webview::rich_page_view`), and
//! - rich message bubbles (`history::bubble`).

use std::cell::RefCell;

use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect, Response, Sense, Shape,
    Stroke, TextFormat, Ui, Vec2, Widget,
};
use tg_iv::markdown::{
    FormulaLayout, FormulaLayoutRequest, MathFontStyle, MathKind, MathLayoutEngine,
    PositionedGlyphRun, RenderedFormula,
};

thread_local! {
    static SHARED_MATH_ENGINE: RefCell<MathLayoutEngine> = RefCell::new(MathLayoutEngine::default());
}

/// Runs `f` with the thread-local caching [`MathLayoutEngine`].
pub fn with_math_engine<R>(f: impl FnOnce(&mut MathLayoutEngine) -> R) -> R {
    SHARED_MATH_ENGINE.with(|cell| {
        let mut engine = cell.borrow_mut();
        f(&mut engine)
    })
}

/// Visual and layout configuration for [`FormulaWidget`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormulaWidgetStyle {
    /// Display vs inline TeX math mode.
    pub kind: MathKind,
    /// Base text font size in logical pixels.
    pub font_size: f32,
    /// Foreground color for glyphs, rules, and polylines.
    pub color: Color32,
    /// Fallback text color when layout fails.
    pub fallback_color: Color32,
    /// Optional subtle background fill behind the formula box.
    pub background: Option<Color32>,
    /// Horizontal padding around the formula box in logical pixels.
    pub pad_x: f32,
    /// Vertical padding around the formula box in logical pixels.
    pub pad_y: f32,
    /// Maximum render width cap in logical pixels.
    pub max_width: i32,
    /// Maximum render height cap in logical pixels.
    pub max_height: i32,
}

impl Default for FormulaWidgetStyle {
    fn default() -> Self {
        Self {
            kind: MathKind::Inline,
            font_size: 14.0,
            color: Color32::BLACK,
            fallback_color: Color32::from_rgb(160, 60, 60),
            background: None,
            pad_x: 2.0,
            pad_y: 1.0,
            max_width: 1024,
            max_height: 512,
        }
    }
}

impl FormulaWidgetStyle {
    /// Inline formula style matched to surrounding text size and color.
    #[must_use]
    pub const fn inline(font_size: f32, color: Color32) -> Self {
        Self {
            kind: MathKind::Inline,
            font_size,
            color,
            fallback_color: color,
            background: None,
            pad_x: 2.0,
            pad_y: 0.0,
            max_width: 1024,
            max_height: 512,
        }
    }

    /// Display block formula style (centered block math `$$...$$`).
    #[must_use]
    pub const fn display(font_size: f32, color: Color32) -> Self {
        Self {
            kind: MathKind::Display,
            font_size,
            color,
            fallback_color: color,
            background: None,
            pad_x: 8.0,
            pad_y: 6.0,
            max_width: 2048,
            max_height: 1024,
        }
    }
}

/// Measured geometry and baseline metadata for a laid-out formula.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormulaRenderMetrics {
    /// Total widget size including padding (`width`, `height`).
    pub size: Vec2,
    /// Raw formula box size excluding padding.
    pub formula_size: Vec2,
    /// Distance from the top of the widget box (`y = 0`) to the main math baseline.
    pub baseline_y: f32,
    /// Ascent above the main math baseline (excluding outer `pad_y`).
    pub ascent: f32,
    /// Depth below the main math baseline (excluding outer `pad_y`).
    pub depth: f32,
    /// Whether `MathLayoutEngine` produced a valid layout.
    pub success: bool,
}

/// Computes the layout and baseline metrics for `tex` using `engine` and `style`.
#[must_use]
pub fn layout_formula_for_style(
    engine: &mut MathLayoutEngine,
    tex: &str,
    style: &FormulaWidgetStyle,
) -> (RenderedFormula, FormulaRenderMetrics) {
    let text_size = style.font_size.round().clamp(6.0, 96.0) as i32;
    let request = FormulaLayoutRequest {
        trimmed_tex: tex.trim().to_owned(),
        kind: style.kind,
        text_size,
        render_width_cap: style.max_width.max(32),
        render_height_cap: style.max_height.max(16),
    };
    let rendered = engine.render(&request);
    let metrics = metrics_from_rendered(&rendered, style);
    (rendered, metrics)
}

/// Derives [`FormulaRenderMetrics`] from an already laid-out [`RenderedFormula`].
#[must_use]
pub fn metrics_from_rendered(
    rendered: &RenderedFormula,
    style: &FormulaWidgetStyle,
) -> FormulaRenderMetrics {
    if let Some(ref layout) = rendered.layout {
        let w = layout.width.max(1.0);
        let ascent = layout.ascent.max(0.0);
        let depth = layout.depth.max(0.0);
        let h = layout.height.max(ascent + depth).max(1.0);
        FormulaRenderMetrics {
            size: Vec2::new(w + style.pad_x * 2.0, h + style.pad_y * 2.0),
            formula_size: Vec2::new(w, h),
            baseline_y: style.pad_y + ascent,
            ascent,
            depth,
            success: true,
        }
    } else {
        let fallback_len = rendered.measured.fallback_text.chars().count().max(1) as f32;
        let w = (fallback_len * style.font_size * 0.58).max(style.font_size);
        let ascent = style.font_size * 0.78;
        let depth = style.font_size * 0.22;
        let h = ascent + depth;
        FormulaRenderMetrics {
            size: Vec2::new(w + style.pad_x * 2.0, h + style.pad_y * 2.0),
            formula_size: Vec2::new(w, h),
            baseline_y: style.pad_y + ascent,
            ascent,
            depth,
            success: false,
        }
    }
}

/// Maps a [`MathFontStyle`] and font size to an `egui` [`TextFormat`].
#[must_use]
pub fn text_format_for_math_style(
    style: MathFontStyle,
    font_size: f32,
    color: Color32,
) -> TextFormat {
    let family = match style {
        MathFontStyle::Monospace => FontFamily::Monospace,
        _ => FontFamily::Proportional,
    };
    let italics = matches!(
        style,
        MathFontStyle::Italic | MathFontStyle::BoldItalic | MathFontStyle::Script
    );
    TextFormat {
        font_id: FontId::new(font_size.max(6.0), family),
        color,
        italics,
        ..TextFormat::default()
    }
}

/// Paints a single [`PositionedGlyphRun`] relative to `origin`, placing its typographic baseline
/// at `origin.y + run.y`.
fn paint_glyph_run(painter: &Painter, origin: Pos2, run: &PositionedGlyphRun, color: Color32) {
    if run.text.is_empty() {
        return;
    }
    let fmt = text_format_for_math_style(run.style, run.font_size, color);
    let mut job = LayoutJob::default();
    job.append(&run.text, 0.0, fmt);
    let galley = painter.layout_job(job);

    // Align the glyph top so the baseline sits at `origin.y + run.y`.
    // In `PositionedGlyphRun`, `y` is the baseline and `y - ascent` is the top edge.
    let top_y = origin.y + run.y - run.ascent;
    let pos = Pos2::new(origin.x + run.x, top_y);
    painter.galley(pos, galley.clone(), color);

    // Subtle faux-bold horizontal offset for Bold / BoldItalic / Blackboard styles.
    if matches!(
        run.style,
        MathFontStyle::Bold | MathFontStyle::BoldItalic | MathFontStyle::Blackboard
    ) {
        painter.galley(pos + Vec2::new(0.55, 0.0), galley, color);
    }
}

/// Paints a complete [`FormulaLayout`] (glyph runs, filled rules, and stroked polylines)
/// with top-left corner at `origin`.
pub fn paint_formula_layout(
    painter: &Painter,
    origin: Pos2,
    layout: &FormulaLayout,
    color: Color32,
) {
    // 1. Filled horizontal/vertical rules (fraction bars, radical vincula, over/underlines).
    for rule in &layout.rules {
        let rect = Rect::from_min_size(
            Pos2::new(origin.x + rule.x, origin.y + rule.y),
            Vec2::new(rule.width.max(0.5), rule.height.max(0.75)),
        );
        painter.rect_filled(rect, CornerRadius::ZERO, color);
    }

    // 2. Stroked polylines (radical checkmarks, stretched delimiters).
    for poly in &layout.polylines {
        if poly.points.len() >= 2 {
            let pts: Vec<Pos2> = poly
                .points
                .iter()
                .map(|&(px, py)| Pos2::new(origin.x + px, origin.y + py))
                .collect();
            painter.add(Shape::line(
                pts,
                Stroke::new(poly.stroke_width.max(1.0), color),
            ));
        }
    }

    // 3. Positioned glyph runs.
    for run in &layout.glyph_runs {
        paint_glyph_run(painter, origin, run, color);
    }
}

/// Paints a [`RenderedFormula`] so its main math baseline aligns with `baseline_left.y`
/// and its left edge sits at `baseline_left.x`.
///
/// Returns the painted bounding [`Rect`] (including optional padding from `style`).
pub fn paint_formula_at_baseline(
    painter: &Painter,
    baseline_left: Pos2,
    rendered: &RenderedFormula,
    style: &FormulaWidgetStyle,
) -> Rect {
    let metrics = metrics_from_rendered(rendered, style);
    let top_left = Pos2::new(baseline_left.x, baseline_left.y - metrics.baseline_y);
    let outer_rect = Rect::from_min_size(top_left, metrics.size);

    if let Some(bg) = style.background {
        painter.rect_filled(outer_rect, CornerRadius::same(4), bg);
    }

    let content_origin = Pos2::new(top_left.x + style.pad_x, top_left.y + style.pad_y);
    if let Some(ref layout) = rendered.layout {
        paint_formula_layout(painter, content_origin, layout, style.color);
    } else {
        let fallback = if rendered.measured.fallback_text.is_empty() {
            "[math]"
        } else {
            &rendered.measured.fallback_text
        };
        painter.text(
            Pos2::new(content_origin.x, content_origin.y),
            Align2::LEFT_TOP,
            fallback,
            FontId::monospace(style.font_size.max(8.0)),
            style.fallback_color,
        );
    }
    outer_rect
}

/// Reusable `egui` widget that lays out and paints a TeX formula through [`MathLayoutEngine`].
pub struct FormulaWidget<'a> {
    tex: &'a str,
    style: FormulaWidgetStyle,
    engine: Option<&'a mut MathLayoutEngine>,
}

impl<'a> FormulaWidget<'a> {
    /// Creates a formula widget backed by the shared thread-local [`MathLayoutEngine`].
    #[must_use]
    pub const fn new(tex: &'a str, style: FormulaWidgetStyle) -> Self {
        Self {
            tex,
            style,
            engine: None,
        }
    }

    /// Uses an explicit [`MathLayoutEngine`] instance instead of the thread-local cache.
    #[must_use]
    pub fn with_engine(mut self, engine: &'a mut MathLayoutEngine) -> Self {
        self.engine = Some(engine);
        self
    }

    /// Allocates and paints the formula in `ui`, returning both the [`Response`] and
    /// exact [`FormulaRenderMetrics`] (including `baseline_y`).
    pub fn show(self, ui: &mut Ui) -> (Response, FormulaRenderMetrics) {
        let (rendered, metrics) = if let Some(engine) = self.engine {
            layout_formula_for_style(engine, self.tex, &self.style)
        } else {
            with_math_engine(|engine| layout_formula_for_style(engine, self.tex, &self.style))
        };

        let (rect, response) = ui.allocate_exact_size(metrics.size, Sense::hover());
        if ui.is_rect_visible(rect) {
            let baseline_left = Pos2::new(rect.left(), rect.top() + metrics.baseline_y);
            paint_formula_at_baseline(ui.painter(), baseline_left, &rendered, &self.style);
        }
        (response, metrics)
    }
}

impl Widget for FormulaWidget<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).0
    }
}

/// Convenience helper: renders a formula widget with an explicit [`MathLayoutEngine`].
pub fn show_formula_widget(
    ui: &mut Ui,
    engine: &mut MathLayoutEngine,
    tex: &str,
    style: FormulaWidgetStyle,
) -> (Response, FormulaRenderMetrics) {
    FormulaWidget::new(tex, style).with_engine(engine).show(ui)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_metrics_size_and_baseline_align_with_layout() {
        let mut engine = MathLayoutEngine::default();

        let inline_style = FormulaWidgetStyle::inline(14.0, Color32::WHITE);
        let (simple_rendered, simple_metrics) =
            layout_formula_for_style(&mut engine, "x^2 + y^2", &inline_style);
        assert!(simple_metrics.success);
        let simple_layout = simple_rendered.layout.as_ref().expect("simple layout");
        assert!(simple_metrics.formula_size.x > 0.0);
        assert!(simple_metrics.formula_size.y > 0.0);
        assert!(
            (simple_metrics.ascent - simple_layout.ascent).abs() < 0.01,
            "ascent must match layout.ascent"
        );
        assert!(
            (simple_metrics.depth - simple_layout.depth).abs() < 0.01,
            "depth must match layout.depth"
        );
        assert!(
            (simple_metrics.baseline_y - (inline_style.pad_y + simple_layout.ascent)).abs() < 0.01,
            "baseline_y must equal pad_y + ascent"
        );
        assert!(
            (simple_metrics.formula_size.y - (simple_metrics.ascent + simple_metrics.depth)).abs()
                < 0.5
        );

        // Display fraction has a horizontal rule, taller height, and larger ascent than simple text.
        let display_style = FormulaWidgetStyle::display(16.0, Color32::WHITE);
        let (frac_rendered, frac_metrics) =
            layout_formula_for_style(&mut engine, r"\frac{a + b}{c + d}", &display_style);
        assert!(frac_metrics.success);
        let frac_layout = frac_rendered.layout.as_ref().expect("frac layout");
        assert!(
            !frac_layout.rules.is_empty(),
            "fraction must emit a fraction bar rule"
        );
        assert!(frac_metrics.formula_size.y > simple_metrics.formula_size.y);
        assert!(frac_metrics.baseline_y > display_style.pad_y);
        assert!(frac_metrics.baseline_y < frac_metrics.size.y);

        // Radical emits both a polyline checkmark and a vinculum rule.
        let (sqrt_rendered, sqrt_metrics) =
            layout_formula_for_style(&mut engine, r"\sqrt{x^2 + 1}", &display_style);
        assert!(sqrt_metrics.success);
        let sqrt_layout = sqrt_rendered.layout.as_ref().expect("sqrt layout");
        assert!(!sqrt_layout.polylines.is_empty());
        assert!(!sqrt_layout.rules.is_empty());
    }

    #[test]
    fn formula_widget_baseline_placement_and_empty_fallback() {
        let ctx = egui::Context::default();
        let mut engine = MathLayoutEngine::default();
        let style = FormulaWidgetStyle::inline(15.0, Color32::BLACK);

        let (rendered, metrics) = layout_formula_for_style(&mut engine, r"\frac{1}{2}", &style);
        assert!(metrics.success);

        let baseline_y = 100.0_f32;
        let mut painted_rect = Rect::NOTHING;
        let mut widget_rect = Rect::NOTHING;
        let mut widget_metrics = None;

        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            painted_rect = paint_formula_at_baseline(
                ui.painter(),
                Pos2::new(20.0, baseline_y),
                &rendered,
                &style,
            );
            let (resp, m) = show_formula_widget(ui, &mut engine, r"\frac{1}{2}", style);
            widget_rect = resp.rect;
            widget_metrics = Some(m);
        });
        out.textures_delta.clear();

        // Top of painted formula sits at `baseline_y - metrics.baseline_y`, bottom at `top + height`.
        assert!((painted_rect.top() - (baseline_y - metrics.baseline_y)).abs() < 0.01);
        assert!((painted_rect.width() - metrics.size.x).abs() < 0.01);
        assert!((painted_rect.height() - metrics.size.y).abs() < 0.01);

        // Widget allocated rect matches computed metrics exactly.
        let wm = widget_metrics.expect("widget metrics");
        assert!((widget_rect.width() - wm.size.x).abs() < 0.01);
        assert!((widget_rect.height() - wm.size.y).abs() < 0.01);
        assert!(wm.baseline_y > 0.0 && wm.baseline_y < wm.size.y);

        // Empty TeX gracefully falls back without panicking.
        let (empty_rendered, empty_metrics) = layout_formula_for_style(&mut engine, "   ", &style);
        assert!(!empty_metrics.success);
        assert!(empty_rendered.layout.is_none());
        assert!(empty_metrics.size.x > 0.0 && empty_metrics.size.y > 0.0);
    }
}
