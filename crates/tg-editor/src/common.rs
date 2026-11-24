//! Core photo editor data types and modification state.
//!
//! Port of `editor/photo_editor_common.h`, `editor/photo_editor_common.cpp`,
//! and `editor/photo_editor_inner_common.h`.

use serde::{Deserialize, Serialize};

use crate::geometry::{Color, Rect, Size, normalize_angle_90};
use crate::scene::PaintScene;

/// Forum/group userpic rounded-corner radius multiplier (`Ui::ForumUserpicRadiusMultiplier`).
pub const FORUM_USERPIC_RADIUS_MULTIPLIER: f64 = 0.3;

/// Default brush size slider ratio (`kDefaultBrushSizeRatio` in `photo_editor.cpp`).
pub const DEFAULT_BRUSH_SIZE_RATIO: f64 = 0.9;

/// Minimum brush stroke width in scene pixels (`kMinBrush` in `editor_paint.cpp`).
pub const MIN_BRUSH_WIDTH: f64 = 1.0;

/// Maximum brush stroke width in scene pixels (`kMaxBrush` in `editor_paint.cpp`).
pub const MAX_BRUSH_WIDTH: f64 = 25.0;

/// Corner rounding presets for rounded-rect userpics/masks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoundedCornersLevel {
    /// Full circle (`0.5`).
    Circle,
    /// Large forum-style rounding (`0.3`).
    #[default]
    Large,
    /// Medium rounding (`0.2`).
    Medium,
    /// Small rounding (`0.12`).
    Small,
    /// Sharp rectangle (`0.0`).
    None,
}

impl RoundedCornersLevel {
    /// Returns the corner radius multiplier relative to `min(width, height)`.
    ///
    /// Matches `Editor::RoundedCornersMultiplier`.
    #[must_use]
    pub const fn multiplier(self) -> f64 {
        match self {
            Self::Circle => 0.5,
            Self::Large => FORUM_USERPIC_RADIUS_MULTIPLIER,
            Self::Medium => 0.2,
            Self::Small => 0.12,
            Self::None => 0.0,
        }
    }
}

/// Visual shape of the crop area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CropType {
    /// Standard rectangular crop.
    #[default]
    Rect,
    /// Circular/elliptical crop (used for userpics).
    Ellipse,
    /// Rounded-rectangle crop (used for forum/bot userpics).
    RoundedRect,
}

/// Whether the non-rectangular crop shape is a visual hint or baked into alpha.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CropMode {
    /// Show the shape as an overlay hint only; output stays opaque rectangular.
    #[default]
    Hint,
    /// Apply the shape as a transparency mask to the output image.
    Mask,
}

/// Configuration for launching a photo editor session (`Editor::EditorData`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorData {
    /// Optional target aspect/exact size (e.g. square for profile photos).
    pub exact_size: Option<Size>,
    /// Crop overlay/mask shape.
    pub crop_type: CropType,
    /// Whether the shape is a hint or baked mask.
    pub crop_mode: CropMode,
    /// Original aspect ratio hint (used by rounded-corner ratio toggle).
    pub original_ratio: f64,
    /// Locks crop aspect ratio (true for userpics or fixed-ratio crops).
    pub keep_aspect_ratio: bool,
    /// Disables canvas expansion outside the image bounds.
    pub fixed_crop: bool,
    /// True when setting a profile photo for another user.
    pub for_other_user: bool,
}

impl Default for EditorData {
    fn default() -> Self {
        Self {
            exact_size: None,
            crop_type: CropType::Rect,
            crop_mode: CropMode::Hint,
            original_ratio: 0.0,
            keep_aspect_ratio: false,
            fixed_crop: false,
            for_other_user: false,
        }
    }
}

/// Top-level editor mode (`Editor::PhotoEditorMode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhotoEditorMode {
    /// Active editor screen.
    pub mode: EditorModeKind,
    /// Transition action.
    pub action: EditorModeAction,
}

/// Screen within the photo editor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EditorModeKind {
    /// Crop / rotate / flip mode.
    #[default]
    Transform,
    /// Freehand drawing / stickers mode.
    Paint,
    /// Exiting the editor.
    Out,
}

/// Transition action when switching editor modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EditorModeAction {
    /// No commit/discard action.
    #[default]
    None,
    /// Commit changes from the previous mode.
    Save,
    /// Discard changes from the previous mode.
    Discard,
}

/// Checkpoint slot for scene save/restore (`Editor::SaveState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SaveState {
    /// Whole-editor saved state (restored when cancelling the entire editor).
    Save,
    /// Paint-mode checkpoint (restored when cancelling paint mode only).
    Keep,
}

/// Brush drawing tool (`Editor::Brush::Tool`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum BrushTool {
    /// Solid anti-aliased pen.
    #[default]
    Pen = 0,
    /// Solid pen with directional arrowhead at the end of the stroke.
    Arrow = 1,
    /// Semi-transparent highlighter (`0.35` opacity, `2.5x` width, `Source` blend).
    Marker = 2,
    /// Eraser removing pixels from underlying brush strokes (`DestinationOut`).
    Eraser = 3,
    /// Stack-blur brush (`3.0x` width, radius 20) over base image + scene.
    Blur = 4,
}

impl BrushTool {
    /// UI order index matching `ToolIndex` in `photo_editor.cpp` and `color_picker.cpp`:
    /// `Pen=0, Arrow=1, Marker=2, Blur=3, Eraser=4`.
    #[must_use]
    pub const fn ui_index(self) -> usize {
        match self {
            Self::Pen => 0,
            Self::Arrow => 1,
            Self::Marker => 2,
            Self::Blur => 3,
            Self::Eraser => 4,
        }
    }

    /// Converts a UI order index (`0..5`) back to a [`BrushTool`].
    #[must_use]
    pub const fn from_ui_index(index: usize) -> Self {
        match index {
            0 => Self::Pen,
            1 => Self::Arrow,
            2 => Self::Marker,
            3 => Self::Blur,
            4 => Self::Eraser,
            _ => Self::Pen,
        }
    }

    /// Converts a serialized enum integer (`0..=4`) to [`BrushTool`].
    #[must_use]
    pub const fn from_serialized(value: i32) -> Self {
        match value {
            0 => Self::Pen,
            1 => Self::Arrow,
            2 => Self::Marker,
            3 => Self::Eraser,
            4 => Self::Blur,
            _ => Self::Pen,
        }
    }

    /// Returns true if this tool ignores user color selection (`Eraser` or `Blur`).
    #[must_use]
    pub const fn has_fixed_color(self) -> bool {
        matches!(self, Self::Eraser | Self::Blur)
    }

    /// Default color for this tool (`DefaultBrushColor` in `photo_editor.cpp`).
    #[must_use]
    pub const fn default_color(self) -> Color {
        match self {
            Self::Pen => Color::rgb(234, 39, 57),
            Self::Arrow => Color::rgb(252, 150, 77),
            Self::Marker => Color::rgb(252, 222, 101),
            Self::Eraser | Self::Blur => Color::BLACK,
        }
    }
}

/// Brush tool settings (`Editor::Brush`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Brush {
    /// Normalized size slider ratio (`0.1..=1.0`, default `0.9`).
    pub size_ratio: f64,
    /// Selected brush color.
    pub color: Color,
    /// Active brush tool.
    pub tool: BrushTool,
}

impl Default for Brush {
    fn default() -> Self {
        Self::default_for(BrushTool::Pen)
    }
}

impl Brush {
    /// Creates the default brush for a specific tool (`DefaultBrush` in `photo_editor.cpp`).
    #[must_use]
    pub const fn default_for(tool: BrushTool) -> Self {
        Self {
            size_ratio: DEFAULT_BRUSH_SIZE_RATIO,
            color: tool.default_color(),
            tool,
        }
    }

    /// Computes base brush stroke width in scene pixels (`BrushSize` in `editor_paint.cpp`).
    #[must_use]
    pub fn stroke_width(self) -> f64 {
        MIN_BRUSH_WIDTH + (MAX_BRUSH_WIDTH - MIN_BRUSH_WIDTH) * self.size_ratio
    }

    /// Normalizes color for fixed-color tools (`NormalizeBrushColor` in `color_picker.cpp`).
    pub fn normalize_color(&mut self) {
        if self.tool.has_fixed_color() {
            self.color = Color::BLACK;
        }
    }
}

/// Text overlay style (`Editor::TextStyle`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum TextStyle {
    /// Framed text box.
    Framed = 0,
    /// Semi-transparent background.
    SemiTransparent = 1,
    /// Plain text without background.
    #[default]
    Plain = 2,
    /// Opaque filled background.
    Opaque = 3,
}

impl TextStyle {
    /// Converts from serialized integer.
    #[must_use]
    pub const fn from_serialized(value: i32) -> Self {
        match value {
            0 => Self::Framed,
            1 => Self::SemiTransparent,
            2 => Self::Plain,
            3 => Self::Opaque,
            _ => Self::Plain,
        }
    }
}

/// Text overlay typeface (`Editor::TextTypeface`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum TextTypeface {
    /// Default sans-serif font.
    #[default]
    Default = 0,
    /// Italic style.
    Italic = 1,
    /// Serif font.
    Serif = 2,
    /// Condensed sans-serif.
    Condensed = 3,
    /// Monospace font.
    Monospace = 4,
}

impl TextTypeface {
    /// Converts from serialized integer.
    #[must_use]
    pub const fn from_serialized(value: i32) -> Self {
        match value {
            0 => Self::Default,
            1 => Self::Italic,
            2 => Self::Serif,
            3 => Self::Condensed,
            4 => Self::Monospace,
            _ => Self::Default,
        }
    }
}

/// Text overlay horizontal alignment (`Editor::TextAlignment`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum TextAlignment {
    /// Centered text.
    #[default]
    Center = 0,
    /// Left-aligned text.
    Left = 1,
    /// Right-aligned text.
    Right = 2,
}

impl TextAlignment {
    /// Converts from serialized integer.
    #[must_use]
    pub const fn from_serialized(value: i32) -> Self {
        match value {
            0 => Self::Center,
            1 => Self::Left,
            2 => Self::Right,
            _ => Self::Center,
        }
    }
}

/// Saved text overlay preferences (`Editor::TextPrefs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TextPrefs {
    /// Text background style.
    pub style: TextStyle,
    /// Text typeface.
    pub typeface: TextTypeface,
    /// Text alignment.
    pub alignment: TextAlignment,
    /// Saved font size slider ratio.
    pub size_ratio: f64,
}

/// Vector shape tool types (`Editor::ShapeType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ShapeType {
    /// Circle/ellipse.
    #[default]
    Circle = 0,
    /// Rectangle.
    Rectangle = 1,
    /// Five-pointed star.
    Star = 2,
    /// Speech bubble.
    Bubble = 3,
    /// Directional arrow.
    Arrow = 4,
}

/// Non-destructive photo edits applied on export (`Editor::PhotoModifications`).
///
/// Serializable to JSON for drafts via `serde`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhotoModifications {
    /// Clockwise rotation in 90-degree steps (`0`, `90`, `180`, `270`).
    #[serde(default)]
    pub angle: i32,
    /// Horizontal mirror applied *after* rotation (matching Qt `scale(-1, 1)` + `rotate(angle)`).
    #[serde(default)]
    pub flipped: bool,
    /// Optional crop rectangle in original image coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Rect>,
    /// Crop overlay/mask shape.
    #[serde(default)]
    pub crop_type: CropType,
    /// Crop overlay vs baked mask mode.
    #[serde(default)]
    pub crop_mode: CropMode,
    /// Rounded corners preset when `crop_type == CropType::RoundedRect`.
    #[serde(default)]
    pub corners_level: RoundedCornersLevel,
    /// Optional paint scene (brush strokes, erasers, stickers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<PaintScene>,
    /// Optional placement rect when editing media on a story canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_media_rect: Option<Rect>,
    /// Auto-enhance strength (`0..=100`).
    #[serde(default)]
    pub enhance: i32,
}

impl Default for PhotoModifications {
    fn default() -> Self {
        Self {
            angle: 0,
            flipped: false,
            crop: None,
            crop_type: CropType::Rect,
            crop_mode: CropMode::Hint,
            corners_level: RoundedCornersLevel::Large,
            paint: None,
            canvas_media_rect: None,
            enhance: 0,
        }
    }
}

impl PhotoModifications {
    /// Returns true if no modifications have been applied (`PhotoModifications::empty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let has_paint = self
            .paint
            .as_ref()
            .is_some_and(PaintScene::has_active_items);
        let has_crop = self.crop.is_some_and(Rect::is_valid);
        let has_canvas_media = self.canvas_media_rect.is_some_and(Rect::is_valid);
        normalize_angle_90(self.angle) == 0
            && !self.flipped
            && !has_crop
            && !has_paint
            && !has_canvas_media
            && self.enhance <= 0
    }

    /// Rotates clockwise by 90 degrees (`0 -> 90 -> 180 -> 270 -> 0`).
    pub fn rotate_90_cw(&mut self) {
        self.angle = normalize_angle_90(self.angle + 90);
    }

    /// Toggles horizontal flip.
    pub fn toggle_flip(&mut self) {
        self.flipped = !self.flipped;
    }

    /// Serializes these modifications to JSON for draft persistence.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserializes modifications from a draft JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}
