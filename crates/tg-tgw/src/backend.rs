//! Widget rendering backend: font resolution, canvas font spelling cache, data-URL image
//! decoding with header/memory budget checks, canvas store, and CPU metering.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_backend.h/.cpp` and `tgw_renderer_rules.h`.

use std::collections::HashMap;

use base64::Engine as _;

use crate::canvas::{CanvasOp, CanvasStore};
use crate::emoji::{EmojiAccount, find_emoji_cells, walk_text_with_emoji};
use crate::error::{Result, TgwError};
use crate::state_machine::WidgetMeter;

/// Maximum CSS font size accepted (`kMaxFontSize = 1e6` in `tgw_backend.cpp:34`).
pub const MAX_FONT_SIZE: f64 = 1e6;

/// Maximum rasterized pixel font size (`kMaxFontPixelSize = 1024` in `tgw_backend.cpp:35`).
pub const MAX_FONT_PIXEL_SIZE: i32 = 1024;

/// Maximum simultaneously live fonts per backend (`kMaxLiveFonts = 16384`).
pub const MAX_LIVE_FONTS: usize = 16_384;

/// Maximum length of a CSS `font-family` list (`kMaxFamiliesBytes = 128 KiB`).
pub const MAX_FAMILIES_BYTES: usize = 128 * 1024;

/// Maximum length of text measured in one `text_width` call (`kMaxMeasuredText = 320 KiB`).
pub const MAX_MEASURED_TEXT: usize = 320 * 1024;

/// Maximum `data:` image URL length (`kMaxImageUrlSize = 5 MiB`).
pub const MAX_IMAGE_URL_SIZE: usize = 5 * 1024 * 1024;

/// Maximum decoded image width or height (`kMaxImageSide = 4096`).
pub const MAX_IMAGE_SIDE: u32 = 4096;

/// Maximum decoded image pixels (`kMaxImagePixels = 4_194_304`).
pub const MAX_IMAGE_PIXELS: u64 = 4 * 1024 * 1024;

/// Maximum total image bytes per widget backend (`kMaxBackendImageBytes = 32 MiB`).
pub const MAX_BACKEND_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// Maximum images per widget backend (`kMaxBackendImages = 4096`).
pub const MAX_BACKEND_IMAGES: usize = 4096;

/// Maximum new canvas fonts allocated in a single frame (`kMaxNewCanvasFonts = 16`).
pub const MAX_NEW_CANVAS_FONTS: usize = 16;

/// Maximum total canvas fonts per widget (`kMaxCanvasFonts = 32`).
pub const MAX_CANVAS_FONTS: usize = 32;

/// Maximum canvas font CSS string parses per frame (`kMaxCanvasFontParses = 64`).
pub const MAX_CANVAS_FONT_PARSES: usize = 64;

/// Maximum cached canvas font spelling entries (`kMaxCanvasFontSpellings = 64`).
pub const MAX_CANVAS_FONT_SPELLINGS: usize = 64;

/// Maximum length of a canvas CSS font string (`kMaxCanvasFontSpellingSize = 512`).
pub const MAX_CANVAS_FONT_SPELLING_SIZE: usize = 512;

/// Resolved generic font class (`FontKind` in `tgw_backend.cpp:72-75`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FontKind {
    /// Sans-serif / UI regular font.
    #[default]
    Regular,
    /// Monospace font (`monospace`, `ui-monospace`).
    Monospace,
}

/// Metrics returned for a created font (`twlite::FontMetrics`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FontMetrics {
    /// Font ascent in CSS pixels.
    pub ascent: f32,
    /// Font descent in CSS pixels.
    pub descent: f32,
    /// Line height in CSS pixels.
    pub line_height: f32,
    /// Em-box ascent.
    pub em_ascent: f32,
    /// Em-box descent.
    pub em_descent: f32,
}

/// Allocated font entry owned by [`WidgetBackend`].
#[derive(Debug, Clone, PartialEq)]
pub struct BackendFont {
    /// Resolved family kind.
    pub kind: FontKind,
    /// Requested size in CSS pixels.
    pub size: f64,
    /// Clamped CSS weight bucket (`100..=900`).
    pub weight: u16,
    /// Italic flag.
    pub italic: bool,
    /// Whether created via canvas font API.
    pub is_canvas: bool,
    /// Computed metrics.
    pub metrics: FontMetrics,
}

/// Decoded RGBA8 image owned by [`WidgetBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Unpremultiplied RGBA8 pixel buffer (`width * height * 4`).
    pub rgba: Vec<u8>,
}

/// Supported image formats for `data:` URLs (`kImageTypes` in `tgw_backend.cpp:105-111`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

/// Parsed `data:image/...` URL (`DataUrl` in `tgw_backend.cpp:113-116`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDataUrl {
    /// Decoded format from MIME header.
    pub format: DataImageFormat,
    /// Raw decoded image bytes.
    pub bytes: Vec<u8>,
}

/// Bounded per-frame cache of parsed canvas font spellings (`RendererRules::CanvasSpellings`).
#[derive(Debug, Clone)]
pub struct CanvasSpellings<T> {
    entries: HashMap<String, T>,
    parses_left: usize,
}

impl<T> Default for CanvasSpellings<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> CanvasSpellings<T> {
    /// Creates a spelling cache with a full per-frame parse budget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            parses_left: MAX_CANVAS_FONT_PARSES,
        }
    }

    /// Looks up `css` or parses it using `parse` if within length and frame-parse limits.
    pub fn find_or_parse<F>(&mut self, css: &str, parse: F) -> Option<&mut T>
    where
        F: FnOnce(&str) -> T,
    {
        if css.len() > MAX_CANVAS_FONT_SPELLING_SIZE {
            return None;
        }
        if self.entries.contains_key(css) {
            return self.entries.get_mut(css);
        }
        if self.parses_left == 0 {
            return None;
        }
        self.parses_left -= 1;
        let entry = parse(css);
        if self.entries.len() >= MAX_CANVAS_FONT_SPELLINGS {
            self.entries.clear();
        }
        self.entries.insert(css.to_owned(), entry);
        self.entries.get_mut(css)
    }

    /// Refills the per-frame parse budget.
    pub fn refill(&mut self) {
        self.parses_left = MAX_CANVAS_FONT_PARSES;
    }

    /// Clears all cached spellings and zeroes remaining parses.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.parses_left = 0;
    }
}

/// Resolves a CSS `font-family` list to [`FontKind`] respecting quotes and backslash escapes
/// (`ResolveFamilies` in `tgw_backend.cpp:182-207`).
#[must_use]
pub fn resolve_font_families(families: &str) -> FontKind {
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut from = 0usize;
    let bytes = families.as_bytes();

    for i in 0..=bytes.len() {
        let ch = if i < bytes.len() { bytes[i] } else { b',' };
        if escaped {
            escaped = false;
        } else if ch == b'\\' {
            escaped = true;
        } else if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
        } else if ch == b'"' || ch == b'\'' {
            quote = Some(ch);
        } else if ch == b',' {
            let token = families[from..i].trim().to_ascii_lowercase();
            match token.as_str() {
                "ui-monospace" | "monospace" => return FontKind::Monospace,
                "-apple-system" | "blinkmacsystemfont" | "system-ui" | "sans-serif"
                | "ui-sans-serif" | "serif" | "ui-serif" | "ui-rounded" | "cursive" | "fantasy"
                | "emoji" | "math" | "fangsong" => return FontKind::Regular,
                _ => {}
            }
            from = i + 1;
        }
    }
    FontKind::Regular
}

/// Snaps a CSS font weight into the nearest 100..=900 bucket (`FontWeightIndex`).
#[must_use]
pub fn normalize_font_weight(weight: i32) -> u16 {
    let clamped = weight.clamp(100, 900);
    (((clamped + 50) / 100) * 100) as u16
}

fn percent_decode_strict(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Parses a `data:image/{png,jpeg,jpg,gif,webp}[;base64],...` URL (`ParseDataUrl` in `tgw_backend.cpp:276-300`).
#[must_use]
pub fn parse_data_url(url: &str) -> Option<ParsedDataUrl> {
    if url.len() > MAX_IMAGE_URL_SIZE {
        return None;
    }
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.split(';').next()?.trim().to_ascii_lowercase();
    let format = match mime.as_str() {
        "image/png" => DataImageFormat::Png,
        "image/jpeg" | "image/jpg" => DataImageFormat::Jpeg,
        "image/gif" => DataImageFormat::Gif,
        "image/webp" => DataImageFormat::Webp,
        _ => return None,
    };
    let is_base64 = meta
        .rsplit_once(';')
        .is_some_and(|(_, tail)| tail.trim().eq_ignore_ascii_case("base64"));

    let decoded_pct = percent_decode_strict(payload)?;
    let bytes = if is_base64 {
        let stripped: Vec<u8> = decoded_pct
            .into_iter()
            .filter(|b| !matches!(b, b' ' | b'\t' | b'\n' | 0x0C | b'\r'))
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(&stripped)
            .ok()?
    } else {
        decoded_pct
    };
    Some(ParsedDataUrl { format, bytes })
}

/// Checks image dimensions against `MAX_IMAGE_SIDE` and `MAX_IMAGE_PIXELS` (`ValidImageSize`).
#[must_use]
pub const fn valid_image_size(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_IMAGE_SIDE
        && height <= MAX_IMAGE_SIDE
        && (width as u64) * (height as u64) <= MAX_IMAGE_PIXELS
}

fn gif_screen_fits(bytes: &[u8], budget: usize) -> bool {
    if bytes.len() < 10 || (!bytes.starts_with(b"GIF87a") && !bytes.starts_with(b"GIF89a")) {
        return false;
    }
    let w = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]));
    let h = u32::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    valid_image_size(w, h) && (w as usize).saturating_mul(h as usize).saturating_mul(4) <= budget
}

/// Core widget backend owning fonts, decoded images, canvases, emoji account, and CPU meter
/// (`Tgw::QtBackend` in `tgw_backend.h/.cpp`).
#[derive(Debug)]
pub struct WidgetBackend {
    fonts: HashMap<i32, BackendFont>,
    images: HashMap<i32, BackendImage>,
    canvases: CanvasStore,
    emoji: EmojiAccount,
    meter: WidgetMeter,
    canvas_spellings: CanvasSpellings<(FontKind, f64, u16, bool)>,
    canvas_fonts_count: usize,
    canvas_fonts_left_in_frame: usize,
    image_bytes: usize,
    last_font_id: i32,
    refusal: Option<String>,
}

impl Default for WidgetBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetBackend {
    /// Creates a new widget backend with full per-frame budgets.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fonts: HashMap::new(),
            images: HashMap::new(),
            canvases: CanvasStore::new(),
            emoji: EmojiAccount::new(),
            meter: WidgetMeter::new(),
            canvas_spellings: CanvasSpellings::new(),
            canvas_fonts_count: 0,
            canvas_fonts_left_in_frame: MAX_NEW_CANVAS_FONTS,
            image_bytes: 0,
            last_font_id: 0,
            refusal: None,
        }
    }

    /// Resets per-frame canvas work & font quotas (`QtBackend::beginFrame`).
    pub fn begin_frame(&mut self) {
        self.canvas_fonts_left_in_frame = MAX_NEW_CANVAS_FONTS;
        self.canvas_spellings.refill();
        self.canvases.refill();
    }

    /// Creates a DOM font and returns `(font_id, FontMetrics)` (`QtBackend::create_font`).
    pub fn create_font(
        &mut self,
        families: &str,
        size: f32,
        weight: i32,
        italic: bool,
    ) -> Result<(i32, FontMetrics)> {
        if families.len() > MAX_FAMILIES_BYTES {
            return Err(TgwError::ResourceLimit(
                "font-family string exceeded 128 KiB limit".to_owned(),
            ));
        }
        if self.fonts.len() >= MAX_LIVE_FONTS {
            return Err(TgwError::ResourceLimit(format!(
                "live font count exceeded {MAX_LIVE_FONTS}"
            )));
        }
        self.allocate_font(families, size, weight, italic, false)
    }

    /// Creates a canvas font subject to per-frame (`16`) and per-widget (`32`) canvas font caps.
    pub fn create_canvas_font(
        &mut self,
        families: &str,
        size: f32,
        weight: i32,
        italic: bool,
    ) -> Result<(i32, FontMetrics)> {
        if self.canvas_fonts_left_in_frame == 0 || self.canvas_fonts_count >= MAX_CANVAS_FONTS {
            return Err(TgwError::ResourceLimit(
                "canvas text exceeded the font allowance".to_owned(),
            ));
        }
        self.canvas_fonts_left_in_frame -= 1;
        self.canvas_fonts_count += 1;
        self.allocate_font(families, size, weight, italic, true)
    }

    fn allocate_font(
        &mut self,
        families: &str,
        size: f32,
        weight: i32,
        italic: bool,
        is_canvas: bool,
    ) -> Result<(i32, FontMetrics)> {
        let kind = resolve_font_families(families);
        let req_size = if size.is_finite() {
            f64::from(size).clamp(0.0, MAX_FONT_SIZE)
        } else {
            0.0
        };
        let norm_weight = normalize_font_weight(weight);
        let eff_size = req_size.min(f64::from(MAX_FONT_PIXEL_SIZE)) as f32;
        let ascent = eff_size * 0.8;
        let descent = eff_size * 0.2;
        let metrics = FontMetrics {
            ascent,
            descent,
            line_height: eff_size * 1.2,
            em_ascent: ascent,
            em_descent: descent,
        };
        self.last_font_id = self.last_font_id.saturating_add(1);
        let id = self.last_font_id;
        self.fonts.insert(
            id,
            BackendFont {
                kind,
                size: req_size,
                weight: norm_weight,
                italic,
                is_canvas,
                metrics,
            },
        );
        Ok((id, metrics))
    }

    /// Deletes a previously created font (`QtBackend::delete_font`).
    pub fn delete_font(&mut self, id: i32) {
        if let Some(font) = self.fonts.remove(&id)
            && font.is_canvas
        {
            self.canvas_fonts_count = self.canvas_fonts_count.saturating_sub(1);
        }
    }

    /// Measures text width accounting for emoji cell advances (`QtBackend::text_width`).
    pub fn text_width(&self, font_id: i32, text: &str) -> Result<f32> {
        if text.len() > MAX_MEASURED_TEXT {
            return Err(TgwError::ResourceLimit(format!(
                "measured text {} exceeds limit {MAX_MEASURED_TEXT}",
                text.len()
            )));
        }
        let Some(font) = self.fonts.get(&font_id) else {
            return Ok(0.0);
        };
        let factor = match font.kind {
            FontKind::Monospace => 0.6,
            FontKind::Regular => 0.55,
        };
        let cells = find_emoji_cells(text);
        let width = walk_text_with_emoji(
            text,
            &cells,
            font.size,
            |run| (run.chars().count() as f64) * font.size * factor,
            |_, _, _| {},
        );
        Ok(width as f32)
    }

    /// Decodes a `data:image/...` URL into the image cache enforcing memory and dimension limits
    /// (`QtBackend::load_image`).
    pub fn load_image(&mut self, id: i32, url: &str) -> Result<(u32, u32)> {
        if !self.images.contains_key(&id) && self.images.len() >= MAX_BACKEND_IMAGES {
            return Err(TgwError::ResourceLimit(format!(
                "backend image count limit ({MAX_BACKEND_IMAGES}) exceeded"
            )));
        }
        let parsed = parse_data_url(url).ok_or_else(|| {
            TgwError::ResourceLimit("unsupported or malformed image data URL".to_owned())
        })?;
        let prev_bytes = self.images.get(&id).map_or(0, |img| img.rgba.len());
        let budget =
            MAX_BACKEND_IMAGE_BYTES.saturating_sub(self.image_bytes.saturating_sub(prev_bytes));
        if parsed.format == DataImageFormat::Gif && !gif_screen_fits(&parsed.bytes, budget) {
            return Err(TgwError::ResourceLimit(
                "GIF dimensions exceed image budget".to_owned(),
            ));
        }

        let fmt = match parsed.format {
            DataImageFormat::Png => image::ImageFormat::Png,
            DataImageFormat::Jpeg => image::ImageFormat::Jpeg,
            DataImageFormat::Gif => image::ImageFormat::Gif,
            DataImageFormat::Webp => image::ImageFormat::WebP,
        };
        let reader = image::ImageReader::with_format(std::io::Cursor::new(&parsed.bytes), fmt);
        let (w, h) = reader
            .into_dimensions()
            .map_err(|e| TgwError::ResourceLimit(format!("image header decode failed: {e}")))?;
        if !valid_image_size(w, h) {
            return Err(TgwError::ResourceLimit(format!(
                "image dimensions {w}x{h} exceed limits"
            )));
        }
        let needed = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if needed > budget {
            return Err(TgwError::ResourceLimit(format!(
                "image memory {needed} exceeds remaining budget {budget}"
            )));
        }

        let img = image::load_from_memory_with_format(&parsed.bytes, fmt)
            .map_err(|e| TgwError::ResourceLimit(format!("image decode failed: {e}")))?
            .to_rgba8();
        let (dw, dh) = img.dimensions();
        let rgba = img.into_raw();
        self.image_bytes = self
            .image_bytes
            .saturating_sub(prev_bytes)
            .saturating_add(rgba.len());
        self.images.insert(
            id,
            BackendImage {
                width: dw,
                height: dh,
                rgba,
            },
        );
        Ok((dw, dh))
    }

    /// Runs a batch of canvas ops (`QtBackend::canvas_run`).
    pub fn canvas_run(&mut self, id: i32, width: u32, height: u32, ops: &[CanvasOp]) -> Result<()> {
        self.canvases.run(id, width, height, ops)
    }

    /// Reads canvas pixels (`QtBackend::canvas_read`).
    pub fn canvas_read(&self, id: i32, x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>> {
        self.canvases.read(id, x, y, width, height)
    }

    /// Releases a canvas (`QtBackend::canvas_release`).
    pub fn canvas_release(&mut self, id: i32) {
        self.canvases.release(id);
    }

    /// Returns a font by ID.
    #[must_use]
    pub fn font(&self, id: i32) -> Option<&BackendFont> {
        self.fonts.get(&id)
    }

    /// Returns a decoded image by ID.
    #[must_use]
    pub fn image(&self, id: i32) -> Option<&BackendImage> {
        self.images.get(&id)
    }

    /// Returns the underlying canvas store.
    #[must_use]
    pub fn canvases(&self) -> &CanvasStore {
        &self.canvases
    }

    /// Returns mutable access to the canvas store.
    pub fn canvases_mut(&mut self) -> &mut CanvasStore {
        &mut self.canvases
    }

    /// Returns the backend's emoji account.
    #[must_use]
    pub fn emoji(&self) -> &EmojiAccount {
        &self.emoji
    }

    /// Returns mutable access to the CPU meter.
    pub fn meter_mut(&mut self) -> &mut WidgetMeter {
        &mut self.meter
    }

    /// Marks the backend as refused with `reason`.
    pub fn refuse(&mut self, reason: impl Into<String>) {
        if self.refusal.is_none() {
            self.refusal = Some(reason.into());
        }
    }

    /// Returns the refusal reason, if any.
    #[must_use]
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    /// Releases all live resources (fonts, images, canvases, emoji waits).
    pub fn release_live(&mut self) {
        self.fonts.clear();
        self.images.clear();
        self.image_bytes = 0;
        self.canvas_fonts_count = 0;
        self.canvas_spellings.clear();
        self.canvases.clear();
        self.emoji.close();
    }
}
