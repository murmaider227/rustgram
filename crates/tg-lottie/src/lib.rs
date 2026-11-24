//! Pure-Rust Lottie / `.tgs` animated sticker parser, `tiny-skia` software rasterizer,
//! compressed YUV420A4+LZ4 frame cache, and player.
//!
//! Ports Telegram Desktop's `Telegram/lib_lottie` (`lottie_animation.cpp`,
//! `lottie_single_player.cpp`, `lottie_multi_player.cpp`, `lottie_frame_renderer.cpp`,
//! `lottie_frame_generator.cpp`, `lottie_cache.cpp`, `lottie_cache_frame_storage.cpp`,
//! `lottie_frame_provider_*.cpp`, `lottie_instance.cpp`) together with an embedded
//! pure-Rust Lottie renderer covering the Telegram `.tgs` subset.

pub mod cache;
pub mod color;
pub mod common;
pub mod error;
pub mod model;
pub mod path;
pub mod player;
pub mod provider;
pub mod render;

pub use cache::{CACHE_SIZE_ROUNDING, Cache, CacheEncoder, CacheReadContext, MAX_CACHE_SIZE};
pub use color::{apply_color_replacements, build_replacement_map, fitzpatrick_replacements};
pub use common::{
    Color, ColorReplacements, DISPLAYED_INITIAL, FRAME_DISPLAY_TIME_ALREADY_DONE, FrameBuffer,
    FrameRenderResult, FrameRequest, IDEAL_SIZE, Information, MAX_FILE_SIZE, MAX_FRAME_RATE,
    MAX_FRAMES_COUNT, MAX_GZIP_FILE_SIZE, MAX_SIZE, NORMAL_FRAME_RATE, Quality, Size, SkinModifier,
    TGS_MAX_COMPRESSED_SIZE, TIME_UNKNOWN, content_error, is_gzip, read_utf8, unpack_gzip,
    validate_information,
};
pub use error::{Error, ErrorKind};
pub use model::{
    AnimatedColor, AnimatedGradient, AnimatedPath, AnimatedPosition, AnimatedScalar, AnimatedVec2,
    BlendMode, Composition, DashItem, Direction, EasingHandles, EllipseShape, FillRule, FillShape,
    GradientData, GradientFillShape, GradientStrokeShape, GradientType, Keyframe, Layer, LayerKind,
    LineCap, LineJoin, LottiePathData, Marker, Mask, MaskMode, MatteType, PathShape, PolystarShape,
    PolystarType, PrecompAsset, RectShape, RepeaterComposite, RepeaterShape, ShapeGroup, ShapeItem,
    StrokeShape, TransformProp, TrimMode, TrimPathsShape, cubic_bezier_ease,
};
pub use player::{
    AnimationId, AspectRatioMode, Frame, FrameGenerator, FrameInfo, GeneratedFrame, MultiPlayer,
    RING_FRAMES_COUNT, SharedState, SinglePlayer, good_for_request, prepare_by_request,
    prepare_frame_by_request, read_thumbnail,
};
pub use provider::{
    DIRECT_SIZE_ROUNDING, FrameProvider, FrameProviderCached, FrameProviderCachedMulti,
    FrameProviderDirect, FrameProviderShared, FrameProviderToken, Instance, MultiCachePutFn,
};
pub use render::{render_composition, render_composition_into};
