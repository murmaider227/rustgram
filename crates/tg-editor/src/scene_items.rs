//! Rich photo/video editor scene items: text overlays, vector shapes, location & weather
//! tags, story areas collection, audio track timeline, and draw-to-reply sessions.
//!
//! Port of:
//! - `Telegram/SourceFiles/editor/scene/scene_item_text.{h,cpp}`
//! - `Telegram/SourceFiles/editor/scene/scene_item_shape.{h,cpp}`
//! - `Telegram/SourceFiles/editor/scene/scene_item_location.{h,cpp}`
//! - `Telegram/SourceFiles/editor/scene/scene_item_weather.{h,cpp}`
//! - `Telegram/SourceFiles/editor/scene/scene_item_link.{h,cpp}`
//! - `Telegram/SourceFiles/editor/scene/scene_item_reaction.{h,cpp}`
//! - `Telegram/SourceFiles/editor/scene/scene_item_message.{h,cpp}`
//! - `Telegram/SourceFiles/editor/editor_story_areas.{h,cpp}`
//! - `Telegram/SourceFiles/editor/audio_track_timeline.{h,cpp}`
//! - `Telegram/SourceFiles/editor/editor_audio_timeline.{h,cpp}`
//! - `Telegram/SourceFiles/history/view/history_view_draw_to_reply.{h,cpp}`

use std::f64::consts::PI;
use std::path::PathBuf;

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect as SkiaRect, Stroke,
};

use crate::EditorError;
use crate::apply::apply;
use crate::common::{PhotoModifications, ShapeType, TextAlignment, TextStyle, TextTypeface};
use crate::export::{ExportedJpeg, PhotoExportSettings, export_jpeg};
use crate::geometry::{Affine2D, Color, PointF, RectF, Size};
use crate::scene::{ItemPlacement, ItemStatus, PaintScene, SceneItemKind};

// ---------------------------------------------------------------------------
// Shape geometry constants (`scene_item_shape.cpp`)
// ---------------------------------------------------------------------------

/// Inner-to-outer radius ratio for a 5-pointed star (`kStarInnerRatio`).
pub const STAR_INNER_RATIO: f64 = 0.4316;
/// Corner rounding ratio for rounded rectangle and speech bubble (`kRoundingRatio`).
pub const SHAPE_ROUNDING_RATIO: f64 = 0.234;
/// Body height fraction of a speech bubble (`kBubbleBodyRatio`).
pub const BUBBLE_BODY_RATIO: f64 = 2.0 / 2.25;
/// Tail horizontal offset ratio of a speech bubble (`kBubbleTailRatio`).
pub const BUBBLE_TAIL_RATIO: f64 = 0.75;
/// Arrowhead length ratio relative to shape width (`kArrowHeadRatio`).
pub const ARROW_HEAD_RATIO: f64 = 0.16;
/// Arrowhead minimum scale factor relative to stroke width (`kArrowHeadStrokeFactor`).
pub const ARROW_HEAD_STROKE_FACTOR: f64 = 2.5;
/// Arrowhead half-angle in radians (`30°`, `kArrowHeadAngle`).
pub const ARROW_HEAD_ANGLE_RAD: f64 = 30.0 * PI / 180.0;
/// Maximum quadratic bend ratio for a curved arrow (`kArrowMaxBendRatio`).
pub const ARROW_MAX_BEND_RATIO: f64 = 0.35;
/// Minimum shape side length (`kMinShapeSide`).
pub const MIN_SHAPE_SIDE: f64 = 1.0;
/// Star bounding width in unit-radius coordinates (`kStarBoxWidth`).
pub const STAR_BOX_WIDTH: f64 = 1.902_113;
/// Star bounding height in unit-radius coordinates (`kStarBoxHeight`).
pub const STAR_BOX_HEIGHT: f64 = 1.809_017;

/// Story pill corner radius ratio (`kPillRadiusRatio` in `editor_story_areas.cpp`).
pub const STORY_PILL_RADIUS_RATIO: f64 = 0.2;

/// Minimum trim length for an audio track overlay clip in milliseconds.
pub const AUDIO_TRACK_MIN_DURATION_MS: i64 = 1_000;

// ---------------------------------------------------------------------------
// Text scene item (`scene_item_text.{h,cpp}`)
// ---------------------------------------------------------------------------

/// Layout specification computed for an [`EditorTextItem`] (`Editor::TextLayoutSpec`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextLayoutSpec {
    /// Effective pixel font size.
    pub font_size: f64,
    /// Padding around text inside background pill/frame.
    pub padding: f64,
    /// Maximum wrapped text width in scene pixels.
    pub max_text_width: f64,
}

/// Computes the effective foreground text color given base `color` and [`TextStyle`]
/// (`EffectiveTextColor` in `scene_item_text.cpp`).
#[must_use]
pub fn effective_text_color(color: Color, style: TextStyle) -> Color {
    match style {
        TextStyle::Opaque => {
            // Pick high-contrast black or white over the opaque fill.
            let luma = 0.299 * f64::from(color.r)
                + 0.587 * f64::from(color.g)
                + 0.114 * f64::from(color.b);
            if luma > 160.0 {
                Color::BLACK
            } else {
                Color::WHITE
            }
        }
        TextStyle::Plain | TextStyle::SemiTransparent | TextStyle::Framed => color,
    }
}

/// Computes background fill color for a text overlay (`TextBackgroundColor`).
#[must_use]
pub fn text_background_color(color: Color, style: TextStyle) -> Option<Color> {
    match style {
        TextStyle::Plain => None,
        TextStyle::Opaque => Some(Color::rgba(color.r, color.g, color.b, 255)),
        TextStyle::SemiTransparent => Some(Color::rgba(0, 0, 0, 128)),
        TextStyle::Framed => Some(Color::rgba(0, 0, 0, 96)),
    }
}

/// Computes background padding around a text overlay (`TextBackgroundPadding`).
#[must_use]
pub fn text_background_padding(font_size: f64, style: TextStyle) -> f64 {
    match style {
        TextStyle::Plain => (font_size * 0.15).max(2.0),
        TextStyle::SemiTransparent | TextStyle::Opaque | TextStyle::Framed => {
            (font_size * 0.35).max(4.0)
        }
    }
}

/// Computes [`TextLayoutSpec`] for the given font size, canvas size, style, and typeface.
#[must_use]
pub fn compute_text_layout_spec(
    font_size: f64,
    image_size: Size,
    style: TextStyle,
    typeface: TextTypeface,
) -> TextLayoutSpec {
    let min_side = f64::from(image_size.width.min(image_size.height).max(1));
    let clamped_font = font_size.clamp(10.0, (min_side * 0.25).max(16.0));
    let width_factor = match typeface {
        TextTypeface::Condensed => 0.92,
        TextTypeface::Monospace => 0.85,
        _ => 0.88,
    };
    let padding = text_background_padding(clamped_font, style);
    let max_text_width =
        (f64::from(image_size.width.max(1)) * width_factor - padding * 2.0).max(clamped_font * 2.0);
    TextLayoutSpec {
        font_size: clamped_font,
        padding,
        max_text_width,
    }
}

/// Estimates content bounding box `(width, height)` for a text overlay (`ItemText::computeContentSize`).
#[must_use]
pub fn compute_text_content_size(
    text: &str,
    font_size: f64,
    image_size: Size,
    style: TextStyle,
    typeface: TextTypeface,
) -> (f64, f64) {
    let spec = compute_text_layout_spec(font_size, image_size, style, typeface);
    let char_advance = match typeface {
        TextTypeface::Condensed => spec.font_size * 0.50,
        TextTypeface::Monospace => spec.font_size * 0.62,
        _ => spec.font_size * 0.58,
    };
    let line_height = spec.font_size * 1.25;
    let mut max_line_w = spec.font_size * 1.5;
    let mut line_count = 0usize;
    for raw_line in text.split('\n') {
        let len = raw_line.chars().count().max(1);
        let raw_w = len as f64 * char_advance;
        if raw_w <= spec.max_text_width {
            max_line_w = max_line_w.max(raw_w);
            line_count += 1;
        } else {
            let wraps = (raw_w / spec.max_text_width).ceil() as usize;
            max_line_w = max_line_w.max(spec.max_text_width);
            line_count += wraps.max(1);
        }
    }
    let lines = line_count.max(1) as f64;
    let width = (max_line_w + spec.padding * 2.0).max(24.0);
    let height = (lines * line_height + spec.padding * 2.0).max(20.0);
    (width, height)
}

/// Text overlay layer on a [`PaintScene`] (`Editor::ItemText`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextItem {
    /// Text string displayed by this overlay.
    pub text: String,
    /// Primary text/background color.
    pub color: Color,
    /// Logical font size in scene units.
    pub font_size: f64,
    /// Visual background style (`Framed`, `SemiTransparent`, `Plain`, `Opaque`).
    #[serde(default)]
    pub style: TextStyle,
    /// Typeface family.
    #[serde(default)]
    pub typeface: TextTypeface,
    /// Horizontal paragraph alignment.
    #[serde(default)]
    pub alignment: TextAlignment,
    /// Position, rotation, scale, z-value, and bounding size.
    pub placement: ItemPlacement,
}

impl TextItem {
    /// Creates a new text item with auto-computed bounding size and aspect ratio.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        text: impl Into<String>,
        color: Color,
        font_size: f64,
        style: TextStyle,
        typeface: TextTypeface,
        alignment: TextAlignment,
        image_size: Size,
        center: PointF,
    ) -> Self {
        let text = text.into();
        let (w, h) = compute_text_content_size(&text, font_size, image_size, style, typeface);
        let placement = ItemPlacement {
            position: center,
            size: w,
            aspect_ratio: (h / w.max(1.0)).max(0.05),
            ..ItemPlacement::default()
        };
        Self {
            text,
            color,
            font_size,
            style,
            typeface,
            alignment,
            placement,
        }
    }

    /// Recomputes `placement.size` and `placement.aspect_ratio` after editing text/style.
    pub fn recompute_bounds(&mut self, image_size: Size) {
        let (w, h) = compute_text_content_size(
            &self.text,
            self.font_size,
            image_size,
            self.style,
            self.typeface,
        );
        self.placement.size = w;
        self.placement.aspect_ratio = (h / w.max(1.0)).max(0.05);
    }
}

// ---------------------------------------------------------------------------
// Shape scene item (`scene_item_shape.{h,cpp}`)
// ---------------------------------------------------------------------------

/// Computed geometry endpoints for a curved/straight [`ShapeType::Arrow`] (`ItemShape::ArrowPoints`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArrowPoints {
    /// Tail start point in local coordinates.
    pub start: PointF,
    /// Quadratic Bezier control point in local coordinates.
    pub control: PointF,
    /// Tip end point in local coordinates.
    pub end: PointF,
    /// Triangular arrowhead side length.
    pub head_side: f64,
}

/// Vector shape overlay layer on a [`PaintScene`] (`Editor::ItemShape`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeItem {
    /// Shape kind (`Circle`, `Rectangle`, `Star`, `Bubble`, `Arrow`).
    pub shape: ShapeType,
    /// Stroke and optional fill color.
    pub color: Color,
    /// Stroke thickness in scene pixels.
    pub stroke_width: f64,
    /// Whether the shape interior is filled (ignored for `Arrow` shaft).
    #[serde(default)]
    pub fill: bool,
    /// Arrow bend factor in `[-1.0, 1.0]` (`0.0` = straight arrow).
    #[serde(default)]
    pub bend: f64,
    /// Position, rotation, scale, flip, z-value, and bounding size.
    pub placement: ItemPlacement,
}

impl ShapeItem {
    /// Creates a new vector shape item with default aspect ratio matching `ItemShape`.
    #[must_use]
    pub fn new(
        shape: ShapeType,
        color: Color,
        stroke_width: f64,
        fill: bool,
        center: PointF,
        size: f64,
    ) -> Self {
        let mut item = Self {
            shape,
            color,
            stroke_width: stroke_width.max(1.0),
            fill,
            bend: 0.0,
            placement: ItemPlacement {
                position: center,
                size: size.max(MIN_SHAPE_SIDE),
                aspect_ratio: default_shape_aspect_ratio(shape),
                ..ItemPlacement::default()
            },
        };
        if shape == ShapeType::Arrow {
            item.update_arrow_frame();
        }
        item
    }

    /// Sets arrow curvature `bend` clamped to `[-1.0, 1.0]` and refreshes arrow frame aspect.
    pub fn set_bend(&mut self, bend: f64) {
        self.bend = bend.clamp(-1.0, 1.0);
        self.update_arrow_frame();
    }

    /// Direction sign (`-1.0` when horizontally flipped, `+1.0` otherwise).
    #[must_use]
    pub fn direction(&self) -> f64 {
        if self.placement.flipped { -1.0 } else { 1.0 }
    }

    /// Local inset rect inside `inner_rect` accounting for stroke width (`ItemShape::shapeRect`).
    #[must_use]
    pub fn shape_rect(&self) -> RectF {
        let content = self.placement.inner_rect();
        let w = (content.width - self.stroke_width).max(MIN_SHAPE_SIDE);
        let h = (content.height - self.stroke_width).max(MIN_SHAPE_SIDE);
        let c = content.center();
        RectF::new(c.x - w * 0.5, c.y - h * 0.5, w, h)
    }

    /// Computes arrowhead side length for a given width (`ItemShape::arrowHeadSide`).
    #[must_use]
    pub fn arrow_head_side(&self, width: f64) -> f64 {
        if width <= 0.0 {
            return 0.0;
        }
        (ARROW_HEAD_RATIO * width)
            .max(ARROW_HEAD_STROKE_FACTOR * self.stroke_width)
            .clamp(0.0, width / 3.0)
    }

    /// Updates `placement.aspect_ratio` for [`ShapeType::Arrow`] (`ItemShape::updateArrowFrame`).
    pub fn update_arrow_frame(&mut self) {
        if self.shape != ShapeType::Arrow {
            return;
        }
        let width = self.placement.horizontal_size();
        if width <= 0.0 {
            return;
        }
        let side = self.arrow_head_side(width);
        let middle = self.bend.abs() * ARROW_MAX_BEND_RATIO * width;
        let padding = side * 0.75 + self.stroke_width;
        self.placement.aspect_ratio = (2.0 * (middle + padding) / width).max(0.05);
    }

    /// Computes local quadratic arrow control points (`ItemShape::arrowPoints`).
    #[must_use]
    pub fn arrow_points(&self) -> ArrowPoints {
        let rect = self.shape_rect();
        let side = self.arrow_head_side(rect.width);
        let center = rect.center();
        let dir = self.direction();
        let middle = self.bend * ARROW_MAX_BEND_RATIO * rect.width;
        ArrowPoints {
            start: PointF::new(center.x - dir * rect.width * 0.5, center.y - middle),
            control: PointF::new(center.x, center.y + 3.0 * middle),
            end: PointF::new(
                center.x + dir * (rect.width * 0.5 - side * 0.5),
                center.y - middle,
            ),
            head_side: side,
        }
    }

    /// Center of the interactive arrow bend handle in local coordinates (`ItemShape::bendHandleRect`).
    #[must_use]
    pub fn bend_handle_center(&self) -> PointF {
        let pts = self.arrow_points();
        PointF::new(
            (pts.start.x + pts.end.x) * 0.25 + pts.control.x * 0.5,
            (pts.start.y + pts.end.y) * 0.25 + pts.control.y * 0.5,
        )
    }
}

/// Default aspect ratio for a [`ShapeType`] (`ItemShape::defaultAspectRatio`).
#[must_use]
pub fn default_shape_aspect_ratio(shape: ShapeType) -> f64 {
    match shape {
        ShapeType::Star => STAR_BOX_HEIGHT / STAR_BOX_WIDTH,
        _ => 1.0,
    }
}

// ---------------------------------------------------------------------------
// Location & Weather pills (`scene_item_location.h`, `scene_item_weather.h`)
// ---------------------------------------------------------------------------

/// Visual style preset for location/link pills (`Editor::LinkStyle`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkPillStyle {
    /// Light pill with dark text.
    #[default]
    Light,
    /// Dark pill with white text.
    Dark,
    /// Translucent dark pill.
    Transparent,
    /// Solid accent-tinted pill.
    Color,
}

impl LinkPillStyle {
    /// Cycles to the next pill visual preset on click (`ItemLocation` tap behavior).
    #[must_use]
    pub const fn cycle_next(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Transparent,
            Self::Transparent => Self::Color,
            Self::Color => Self::Light,
        }
    }

    /// Background fill RGBA for this pill style.
    #[must_use]
    pub const fn background_color(self) -> Color {
        match self {
            Self::Light => Color::rgba(255, 255, 255, 240),
            Self::Dark => Color::rgba(28, 32, 38, 235),
            Self::Transparent => Color::rgba(0, 0, 0, 128),
            Self::Color => Color::rgba(64, 167, 227, 235),
        }
    }

    /// Foreground text/icon RGBA for this pill style.
    #[must_use]
    pub const fn foreground_color(self) -> Color {
        match self {
            Self::Light => Color::rgba(24, 24, 28, 255),
            Self::Dark | Self::Transparent | Self::Color => Color::WHITE,
        }
    }
}

/// Geo-location metadata carried by a [`LocationItem`] (`Editor::LocationData`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocationData {
    /// Latitude in degrees (`-90.0..=90.0`).
    pub lat: f64,
    /// Longitude in degrees (`-180.0..=180.0`).
    pub lon: f64,
    /// Display venue / place title.
    pub title: String,
    /// Optional street address.
    #[serde(default)]
    pub address: String,
    /// Optional ISO 3166-1 alpha-2 country code.
    #[serde(default)]
    pub country_iso2: String,
    /// Optional city name.
    #[serde(default)]
    pub city: String,
    /// Optional venue provider (e.g. `"foursquare"`).
    #[serde(default)]
    pub provider: String,
    /// Optional external venue ID.
    #[serde(default)]
    pub venue_id: String,
    /// Optional external venue type.
    #[serde(default)]
    pub venue_type: String,
}

impl LocationData {
    /// Returns `true` when latitude/longitude are finite and within WGS-84 bounds.
    #[must_use]
    pub fn has_valid_coordinates(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
    }

    /// Display label rendered inside the location pill.
    #[must_use]
    pub fn display_label(&self) -> String {
        if !self.title.trim().is_empty() {
            self.title.trim().to_owned()
        } else if !self.city.trim().is_empty() {
            self.city.trim().to_owned()
        } else {
            format!("{:.4}, {:.4}", self.lat, self.lon)
        }
    }
}

/// Interactive location tag scene item (`Editor::ItemLocation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocationItem {
    /// Geo/venue data.
    pub location: LocationData,
    /// Pill appearance style.
    #[serde(default)]
    pub style: LinkPillStyle,
    /// Placement on the scene canvas.
    pub placement: ItemPlacement,
}

impl LocationItem {
    /// Creates a location tag sized proportionally to `image_size` centered at `center`.
    #[must_use]
    pub fn new(location: LocationData, image_size: Size, center: PointF) -> Self {
        let (w, h) = compute_pill_size(&location.display_label(), image_size);
        Self {
            location,
            style: LinkPillStyle::Light,
            placement: ItemPlacement {
                position: center,
                size: w,
                aspect_ratio: (h / w.max(1.0)).max(0.1),
                ..ItemPlacement::default()
            },
        }
    }

    /// Cycles the location pill style preset.
    pub fn cycle_style(&mut self) {
        self.style = self.style.cycle_next();
    }
}

/// Weather tag metadata (`Editor::WeatherData`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeatherData {
    /// Weather emoji (e.g. `"☀️"`, `"🌧️"`).
    pub emoji: String,
    /// Temperature in degrees Celsius.
    pub celsius: f64,
    /// Background pill color.
    pub color: Color,
}

impl Default for WeatherData {
    fn default() -> Self {
        Self {
            emoji: "☀️".to_owned(),
            celsius: 22.0,
            color: Color::rgb(64, 167, 227),
        }
    }
}

impl WeatherData {
    /// Temperature in millicelsius matching Telegram's `mediaAreaWeather` (`round(c * 1000)`).
    #[must_use]
    pub fn millicelsius(&self) -> i32 {
        (self.celsius * 1000.0).round() as i32
    }

    /// Formats temperature in Celsius (e.g. `"22°C"`).
    #[must_use]
    pub fn formatted_celsius(&self) -> String {
        format!("{} {}°C", self.emoji, self.celsius.round() as i32)
    }

    /// Formats temperature in Fahrenheit (e.g. `"72°F"`).
    #[must_use]
    pub fn formatted_fahrenheit(&self) -> String {
        let f = (self.celsius * 9.0 / 5.0 + 32.0).round() as i32;
        format!("{} {}°F", self.emoji, f)
    }

    /// High-contrast foreground color for the pill (`ItemWeather::foreground`).
    #[must_use]
    pub fn foreground(&self) -> Color {
        let luma = 0.299 * f64::from(self.color.r)
            + 0.587 * f64::from(self.color.g)
            + 0.114 * f64::from(self.color.b);
        if luma > 165.0 {
            Color::BLACK
        } else {
            Color::WHITE
        }
    }
}

/// Interactive weather tag scene item (`Editor::ItemWeather`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeatherItem {
    /// Weather payload.
    pub weather: WeatherData,
    /// Placement on the scene canvas.
    pub placement: ItemPlacement,
}

impl WeatherItem {
    /// Creates a weather pill centered at `center` sized for `image_size`.
    #[must_use]
    pub fn new(weather: WeatherData, image_size: Size, center: PointF) -> Self {
        let (w, h) = compute_pill_size(&weather.formatted_celsius(), image_size);
        Self {
            weather,
            placement: ItemPlacement {
                position: center,
                size: w,
                aspect_ratio: (h / w.max(1.0)).max(0.1),
                ..ItemPlacement::default()
            },
        }
    }
}

fn compute_pill_size(label: &str, image_size: Size) -> (f64, f64) {
    let min_side = f64::from(image_size.width.min(image_size.height).max(100));
    let pill_h = (min_side * 0.11).clamp(24.0, 96.0);
    let char_w = pill_h * 0.34;
    let chars = label.chars().count().clamp(4, 28) as f64;
    let pill_w = (chars * char_w + pill_h * 1.1)
        .clamp(pill_h * 2.0, f64::from(image_size.width.max(100)) * 0.85);
    (pill_w, pill_h)
}

// ---------------------------------------------------------------------------
// Story area items & collection (`editor_story_areas.{h,cpp}`)
// ---------------------------------------------------------------------------

/// Story URL link pill item (`Editor::ItemLink`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryLinkItem {
    /// Target HTTPS URL.
    pub url: String,
    /// Display title on the link pill.
    pub title: String,
    /// Visual style.
    #[serde(default)]
    pub style: LinkPillStyle,
    /// Placement on canvas.
    pub placement: ItemPlacement,
}

impl StoryLinkItem {
    /// Creates a clickable story URL link pill.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        title: impl Into<String>,
        image_size: Size,
        center: PointF,
    ) -> Self {
        let url = url.into();
        let title = title.into();
        let label = if title.trim().is_empty() {
            url.clone()
        } else {
            title.clone()
        };
        let (w, h) = compute_pill_size(&label, image_size);
        Self {
            url,
            title,
            style: LinkPillStyle::Light,
            placement: ItemPlacement {
                position: center,
                size: w,
                aspect_ratio: (h / w.max(1.0)).max(0.1),
                ..ItemPlacement::default()
            },
        }
    }
}

/// Interactive story reaction area item (`Editor::ItemReaction`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryReactionItem {
    /// Emoji glyph or custom emoji ID.
    pub reaction: String,
    /// Dark bubble background flag.
    #[serde(default)]
    pub dark: bool,
    /// Placement on canvas.
    pub placement: ItemPlacement,
}

impl StoryReactionItem {
    /// Creates a suggested-reaction story bubble centered at `center`.
    #[must_use]
    pub fn new(reaction: impl Into<String>, dark: bool, size: f64, center: PointF) -> Self {
        Self {
            reaction: reaction.into(),
            dark,
            placement: ItemPlacement {
                position: center,
                size: size.max(32.0),
                aspect_ratio: 1.0,
                ..ItemPlacement::default()
            },
        }
    }
}

/// Interactive story channel post repost card (`Editor::ItemMessage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryChannelPostItem {
    /// Source channel peer ID.
    pub channel_id: i64,
    /// Source message ID.
    pub msg_id: i32,
    /// Optional preview snippet.
    #[serde(default)]
    pub preview_text: String,
    /// Placement on canvas.
    pub placement: ItemPlacement,
}

impl StoryChannelPostItem {
    /// Creates a reposted channel message card on the story canvas.
    #[must_use]
    pub fn new(
        channel_id: i64,
        msg_id: i32,
        preview_text: impl Into<String>,
        image_size: Size,
        center: PointF,
    ) -> Self {
        let w = (f64::from(image_size.width.max(100)) * 0.65).clamp(80.0, 420.0);
        let h = (w * 0.35).max(40.0);
        Self {
            channel_id,
            msg_id,
            preview_text: preview_text.into(),
            placement: ItemPlacement {
                position: center,
                size: w,
                aspect_ratio: h / w,
                ..ItemPlacement::default()
            },
        }
    }
}

/// Normalized `[0.0, 1.0]` story media area geometry matching Telegram's `MediaAreaCoordinates`
/// (`Data::StoryArea` in `editor_story_areas.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StoryAreaGeometry {
    /// Normalized left edge (`cx - width / 2`).
    pub x: f64,
    /// Normalized top edge (`cy - height / 2`).
    pub y: f64,
    /// Normalized width in `(0.0, 1.0]`.
    pub width: f64,
    /// Normalized height in `(0.0, 1.0]`.
    pub height: f64,
    /// Clockwise rotation in degrees.
    pub rotation: f64,
    /// Optional pill corner radius percentage (`0.0` if unset).
    #[serde(default)]
    pub radius: f64,
}

/// Collected suggested reaction story area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectedReactionArea {
    /// Normalized coordinates.
    pub area: StoryAreaGeometry,
    /// Emoji glyph or custom reaction identifier.
    pub reaction: String,
    /// Whether horizontally flipped.
    pub flipped: bool,
    /// Whether dark theme bubble is active.
    pub dark: bool,
}

/// Collected clickable URL story area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectedUrlArea {
    /// Normalized coordinates.
    pub area: StoryAreaGeometry,
    /// Target URL.
    pub url: String,
}

/// Collected geo-location / venue story area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectedLocationArea {
    /// Normalized coordinates (with pill `radius` populated).
    pub area: StoryAreaGeometry,
    /// Geo/venue metadata.
    pub location: LocationData,
}

/// Collected weather story area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectedWeatherArea {
    /// Normalized coordinates (with pill `radius` populated).
    pub area: StoryAreaGeometry,
    /// Weather emoji.
    pub emoji: String,
    /// Background color.
    pub color: Color,
    /// Temperature in millicelsius.
    pub millicelsius: i32,
}

/// Collected channel post repost story area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectedChannelPostArea {
    /// Normalized coordinates.
    pub area: StoryAreaGeometry,
    /// Source channel peer ID.
    pub channel_id: i64,
    /// Source message ID.
    pub msg_id: i32,
}

/// All interactive story areas extracted from a [`PaintScene`] (`Editor::StoryAreas`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoryAreas {
    /// Suggested reaction stickers.
    pub reactions: Vec<CollectedReactionArea>,
    /// Clickable URL pills.
    pub urls: Vec<CollectedUrlArea>,
    /// Geo-location / venue pills.
    pub locations: Vec<CollectedLocationArea>,
    /// Weather pills.
    pub weather: Vec<CollectedWeatherArea>,
    /// Channel post repost cards.
    pub channel_posts: Vec<CollectedChannelPostArea>,
}

impl StoryAreas {
    /// Returns the total number of collected interactive story areas.
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.reactions.len()
            + self.urls.len()
            + self.locations.len()
            + self.weather.len()
            + self.channel_posts.len()
    }

    /// Returns `true` if no interactive story areas exist on the scene.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total_count() == 0
    }
}

/// Computes normalized [`StoryAreaGeometry`] for `placement` on `canvas`
/// (`AreaOf` in `editor_story_areas.cpp`).
#[must_use]
pub fn story_area_of(
    placement: &ItemPlacement,
    canvas: Size,
    with_pill_radius: bool,
) -> StoryAreaGeometry {
    let cw = f64::from(canvas.width.max(1));
    let ch = f64::from(canvas.height.max(1));
    let content = placement.inner_rect();
    let width = (content.width * placement.scale / cw).clamp(0.0, 1.0);
    let height = (content.height * placement.scale / ch).clamp(0.0, 1.0);
    let cx = (placement.position.x / cw).clamp(0.0, 1.0);
    let cy = (placement.position.y / ch).clamp(0.0, 1.0);
    let radius = if with_pill_radius {
        STORY_PILL_RADIUS_RATIO * content.height * placement.scale / cw * 100.0
    } else {
        0.0
    };
    StoryAreaGeometry {
        x: cx - width * 0.5,
        y: cy - height * 0.5,
        width,
        height,
        rotation: placement.rotation,
        radius,
    }
}

/// Collects all active interactive story areas from `scene` in ascending z-order
/// (`CollectStoryAreas` in `editor_story_areas.cpp`).
#[must_use]
pub fn collect_story_areas(scene: &PaintScene, canvas: Size) -> StoryAreas {
    let mut result = StoryAreas::default();
    if canvas.width <= 0 || canvas.height <= 0 {
        return result;
    }
    for entry in scene.active_items_by_z() {
        if entry.status != ItemStatus::Normal {
            continue;
        }
        match &entry.item {
            SceneItemKind::StoryLink(link) => {
                result.urls.push(CollectedUrlArea {
                    area: story_area_of(&link.placement, canvas, false),
                    url: link.url.clone(),
                });
            }
            SceneItemKind::StoryReaction(rx) => {
                result.reactions.push(CollectedReactionArea {
                    area: story_area_of(&rx.placement, canvas, false),
                    reaction: rx.reaction.clone(),
                    flipped: rx.placement.flipped,
                    dark: rx.dark,
                });
            }
            SceneItemKind::Location(loc) => {
                if !loc.location.has_valid_coordinates() {
                    continue;
                }
                result.locations.push(CollectedLocationArea {
                    area: story_area_of(&loc.placement, canvas, true),
                    location: loc.location.clone(),
                });
            }
            SceneItemKind::Weather(w) => {
                result.weather.push(CollectedWeatherArea {
                    area: story_area_of(&w.placement, canvas, true),
                    emoji: w.weather.emoji.clone(),
                    color: w.weather.color,
                    millicelsius: w.weather.millicelsius(),
                });
            }
            SceneItemKind::StoryChannelPost(post) => {
                result.channel_posts.push(CollectedChannelPostArea {
                    area: story_area_of(&post.placement, canvas, false),
                    channel_id: post.channel_id,
                    msg_id: post.msg_id,
                });
            }
            _ => {}
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Audio Track Timeline (`audio_track_timeline.h`, `editor_audio_timeline.h`)
// ---------------------------------------------------------------------------

/// External/overlay audio track attached to a photo/video story or clip
/// (`Editor::AudioTrack` in `editor/audio_track_timeline.h`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioTrackClip {
    /// Track display title.
    pub title: String,
    /// Track artist / performer.
    #[serde(default)]
    pub performer: String,
    /// Optional path to the local audio file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Total source audio duration in milliseconds.
    pub total_duration_ms: i64,
    /// Trim start position inside the audio track (ms).
    pub trim_from_ms: i64,
    /// Trim end position inside the audio track (ms).
    pub trim_till_ms: i64,
    /// Start offset on the video timeline (ms).
    #[serde(default)]
    pub offset_ms: i64,
    /// Playback gain in `[0.0, 1.0]`.
    pub volume: f32,
    /// Whether this overlay track is temporarily muted.
    #[serde(default)]
    pub muted: bool,
    /// Peak amplitude waveform bars (`0..=31`, matching Telegram's 5-bit waveform format).
    #[serde(default)]
    pub waveform: Vec<u8>,
}

impl AudioTrackClip {
    /// Creates a new audio track clip with deterministic fallback waveform bars if `waveform` is empty.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        performer: impl Into<String>,
        total_duration_ms: i64,
    ) -> Self {
        let dur = total_duration_ms.max(AUDIO_TRACK_MIN_DURATION_MS);
        let title = title.into();
        let performer = performer.into();
        let waveform = synthesize_waveform(&title, 48);
        Self {
            title,
            performer,
            path: None,
            total_duration_ms: dur,
            trim_from_ms: 0,
            trim_till_ms: dur,
            offset_ms: 0,
            volume: 1.0,
            muted: false,
            waveform,
        }
    }

    /// Selected trimmed duration in milliseconds.
    #[must_use]
    pub fn selected_duration_ms(&self) -> i64 {
        (self.trim_till_ms - self.trim_from_ms).max(0)
    }

    /// Returns `true` if this track produces audible output.
    #[must_use]
    pub fn is_audible(&self) -> bool {
        !self.muted && self.volume > 0.001 && self.selected_duration_ms() > 0
    }
}

/// Interactive audio track timeline state (`Editor::AudioTrackTimeline` + `Editor::AudioTimeline`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioTrackTimelineModel {
    /// Attached audio track, if any.
    pub track: Option<AudioTrackClip>,
    /// Optional maximum allowed clip duration (e.g. matching parent video trim length).
    pub max_clip_duration_ms: Option<i64>,
    /// Current preview playhead position in milliseconds inside `[trim_from_ms, trim_till_ms]`.
    pub playhead_ms: i64,
    /// Whether preview playback is currently running.
    pub playing: bool,
}

impl AudioTrackTimelineModel {
    /// Creates an empty audio track timeline optionally bounded by `max_clip_duration_ms`.
    #[must_use]
    pub fn new(max_clip_duration_ms: Option<i64>) -> Self {
        Self {
            track: None,
            max_clip_duration_ms,
            playhead_ms: 0,
            playing: false,
        }
    }

    /// Attaches or replaces the audio track, clamping its trim window to `max_clip_duration_ms`.
    pub fn set_track(&mut self, mut clip: AudioTrackClip) {
        clip.total_duration_ms = clip.total_duration_ms.max(AUDIO_TRACK_MIN_DURATION_MS);
        clip.trim_from_ms = clip.trim_from_ms.clamp(
            0,
            (clip.total_duration_ms - AUDIO_TRACK_MIN_DURATION_MS).max(0),
        );
        let max_len = self
            .max_clip_duration_ms
            .unwrap_or(clip.total_duration_ms)
            .max(AUDIO_TRACK_MIN_DURATION_MS);
        let max_till = (clip.trim_from_ms + max_len).min(clip.total_duration_ms);
        clip.trim_till_ms = clip.trim_till_ms.clamp(
            (clip.trim_from_ms + AUDIO_TRACK_MIN_DURATION_MS).min(clip.total_duration_ms),
            max_till,
        );
        clip.volume = clip.volume.clamp(0.0, 1.0);
        self.playhead_ms = clip.trim_from_ms;
        self.playing = false;
        self.track = Some(clip);
    }

    /// Removes the attached audio track (`removeRequests` in `audio_track_timeline.h`).
    pub fn clear_track(&mut self) {
        self.track = None;
        self.playhead_ms = 0;
        self.playing = false;
    }

    /// Updates the maximum allowed duration (e.g. when the user trims the video) and clamps
    /// the audio track trim window accordingly.
    pub fn set_max_clip_duration(&mut self, max_ms: Option<i64>) {
        self.max_clip_duration_ms = max_ms;
        if let Some(clip) = self.track.clone() {
            self.set_track(clip);
        }
    }

    /// Sets trim `[from_ms, till_ms]` respecting minimum clip length and `max_clip_duration_ms`.
    pub fn set_trim(&mut self, from_ms: i64, till_ms: i64) {
        let Some(ref mut clip) = self.track else {
            return;
        };
        let total = clip.total_duration_ms.max(AUDIO_TRACK_MIN_DURATION_MS);
        let min_len = AUDIO_TRACK_MIN_DURATION_MS.min(total);
        let max_len = self
            .max_clip_duration_ms
            .unwrap_or(total)
            .clamp(min_len, total);

        let mut from = from_ms.clamp(0, (total - min_len).max(0));
        let mut till = till_ms.clamp(from + min_len, total);
        if till - from > max_len {
            till = (from + max_len).min(total);
            if till - from < min_len {
                from = (till - min_len).max(0);
            }
        }
        clip.trim_from_ms = from;
        clip.trim_till_ms = till;
        self.playhead_ms = self.playhead_ms.clamp(from, till);
    }

    /// Slides the trimmed window to start at `new_from_ms` preserving its current length.
    pub fn slide_window_to(&mut self, new_from_ms: i64) {
        let Some(ref clip) = self.track else {
            return;
        };
        let span = clip.selected_duration_ms().max(AUDIO_TRACK_MIN_DURATION_MS);
        let max_from = (clip.total_duration_ms - span).max(0);
        let from = new_from_ms.clamp(0, max_from);
        self.set_trim(from, from + span);
    }

    /// Sets track volume in `[0.0, 1.0]`.
    pub fn set_volume(&mut self, volume: f32) {
        if let Some(ref mut clip) = self.track {
            clip.volume = volume.clamp(0.0, 1.0);
        }
    }

    /// Sets track mute flag.
    pub fn set_muted(&mut self, muted: bool) {
        if let Some(ref mut clip) = self.track {
            clip.muted = muted;
        }
    }

    /// Seeks preview playhead within `[trim_from_ms, trim_till_ms]`.
    pub fn seek(&mut self, time_ms: i64) {
        if let Some(ref clip) = self.track {
            self.playhead_ms = time_ms.clamp(clip.trim_from_ms, clip.trim_till_ms);
        }
    }

    /// Maps a timestamp (ms) to normalized strip fraction `[0.0, 1.0]`.
    #[must_use]
    pub fn fraction_at_time(&self, time_ms: i64) -> f64 {
        let Some(ref clip) = self.track else {
            return 0.0;
        };
        if clip.total_duration_ms <= 0 {
            return 0.0;
        }
        (time_ms as f64 / clip.total_duration_ms as f64).clamp(0.0, 1.0)
    }

    /// Maps a normalized strip fraction `[0.0, 1.0]` back to timestamp (ms).
    #[must_use]
    pub fn time_at_fraction(&self, fraction: f64) -> i64 {
        let Some(ref clip) = self.track else {
            return 0;
        };
        (fraction.clamp(0.0, 1.0) * clip.total_duration_ms as f64).round() as i64
    }

    /// Resamples the track waveform into `bar_count` normalized `[0.08, 1.0]` heights for painting.
    #[must_use]
    pub fn normalized_waveform_bars(&self, bar_count: usize) -> Vec<f32> {
        if bar_count == 0 {
            return Vec::new();
        }
        let Some(ref clip) = self.track else {
            return vec![0.15; bar_count];
        };
        if clip.waveform.is_empty() {
            return vec![0.25; bar_count];
        }
        let max_val = f32::from(*clip.waveform.iter().max().unwrap_or(&31)).max(1.0);
        let src_len = clip.waveform.len();
        (0..bar_count)
            .map(|i| {
                let idx = (i * src_len) / bar_count;
                let v = f32::from(clip.waveform[idx.min(src_len - 1)]) / max_val;
                v.clamp(0.08, 1.0)
            })
            .collect()
    }
}

fn synthesize_waveform(seed_text: &str, count: usize) -> Vec<u8> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed_text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (0..count)
        .map(|i| {
            h = h
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let env = ((i as f64 / count.max(1) as f64) * PI).sin();
            let raw = ((h >> 32) % 24) as f64;
            (6.0 + raw * (0.45 + 0.55 * env)).round().clamp(3.0, 31.0) as u8
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Draw-to-reply session (`history_view_draw_to_reply.{h,cpp}`)
// ---------------------------------------------------------------------------

/// Request to annotate a photo message and send the drawing back as a reply
/// (`Data::DrawToReplyRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawToReplyRequest {
    /// Chat peer ID where the reply will be sent.
    pub peer_id: i64,
    /// Message ID being replied to.
    pub reply_to_msg_id: i32,
    /// Optional photo ID if the source message is a photo.
    pub photo_id: Option<u64>,
    /// Optional document ID if the source message is an image document.
    pub document_id: Option<u64>,
    /// Display file name for the reply photo.
    pub file_name: String,
}

/// Self-contained draw-to-reply editing session (`OpenDrawToReplyEditor` in
/// `history_view_draw_to_reply.cpp`).
#[derive(Debug, Clone)]
pub struct DrawToReplySession {
    /// Reply metadata linking back to the original message.
    pub request: DrawToReplyRequest,
    /// Original resolved photo image.
    pub source_image: RgbaImage,
    /// Active non-destructive edits (crop/rotate/flip/enhance).
    pub modifications: PhotoModifications,
    /// Paint scene holding freehand strokes, text, shapes, and tags.
    pub scene: PaintScene,
}

impl DrawToReplySession {
    /// Starts a draw-to-reply session from `request` and `source_image`.
    /// Returns [`EditorError::EmptyImage`] if `source_image` has zero dimensions.
    pub fn new(request: DrawToReplyRequest, source_image: RgbaImage) -> Result<Self, EditorError> {
        if source_image.width() == 0 || source_image.height() == 0 {
            return Err(EditorError::EmptyImage);
        }
        let scene = PaintScene::from_size(Size::new(
            source_image.width() as i32,
            source_image.height() as i32,
        ));
        Ok(Self {
            request,
            source_image,
            modifications: PhotoModifications::default(),
            scene,
        })
    }

    /// Produces the baked annotated `RgbaImage` (`Editor::ImageModified`).
    #[must_use]
    pub fn render_image(&self) -> RgbaImage {
        let mut mods = self.modifications.clone();
        if self.scene.has_active_items() {
            mods.paint = Some(self.scene.clone());
        }
        apply(&self.source_image, &mods)
    }

    /// Exports the annotated draw-to-reply image to Telegram-compliant JPEG bytes.
    pub fn export_reply_jpeg(&self, large: bool) -> Result<ExportedJpeg, EditorError> {
        let rendered = self.render_image();
        let settings = if large {
            PhotoExportSettings::large()
        } else {
            PhotoExportSettings::standard()
        };
        export_jpeg(&rendered, &settings)
    }
}

// ---------------------------------------------------------------------------
// Software rasterization helpers (`tiny-skia`) called by `PaintScene::render_baked`
// ---------------------------------------------------------------------------

pub(crate) fn render_shape_onto(target: &mut Pixmap, item: &ShapeItem, scene_to_target: Affine2D) {
    let full = item
        .placement
        .scene_transform()
        .then(scene_to_target)
        .to_skia();
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color_rgba8(item.color.r, item.color.g, item.color.b, item.color.a);

    let stroke = Stroke {
        width: item.stroke_width.max(1.0) as f32,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };

    if item.shape == ShapeType::Arrow {
        if let Some(shaft) = build_arrow_shaft_path(item) {
            target.stroke_path(&shaft, &paint, &stroke, full, None);
        }
        if let Some(head) = build_arrow_head_path(item) {
            target.fill_path(&head, &paint, FillRule::Winding, full, None);
            target.stroke_path(&head, &paint, &stroke, full, None);
        }
        return;
    }

    let Some(path) = build_shape_path(item) else {
        return;
    };
    if item.fill {
        target.fill_path(&path, &paint, FillRule::Winding, full, None);
    }
    target.stroke_path(&path, &paint, &stroke, full, None);
}

pub(crate) fn render_text_onto(target: &mut Pixmap, item: &TextItem, scene_to_target: Affine2D) {
    let rect = item.placement.inner_rect();
    let full = item
        .placement
        .scene_transform()
        .then(scene_to_target)
        .to_skia();

    if let Some(bg) = text_background_color(item.color, item.style) {
        let radius = (rect.height * 0.22).min(16.0) as f32;
        if let Some(path) = rounded_rect_path_f32(
            rect.x as f32,
            rect.y as f32,
            rect.width as f32,
            rect.height as f32,
            radius,
        ) {
            let mut bg_paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            bg_paint.set_color_rgba8(bg.r, bg.g, bg.b, bg.a);
            target.fill_path(&path, &bg_paint, FillRule::Winding, full, None);
            if item.style == TextStyle::Framed {
                let mut border_paint = Paint {
                    anti_alias: true,
                    ..Paint::default()
                };
                border_paint.set_color_rgba8(
                    item.color.r,
                    item.color.g,
                    item.color.b,
                    item.color.a,
                );
                let stroke = Stroke {
                    width: 2.0,
                    ..Stroke::default()
                };
                target.stroke_path(&path, &border_paint, &stroke, full, None);
            }
        }
    }

    // Rasterize crisp 5x7 bitmap glyph strokes for ASCII/fallback characters so baked exports
    // always show legible text even without a system font rasterizer.
    let fg = effective_text_color(item.color, item.style);
    render_bitmap_label(target, &item.text, rect, fg, item.alignment, full);
}

pub(crate) fn render_pill_onto(
    target: &mut Pixmap,
    placement: &ItemPlacement,
    bg: Color,
    fg: Color,
    label: &str,
    scene_to_target: Affine2D,
) {
    let rect = placement.inner_rect();
    let full = placement.scene_transform().then(scene_to_target).to_skia();
    let radius = (rect.height * STORY_PILL_RADIUS_RATIO) as f32;
    if let Some(path) = rounded_rect_path_f32(
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
        radius.max(4.0),
    ) {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color_rgba8(bg.r, bg.g, bg.b, bg.a);
        target.fill_path(&path, &paint, FillRule::Winding, full, None);
    }
    render_bitmap_label(target, label, rect, fg, TextAlignment::Center, full);
}

fn build_shape_path(item: &ShapeItem) -> Option<Path> {
    let rect = item.shape_rect();
    match item.shape {
        ShapeType::Circle => SkiaRect::from_xywh(
            rect.x as f32,
            rect.y as f32,
            rect.width as f32,
            rect.height as f32,
        )
        .and_then(PathBuilder::from_oval),
        ShapeType::Rectangle => {
            let r = (SHAPE_ROUNDING_RATIO * rect.width.min(rect.height) * 0.5) as f32;
            rounded_rect_path_f32(
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                rect.height as f32,
                r,
            )
        }
        ShapeType::Star => {
            let outer = (rect.width / STAR_BOX_WIDTH).min(rect.height / STAR_BOX_HEIGHT);
            let inner = outer * STAR_INNER_RATIO;
            let c = rect.center();
            let cy = c.y - (STAR_BOX_HEIGHT * outer * 0.5 - outer);
            let mut pb = PathBuilder::new();
            for i in 0..5 {
                let angle = -PI * 0.5 + (i as f64) * 2.0 * PI / 5.0;
                let inner_angle = angle + PI / 5.0;
                let ox = (c.x + outer * angle.cos()) as f32;
                let oy = (cy + outer * angle.sin()) as f32;
                let ix = (c.x + inner * inner_angle.cos()) as f32;
                let iy = (cy + inner * inner_angle.sin()) as f32;
                if i == 0 {
                    pb.move_to(ox, oy);
                } else {
                    pb.line_to(ox, oy);
                }
                pb.line_to(ix, iy);
            }
            pb.close();
            pb.finish()
        }
        ShapeType::Bubble => {
            let body_h = rect.height * BUBBLE_BODY_RATIO;
            let r = (SHAPE_ROUNDING_RATIO * rect.width.min(body_h) * 0.5) as f32;
            let mut pb = PathBuilder::new();
            append_rounded_rect(
                &mut pb,
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                body_h as f32,
                r,
            );
            let tip_x = rect.center().x + item.direction() * BUBBLE_TAIL_RATIO * rect.width * 0.5;
            let tip_y = rect.bottom();
            let bx = rect.center().x;
            let by = rect.y + body_h * 0.78;
            let half_base = (rect.width.min(body_h) * 0.18).max(3.0);
            pb.move_to((bx - half_base) as f32, by as f32);
            pb.line_to(tip_x as f32, tip_y as f32);
            pb.line_to((bx + half_base) as f32, by as f32);
            pb.close();
            pb.finish()
        }
        ShapeType::Arrow => None,
    }
}

fn build_arrow_shaft_path(item: &ShapeItem) -> Option<Path> {
    let pts = item.arrow_points();
    let mut pb = PathBuilder::new();
    pb.move_to(pts.start.x as f32, pts.start.y as f32);
    pb.quad_to(
        pts.control.x as f32,
        pts.control.y as f32,
        pts.end.x as f32,
        pts.end.y as f32,
    );
    pb.finish()
}

fn build_arrow_head_path(item: &ShapeItem) -> Option<Path> {
    let pts = item.arrow_points();
    let dx = pts.end.x - pts.control.x;
    let dy = pts.end.y - pts.control.y;
    let len = dx.hypot(dy);
    if len <= 1e-6 {
        return None;
    }
    let fx = dx / len;
    let fy = dy / len;
    let rot = |ang: f64| -> (f64, f64) {
        (
            fx * ang.cos() - fy * ang.sin(),
            fx * ang.sin() + fy * ang.cos(),
        )
    };
    let apex_x = pts.end.x + fx * (pts.head_side * 0.5);
    let apex_y = pts.end.y + fy * (pts.head_side * 0.5);
    let (w1x, w1y) = rot(ARROW_HEAD_ANGLE_RAD);
    let (w2x, w2y) = rot(-ARROW_HEAD_ANGLE_RAD);
    let mut pb = PathBuilder::new();
    pb.move_to(apex_x as f32, apex_y as f32);
    pb.line_to(
        (apex_x - w1x * pts.head_side) as f32,
        (apex_y - w1y * pts.head_side) as f32,
    );
    pb.line_to(
        (apex_x - w2x * pts.head_side) as f32,
        (apex_y - w2y * pts.head_side) as f32,
    );
    pb.close();
    pb.finish()
}

fn rounded_rect_path_f32(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    append_rounded_rect(&mut pb, x, y, w, h, radius);
    pb.finish()
}

fn append_rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, radius: f32) {
    let r = radius.min(w * 0.5).min(h * 0.5).max(0.0);
    if r <= 0.01 {
        pb.move_to(x, y);
        pb.line_to(x + w, y);
        pb.line_to(x + w, y + h);
        pb.line_to(x, y + h);
        pb.close();
        return;
    }
    const KAPPA: f32 = 0.552_284_8;
    let c = r * KAPPA;
    let right = x + w;
    let bottom = y + h;
    pb.move_to(x + r, y);
    pb.line_to(right - r, y);
    pb.cubic_to(right - r + c, y, right, y + r - c, right, y + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(
        right,
        bottom - r + c,
        right - r + c,
        bottom,
        right - r,
        bottom,
    );
    pb.line_to(x + r, bottom);
    pb.cubic_to(x + r - c, bottom, x, bottom - r + c, x, bottom - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - c, x + r - c, y, x + r, y);
    pb.close();
}

fn render_bitmap_label(
    target: &mut Pixmap,
    text: &str,
    rect: RectF,
    color: Color,
    alignment: TextAlignment,
    transform: tiny_skia::Transform,
) {
    let trimmed = text.trim();
    if trimmed.is_empty() || rect.width <= 4.0 || rect.height <= 4.0 {
        return;
    }
    let mut paint = Paint {
        anti_alias: false,
        ..Paint::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);

    let lines: Vec<&str> = trimmed.split('\n').take(4).collect();
    let line_count = lines.len().max(1) as f64;
    let inner_h = rect.height * 0.72;
    let inner_w = rect.width * 0.84;
    let line_h = inner_h / line_count;
    let cell = (line_h / 7.5).clamp(1.0, 6.0);
    let top_start = rect.y + (rect.height - line_h * line_count) * 0.5;

    for (li, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().take(24).collect();
        if chars.is_empty() {
            continue;
        }
        let glyph_step = cell * 4.2;
        let line_w = (chars.len() as f64 * glyph_step).min(inner_w);
        let start_x = match alignment {
            TextAlignment::Left => rect.x + rect.width * 0.08,
            TextAlignment::Right => rect.right() - rect.width * 0.08 - line_w,
            TextAlignment::Center => rect.x + (rect.width - line_w) * 0.5,
        };
        let ly = top_start + li as f64 * line_h + (line_h - cell * 5.0) * 0.5;
        let mut pb = PathBuilder::new();
        for (ci, ch) in chars.iter().enumerate() {
            if ch.is_whitespace() {
                continue;
            }
            let gx = start_x + ci as f64 * glyph_step;
            if gx + cell * 3.0 > rect.right() - 2.0 {
                break;
            }
            let bits = mini_glyph_3x5(*ch);
            for row in 0..5usize {
                for col in 0..3usize {
                    let bit_idx = (4 - row) * 3 + (2 - col);
                    if ((bits >> bit_idx) & 1) != 0 {
                        let px = (gx + col as f64 * cell) as f32;
                        let py = (ly + row as f64 * cell) as f32;
                        let c = cell as f32;
                        pb.move_to(px, py);
                        pb.line_to(px + c, py);
                        pb.line_to(px + c, py + c);
                        pb.line_to(px, py + c);
                        pb.close();
                    }
                }
            }
        }
        if let Some(path) = pb.finish() {
            target.fill_path(&path, &paint, FillRule::Winding, transform, None);
        }
    }
}

/// Compact 3x5 pixel font bitmask for ASCII alphanumerics & symbols (15 bits, MSB top-left).
fn mini_glyph_3x5(ch: char) -> u16 {
    match ch.to_ascii_uppercase() {
        'A' => 0b010_101_111_101_101,
        'B' => 0b110_101_110_101_110,
        'C' => 0b011_100_100_100_011,
        'D' => 0b110_101_101_101_110,
        'E' => 0b111_100_110_100_111,
        'F' => 0b111_100_110_100_100,
        'G' => 0b011_100_101_101_011,
        'H' => 0b101_101_111_101_101,
        'I' => 0b111_010_010_010_111,
        'J' => 0b001_001_001_101_010,
        'K' => 0b101_101_110_101_101,
        'L' => 0b100_100_100_100_111,
        'M' => 0b101_111_111_101_101,
        'N' => 0b110_101_101_101_101,
        'O' | '0' => 0b111_101_101_101_111,
        'P' => 0b110_101_110_100_100,
        'Q' => 0b111_101_101_111_001,
        'R' => 0b110_101_110_101_101,
        'S' | '5' => 0b011_100_010_001_110,
        'T' => 0b111_010_010_010_010,
        'U' => 0b101_101_101_101_111,
        'V' => 0b101_101_101_101_010,
        'W' => 0b101_101_111_111_101,
        'X' => 0b101_101_010_101_101,
        'Y' => 0b101_101_010_010_010,
        'Z' => 0b111_001_010_100_111,
        '1' => 0b010_110_010_010_111,
        '2' => 0b110_001_010_100_111,
        '3' => 0b110_001_010_001_110,
        '4' => 0b101_101_111_001_001,
        '6' => 0b011_100_111_101_111,
        '7' => 0b111_001_010_010_010,
        '8' => 0b111_101_111_101_111,
        '9' => 0b111_101_111_001_110,
        '+' => 0b000_010_111_010_000,
        '-' => 0b000_000_111_000_000,
        '.' => 0b000_000_000_000_010,
        ':' => 0b000_010_000_010_000,
        '°' => 0b010_101_010_000_000,
        '!' => 0b010_010_010_000_010,
        _ => 0b111_101_101_101_111,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{BrushStroke, StrokePoint};
    use crate::common::BrushTool;

    #[test]
    fn text_item_layout_colors_and_rasterization() {
        let canvas = Size::new(400, 300);
        assert_eq!(
            effective_text_color(Color::WHITE, TextStyle::Opaque),
            Color::BLACK
        );
        assert_eq!(
            effective_text_color(Color::BLACK, TextStyle::Opaque),
            Color::WHITE
        );
        assert!(text_background_color(Color::WHITE, TextStyle::Plain).is_none());
        assert!(text_background_color(Color::WHITE, TextStyle::SemiTransparent).is_some());

        let mut scene = PaintScene::from_size(canvas);
        let item = TextItem::new(
            "HELLO",
            Color::rgb(255, 220, 60),
            28.0,
            TextStyle::Opaque,
            TextTypeface::Default,
            TextAlignment::Center,
            canvas,
            PointF::new(200.0, 150.0),
        );
        let id = scene.add_text(item);
        assert!(scene.has_active_items());

        let base = RgbaImage::from_pixel(400, 300, image::Rgba([20, 30, 40, 255]));
        let mods = PhotoModifications {
            paint: Some(scene.clone()),
            ..PhotoModifications::default()
        };
        let rendered = apply(&base, &mods);
        // Center region should be altered by the yellow opaque text pill.
        let center_px = rendered.get_pixel(200, 150);
        assert_ne!(center_px.0, [20, 30, 40, 255]);

        // Placement edit + undo/redo work uniformly on text items.
        let mut moved = scene
            .item_placement(id)
            .expect("text item should expose placement");
        moved.position = PointF::new(120.0, 80.0);
        assert!(scene.set_item_placement(id, moved));
        assert_eq!(
            scene.item_placement(id).expect("placement").position,
            PointF::new(120.0, 80.0)
        );
        assert!(scene.undo());
        assert_eq!(
            scene.item_placement(id).expect("placement").position,
            PointF::new(200.0, 150.0)
        );
        assert!(scene.redo());
        assert_eq!(
            scene.item_placement(id).expect("placement").position,
            PointF::new(120.0, 80.0)
        );
    }

    #[test]
    fn shape_items_geometry_arrow_bend_and_rasterization() {
        let mut arrow = ShapeItem::new(
            ShapeType::Arrow,
            Color::rgb(255, 64, 64),
            6.0,
            false,
            PointF::new(160.0, 120.0),
            120.0,
        );
        let straight_handle = arrow.bend_handle_center();
        arrow.set_bend(0.75);
        assert!((arrow.bend - 0.75).abs() < 1e-6);
        let bent_handle = arrow.bend_handle_center();
        assert!(
            (bent_handle.y - straight_handle.y).abs() > 10.0,
            "bending arrow should shift handle vertically"
        );
        // Out-of-range bend clamps to [-1, 1].
        arrow.set_bend(5.0);
        assert!((arrow.bend - 1.0).abs() < 1e-6);

        let star = ShapeItem::new(
            ShapeType::Star,
            Color::rgb(255, 200, 40),
            4.0,
            true,
            PointF::new(80.0, 80.0),
            70.0,
        );
        assert!((star.placement.aspect_ratio - STAR_BOX_HEIGHT / STAR_BOX_WIDTH).abs() < 1e-6);

        let mut scene = PaintScene::from_size(Size::new(320, 240));
        scene.add_shape(star);
        scene.add_shape(arrow);
        scene.add_shape(ShapeItem::new(
            ShapeType::Bubble,
            Color::rgb(80, 180, 255),
            4.0,
            true,
            PointF::new(240.0, 170.0),
            80.0,
        ));

        let base = RgbaImage::from_pixel(320, 240, image::Rgba([10, 10, 10, 255]));
        let out = apply(
            &base,
            &PhotoModifications {
                paint: Some(scene),
                ..PhotoModifications::default()
            },
        );
        assert_ne!(out.get_pixel(80, 80).0, [10, 10, 10, 255]);
        assert_ne!(out.get_pixel(240, 160).0, [10, 10, 10, 255]);
    }

    #[test]
    fn story_areas_collect_location_weather_url_reaction_and_post() {
        let canvas = Size::new(1080, 1920);
        let mut scene = PaintScene::from_size(canvas);

        let loc = LocationItem::new(
            LocationData {
                lat: 47.3769,
                lon: 8.5417,
                title: "Central Plaza".to_owned(),
                city: "Sample City".to_owned(),
                country_iso2: "CH".to_owned(),
                ..LocationData::default()
            },
            canvas,
            PointF::new(540.0, 960.0),
        );
        scene.add_location(loc);

        // Invalid coordinates must be skipped by collect_story_areas.
        let bad_loc = LocationItem::new(
            LocationData {
                lat: 999.0,
                lon: 0.0,
                title: "Invalid".to_owned(),
                ..LocationData::default()
            },
            canvas,
            PointF::new(200.0, 200.0),
        );
        scene.add_location(bad_loc);

        let weather = WeatherItem::new(
            WeatherData {
                emoji: "🌤️".to_owned(),
                celsius: 21.5,
                color: Color::rgb(64, 167, 227),
            },
            canvas,
            PointF::new(300.0, 400.0),
        );
        scene.add_weather(weather);

        scene.add_story_link(StoryLinkItem::new(
            "https://example.org",
            "Example",
            canvas,
            PointF::new(540.0, 1400.0),
        ));
        scene.add_story_reaction(StoryReactionItem::new(
            "🔥",
            true,
            120.0,
            PointF::new(800.0, 600.0),
        ));
        scene.add_story_channel_post(StoryChannelPostItem::new(
            100_555,
            42,
            "Announcement",
            canvas,
            PointF::new(540.0, 1200.0),
        ));

        let areas = collect_story_areas(&scene, canvas);
        assert_eq!(areas.locations.len(), 1);
        assert_eq!(areas.locations[0].location.title, "Central Plaza");
        assert!(areas.locations[0].area.radius > 0.0);
        assert_eq!(areas.weather.len(), 1);
        assert_eq!(areas.weather[0].millicelsius, 21_500);
        assert!(areas.weather[0].area.radius > 0.0);
        assert_eq!(areas.urls.len(), 1);
        assert_eq!(areas.urls[0].url, "https://example.org");
        assert_eq!(areas.reactions.len(), 1);
        assert_eq!(areas.reactions[0].reaction, "🔥");
        assert!(areas.reactions[0].dark);
        assert_eq!(areas.channel_posts.len(), 1);
        assert_eq!(areas.channel_posts[0].channel_id, 100_555);
        assert_eq!(areas.channel_posts[0].msg_id, 42);
        assert_eq!(areas.total_count(), 5);

        // JSON draft serialization round-trip preserves all scene item kinds.
        let json = serde_json::to_string(&scene).expect("serialize scene");
        let parsed: PaintScene = serde_json::from_str(&json).expect("deserialize scene");
        assert_eq!(parsed.fingerprint(), scene.fingerprint());
    }

    #[test]
    fn audio_track_timeline_trim_clamp_slide_and_waveform() {
        let mut tl = AudioTrackTimelineModel::new(Some(15_000));
        let clip = AudioTrackClip::new("Ambient Loop", "Synth Artist", 60_000);
        tl.set_track(clip);
        let tr = tl.track.as_ref().expect("track set");
        assert_eq!(tr.trim_from_ms, 0);
        assert_eq!(tr.trim_till_ms, 15_000);

        // Trimming beyond max_clip_duration clamps to 15_000ms span.
        tl.set_trim(5_000, 35_000);
        let tr = tl.track.as_ref().expect("track");
        assert_eq!(tr.trim_from_ms, 5_000);
        assert_eq!(tr.trim_till_ms, 20_000);

        // Sliding window preserves 15_000ms span and clamps at end of track.
        tl.slide_window_to(50_000);
        let tr = tl.track.as_ref().expect("track");
        assert_eq!(tr.trim_from_ms, 45_000);
        assert_eq!(tr.trim_till_ms, 60_000);

        // Seek clamps inside [trim_from_ms, trim_till_ms].
        tl.seek(10_000);
        assert_eq!(tl.playhead_ms, 45_000);

        // Volume & mute flags.
        tl.set_volume(0.6);
        assert!(tl.track.as_ref().expect("track").is_audible());
        tl.set_muted(true);
        assert!(!tl.track.as_ref().expect("track").is_audible());

        // Waveform resampling produces requested bar count in [0.08, 1.0].
        let bars = tl.normalized_waveform_bars(24);
        assert_eq!(bars.len(), 24);
        assert!(bars.iter().all(|&b| (0.08..=1.0).contains(&b)));
    }

    #[test]
    fn draw_to_reply_session_annotates_and_exports_jpeg() {
        let base = RgbaImage::from_pixel(200, 150, image::Rgba([60, 120, 180, 255]));
        let req = DrawToReplyRequest {
            peer_id: 42,
            reply_to_msg_id: 777,
            photo_id: Some(9001),
            document_id: None,
            file_name: "reply_sketch.jpg".to_owned(),
        };
        let mut session =
            DrawToReplySession::new(req.clone(), base.clone()).expect("valid session");
        session.scene.add_stroke(BrushStroke {
            tool: BrushTool::Pen,
            color: Color::rgb(255, 40, 40),
            size: 6.0,
            points: vec![StrokePoint::new(20.0, 20.0), StrokePoint::new(180.0, 130.0)],
        });
        session.scene.add_text(TextItem::new(
            "HERE!",
            Color::WHITE,
            20.0,
            TextStyle::SemiTransparent,
            TextTypeface::Default,
            TextAlignment::Center,
            Size::new(200, 150),
            PointF::new(100.0, 75.0),
        ));

        let rendered = session.render_image();
        assert_ne!(rendered.as_raw(), base.as_raw());
        let exported = session.export_reply_jpeg(false).expect("jpeg export");
        assert!(exported.bytes.starts_with(&[0xFF, 0xD8]));
        assert_eq!(exported.width, 200);
        assert_eq!(exported.height, 150);
    }
}
