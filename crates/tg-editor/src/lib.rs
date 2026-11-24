//! Pure-Rust photo editor model, rasterization, and JPEG export for Rustgram.
//!
//! Port of `Telegram/SourceFiles/editor/` (`photo_editor*.cpp`, `editor_crop.cpp`,
//! `editor_paint.cpp`, `scene/*`, `color_picker.cpp`, `editor_enhance.cpp`,
//! `photo_editor_layer_widget.cpp`) and photo JPEG export from
//! `storage/localimageloader.cpp` / `lib_ui/ui/image/image_prepare.cpp`.

pub mod apply;
pub mod blur;
pub mod brush;
pub mod canvas_bg;
pub mod color_picker;
pub mod common;
pub mod crop;
pub mod enhance;
pub mod export;
pub mod geometry;
pub mod layout;
pub mod scene;
pub mod scene_draft;
pub mod scene_items;
pub mod userpic;
pub mod video;

pub use apply::{apply, apply_shape_mask, apply_with_stickers};
pub use blur::{BLUR_BRUSH_RADIUS, stack_blur_rgba};
pub use brush::{
    ARROW_HEAD_ANGLE_DEGREES, ARROW_HEAD_LENGTH_FACTOR, ARROW_HEAD_MIN_DISTANCE_FACTOR,
    BLUR_SIZE_MULTIPLIER, BrushStroke, MARKER_OPACITY, MARKER_SIZE_MULTIPLIER, MAX_POINT_DISTANCE,
    MIN_POINT_DISTANCE_BASE, SMOOTHING_STRENGTH, StrokeBuilder, StrokePoint, build_arrow_head_path,
    build_stroke_path, compute_arrow_head_wings, rasterize_stroke, rasterize_stroke_mask,
    smooth_stroke, smooth_stroke_once,
};
pub use canvas_bg::{
    CanvasBackground, canvas_placement, dominant_canvas_background, paint_canvas_background,
    rounded_canvas_rect,
};
pub use color_picker::{
    BrushSizeSlider, BrushState, ColorSliderMode, ColorSliderState, DEFAULT_PALETTE, HslColor,
    HsvColor, MIN_BRUSH_SIZE_RATIO, hsl_to_rgba, hsv_f_to_rgb, hsv_to_rgba, percent_from_byte,
    percent_to_byte, rgb_to_hsl, rgb_to_hsv, rgb_to_hsv_f,
};
pub use common::{
    Brush, BrushTool, CropMode, CropType, DEFAULT_BRUSH_SIZE_RATIO, EditorData, EditorModeAction,
    EditorModeKind, FORUM_USERPIC_RADIUS_MULTIPLIER, MAX_BRUSH_WIDTH, MIN_BRUSH_WIDTH,
    PhotoEditorMode, PhotoModifications, RoundedCornersLevel, SaveState, ShapeType, TextAlignment,
    TextPrefs, TextStyle, TextTypeface,
};
pub use crop::{
    CROP_MIN_SIZE, CROP_POINT_SIZE, CropController, CropCursor, CropEdge, CropPreset,
    MAX_CANVAS_RATIO, adjust_crop_to_inner, original_crop,
};
pub use enhance::{EnhanceLut, EnhanceTile, apply_enhance, enhance_image, prepare_enhance};
pub use export::{
    ExportedJpeg, MAX_THUMB_ASPECT_RATIO, PHOTO_JPEG_QUALITY, PHOTO_SIDE_LIMIT_LARGE,
    PHOTO_SIDE_LIMIT_STANDARD, PhotoExportSettings, RECOMPRESS_AFTER_BPP, THUMBNAIL_JPEG_QUALITY,
    THUMBNAIL_MAX_SIDE, apply_and_export_jpeg, apply_and_export_jpeg_with_stickers, export_jpeg,
    opaque_on_white, photo_side_limit, scale_to_fit, validate_thumb_dimensions,
    within_recompress_threshold,
};
pub use geometry::{
    Affine2D, Color, Margins, PointF, Rect, RectF, Size, SizeF, flip_size_by_rotation,
    flip_size_f_by_rotation, normalize_angle_90,
};
pub use layout::{
    BUTTON_BAR_HEIGHT, BUTTON_BAR_WIDTH, CONTENT_MARGINS, CONTROLS_BOTTOM_SKIP,
    CONTROLS_CENTER_SKIP, CONTROLS_HEIGHT, EXPANSION_ROOM_RATIO, PhotoEditorInnerLayout,
    compute_inner_layout, layout_target,
};
pub use scene::{
    EraserAction, ITEM_HANDLE_SIZE, ITEM_MIN_SIZE_RATIO, ITEM_SNAP_ANGLE, ITEM_STICKY_TRIGGER,
    ITEMS_BASE_Z, ItemId, ItemPlacement, ItemStatus, LineItem, PaintScene, PlacementAction,
    RemoveAction, STICKY_LINES, SceneEntry, SceneItemKind, StickerItem, StickerRef, ZOrderAction,
    pixmap_to_rgba, rgba_to_pixmap,
};
pub use scene_draft::{
    CropEnhanceParams, DraftLink, DraftLocation, DraftMessage, DraftReaction, DraftShape,
    DraftSticker, DraftText, DraftVideo, DraftWeather, MAX_DRAFT_BINARY_BYTES,
    SCENE_DRAFT_BINARY_MAGIC, SCENE_DRAFT_VERSION, SceneDraft, SceneItemsDraft, VideoTimelineDraft,
    collect_scene_items, parse_scene_items_json, serialize_scene_items_json,
};
pub use scene_items::{
    ARROW_HEAD_ANGLE_RAD, ARROW_HEAD_RATIO, ARROW_HEAD_STROKE_FACTOR, ARROW_MAX_BEND_RATIO,
    AUDIO_TRACK_MIN_DURATION_MS, ArrowPoints, AudioTrackClip, AudioTrackTimelineModel,
    BUBBLE_BODY_RATIO, BUBBLE_TAIL_RATIO, CollectedChannelPostArea, CollectedLocationArea,
    CollectedReactionArea, CollectedUrlArea, CollectedWeatherArea, DrawToReplyRequest,
    DrawToReplySession, LinkPillStyle, LocationData, LocationItem, MIN_SHAPE_SIDE,
    SHAPE_ROUNDING_RATIO, STAR_BOX_HEIGHT, STAR_BOX_WIDTH, STAR_INNER_RATIO,
    STORY_PILL_RADIUS_RATIO, ShapeItem, StoryAreaGeometry, StoryAreas, StoryChannelPostItem,
    StoryLinkItem, StoryReactionItem, TextItem, TextLayoutSpec, WeatherData, WeatherItem,
    collect_story_areas, compute_text_content_size, compute_text_layout_spec,
    default_shape_aspect_ratio, effective_text_color, story_area_of, text_background_color,
    text_background_padding,
};
pub use userpic::{
    MAX_PROFILE_ASPECT_RATIO, PROFILE_PHOTO_SIZE, PreparedProfilePhoto, finalize_profile_photo,
    finalize_profile_photo_with_stickers, prepare_profile_photo_input,
    validate_profile_photo_dimensions,
};

use thiserror::Error;

/// Errors returned by `tg-editor` operations.
#[derive(Debug, Error)]
pub enum EditorError {
    /// Input image has zero width or zero height.
    #[error("input image is empty")]
    EmptyImage,
    /// Image dimensions violate Telegram aspect ratio limits.
    #[error("invalid photo dimensions: {width}x{height}")]
    InvalidPhotoDimensions {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// JPEG encoding failed.
    #[error("failed to encode JPEG: {0}")]
    JpegEncode(String),
    /// Video probing or frame extraction failed.
    #[error("video probe/frame error: {0}")]
    VideoProbe(String),
    /// Video transcoding/export failed.
    #[error("video export error: {0}")]
    VideoExport(String),
    /// Invalid multi-clip timeline operation or trim parameter.
    #[error("invalid video timeline: {0}")]
    InvalidTimeline(String),
    /// Overlay item visibility range had `start_ms < 0` or `start_ms >= end_ms`.
    #[error("invalid overlay visibility range: [{start_ms}, {end_ms})")]
    InvalidVisibilityRange {
        /// Inclusive start timestamp in milliseconds.
        start_ms: i64,
        /// Exclusive end timestamp in milliseconds.
        end_ms: i64,
    },
    /// Scene draft used an unsupported schema version.
    #[error("unsupported scene draft version {found} (max supported: {max_supported})")]
    UnsupportedDraftVersion {
        /// Version number found in the serialized draft.
        found: u32,
        /// Highest version supported by this build.
        max_supported: u32,
    },
    /// Scene draft serialization failed.
    #[error("failed to serialize scene draft: {0}")]
    DraftSerialization(String),
    /// Scene draft deserialization failed.
    #[error("failed to deserialize scene draft: {0}")]
    DraftDeserialization(String),
}
