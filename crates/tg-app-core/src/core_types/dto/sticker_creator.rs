//! Pure sticker-creator DTOs, image prep helpers, backend trait, and mock store.

use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, RgbaImage};

/// Maximum regular sticker dimension (`kStickerSide = 512`).
pub const STICKER_SIDE: u32 = 512;
/// Maximum custom emoji dimension (`kEmojiStickerSideMax = 100`).
pub const EMOJI_STICKER_SIDE: u32 = 100;
/// Maximum emojis assigned to a single created sticker (`kMaxEmojis = 7`).
pub const MAX_STICKER_EMOJIS: usize = 7;
/// Maximum stickers in one owned sticker set (`kStickersInOwnedSetMax = 120`).
pub const MAX_STICKERS_IN_OWNED_SET: usize = 120;
/// Maximum custom emoji in one owned emoji set (`kEmojiInOwnedSetMax = 200`).
pub const MAX_EMOJI_IN_OWNED_SET: usize = 200;
/// Server byte limit for WebM video stickers (`kMaxStickerWebmBytes = 256 KiB`).
pub const MAX_STICKER_WEBM_BYTES: usize = 256 * 1024;
/// Server byte limit for WebM custom emoji (`kMaxEmojiWebmBytes = 64 KiB`).
pub const MAX_EMOJI_WEBM_BYTES: usize = 64 * 1024;
/// Default software tag attached to `stickers.createStickerSet`.
pub const STICKER_CREATOR_SOFTWARE: &str = "rustgram";

/// Sticker set target kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StickerSetKind {
    /// Regular 512×512 chat sticker set.
    #[default]
    Regular,
    /// 100×100 custom emoji pack (`emojis = true`).
    CustomEmoji,
    /// Mask sticker set (`masks = true`).
    Masks,
}

impl StickerSetKind {
    /// Maximum output side in pixels (`SideForType` in `api_stickers_creator.cpp`).
    #[must_use]
    pub const fn max_side(self) -> u32 {
        match self {
            Self::CustomEmoji => EMOJI_STICKER_SIDE,
            Self::Regular | Self::Masks => STICKER_SIDE,
        }
    }

    /// Maximum stickers allowed in an owned set of this kind.
    #[must_use]
    pub const fn max_items(self) -> usize {
        match self {
            Self::CustomEmoji => MAX_EMOJI_IN_OWNED_SET,
            Self::Regular | Self::Masks => MAX_STICKERS_IN_OWNED_SET,
        }
    }
}

/// Summary of a sticker/emoji pack owned by the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedStickerSetSummary {
    /// Telegram sticker set ID.
    pub id: i64,
    /// Telegram sticker set access hash.
    pub access_hash: i64,
    /// Shareable short name (`t.me/addstickers/<short_name>`).
    pub short_name: String,
    /// Pack display title.
    pub title: String,
    /// Current item count.
    pub count: usize,
    /// Sticker set kind.
    pub kind: StickerSetKind,
}

/// Prepared sticker payload ready for upload + `stickers.createStickerSet` / `addStickerToSet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStickerAsset {
    /// Encoded image/WebP (or WebM video) bytes.
    pub bytes: Vec<u8>,
    /// MIME type (`"image/webp"`, `"image/png"`, or `"video/webm"`).
    pub mime_type: String,
    /// Suggested file name (`"sticker.webp"`, `"emoji.webp"`, `"sticker.webm"`, etc.).
    pub file_name: String,
    /// Encoded pixel width.
    pub width: u32,
    /// Encoded pixel height.
    pub height: u32,
    /// Optional video duration in milliseconds (`0` for static image stickers).
    pub video_duration_ms: u32,
}

/// Fits `image` into the Telegram sticker canvas (`512×512` for stickers, `100×100` for custom
/// emoji) preserving aspect ratio, rejecting extreme `> 10:1` aspect ratios (matching
/// `PrepareEditorState` in `sticker_creator_box.cpp:113-122`), and encodes lossless WebP bytes.
pub fn prepare_static_sticker_image(
    image: &RgbaImage,
    kind: StickerSetKind,
) -> Result<PreparedStickerAsset, String> {
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return Err("Sticker image cannot be empty.".to_owned());
    }
    if w > h.saturating_mul(10) || h > w.saturating_mul(10) {
        return Err("Image aspect ratio is too extreme for a sticker.".to_owned());
    }

    let max_side = kind.max_side();
    let resized: RgbaImage = if kind == StickerSetKind::CustomEmoji {
        // Custom emoji must be 100x100 square; center-fit on transparent 100x100 canvas.
        let fitted = DynamicImage::ImageRgba8(image.clone()).resize(
            max_side,
            max_side,
            FilterType::Lanczos3,
        );
        let fitted_rgba = fitted.to_rgba8();
        let mut canvas = RgbaImage::new(max_side, max_side);
        let ox = (max_side.saturating_sub(fitted_rgba.width())) / 2;
        let oy = (max_side.saturating_sub(fitted_rgba.height())) / 2;
        image::imageops::overlay(&mut canvas, &fitted_rgba, i64::from(ox), i64::from(oy));
        canvas
    } else {
        // Regular stickers require one side to be 512px and the other <= 512px.
        let scale = f64::from(max_side) / f64::from(w.max(h));
        let target_w = ((f64::from(w) * scale).round() as u32).clamp(1, max_side);
        let target_h = ((f64::from(h) * scale).round() as u32).clamp(1, max_side);
        if target_w == w && target_h == h {
            image.clone()
        } else {
            image::imageops::resize(image, target_w, target_h, FilterType::Lanczos3)
        }
    };

    let (out_w, out_h) = resized.dimensions();
    let mut out_bytes = Vec::new();
    let dyn_img = DynamicImage::ImageRgba8(resized);
    let mut cursor = std::io::Cursor::new(&mut out_bytes);
    let (mime_type, file_name) = if dyn_img.write_to(&mut cursor, ImageFormat::WebP).is_ok() {
        (
            "image/webp".to_owned(),
            if kind == StickerSetKind::CustomEmoji {
                "emoji.webp".to_owned()
            } else {
                "sticker.webp".to_owned()
            },
        )
    } else {
        out_bytes.clear();
        let mut png_cur = std::io::Cursor::new(&mut out_bytes);
        dyn_img
            .write_to(&mut png_cur, ImageFormat::Png)
            .map_err(|e| format!("Failed to encode sticker image: {e}"))?;
        (
            "image/png".to_owned(),
            if kind == StickerSetKind::CustomEmoji {
                "emoji.png".to_owned()
            } else {
                "sticker.png".to_owned()
            },
        )
    };

    Ok(PreparedStickerAsset {
        bytes: out_bytes,
        mime_type,
        file_name,
        width: out_w,
        height: out_h,
        video_duration_ms: 0,
    })
}

/// Loads an image file from disk and prepares it as a Telegram sticker asset.
pub fn prepare_sticker_from_file(
    path: &Path,
    kind: StickerSetKind,
) -> Result<PreparedStickerAsset, String> {
    let raw = std::fs::read(path)
        .map_err(|e| format!("Failed to read sticker file '{}': {e}", path.display()))?;
    let is_webm = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("webm"));
    if is_webm {
        let max_bytes = if kind == StickerSetKind::CustomEmoji {
            MAX_EMOJI_WEBM_BYTES
        } else {
            MAX_STICKER_WEBM_BYTES
        };
        if raw.len() > max_bytes {
            return Err(format!(
                "Video sticker exceeds {} KiB limit.",
                max_bytes / 1024
            ));
        }
        let side = kind.max_side();
        return Ok(PreparedStickerAsset {
            bytes: raw,
            mime_type: "video/webm".to_owned(),
            file_name: if kind == StickerSetKind::CustomEmoji {
                "emoji.webm".to_owned()
            } else {
                "sticker.webm".to_owned()
            },
            width: side,
            height: side,
            video_duration_ms: 2_500,
        });
    }

    let decoded = image::load_from_memory(&raw)
        .map_err(|e| format!("Unsupported sticker image format: {e}"))?;
    prepare_static_sticker_image(&decoded.to_rgba8(), kind)
}

/// Generates a procedural GPL-3.0 geometric badge RGBA image for demo/sample sticker creation.
#[must_use]
pub fn generate_sample_sticker_rgba(seed: u8, side: u32) -> RgbaImage {
    let side = side.clamp(32, STICKER_SIDE);
    let mut img = RgbaImage::new(side, side);
    let cx = side as f32 * 0.5;
    let cy = side as f32 * 0.5;
    let r_outer = side as f32 * 0.44;
    let r_inner = side as f32 * 0.28;
    let tint_r = 65u8.saturating_add(seed.wrapping_mul(37) % 150);
    let tint_g = 120u8.saturating_add(seed.wrapping_mul(53) % 110);
    let tint_b = 185u8.saturating_add(seed.wrapping_mul(19) % 60);

    for y in 0..side {
        for x in 0..side {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= r_outer {
                let px = if dist <= r_inner {
                    [255, 224, 102, 255]
                } else {
                    [tint_r, tint_g, tint_b, 255]
                };
                img.put_pixel(x, y, image::Rgba(px));
            }
        }
    }
    img
}

/// Request payload for `stickers.createStickerSet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateStickerSetRequest {
    /// Pack title (1..=64 chars).
    pub title: String,
    /// Unique shareable short name (`a-zA-Z0-9_`, starting with a letter).
    pub short_name: String,
    /// Pack kind (`Regular`, `CustomEmoji`, `Masks`).
    pub kind: StickerSetKind,
    /// Prepared first sticker asset.
    pub asset: PreparedStickerAsset,
    /// Associated emoji string (1..=7 emojis, defaults to `"🙂"`).
    pub emoji: String,
    /// Optional comma-separated search keywords.
    pub keywords: String,
}

/// Request payload for `stickers.addStickerToSet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddStickerToSetRequest {
    /// Target owned sticker set ID.
    pub set_id: i64,
    /// Target owned sticker set access hash.
    pub set_access_hash: i64,
    /// Target owned sticker set short name.
    pub set_short_name: String,
    /// Target pack kind.
    pub kind: StickerSetKind,
    /// Prepared sticker asset.
    pub asset: PreparedStickerAsset,
    /// Associated emoji string.
    pub emoji: String,
    /// Optional comma-separated search keywords.
    pub keywords: String,
}

/// Backend contract for `StickerCreatorBox` (`api_stickers_creator.cpp`).
pub trait StickerCreatorBackend {
    /// Lists sticker/emoji sets created by the current user (`AmCreator`).
    fn owned_sticker_sets(&mut self, kind: StickerSetKind) -> Vec<OwnedStickerSetSummary>;

    /// Suggests an available short name for `title` (`stickers.suggestShortName`).
    fn suggest_sticker_short_name(&mut self, title: &str) -> Result<String, String>;

    /// Checks whether `short_name` is available (`stickers.checkShortName`).
    fn check_sticker_short_name(&mut self, short_name: &str) -> Result<bool, String>;

    /// Uploads a sticker asset and creates a new sticker set (`stickers.createStickerSet`).
    fn create_sticker_set(
        &mut self,
        request: CreateStickerSetRequest,
    ) -> Result<OwnedStickerSetSummary, String>;

    /// Uploads a sticker asset and appends it to an owned set (`stickers.addStickerToSet`).
    fn add_sticker_to_set(
        &mut self,
        request: AddStickerToSetRequest,
    ) -> Result<OwnedStickerSetSummary, String>;

    /// Deletes an owned sticker set (`stickers.deleteStickerSet`).
    fn delete_sticker_set(
        &mut self,
        set_id: i64,
        access_hash: i64,
        short_name: &str,
    ) -> Result<(), String>;
}

/// Validates a sticker set title and short name before RPC submission.
pub fn validate_sticker_set_meta(title: &str, short_name: &str) -> Result<(), String> {
    let t = title.trim();
    if t.is_empty() || t.chars().count() > 64 {
        return Err("Sticker pack title must be 1–64 characters.".to_owned());
    }
    let s = short_name.trim();
    if s.len() < 3 || s.len() > 64 {
        return Err("Short name must be 3–64 characters.".to_owned());
    }
    let first_ok = s.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    let chars_ok = s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !first_ok || !chars_ok || s.contains("__") || s.ends_with('_') {
        return Err(
            "Short name must start with a letter and use letters, digits, or single underscores."
                .to_owned(),
        );
    }
    Ok(())
}

/// Normalizes sticker emoji list (defaulting to `"🙂"` matching `StickerEmojiOrDefault`).
#[must_use]
pub fn normalize_sticker_emoji(raw: &str) -> String {
    let trimmed: String = raw
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(MAX_STICKER_EMOJIS)
        .collect();
    if trimmed.is_empty() {
        "🙂".to_owned()
    } else {
        trimmed
    }
}

// ---------------------------------------------------------------------------
// Offline Mock Store (`MockSession` demo data)
// ---------------------------------------------------------------------------

/// Offline demo store for [`StickerCreatorBackend`].
#[derive(Debug, Clone)]
pub struct MockStickerCreatorStore {
    sets: Vec<OwnedStickerSetSummary>,
    next_id: i64,
}

impl Default for MockStickerCreatorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MockStickerCreatorStore {
    /// Creates a demo store with two fictional owned sticker & emoji packs.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sets: vec![
                OwnedStickerSetSummary {
                    id: 700_001,
                    access_hash: 700_001_999,
                    short_name: "rustgram_mascots_pack".to_owned(),
                    title: "Rustgram Mascots".to_owned(),
                    count: 8,
                    kind: StickerSetKind::Regular,
                },
                OwnedStickerSetSummary {
                    id: 700_002,
                    access_hash: 700_002_999,
                    short_name: "core_dev_emoji_pack".to_owned(),
                    title: "Core Dev Emoji".to_owned(),
                    count: 14,
                    kind: StickerSetKind::CustomEmoji,
                },
            ],
            next_id: 700_100,
        }
    }
}

impl StickerCreatorBackend for MockStickerCreatorStore {
    fn owned_sticker_sets(&mut self, kind: StickerSetKind) -> Vec<OwnedStickerSetSummary> {
        self.sets
            .iter()
            .filter(|s| s.kind == kind)
            .cloned()
            .collect()
    }

    fn suggest_sticker_short_name(&mut self, title: &str) -> Result<String, String> {
        let base: String = title
            .trim()
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let collapsed = base
            .split('_')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("_");
        let base_name = if collapsed.len() < 3 {
            "custom_pack".to_owned()
        } else {
            collapsed
        };
        let mut candidate = base_name.clone();
        let mut suffix = 2u32;
        while self
            .sets
            .iter()
            .any(|s| s.short_name.eq_ignore_ascii_case(&candidate))
        {
            candidate = format!("{base_name}_{suffix}");
            suffix += 1;
        }
        Ok(candidate)
    }

    fn check_sticker_short_name(&mut self, short_name: &str) -> Result<bool, String> {
        validate_sticker_set_meta("Check", short_name)?;
        let taken = self
            .sets
            .iter()
            .any(|s| s.short_name.eq_ignore_ascii_case(short_name.trim()));
        Ok(!taken)
    }

    fn create_sticker_set(
        &mut self,
        request: CreateStickerSetRequest,
    ) -> Result<OwnedStickerSetSummary, String> {
        validate_sticker_set_meta(&request.title, &request.short_name)?;
        if request.asset.bytes.is_empty() {
            return Err("Sticker payload cannot be empty.".to_owned());
        }
        if !self.check_sticker_short_name(&request.short_name)? {
            return Err("SHORT_NAME_OCCUPIED".to_owned());
        }
        self.next_id += 1;
        let summary = OwnedStickerSetSummary {
            id: self.next_id,
            access_hash: self.next_id.wrapping_mul(17),
            short_name: request.short_name.trim().to_owned(),
            title: request.title.trim().to_owned(),
            count: 1,
            kind: request.kind,
        };
        self.sets.insert(0, summary.clone());
        Ok(summary)
    }

    fn add_sticker_to_set(
        &mut self,
        request: AddStickerToSetRequest,
    ) -> Result<OwnedStickerSetSummary, String> {
        if request.asset.bytes.is_empty() {
            return Err("Sticker payload cannot be empty.".to_owned());
        }
        let Some(set) = self
            .sets
            .iter_mut()
            .find(|s| s.id == request.set_id || s.short_name == request.set_short_name)
        else {
            return Err("STICKERSET_INVALID".to_owned());
        };
        if set.count >= set.kind.max_items() {
            return Err("STICKERS_TOO_MUCH".to_owned());
        }
        set.count += 1;
        Ok(set.clone())
    }

    fn delete_sticker_set(
        &mut self,
        set_id: i64,
        _access_hash: i64,
        short_name: &str,
    ) -> Result<(), String> {
        self.sets
            .retain(|s| s.id != set_id && s.short_name != short_name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// StickerCreatorBox Modal (`boxes/sticker_creator_box.cpp`)
// ---------------------------------------------------------------------------
