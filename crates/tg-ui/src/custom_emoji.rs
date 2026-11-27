//! Animated inline custom-emoji decoder, frame cache, and egui texture renderer.
//!
//! Supports:
//! - Lottie JSON and gzip-compressed `.tgs` custom emoji via `tg-lottie`
//! - WebM/VP9 video custom emoji via `tg-video` (`ClipReader` + `extract_thumbnail_from_bytes`)
//! - Static RGBA bitmaps (e.g. WebP/PNG custom emoji decoded upstream)
//!
//! Port of `Telegram/SourceFiles/ui/text/text_lottie_custom_emoji.cpp`,
//! `Telegram/lib_ui/ui/text/custom_emoji_instance.cpp`, and
//! `Telegram/lib_ui/ui/text/text_custom_emoji.cpp`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use egui::{Color32, ColorImage, Context, Painter, Rect, TextureHandle, TextureOptions, Vec2};
use tg_lottie::{Composition, Size as LottieSize, render_composition, unpack_gzip};
use tg_video::{ClipMode, ClipReader, FrameData, Size as VideoSize, ThumbnailOptions};

/// Default square rasterization size in physical pixels for inline custom emoji.
pub const DEFAULT_CUSTOM_EMOJI_PX: u32 = 40;

/// Maximum pre-decoded WebM frames cached per custom emoji to bound memory usage.
const MAX_CACHED_WEBM_FRAMES: usize = 90;

/// Maximum total cached frame entries per [`CustomEmojiRenderer`].
const MAX_RENDERER_FRAME_CACHE: usize = 1024;

/// Entries a full cache keeps: the least recently used quarter goes in one pass, so the next
/// 256 misses insert without another scan (TGB-3078).
const RENDERER_CACHE_KEEP: usize = MAX_RENDERER_FRAME_CACHE * 3 / 4;

/// Source payload registered for a Telegram `CustomEmoji` document ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomEmojiAsset {
    /// Uncompressed Lottie JSON source (`application/x-tgsticker` unpacked).
    LottieJson {
        /// Lottie JSON text.
        json: String,
        /// Whether monochrome/template tinting with surrounding text color is enabled
        /// (`DocumentAttributeCustomEmoji::text_color`).
        text_color_tinted: bool,
    },
    /// Gzip-compressed Telegram `.tgs` sticker bytes (`application/x-tgsticker`).
    TgsBytes {
        /// Raw `.tgs` gzip bytes.
        bytes: Vec<u8>,
        /// Whether adaptive text-color tinting is enabled.
        text_color_tinted: bool,
    },
    /// WebM/VP9 animated video custom emoji bytes (`video/webm`).
    WebmBytes {
        /// Raw WebM file bytes.
        bytes: Vec<u8>,
        /// Whether adaptive text-color tinting is enabled.
        text_color_tinted: bool,
    },
    /// Pre-decoded static RGBA8888 bitmap (e.g. WebP/PNG custom emoji).
    StaticRgba {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Straight RGBA8888 bytes (`width * height * 4`).
        rgba: Vec<u8>,
        /// Whether adaptive text-color tinting is enabled.
        text_color_tinted: bool,
    },
}

impl CustomEmojiAsset {
    /// Convenience constructor for a Lottie JSON custom emoji.
    #[must_use]
    pub fn lottie(json: impl Into<String>) -> Self {
        Self::LottieJson {
            json: json.into(),
            text_color_tinted: false,
        }
    }

    /// Convenience constructor for a gzip-compressed `.tgs` custom emoji.
    #[must_use]
    pub fn tgs(bytes: Vec<u8>) -> Self {
        Self::TgsBytes {
            bytes,
            text_color_tinted: false,
        }
    }

    /// Convenience constructor for a WebM video custom emoji.
    #[must_use]
    pub fn webm(bytes: Vec<u8>) -> Self {
        Self::WebmBytes {
            bytes,
            text_color_tinted: false,
        }
    }

    /// Convenience constructor for a static RGBA8888 custom emoji.
    #[must_use]
    pub fn static_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self::StaticRgba {
            width,
            height,
            rgba,
            text_color_tinted: false,
        }
    }

    /// Returns whether this custom emoji should be tinted with surrounding text color.
    #[must_use]
    pub const fn is_text_color_tinted(&self) -> bool {
        match self {
            Self::LottieJson {
                text_color_tinted, ..
            }
            | Self::TgsBytes {
                text_color_tinted, ..
            }
            | Self::WebmBytes {
                text_color_tinted, ..
            }
            | Self::StaticRgba {
                text_color_tinted, ..
            } => *text_color_tinted,
        }
    }
}

/// One decoded RGBA8888 frame produced by [`CustomEmojiRenderer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedEmojiFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Straight RGBA8888 bytes (`width * height * 4`).
    pub rgba: Vec<u8>,
    /// Display duration of this frame in milliseconds (used for repaint scheduling).
    pub duration_ms: u32,
    /// Whether this frame should be multiplied by the surrounding text color.
    pub text_color_tinted: bool,
    /// `true` if the underlying asset has more than one animated frame.
    pub animated: bool,
    /// Current 0-based frame index within the animation cycle.
    pub frame_index: usize,
    /// Total frame count in one animation cycle.
    pub frame_count: usize,
}

impl DecodedEmojiFrame {
    /// Converts this RGBA frame into an [`egui::ColorImage`].
    #[must_use]
    pub fn to_color_image(&self) -> ColorImage {
        rgba_image(self.width, self.height, &self.rgba)
    }
}

/// `rgba` (unpremultiplied, `width * height * 4` bytes) as an image; a transparent pixel when the
/// sizes do not match.
fn rgba_image(width: u32, height: u32, rgba: &[u8]) -> ColorImage {
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if width == 0 || height == 0 || rgba.len() != expected {
        return ColorImage::filled([1, 1], Color32::TRANSPARENT);
    }
    ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba)
}

/// Paint-time playback context matching `Ui::Text::CustomEmoji::Context`
/// (`ui/text/text_lottie_custom_emoji.cpp:51-69`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CustomEmojiPaintContext {
    /// Current animation clock in seconds (e.g. `ctx.input(|i| i.time)`).
    pub time_secs: f64,
    /// Pause animation (power saving / offscreen / unfocused).
    pub paused: bool,
    /// Force rendering frame 0 regardless of `time_secs`.
    pub force_first_frame: bool,
    /// Force rendering the final frame when paused.
    pub force_last_frame: bool,
    /// Surrounding text foreground color (used when `text_color_tinted == true`).
    pub text_color: Color32,
    /// `Images::Colored` over the frame: `msgStickerOverlay` for a selected message's large or
    /// custom-emoji-only emoji (`CustomEmoji::paintCustom` with `context.selected()`).
    pub overlay: Option<Color32>,
}

impl CustomEmojiPaintContext {
    /// Creates a live playing paint context at `time_secs` with `text_color`.
    #[must_use]
    pub const fn playing(time_secs: f64, text_color: Color32) -> Self {
        Self {
            time_secs,
            paused: false,
            force_first_frame: false,
            force_last_frame: false,
            text_color,
            overlay: None,
        }
    }
}

enum LoadedEmoji {
    Lottie {
        composition: Composition,
        frame_count: usize,
        fps: f64,
        text_color_tinted: bool,
    },
    Webm {
        frames: Vec<WebmFrame>,
        total_duration_ms: u64,
        text_color_tinted: bool,
    },
    Static {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        text_color_tinted: bool,
    },
}

/// One decoded frame of a WebM custom emoji.
struct WebmFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    duration_ms: u32,
}

/// Which frame of an emoji a paint shows, known without touching its pixels.
#[derive(Clone, Copy)]
struct FrameSlot {
    index: usize,
    count: usize,
    duration_ms: u32,
    tinted: bool,
}

impl LoadedEmoji {
    /// The frame `paint_ctx` shows; `None` for a video without frames.
    fn slot(&self, paint_ctx: CustomEmojiPaintContext) -> Option<FrameSlot> {
        let paused = paint_ctx.paused || paint_ctx.force_first_frame;
        let held = |count: usize| {
            if paint_ctx.force_last_frame && !paint_ctx.force_first_frame {
                count.saturating_sub(1)
            } else {
                0
            }
        };
        match self {
            Self::Lottie {
                frame_count,
                fps,
                text_color_tinted,
                ..
            } => {
                let count = (*frame_count).max(1);
                let index = if paused {
                    held(count)
                } else {
                    ((paint_ctx.time_secs.max(0.0) * fps).floor() as usize) % count
                };
                Some(FrameSlot {
                    index,
                    count,
                    duration_ms: ((1000.0 / fps).round() as u32).clamp(16, 1000),
                    tinted: *text_color_tinted,
                })
            }
            Self::Webm {
                frames,
                total_duration_ms,
                text_color_tinted,
            } => {
                let count = frames.len();
                let index = if paused {
                    held(count)
                } else if count == 1 {
                    0
                } else {
                    let t_ms = ((paint_ctx.time_secs.max(0.0) * 1000.0) as u64)
                        % (*total_duration_ms).max(1);
                    frames
                        .iter()
                        .scan(0u64, |end, frame| {
                            *end += u64::from(frame.duration_ms);
                            Some(*end)
                        })
                        .position(|end| t_ms < end)
                        .unwrap_or(0)
                };
                Some(FrameSlot {
                    index,
                    count,
                    duration_ms: frames.get(index)?.duration_ms,
                    tinted: *text_color_tinted,
                })
            }
            Self::Static {
                text_color_tinted, ..
            } => Some(FrameSlot {
                index: 0,
                count: 1,
                duration_ms: 1000,
                tinted: *text_color_tinted,
            }),
        }
    }

    /// Pixels of `slot`: Lottie frames are rasterized at `px`, video and still frames copied.
    fn decode(&self, slot: FrameSlot, px: u32) -> Option<DecodedEmojiFrame> {
        let (width, height, rgba) = match self {
            Self::Lottie { composition, .. } => {
                let comp_frame = composition.in_point + slot.index as f64;
                let fb = render_composition(composition, comp_frame, LottieSize::new(px, px));
                (fb.width, fb.height, fb.data)
            }
            Self::Webm { frames, .. } => {
                let frame = frames.get(slot.index)?;
                (frame.width, frame.height, frame.rgba.clone())
            }
            Self::Static {
                width,
                height,
                rgba,
                ..
            } => (*width, *height, rgba.clone()),
        };
        Some(DecodedEmojiFrame {
            width,
            height,
            rgba,
            duration_ms: slot.duration_ms,
            text_color_tinted: slot.tinted,
            animated: slot.count > 1,
            frame_index: slot.index,
            frame_count: slot.count,
        })
    }

    /// The texture image of `slot` at `px`, built straight from the stored pixels.
    fn color_image(&self, slot: FrameSlot, px: u32) -> ColorImage {
        match self {
            Self::Lottie { .. } => self
                .decode(slot, px)
                .map_or_else(|| rgba_image(0, 0, &[]), |frame| frame.to_color_image()),
            Self::Webm { frames, .. } => frames.get(slot.index).map_or_else(
                || rgba_image(0, 0, &[]),
                |frame| rgba_image(frame.width, frame.height, &frame.rgba),
            ),
            Self::Static {
                width,
                height,
                rgba,
                ..
            } => rgba_image(*width, *height, rgba),
        }
    }

    fn from_asset(asset: &CustomEmojiAsset, target_px: u32) -> Option<Self> {
        let px = target_px.clamp(8, 256);
        match asset {
            CustomEmojiAsset::LottieJson {
                json,
                text_color_tinted,
            } => Self::load_lottie(json, *text_color_tinted),
            CustomEmojiAsset::TgsBytes {
                bytes,
                text_color_tinted,
            } => {
                let raw = unpack_gzip(bytes).ok()?;
                let json = String::from_utf8(raw).ok()?;
                Self::load_lottie(&json, *text_color_tinted)
            }
            CustomEmojiAsset::WebmBytes {
                bytes,
                text_color_tinted,
            } => Self::load_webm(bytes, px, *text_color_tinted),
            CustomEmojiAsset::StaticRgba {
                width,
                height,
                rgba,
                text_color_tinted,
            } => {
                let expected = (*width as usize)
                    .checked_mul(*height as usize)?
                    .checked_mul(4)?;
                if *width == 0 || *height == 0 || rgba.len() != expected {
                    return None;
                }
                Some(Self::Static {
                    width: *width,
                    height: *height,
                    rgba: rgba.clone(),
                    text_color_tinted: *text_color_tinted,
                })
            }
        }
    }

    fn load_lottie(json: &str, text_color_tinted: bool) -> Option<Self> {
        let composition = Composition::from_json(json).ok()?;
        let fps = composition.frame_rate.clamp(1.0, 120.0);
        let span = (composition.out_point - composition.in_point).max(1.0);
        let frame_count = (span.round() as usize).max(1);
        Some(Self::Lottie {
            composition,
            frame_count,
            fps,
            text_color_tinted,
        })
    }

    fn load_webm(bytes: &[u8], target_px: u32, text_color_tinted: bool) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        // Attempt full frame-by-frame decode through `tg-video::ClipReader`.
        if let Ok(mut reader) = ClipReader::from_bytes(bytes.to_vec(), ClipMode::Normal) {
            reader.set_resize(Some(VideoSize::new(target_px, target_px)));
            let mut frames: Vec<WebmFrame> = Vec::new();
            let mut prev_pts_ms: Option<i64> = None;

            for _ in 0..MAX_CACHED_WEBM_FRAMES {
                let prev_loops = reader.loop_count();
                let Ok(frame) = reader.next_frame() else {
                    break;
                };
                if reader.loop_count() > prev_loops && !frames.is_empty() {
                    break;
                }
                let pts = frame.pts_ms;
                let duration_ms = match prev_pts_ms {
                    Some(prev) if pts > prev => (pts - prev).clamp(16, 1000) as u32,
                    _ => 33,
                };
                prev_pts_ms = Some(pts);

                if let FrameData::Rgba(rgba) = frame.data {
                    let w = frame.width;
                    let h = frame.height;
                    if w > 0 && h > 0 && rgba.len() == (w as usize) * (h as usize) * 4 {
                        frames.push(WebmFrame {
                            width: w,
                            height: h,
                            rgba,
                            duration_ms,
                        });
                    }
                }
            }

            if !frames.is_empty() {
                let total_duration_ms: u64 = frames.iter().map(|f| u64::from(f.duration_ms)).sum();
                return Some(Self::Webm {
                    frames,
                    total_duration_ms: total_duration_ms.max(33),
                    text_color_tinted,
                });
            }
        }

        // Fallback: single thumbnail frame via `tg-video::extract_thumbnail_from_bytes`.
        let opts = ThumbnailOptions {
            max_size: Some(tg_video::Size {
                width: target_px,
                height: target_px,
            }),
            ..ThumbnailOptions::default()
        };
        if let Ok(thumb) = tg_video::extract_thumbnail_from_bytes(bytes.to_vec(), opts)
            && let FrameData::Rgba(rgba) = thumb.data
            && thumb.width > 0
            && thumb.height > 0
            && rgba.len() == (thumb.width as usize) * (thumb.height as usize) * 4
        {
            return Some(Self::Static {
                width: thumb.width,
                height: thumb.height,
                rgba,
                text_color_tinted,
            });
        }

        None
    }
}

/// Stateful animated custom-emoji decoder, rasterizer, and texture cache.
///
/// Can be held directly by a widget or accessed transparently via the global
/// [`register_custom_emoji_asset`] registry used by `tg_ui::text::render_text_with_entities`.
#[derive(Default)]
pub struct CustomEmojiRenderer {
    emoji: HashMap<String, EmojiEntry>,
    /// Bumped on every cache use; a full cache drops its oldest ticks first.
    clock: u64,
}

/// A cached frame or texture with the renderer clock tick of its last use.
struct Recent<T> {
    value: T,
    last_used: u64,
}

/// `(frame_index, size_px, overlay)` of a cached frame or texture.
type FrameKey = (usize, u32, Option<Color32>);

/// A registered emoji with the frames and textures made from it.
struct EmojiEntry {
    loaded: LoadedEmoji,
    /// Rasterized Lottie frames [`CustomEmojiRenderer::frame_for`] returned.
    frames: HashMap<FrameKey, Recent<DecodedEmojiFrame>>,
    /// Uploaded egui GPU textures.
    textures: HashMap<FrameKey, Recent<TextureHandle>>,
}

/// Drops the least recently used entries of one cache kind (frames or textures) across all
/// emoji until `keep` remain. Ticks are unique, so the cut is exact.
fn evict_least_recent<T>(
    emoji: &mut HashMap<String, EmojiEntry>,
    cache: fn(&mut EmojiEntry) -> &mut HashMap<FrameKey, Recent<T>>,
    keep: usize,
) {
    let mut ticks: Vec<u64> = emoji
        .values_mut()
        .flat_map(|entry| cache(entry).values().map(|recent| recent.last_used))
        .collect();
    if ticks.len() <= keep {
        return;
    }
    let drop_count = ticks.len() - keep;
    let (_, &mut cutoff, _) = ticks.select_nth_unstable(drop_count - 1);
    for entry in emoji.values_mut() {
        cache(entry).retain(|_, recent| recent.last_used > cutoff);
    }
}

impl std::fmt::Debug for CustomEmojiRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomEmojiRenderer")
            .field("loaded_count", &self.emoji.len())
            .field("frame_cache_count", &self.cached_frame_count())
            .field("textures_count", &self.cached_texture_count())
            .finish()
    }
}

impl CustomEmojiRenderer {
    /// Creates an empty [`CustomEmojiRenderer`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers and decodes `asset` under `document_id`.
    ///
    /// Returns `true` if the asset parsed cleanly and is ready to paint.
    pub fn register_asset(
        &mut self,
        document_id: impl Into<String>,
        asset: &CustomEmojiAsset,
    ) -> bool {
        let Some(loaded) = LoadedEmoji::from_asset(asset, DEFAULT_CUSTOM_EMOJI_PX) else {
            return false;
        };
        self.emoji.insert(
            document_id.into(),
            EmojiEntry {
                loaded,
                frames: HashMap::new(),
                textures: HashMap::new(),
            },
        );
        true
    }

    /// Returns `true` if `document_id` has a ready decoded custom emoji asset.
    #[must_use]
    pub fn contains(&self, document_id: &str) -> bool {
        self.emoji.contains_key(document_id)
    }

    /// Length of one animation loop of `document_id` in seconds; `None` for a still or an
    /// unregistered emoji.
    #[must_use]
    pub fn loop_duration_secs(&self, document_id: &str) -> Option<f64> {
        match &self.emoji.get(document_id)?.loaded {
            LoadedEmoji::Lottie {
                frame_count, fps, ..
            } => (*frame_count > 1).then(|| *frame_count as f64 / fps),
            LoadedEmoji::Webm {
                frames,
                total_duration_ms,
                ..
            } => (frames.len() > 1).then(|| *total_duration_ms as f64 / 1000.0),
            LoadedEmoji::Static { .. } => None,
        }
    }

    /// Removes `document_id` and its cached frames/textures.
    pub fn remove(&mut self, document_id: &str) {
        self.emoji.remove(document_id);
    }

    /// Clears all cached frames and GPU textures while keeping registered emoji definitions.
    pub fn clear_caches(&mut self) {
        for entry in self.emoji.values_mut() {
            entry.frames.clear();
            entry.textures.clear();
        }
    }

    fn cached_frame_count(&self) -> usize {
        self.emoji.values().map(|entry| entry.frames.len()).sum()
    }

    fn cached_texture_count(&self) -> usize {
        self.emoji.values().map(|entry| entry.textures.len()).sum()
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Computes and returns the decoded RGBA frame for `document_id` at `size_px` for `paint_ctx`.
    pub fn frame_for(
        &mut self,
        document_id: &str,
        size_px: u32,
        paint_ctx: CustomEmojiPaintContext,
    ) -> Option<DecodedEmojiFrame> {
        let px = size_px.clamp(8, 256);
        let now = self.tick();
        let entry = self.emoji.get_mut(document_id)?;
        let slot = entry.loaded.slot(paint_ctx)?;
        if !matches!(entry.loaded, LoadedEmoji::Lottie { .. }) {
            return entry.loaded.decode(slot, px);
        }
        // The decoded frames are kept uncoloured; only textures carry the overlay.
        let key = (slot.index, px, None);
        if let Some(cached) = entry.frames.get_mut(&key) {
            cached.last_used = now;
            return Some(cached.value.clone());
        }
        let decoded = entry.loaded.decode(slot, px)?;
        if self.cached_frame_count() >= MAX_RENDERER_FRAME_CACHE {
            evict_least_recent(
                &mut self.emoji,
                |entry| &mut entry.frames,
                RENDERER_CACHE_KEEP,
            );
        }
        if let Some(entry) = self.emoji.get_mut(document_id) {
            entry.frames.insert(
                key,
                Recent {
                    value: decoded.clone(),
                    last_used: now,
                },
            );
        }
        Some(decoded)
    }

    /// Paints custom emoji `document_id` into `rect` on `painter`, uploading/caching GPU textures
    /// and scheduling the next repaint if animated and not paused.
    ///
    /// A frame whose texture is cached is painted without touching its pixels.
    ///
    /// Returns `true` if the custom emoji was painted, or `false` if not registered/ready.
    pub fn paint(
        &mut self,
        ctx: &Context,
        painter: &Painter,
        document_id: &str,
        rect: Rect,
        paint_ctx: CustomEmojiPaintContext,
    ) -> bool {
        let side = rect.width().max(rect.height());
        let px = ((side * ctx.pixels_per_point()).round() as u32).clamp(16, 128);
        let now = self.tick();
        let Some(entry) = self.emoji.get_mut(document_id) else {
            return false;
        };
        let Some(slot) = entry.loaded.slot(paint_ctx) else {
            return false;
        };
        let key = (slot.index, px, paint_ctx.overlay);
        let tex_id = if let Some(texture) = entry.textures.get_mut(&key) {
            texture.last_used = now;
            texture.value.id()
        } else {
            let mut image = entry.loaded.color_image(slot, px);
            if let Some(add) = paint_ctx.overlay {
                crate::colored::colored(&mut image, add);
            }
            if self.cached_texture_count() >= MAX_RENDERER_FRAME_CACHE {
                evict_least_recent(
                    &mut self.emoji,
                    |entry| &mut entry.textures,
                    RENDERER_CACHE_KEEP,
                );
            }
            let name = format!(
                "tg_custom_emoji_{document_id}_{}_{px}{}",
                slot.index,
                if paint_ctx.overlay.is_some() {
                    "_selected"
                } else {
                    ""
                }
            );
            let texture = ctx.load_texture(name, image, TextureOptions::LINEAR);
            let id = texture.id();
            if let Some(entry) = self.emoji.get_mut(document_id) {
                entry.textures.insert(
                    key,
                    Recent {
                        value: texture,
                        last_used: now,
                    },
                );
            }
            id
        };

        let tint = if slot.tinted {
            paint_ctx.text_color
        } else {
            Color32::WHITE
        };
        let square = Rect::from_center_size(
            rect.center(),
            Vec2::splat(rect.width().min(rect.height()).max(12.0)),
        );
        painter.image(
            tex_id,
            square,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            tint,
        );

        if slot.count > 1 && !paint_ctx.paused && !paint_ctx.force_first_frame {
            ctx.request_repaint_after(std::time::Duration::from_millis(u64::from(
                slot.duration_ms.max(16),
            )));
        }
        true
    }
}

/// Process-wide shared custom emoji renderer registry so `tg-app` can register downloaded
/// custom emoji documents (`.tgs`, WebM, WebP) and `tg-ui`'s `render_text_with_entities`
/// renders them animated inline automatically.
fn shared_registry() -> &'static Arc<Mutex<CustomEmojiRenderer>> {
    static REGISTRY: OnceLock<Arc<Mutex<CustomEmojiRenderer>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(Mutex::new(CustomEmojiRenderer::new())))
}

/// Registers a decoded or raw [`CustomEmojiAsset`] in the shared inline custom emoji registry.
pub fn register_custom_emoji_asset(
    document_id: impl Into<String>,
    asset: &CustomEmojiAsset,
) -> bool {
    let mut guard = shared_registry().lock().unwrap_or_else(|e| e.into_inner());
    guard.register_asset(document_id, asset)
}

/// Removes a custom emoji asset from the shared registry.
pub fn unregister_custom_emoji_asset(document_id: &str) {
    let mut guard = shared_registry().lock().unwrap_or_else(|e| e.into_inner());
    guard.remove(document_id);
}

/// Returns `true` if `document_id` is registered in the shared inline custom emoji registry.
#[must_use]
pub fn has_registered_custom_emoji(document_id: &str) -> bool {
    let guard = shared_registry().lock().unwrap_or_else(|e| e.into_inner());
    guard.contains(document_id)
}

/// [`CustomEmojiRenderer::loop_duration_secs`] for an emoji of the shared registry.
#[must_use]
pub fn registered_custom_emoji_loop_secs(document_id: &str) -> Option<f64> {
    let guard = shared_registry().lock().unwrap_or_else(|e| e.into_inner());
    guard.loop_duration_secs(document_id)
}

/// Paints `document_id` from the shared registry into `rect`; returns `true` if painted.
pub fn paint_registered_custom_emoji(
    ctx: &Context,
    painter: &Painter,
    document_id: &str,
    rect: Rect,
    paint_ctx: CustomEmojiPaintContext,
) -> bool {
    let mut guard = shared_registry().lock().unwrap_or_else(|e| e.into_inner());
    guard.paint(ctx, painter, document_id, rect, paint_ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid 10-frame animated Lottie JSON (20fps, solid red circle shifting across x).
    fn sample_animated_lottie_json() -> String {
        r#"{
            "v": "5.5.2",
            "fr": 20,
            "ip": 0,
            "op": 10,
            "w": 64,
            "h": 64,
            "layers": [
                {
                    "ty": 4,
                    "nm": "dot",
                    "ip": 0,
                    "op": 10,
                    "st": 0,
                    "ks": {
                        "o": {"a": 0, "k": 100},
                        "r": {"a": 0, "k": 0},
                        "p": {
                            "a": 1,
                            "k": [
                                {"t": 0, "s": [16, 32, 0], "e": [48, 32, 0]},
                                {"t": 10, "s": [48, 32, 0]}
                            ]
                        },
                        "a": {"a": 0, "k": [0, 0, 0]},
                        "s": {"a": 0, "k": [100, 100, 100]}
                    },
                    "shapes": [
                        {
                            "ty": "gr",
                            "it": [
                                {"ty": "el", "p": {"a": 0, "k": [0, 0]}, "s": {"a": 0, "k": [20, 20]}},
                                {"ty": "fl", "c": {"a": 0, "k": [0.9, 0.2, 0.3, 1]}, "o": {"a": 0, "k": 100}},
                                {"ty": "tr", "p": {"a": 0, "k": [0, 0]}, "a": {"a": 0, "k": [0, 0]}, "s": {"a": 0, "k": [100, 100]}, "r": {"a": 0, "k": 0}, "o": {"a": 0, "k": 100}}
                            ]
                        }
                    ]
                }
            ]
        }"#
        .to_owned()
    }

    #[test]
    fn lottie_custom_emoji_renders_distinct_animated_frames_and_respects_pause() {
        let mut renderer = CustomEmojiRenderer::new();
        let asset = CustomEmojiAsset::lottie(sample_animated_lottie_json());
        assert!(renderer.register_asset("500001", &asset));
        assert!(renderer.contains("500001"));

        let f0 = renderer
            .frame_for(
                "500001",
                32,
                CustomEmojiPaintContext::playing(0.0, Color32::WHITE),
            )
            .expect("frame 0");
        assert_eq!(f0.width, 32);
        assert_eq!(f0.height, 32);
        assert_eq!(f0.rgba.len(), 32 * 32 * 4);
        assert!(f0.animated);
        assert_eq!(f0.frame_index, 0);
        assert_eq!(f0.frame_count, 10);
        assert!(
            f0.rgba.iter().any(|&b| b > 0),
            "rendered frame must have non-empty pixels"
        );

        // Advance 0.25s at 20 fps -> frame 5.
        let f5 = renderer
            .frame_for(
                "500001",
                32,
                CustomEmojiPaintContext::playing(0.25, Color32::WHITE),
            )
            .expect("frame 5");
        assert_eq!(f5.frame_index, 5);
        assert_ne!(
            f0.rgba, f5.rgba,
            "animated frames must differ as circle moves"
        );

        // Paused context stays on frame 0 (or last frame when force_last_frame = true).
        let paused_first = renderer
            .frame_for(
                "500001",
                32,
                CustomEmojiPaintContext {
                    time_secs: 0.35,
                    paused: true,
                    force_first_frame: false,
                    force_last_frame: false,
                    text_color: Color32::WHITE,
                    overlay: None,
                },
            )
            .expect("paused frame");
        assert_eq!(paused_first.frame_index, 0);

        let paused_last = renderer
            .frame_for(
                "500001",
                32,
                CustomEmojiPaintContext {
                    time_secs: 0.35,
                    paused: true,
                    force_first_frame: false,
                    force_last_frame: true,
                    text_color: Color32::WHITE,
                    overlay: None,
                },
            )
            .expect("paused last frame");
        assert_eq!(paused_last.frame_index, 9);
    }

    #[test]
    fn static_rgba_and_invalid_webm_handle_gracefully() {
        let mut renderer = CustomEmojiRenderer::new();
        let rgba = vec![255u8; 4 * 4 * 4];
        let static_asset = CustomEmojiAsset::StaticRgba {
            width: 4,
            height: 4,
            rgba,
            text_color_tinted: true,
        };
        assert!(renderer.register_asset("static_1", &static_asset));
        let frame = renderer
            .frame_for(
                "static_1",
                24,
                CustomEmojiPaintContext::playing(1.2, Color32::RED),
            )
            .expect("static frame");
        assert!(!frame.animated);
        assert!(frame.text_color_tinted);
        assert_eq!(frame.width, 4);

        // Malformed WebM bytes must fail cleanly without panicking.
        let bad_webm = CustomEmojiAsset::webm(b"not a webm video".to_vec());
        assert!(!renderer.register_asset("bad_webm", &bad_webm));
        assert!(!renderer.contains("bad_webm"));
    }
}
