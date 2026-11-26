//! `.tdesktop-palette` and `.tdesktop-theme` parsing, zip packaging, cloud theme metadata,
//! and theme editor export helpers.
//!
//! Port of `SourceFiles/window/themes/window_theme.cpp`, `window_theme_editor.cpp`,
//! `window_theme_editor_box.cpp`, `window_theme_preview.cpp` (data parts),
//! `window_themes_generate_name.cpp`, `SourceFiles/data/data_cloud_themes.h` (data parts),
//! and `lib_base/base/parse_helper.cpp`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use image::{ImageFormat, RgbaImage};
use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::color::Color;
use crate::colorizer::{Colorizer, colorize_hex, colorize_image, colorize_named};
use crate::crc32::crc32;
use crate::gradient::preprocess_background_image;
use crate::palette::{BubblesReadabilityArgs, Palette, SetResult, ensure_bubbles_readable};
use crate::svg_pattern::{SvgRasterOptions, is_gzip_bytes, is_svg_bytes, rasterize_svg_rgba};
use crate::wallpaper::WallPaper;

/// Maximum `.tdesktop-theme` file size (5 MiB, `kThemeFileSizeLimit`).
pub const THEME_FILE_SIZE_LIMIT: usize = 5 * 1024 * 1024;
/// Maximum `.tdesktop-palette` scheme text size (1 MiB, `kThemeSchemeSizeLimit`).
pub const THEME_SCHEME_SIZE_LIMIT: usize = 1024 * 1024;
/// Maximum raw background image size inside a `.tdesktop-theme` zip (4 MiB).
pub const THEME_BACKGROUND_SIZE_LIMIT: usize = 4 * 1024 * 1024;
/// Maximum pixel count for a theme background image (`kBackgroundSizeLimit`).
pub const BACKGROUND_PIXEL_LIMIT: u64 = 25 * 1024 * 1024;

const CLOUD_IN_TEXT_START: &[u8] = b"// THEME EDITOR SERVICE INFO START\n";
const CLOUD_IN_TEXT_END: &[u8] = b"// THEME EDITOR SERVICE INFO END\n\n";
const MIN_SLUG_SIZE: usize = 5;
const MAX_SLUG_SIZE: usize = 64;

/// Palette keys adjusted automatically to match the active chat background
/// (`ChatBackground::_adjustableColors` / `ReplaceAdjustableColors`).
pub const ADJUSTABLE_COLOR_KEYS: &[&str] = &[
    "msgServiceBg",
    "msgServiceBgSelected",
    "historyScrollBg",
    "historyScrollBgOver",
    "historyScrollBarBg",
    "historyScrollBarBgOver",
];

/// Errors produced when loading, parsing, or writing theme files.
#[derive(Debug, Error)]
pub enum ThemeError {
    /// Underlying filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Theme file exceeds size limits or is too short.
    #[error("invalid theme size: {0}")]
    InvalidSize(String),
    /// Syntax error in `.tdesktop-palette` content.
    #[error("palette syntax error: {0}")]
    PaletteSyntax(String),
    /// ZIP archive error or missing required palette entry.
    #[error("theme zip error: {0}")]
    Zip(String),
    /// Background image could not be decoded or exceeded pixel limits.
    #[error("background image error: {0}")]
    BackgroundImage(String),
}

/// Cloud theme variant type (`Data::CloudThemeType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CloudThemeType {
    /// Dark variant.
    Dark,
    /// Light variant.
    Light,
}

/// Per-variant cloud theme settings (`Data::CloudTheme::Settings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudThemeSettings {
    /// Optional chat wallpaper.
    pub paper: Option<WallPaper>,
    /// Primary accent color.
    pub accent_color: Color,
    /// Optional outgoing bubble accent color.
    pub outgoing_accent_color: Option<Color>,
    /// Outgoing message bubble gradient colors.
    pub outgoing_messages_colors: Vec<Color>,
}

/// Cloud theme descriptor (`Data::CloudTheme`, data parts only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudTheme {
    /// Server theme ID.
    pub id: u64,
    /// Server access hash.
    pub access_hash: u64,
    /// Theme share slug (`t.me/addtheme/<slug>`).
    pub slug: String,
    /// Display title.
    pub title: String,
    /// Associated document ID for the `.tdesktop-theme` file.
    pub document_id: u64,
    /// Creator user ID (`0` if not creator).
    pub created_by: u64,
    /// Install count.
    pub users_count: i32,
    /// Emoticon for chat themes (if any).
    pub emoticon: String,
    /// Light/dark chat-theme settings.
    pub settings: BTreeMap<CloudThemeType, CloudThemeSettings>,
}

/// Theme source metadata (`Window::Theme::Object`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeObject {
    /// Path relative to working directory (if any).
    pub path_relative: String,
    /// Absolute or virtual `:/gui/...` path.
    pub path_absolute: String,
    /// Raw `.tdesktop-theme` or `.tdesktop-palette` file bytes.
    pub content: Vec<u8>,
    /// Associated cloud theme metadata.
    pub cloud: CloudTheme,
}

/// Cached preprocessed theme state (`Window::Theme::Cached`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeCache {
    /// Serialized `Palette::save()` RGBA bytes.
    pub colors: Vec<u8>,
    /// Cached background image encoded as BMP.
    pub background: Vec<u8>,
    /// Whether the cached background is tiled.
    pub tiled: bool,
    /// `Palette::CHECKSUM` at the time the cache was built.
    pub palette_checksum: i32,
    /// `crc32` of the original theme file bytes.
    pub content_checksum: i32,
}

/// Combined theme source and cache for local storage (`Window::Theme::Saved`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedTheme {
    /// Theme source object.
    pub object: ThemeObject,
    /// Cached preprocessed theme data.
    pub cache: ThemeCache,
}

/// Loaded runtime theme instance (`Window::Theme::Instance`).
#[derive(Debug, Clone)]
pub struct ThemeInstance {
    /// Finalized palette.
    pub palette: Palette,
    /// Optional preprocessed background image.
    pub background: Option<RgbaImage>,
    /// Optional populated cache.
    pub cached: ThemeCache,
    /// Whether `background` should be tiled.
    pub tiled: bool,
}

impl tg_style::ColorResolver for ThemeInstance {
    fn resolve(&self, name: &str) -> Option<tg_style::Color> {
        self.palette.resolve(name)
    }
}

/// Raw parsed components of a `.tdesktop-theme` archive (`Window::Theme::ParsedTheme`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedTheme {
    /// Palette text content (`colors.tdesktop-theme` or plain `.tdesktop-palette`).
    pub palette: Vec<u8>,
    /// Raw encoded background bytes (JPEG or PNG), empty if no background.
    pub background: Vec<u8>,
    /// `true` if `background` is PNG, `false` if JPEG.
    pub is_png: bool,
    /// `true` if the background entry is `tiled.*`, `false` if `background.*`.
    pub tiled: bool,
}

/// Data-only preview container (`Window::Theme::Preview` / `CurrentData`).
#[derive(Debug, Clone)]
pub struct ThemePreviewData {
    /// Source object.
    pub object: ThemeObject,
    /// Loaded theme instance.
    pub instance: ThemeInstance,
    /// Preview background fallback color.
    pub preview_bg: Color,
}

/// Telegram Mini App / WebView theme parameters (`Webview::ThemeParams` from `WebViewParams()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebViewThemeParams {
    /// Body background color (`windowBg`).
    pub body_bg: Color,
    /// Title background color (`Color::rgba(0, 0, 0, 0)`).
    pub title_bg: Color,
    /// Scrollbar track color.
    pub scroll_bg: Color,
    /// Scrollbar track hover color.
    pub scroll_bg_over: Color,
    /// Scrollbar thumb color.
    pub scroll_bar_bg: Color,
    /// Scrollbar thumb hover color.
    pub scroll_bar_bg_over: Color,
    /// Compact JSON payload of WebView CSS theme variables.
    pub json: String,
}

// --- Comment stripping & palette text parsing (base/parse_helper.cpp + window_theme.cpp) ---

/// Strips C-style `//` and `/* ... */` comments while respecting quoted strings.
/// Exact port of `base::parse::stripComments`.
#[must_use]
pub fn strip_comments(content: &[u8]) -> Vec<u8> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        None,
        SingleLine,
        MultiLine,
    }
    let mut state = State::None;
    let mut inside_string = false;
    let mut result = Vec::new();
    let mut offset = 0usize;
    let len = content.len();

    let feed_content = |res: &mut Vec<u8>, off: &mut usize, idx: usize| {
        if idx > *off {
            if res.is_empty() {
                res.reserve(len.saturating_sub(*off));
            }
            res.extend_from_slice(&content[*off..idx]);
            *off = idx;
        }
    };
    let feed_comment = |res: &mut Vec<u8>, off: &mut usize, idx: usize| {
        if idx > *off {
            if res.is_empty() {
                res.reserve(len.saturating_sub(*off));
            }
            res.push(b' ');
            *off = idx;
        }
    };

    let mut ch = 0usize;
    while ch < len {
        let cur = content[ch];
        let next = if ch + 1 < len { content[ch + 1] } else { 0 };

        if state == State::None && cur == b'"' {
            let escaped =
                (ch > 0 && content[ch - 1] == b'\\') && (ch < 2 || content[ch - 2] != b'\\');
            if !escaped {
                inside_string = !inside_string;
            }
        }
        if inside_string {
            ch += 1;
            continue;
        }

        if state == State::None && cur == b'/' && next == b'/' {
            feed_content(&mut result, &mut offset, ch);
            state = State::SingleLine;
            ch += 2;
        } else if state == State::SingleLine && cur == b'\r' && next == b'\n' {
            feed_comment(&mut result, &mut offset, ch);
            ch += 2;
            state = State::None;
        } else if state == State::SingleLine && cur == b'\n' {
            feed_comment(&mut result, &mut offset, ch);
            ch += 1;
            state = State::None;
        } else if state == State::None && cur == b'/' && next == b'*' {
            feed_content(&mut result, &mut offset, ch);
            ch += 2;
            state = State::MultiLine;
        } else if state == State::MultiLine && cur == b'*' && next == b'/' {
            ch += 2;
            feed_comment(&mut result, &mut offset, ch);
            state = State::None;
        } else if state == State::MultiLine && cur == b'\r' && next == b'\n' {
            feed_comment(&mut result, &mut offset, ch);
            ch += 2;
            feed_content(&mut result, &mut offset, ch);
        } else if state == State::MultiLine && cur == b'\n' {
            feed_comment(&mut result, &mut offset, ch);
            ch += 1;
            feed_content(&mut result, &mut offset, ch);
        } else {
            ch += 1;
        }
    }

    if state == State::None && len > offset {
        if result.is_empty() {
            return content.to_vec();
        }
        result.extend_from_slice(&content[offset..len]);
    }
    result
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\n' | b'\t' | b'\r')
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn skip_whitespaces(data: &[u8], pos: &mut usize) -> bool {
    while *pos < data.len() && is_ws(data[*pos]) {
        *pos += 1;
    }
    *pos < data.len()
}

fn read_name<'a>(data: &'a [u8], pos: &mut usize) -> &'a [u8] {
    let start = *pos;
    while *pos < data.len() && is_name_char(data[*pos]) {
        *pos += 1;
    }
    &data[start..*pos]
}

fn skip_comment_inline(data: &[u8], pos: &mut usize) -> bool {
    if *pos + 1 >= data.len() || data[*pos] != b'/' {
        return false;
    }
    if data[*pos + 1] == b'/' {
        *pos += 2;
        while *pos < data.len() && data[*pos] != b'\n' {
            *pos += 1;
        }
        return true;
    }
    if data[*pos + 1] == b'*' {
        *pos += 2;
        while *pos < data.len() {
            while *pos < data.len() && data[*pos] != b'*' {
                *pos += 1;
            }
            if *pos < data.len() {
                *pos += 1;
                if *pos < data.len() && data[*pos] == b'/' {
                    *pos += 1;
                    break;
                }
            }
        }
        return true;
    }
    false
}

fn skip_ws_and_comments(data: &[u8], pos: &mut usize) {
    while *pos < data.len() {
        if !skip_whitespaces(data, pos) {
            return;
        }
        if !skip_comment_inline(data, pos) {
            return;
        }
    }
}

/// Parses `name: value;` pairs from `.tdesktop-palette` content, stripping comments first.
/// Exact port of `Window::Theme::ReadPaletteValues`.
pub fn read_palette_values<F>(content: &[u8], mut callback: F) -> Result<(), ThemeError>
where
    F: FnMut(&str, &str) -> bool,
{
    if content.len() > THEME_SCHEME_SIZE_LIMIT {
        return Err(ThemeError::InvalidSize(format!(
            "color scheme too large ({} bytes, limit {})",
            content.len(),
            THEME_SCHEME_SIZE_LIMIT
        )));
    }
    let stripped = strip_comments(content);
    let mut pos = 0usize;
    while pos < stripped.len() {
        if !skip_whitespaces(&stripped, &mut pos) {
            return Ok(());
        }
        let name_bytes = read_name(&stripped, &mut pos);
        if name_bytes.is_empty() {
            return Err(ThemeError::PaletteSyntax(
                "could not read color name".to_string(),
            ));
        }
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| ThemeError::PaletteSyntax("non-ASCII color name".to_string()))?;

        if !skip_whitespaces(&stripped, &mut pos) {
            return Err(ThemeError::PaletteSyntax(format!(
                "unexpected end of scheme after '{name}'"
            )));
        }
        if stripped[pos] != b':' {
            return Err(ThemeError::PaletteSyntax(format!(
                "expected ':' after key '{name}'"
            )));
        }
        pos += 1;
        if !skip_whitespaces(&stripped, &mut pos) {
            return Err(ThemeError::PaletteSyntax(format!(
                "unexpected end of scheme after '{name}:'"
            )));
        }

        let val_start = pos;
        if stripped[pos] == b'#' {
            pos += 1;
        }
        if read_name(&stripped, &mut pos).is_empty() {
            return Err(ThemeError::PaletteSyntax(format!(
                "expected color value or reference for key '{name}'"
            )));
        }
        let value = std::str::from_utf8(&stripped[val_start..pos])
            .map_err(|_| ThemeError::PaletteSyntax("non-ASCII color value".to_string()))?;

        if !skip_whitespaces(&stripped, &mut pos) {
            return Err(ThemeError::PaletteSyntax(format!(
                "unexpected end of scheme after value for '{name}'"
            )));
        }
        if stripped[pos] != b';' {
            return Err(ThemeError::PaletteSyntax(format!(
                "expected ';' after value for '{name}'"
            )));
        }
        pos += 1;

        if !callback(name, value) {
            return Err(ThemeError::PaletteSyntax(
                "callback aborted palette parsing".to_string(),
            ));
        }
    }
    Ok(())
}

/// Applies a `.tdesktop-palette` color scheme into `palette` with tdesktop's exact
/// override rules, error tolerance (unknown keys recorded in `unsupported` so later
/// keys can reference them; malformed hex values and unknown references skipped),
/// and optional `Colorizer`.
///
/// Exact port of `loadColorScheme` + `setColorSchemeValue` in `window_theme.cpp`.
pub fn load_color_scheme(
    content: &[u8],
    colorizer: &Colorizer,
    palette: &mut Palette,
) -> Result<(), ThemeError> {
    let mut unsupported: BTreeMap<String, String> = BTreeMap::new();
    read_palette_values(content, |name, raw_value| {
        let value = unsupported
            .get(raw_value)
            .map_or(raw_value, String::as_str)
            .to_string();

        let set_result = if value.starts_with('#') && (value.len() == 7 || value.len() == 9) {
            if let Some(mut c) = Color::from_hex(&value) {
                if colorizer.is_active() {
                    colorize_named(name, &mut c.r, &mut c.g, &mut c.b, colorizer);
                }
                palette.set_color(name, c)
            } else {
                // Error tolerance: skip malformed hex values, do not fail load.
                return true;
            }
        } else {
            palette.set_color_from(name, &value)
        };

        if set_result == SetResult::KeyNotFound {
            unsupported.insert(name.to_string(), value);
        }
        true
    })
}

// --- ZIP .tdesktop-theme reading & writing ---

fn read_zip_entry_case_insensitive(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    target: &str,
    size_limit: usize,
) -> Result<Option<Vec<u8>>, ThemeError> {
    let target_lower = target.to_ascii_lowercase();
    let mut matched_index = None;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| ThemeError::Zip(e.to_string()))?;
        let name = entry.name().map_err(|e| ThemeError::Zip(e.to_string()))?;
        if name.to_ascii_lowercase() == target_lower {
            matched_index = Some(i);
            break;
        }
    }
    let Some(idx) = matched_index else {
        return Ok(None);
    };
    let file = archive
        .by_index(idx)
        .map_err(|e| ThemeError::Zip(e.to_string()))?;
    if file.size() as usize > size_limit {
        return Err(ThemeError::InvalidSize(format!(
            "zip entry '{target}' exceeds limit ({} > {size_limit})",
            file.size()
        )));
    }
    let mut buf = Vec::with_capacity((file.size() as usize).min(size_limit));
    file.take((size_limit + 1) as u64).read_to_end(&mut buf)?;
    if buf.len() > size_limit {
        return Err(ThemeError::InvalidSize(format!(
            "zip entry '{target}' decompressed past limit ({size_limit})"
        )));
    }
    Ok(Some(buf))
}

/// Extracts the `.tdesktop-palette` text bytes from either a `.tdesktop-theme` zip
/// archive or a plain `.tdesktop-palette` text buffer (`ReadPaletteContent`).
pub fn read_palette_content_from_bytes(content: &[u8]) -> Result<Vec<u8>, ThemeError> {
    if content.len() > THEME_FILE_SIZE_LIMIT {
        return Err(ThemeError::InvalidSize(format!(
            "theme file too large ({} bytes, limit {})",
            content.len(),
            THEME_FILE_SIZE_LIMIT
        )));
    }
    if let Ok(mut archive) = ZipArchive::new(Cursor::new(content)) {
        if let Some(bytes) = read_zip_entry_case_insensitive(
            &mut archive,
            "colors.tdesktop-theme",
            THEME_SCHEME_SIZE_LIMIT,
        )? && !bytes.is_empty()
        {
            return Ok(bytes);
        }
        if let Some(bytes) = read_zip_entry_case_insensitive(
            &mut archive,
            "colors.tdesktop-palette",
            THEME_SCHEME_SIZE_LIMIT,
        )? && !bytes.is_empty()
        {
            return Ok(bytes);
        }
        return Err(ThemeError::Zip(
            "missing 'colors.tdesktop-theme' or 'colors.tdesktop-palette' in archive".to_string(),
        ));
    }
    Ok(content.to_vec())
}

/// Reads the palette text bytes from a `.tdesktop-theme` or `.tdesktop-palette` file on disk.
pub fn read_palette_content_from_file(path: &Path) -> Result<Vec<u8>, ThemeError> {
    let meta = fs::metadata(path)?;
    if meta.len() as usize > THEME_FILE_SIZE_LIMIT {
        return Err(ThemeError::InvalidSize(format!(
            "theme file too large ({} bytes)",
            meta.len()
        )));
    }
    let content = fs::read(path)?;
    read_palette_content_from_bytes(&content)
}

fn decode_theme_background(bytes: &[u8]) -> Result<RgbaImage, ThemeError> {
    if is_svg_bytes(bytes) || is_gzip_bytes(bytes) {
        let options = SvgRasterOptions {
            target_size: None,
            max_dimension: None,
            max_pixel_area: BACKGROUND_PIXEL_LIMIT,
            max_xml_bytes: THEME_BACKGROUND_SIZE_LIMIT,
            force_opaque_white: true,
        };
        let rgba = rasterize_svg_rgba(bytes, &options)
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
        return Ok(preprocess_background_image(&rgba));
    }
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > BACKGROUND_PIXEL_LIMIT {
        return Err(ThemeError::BackgroundImage(format!(
            "invalid background dimensions {w}x{h}"
        )));
    }
    let decoded = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?
        .decode()
        .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
    let mut rgba = decoded.to_rgba8();
    // Force opaque over white (matching Images::Read { .forceOpaque = true })
    for px in rgba.pixels_mut() {
        let a = u16::from(px.0[3]);
        if a < 255 {
            let inv = 255 - a;
            px.0[0] = ((u16::from(px.0[0]) * a + 255 * inv + 127) / 255) as u8;
            px.0[1] = ((u16::from(px.0[1]) * a + 255 * inv + 127) / 255) as u8;
            px.0[2] = ((u16::from(px.0[2]) * a + 255 * inv + 127) / 255) as u8;
            px.0[3] = 255;
        }
    }
    Ok(preprocess_background_image(&rgba))
}

fn load_background_from_zip(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
) -> Result<(Option<Vec<u8>>, bool), ThemeError> {
    for (entry, is_tiled) in [
        ("background.jpg", false),
        ("background.png", false),
        ("background.svg", false),
        ("background.tgv", false),
        ("tiled.jpg", true),
        ("tiled.png", true),
        ("tiled.svg", true),
        ("tiled.tgv", true),
    ] {
        if let Some(bg) =
            read_zip_entry_case_insensitive(archive, entry, THEME_BACKGROUND_SIZE_LIMIT)?
        {
            return Ok((Some(bg), is_tiled));
        }
    }
    Ok((None, false))
}

/// Loads a `.tdesktop-theme` (zip) or `.tdesktop-palette` (plain text) from raw bytes.
///
/// Exact port of `LoadTheme` in `window_theme.cpp`.
pub fn load_theme_from_bytes(
    content: &[u8],
    colorizer: &Colorizer,
    edited_palette: Option<&[u8]>,
    ensure_readable: bool,
) -> Result<ThemeInstance, ThemeError> {
    if content.len() < 4 {
        return Err(ThemeError::InvalidSize(format!(
            "bad theme content size: {}",
            content.len()
        )));
    }
    if content.len() > THEME_FILE_SIZE_LIMIT {
        return Err(ThemeError::InvalidSize(format!(
            "theme file too large: {} bytes",
            content.len()
        )));
    }

    let empty_colorizer = Colorizer::default();
    let palette_colorizer = if edited_palette.is_some() {
        &empty_colorizer
    } else {
        colorizer
    };

    let mut archive_opt = ZipArchive::new(Cursor::new(content)).ok();
    let palette_content = match edited_palette {
        Some(ed) => ed.to_vec(),
        None => match archive_opt.as_mut() {
            Some(archive) => {
                if let Some(b) = read_zip_entry_case_insensitive(
                    archive,
                    "colors.tdesktop-theme",
                    THEME_SCHEME_SIZE_LIMIT,
                )? && !b.is_empty()
                {
                    b
                } else if let Some(b) = read_zip_entry_case_insensitive(
                    archive,
                    "colors.tdesktop-palette",
                    THEME_SCHEME_SIZE_LIMIT,
                )? && !b.is_empty()
                {
                    b
                } else {
                    return Err(ThemeError::Zip(
                        "missing 'colors.tdesktop-theme' or 'colors.tdesktop-palette'".to_string(),
                    ));
                }
            }
            None => content.to_vec(),
        },
    };

    let mut palette = Palette::new_uninitialized();
    load_color_scheme(&palette_content, palette_colorizer, &mut palette)?;

    let mut background: Option<RgbaImage> = None;
    let mut tiled = false;
    let mut cached_bg_bytes = Vec::new();

    if let Some(archive) = archive_opt.as_mut() {
        let (bg_bytes_opt, is_tiled) = load_background_from_zip(archive)?;
        if let Some(bg_bytes) = bg_bytes_opt
            && !bg_bytes.is_empty()
        {
            let mut img = decode_theme_background(&bg_bytes)?;
            if colorizer.is_active() {
                colorize_image(&mut img, colorizer);
            }
            let mut bmp_buf = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(img.clone())
                .write_to(&mut bmp_buf, ImageFormat::Bmp)
                .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
            cached_bg_bytes = bmp_buf.into_inner();
            background = Some(img);
            tiled = is_tiled;
        }
    }

    palette.finalize_with(palette_colorizer);

    if ensure_readable && palette_colorizer.is_active() {
        let mut reference = Palette::new_uninitialized();
        if load_color_scheme(&palette_content, &Colorizer::default(), &mut reference).is_ok() {
            reference.finalize();
            ensure_bubbles_readable(
                &mut palette,
                &BubblesReadabilityArgs {
                    dark: reference.is_dark_palette(),
                    outgoing_gradient: Vec::new(),
                    reference: Some(&reference),
                    derive_outgoing_selection: false,
                },
            );
        }
    }

    let cached = ThemeCache {
        colors: palette.save(),
        background: cached_bg_bytes,
        tiled,
        palette_checksum: Palette::CHECKSUM,
        content_checksum: crc32(content),
    };

    Ok(ThemeInstance {
        palette,
        background,
        cached,
        tiled,
    })
}

/// Loads a `.tdesktop-theme` or `.tdesktop-palette` file from disk (`LoadFromFile`).
pub fn load_theme_from_file(
    path: &Path,
    colorizer: &Colorizer,
    ensure_readable: bool,
) -> Result<ThemeInstance, ThemeError> {
    let meta = fs::metadata(path)?;
    if meta.len() as usize > THEME_FILE_SIZE_LIMIT {
        return Err(ThemeError::InvalidSize(format!(
            "theme file too large: {} bytes",
            meta.len()
        )));
    }
    let content = fs::read(path)?;
    load_theme_from_bytes(&content, colorizer, None, ensure_readable)
}

/// Attempts fast initialization from a previously saved [`ThemeCache`]
/// (`InitializeFromCache` in `window_theme.cpp`).
#[must_use]
pub fn load_from_cache(content: &[u8], cache: &ThemeCache) -> Option<ThemeInstance> {
    if cache.palette_checksum != Palette::CHECKSUM {
        return None;
    }
    if cache.content_checksum != crc32(content) {
        return None;
    }
    let background = if cache.background.is_empty() {
        None
    } else {
        Some(decode_theme_background(&cache.background).ok()?)
    };
    let mut palette = Palette::new_uninitialized();
    if !palette.load(&cache.colors) {
        return None;
    }
    Some(ThemeInstance {
        palette,
        background,
        cached: cache.clone(),
        tiled: cache.tiled,
    })
}

/// Extracts raw palette bytes and raw background bytes from a `.tdesktop-theme` or
/// `.tdesktop-palette` payload (`ParseTheme` in `window_theme_editor_box.cpp`).
pub fn parse_theme(
    content: &[u8],
    only_palette: bool,
    colorizer: Option<&Colorizer>,
) -> Result<ParsedTheme, ThemeError> {
    let mut parsed = ParsedTheme::default();
    if let Ok(mut archive) = ZipArchive::new(Cursor::new(content)) {
        parsed.palette = if let Some(b) = read_zip_entry_case_insensitive(
            &mut archive,
            "colors.tdesktop-theme",
            THEME_SCHEME_SIZE_LIMIT,
        )? {
            b
        } else if let Some(b) = read_zip_entry_case_insensitive(
            &mut archive,
            "colors.tdesktop-palette",
            THEME_SCHEME_SIZE_LIMIT,
        )? {
            b
        } else {
            return Err(ThemeError::Zip(
                "missing 'colors.tdesktop-theme' or 'colors.tdesktop-palette'".to_string(),
            ));
        };

        if !only_palette {
            for (entry, is_png, is_tiled) in [
                ("background.jpg", false, false),
                ("background.png", true, false),
                ("background.svg", false, false),
                ("background.tgv", false, false),
                ("tiled.png", true, true),
                ("tiled.jpg", false, true),
                ("tiled.svg", false, true),
                ("tiled.tgv", false, true),
            ] {
                if let Some(bg) = read_zip_entry_case_insensitive(
                    &mut archive,
                    entry,
                    THEME_BACKGROUND_SIZE_LIMIT,
                )? {
                    parsed.background = bg;
                    parsed.is_png = is_png;
                    parsed.tiled = is_tiled;
                    break;
                }
            }
        }
    } else {
        parsed.palette = content.to_vec();
    }

    if let Some(c) = colorizer
        && c.is_active()
    {
        parsed.palette = colorize_in_content(&parsed.palette, c)?;
    }

    Ok(parsed)
}

/// Packs a [`ParsedTheme`] into a `.tdesktop-theme` ZIP archive (`PackTheme`).
///
/// Contains `colors.tdesktop-theme` plus (when `parsed.background` is non-empty)
/// `background.{jpg,png,svg,tgv}` or `tiled.{jpg,png,svg,tgv}`.
pub fn pack_theme(parsed: &ParsedTheme) -> Result<Vec<u8>, ThemeError> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        if !parsed.background.is_empty() {
            let bg_name = if is_svg_bytes(&parsed.background) {
                if parsed.tiled {
                    "tiled.svg"
                } else {
                    "background.svg"
                }
            } else if is_gzip_bytes(&parsed.background) {
                if parsed.tiled {
                    "tiled.tgv"
                } else {
                    "background.tgv"
                }
            } else {
                match (parsed.tiled, parsed.is_png) {
                    (false, false) => "background.jpg",
                    (false, true) => "background.png",
                    (true, false) => "tiled.jpg",
                    (true, true) => "tiled.png",
                }
            };
            zip.start_file(bg_name, options)
                .map_err(|e| ThemeError::Zip(e.to_string()))?;
            zip.write_all(&parsed.background)?;
        }

        zip.start_file("colors.tdesktop-theme", options)
            .map_err(|e| ThemeError::Zip(e.to_string()))?;
        zip.write_all(&parsed.palette)?;

        zip.finish().map_err(|e| ThemeError::Zip(e.to_string()))?;
    }
    Ok(cursor.into_inner())
}

/// Writes a packed `.tdesktop-theme` archive to `path`.
pub fn save_theme_to_file(path: &Path, parsed: &ParsedTheme) -> Result<(), ThemeError> {
    let bytes = pack_theme(parsed)?;
    fs::write(path, bytes)?;
    Ok(())
}

// --- Theme editor palette export & manipulation (window_theme_editor*.cpp) ---

/// Exports the full default palette in `.tdesktop-palette` format with comments
/// (`GenerateDefaultPalette` in `window_theme_editor_box.cpp`).
#[must_use]
pub fn generate_default_palette() -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * 1024);
    for row in Palette::ROWS {
        out.extend_from_slice(row.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(row.value.as_bytes());
        out.extend_from_slice(b"; // ");
        for b in row.description.bytes() {
            out.push(if b == b'\n' || b == b'\r' { b' ' } else { b });
        }
        out.push(b'\n');
    }
    out
}

/// Exports the current resolved colors of `palette` as a `.tdesktop-palette` text file
/// with descriptions (useful for theme editor export).
#[must_use]
pub fn export_palette_for_editor(palette: &Palette) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * 1024);
    for row in Palette::ROWS {
        let color = palette
            .color_at_index(row.index)
            .unwrap_or(row.default_color);
        out.extend_from_slice(row.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(color.to_hex_string().as_bytes());
        out.extend_from_slice(b"; // ");
        for b in row.description.bytes() {
            out.push(if b == b'\n' || b == b'\r' { b' ' } else { b });
        }
        out.push(b'\n');
    }
    out
}

fn read_editor_value<'a>(data: &'a [u8], pos: &mut usize) -> &'a [u8] {
    let start = *pos;
    if *pos < data.len() && data[*pos] == b'#' {
        *pos += 1;
    }
    let _ = read_name(data, pos);
    &data[start..*pos]
}

fn is_valid_hex_value(value: &[u8]) -> bool {
    (value.len() == 7 || value.len() == 9)
        && value[0] == b'#'
        && value[1..].iter().all(u8::is_ascii_hexdigit)
}

/// Replaces the last valid assignment to `name` in `.tdesktop-palette` content, or appends
/// `name: value;` if absent (`Window::Theme::ReplaceValueInPaletteContent`).
pub fn replace_value_in_palette_content(
    content: &[u8],
    name: &str,
    value: &str,
) -> Result<Vec<u8>, ThemeError> {
    let mut valid_names: std::collections::BTreeSet<&[u8]> = std::collections::BTreeSet::new();
    let mut pos = 0usize;
    let mut last_valid_span: Option<(usize, usize)> = None;

    while pos < content.len() {
        skip_ws_and_comments(content, &mut pos);
        if pos >= content.len() {
            break;
        }
        let found_name = read_name(content, &mut pos);
        skip_ws_and_comments(content, &mut pos);
        if pos >= content.len() || content[pos] != b':' {
            return Err(ThemeError::PaletteSyntax("expected ':'".to_string()));
        }
        pos += 1;
        skip_ws_and_comments(content, &mut pos);
        let value_start = pos;
        let found_value = read_editor_value(content, &mut pos);
        let value_end = pos;
        if found_value.is_empty() {
            return Err(ThemeError::PaletteSyntax("empty value".to_string()));
        }
        if valid_names.contains(found_value) || is_valid_hex_value(found_value) {
            valid_names.insert(found_name);
            if found_name == name.as_bytes() {
                last_valid_span = Some((value_start, value_end));
            }
        }
        skip_ws_and_comments(content, &mut pos);
        if pos >= content.len() || content[pos] != b';' {
            return Err(ThemeError::PaletteSyntax("expected ';'".to_string()));
        }
        pos += 1;
    }

    if let Some((start, end)) = last_valid_span {
        let mut out = Vec::with_capacity(start + value.len() + (content.len() - end));
        out.extend_from_slice(&content[..start]);
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(&content[end..]);
        return Ok(out);
    }

    let newline: &[u8] = if content.windows(2).any(|w| w == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut out = Vec::with_capacity(content.len() + name.len() + value.len() + 8);
    out.extend_from_slice(content);
    if !content.ends_with(b"\n") {
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.push(b';');
    out.extend_from_slice(newline);
    Ok(out)
}

/// Applies `colorizer` to all `#rrggbb[aa]` literals inside raw `.tdesktop-palette` text
/// in place while preserving comments and layout (`Editor::ColorizeInContent`).
pub fn colorize_in_content(content: &[u8], colorizer: &Colorizer) -> Result<Vec<u8>, ThemeError> {
    let mut out = content.to_vec();
    let mut pos = 0usize;
    while pos < content.len() {
        skip_ws_and_comments(content, &mut pos);
        if pos >= content.len() {
            break;
        }
        let _found_name = read_name(content, &mut pos);
        skip_ws_and_comments(content, &mut pos);
        if pos >= content.len() || content[pos] != b':' {
            return Err(ThemeError::PaletteSyntax("expected ':'".to_string()));
        }
        pos += 1;
        skip_ws_and_comments(content, &mut pos);
        let val_start = pos;
        let value = read_editor_value(content, &mut pos);
        let val_end = pos;
        if value.is_empty() {
            return Err(ThemeError::PaletteSyntax("empty value".to_string()));
        }
        if is_valid_hex_value(value)
            && let Ok(val_str) = std::str::from_utf8(value)
            && let Some(colorized) = colorize_hex(val_str, colorizer)
            && colorized.len() == value.len()
        {
            out[val_start..val_end].copy_from_slice(colorized.as_bytes());
        }
        skip_ws_and_comments(content, &mut pos);
        if pos >= content.len() || content[pos] != b';' {
            return Err(ThemeError::PaletteSyntax("expected ';'".to_string()));
        }
        pos += 1;
    }
    Ok(out)
}

/// Updates the 6 adjustable chat-background colors in palette text from `palette`
/// (`ReplaceAdjustableColors` in `window_theme_editor_box.cpp`).
pub fn replace_adjustable_colors(content: &[u8], palette: &Palette) -> Result<Vec<u8>, ThemeError> {
    let mut data = content.to_vec();
    for &key in ADJUSTABLE_COLOR_KEYS {
        if let Some(color) = palette.color_by_name(key) {
            data = replace_value_in_palette_content(&data, key, &color.to_hex_string())?;
        }
    }
    Ok(data)
}

/// Serializes cloud theme ID & access hash as a header comment block
/// (`Window::Theme::WriteCloudToText`).
#[must_use]
pub fn write_cloud_to_text(cloud: &CloudTheme) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(CLOUD_IN_TEXT_START);
    out.extend_from_slice(format!("// ID: {}\n", cloud.id).as_bytes());
    out.extend_from_slice(format!("// ACCESS: {}\n", cloud.access_hash).as_bytes());
    out.extend_from_slice(CLOUD_IN_TEXT_END);
    out
}

/// Parses cloud theme ID & access hash from a header comment block
/// (`Window::Theme::ReadCloudFromText`).
#[must_use]
pub fn read_cloud_from_text(text: &[u8]) -> CloudTheme {
    let Some(end_pos) = find_subslice(text, CLOUD_IN_TEXT_END) else {
        return CloudTheme::default();
    };
    if end_pos <= 1 {
        return CloudTheme::default();
    }
    let header = &text[..end_pos - 1];
    let lines: Vec<&[u8]> = header.split(|&b| b == b'\n').collect();
    let take_u64 = |idx: usize| -> Option<u64> {
        let entry = *lines.get(idx)?;
        let pos = find_subslice(entry, b": ")?;
        let s = std::str::from_utf8(&entry[pos + 2..]).ok()?.trim();
        s.parse::<u64>().ok()
    };
    let (Some(id), Some(access_hash)) = (take_u64(1), take_u64(2)) else {
        return CloudTheme::default();
    };
    CloudTheme {
        id,
        access_hash,
        ..CloudTheme::default()
    }
}

/// Strips any leading theme-editor cloud info comment headers
/// (`Window::Theme::StripCloudTextFields`).
#[must_use]
pub fn strip_cloud_text_fields(text: &[u8]) -> Vec<u8> {
    let Some(first_value) = find_subslice(text, b": #") else {
        return text.to_vec();
    };
    let mut start = 0usize;
    while let Some(rel) = find_subslice(&text[start..], CLOUD_IN_TEXT_END) {
        let index = start + rel;
        if index > first_value {
            break;
        }
        start = index + CLOUD_IN_TEXT_END.len();
    }
    text[start..].to_vec()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Validates a cloud theme slug (`IsGoodSlug`: 5..=64 ASCII alphanumeric or `_`).
#[must_use]
pub fn is_good_slug(slug: &str) -> bool {
    (MIN_SLUG_SIZE..=MAX_SLUG_SIZE).contains(&slug.len())
        && slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Computes the cached theme virtual path for a cloud theme document ID (`CachedThemePath`).
#[must_use]
pub fn cached_theme_path(document_id: u64) -> String {
    format!("special://cloud/{document_id}.tdesktop-theme")
}

/// Builds Telegram WebView / Mini App theme parameters from `palette` (`WebViewParams`).
#[must_use]
pub fn web_view_params(palette: &Palette) -> WebViewThemeParams {
    let entries: &[(&str, Color)] = &[
        ("bg_color", palette.window_bg),
        ("secondary_bg_color", palette.box_divider_bg),
        ("text_color", palette.window_fg),
        ("hint_color", palette.window_sub_text_fg),
        ("link_color", palette.window_active_text_fg),
        ("button_color", palette.window_bg_active),
        ("button_text_color", palette.window_fg_active),
        ("header_bg_color", palette.window_bg),
        ("accent_text_color", palette.light_button_fg),
        ("section_bg_color", palette.light_button_bg),
        ("section_header_text_color", palette.window_active_text_fg),
        ("subtitle_text_color", palette.window_sub_text_fg),
        ("destructive_text_color", palette.attention_button_fg),
        ("bottom_bar_bg_color", palette.window_bg),
    ];

    let bg = palette.window_bg;
    let shadow = palette.shadow_fg;
    let sa = shadow.alpha_f();
    let mix = |a: u8, b: u8| -> u8 {
        (f64::from(a) + (f64::from(b) - f64::from(a)) * sa)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    let sep = Color::rgb(
        mix(bg.r, shadow.r),
        mix(bg.g, shadow.g),
        mix(bg.b, shadow.b),
    );

    let mut json = String::with_capacity(512);
    json.push('{');
    for (i, &(k, c)) in entries.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push('"');
        json.push_str(k);
        json.push_str("\":\"#");
        json.push_str(&c.to_rgb_hex());
        json.push('"');
    }
    json.push_str(",\"section_separator_color\":\"#");
    json.push_str(&sep.to_rgb_hex());
    json.push_str("\"}");

    WebViewThemeParams {
        body_bg: palette.window_bg,
        title_bg: Color::rgba(0, 0, 0, 0),
        scroll_bg: palette.scroll_bg,
        scroll_bg_over: palette.scroll_bg_over,
        scroll_bar_bg: palette.scroll_bar_bg,
        scroll_bar_bg_over: palette.scroll_bar_bg_over,
        json,
    }
}

/// Finds the closest named color for `accent` using the perceptual RGB distance
/// from `window_themes_generate_name.cpp`.
#[must_use]
pub fn nearest_theme_color_name(accent: Color) -> &'static str {
    const COLORS: &[(u32, &str)] = &[
        (0x8e0000, "Berry"),
        (0xdec196, "Brandy"),
        (0x800b47, "Cherry"),
        (0xff7f50, "Coral"),
        (0xdb5079, "Cranberry"),
        (0xdc143c, "Crimson"),
        (0xe0b0ff, "Mauve"),
        (0xffc0cb, "Pink"),
        (0xff0000, "Red"),
        (0xff007f, "Rose"),
        (0x80461b, "Russet"),
        (0xff2400, "Scarlet"),
        (0xf1f1f1, "Seashell"),
        (0xff3399, "Strawberry"),
        (0xffbf00, "Amber"),
        (0xeb9373, "Apricot"),
        (0xfbe7b2, "Banana"),
        (0xa1c50a, "Citrus"),
        (0xb06500, "Ginger"),
        (0xffd700, "Gold"),
        (0xfde910, "Lemon"),
        (0xffa500, "Orange"),
        (0xffe5b4, "Peach"),
        (0xff6b53, "Persimmon"),
        (0xe4d422, "Sunflower"),
        (0xf28500, "Tangerine"),
        (0xffc87c, "Topaz"),
        (0xffff00, "Yellow"),
        (0x384910, "Clover"),
        (0x83aa5d, "Cucumber"),
        (0x50c878, "Emerald"),
        (0xb5b35c, "Olive"),
        (0x00ff00, "Green"),
        (0x00a86b, "Jade"),
        (0x29ab87, "Jungle"),
        (0xbfff00, "Lime"),
        (0x0bda51, "Malachite"),
        (0x98ff98, "Mint"),
        (0xaddfad, "Moss"),
        (0x315ba1, "Azure"),
        (0x0000ff, "Blue"),
        (0x0047ab, "Cobalt"),
        (0x4f69c6, "Indigo"),
        (0x017987, "Lagoon"),
        (0x71d9e2, "Aquamarine"),
        (0x120a8f, "Ultramarine"),
        (0x000080, "Navy"),
        (0x2f519e, "Sapphire"),
        (0x76d7ea, "Sky"),
        (0x008080, "Teal"),
        (0x40e0d0, "Turquoise"),
        (0x9966cc, "Amethyst"),
        (0x4d0135, "Blackberry"),
        (0x614051, "Eggplant"),
        (0xc8a2c8, "Lilac"),
        (0xb57edc, "Lavender"),
        (0xccccff, "Periwinkle"),
        (0x843179, "Plum"),
        (0x660099, "Purple"),
        (0xd8bfd8, "Thistle"),
        (0xda70d6, "Orchid"),
        (0x240a40, "Violet"),
        (0x3f2109, "Bronze"),
        (0x370202, "Chocolate"),
        (0x7b3f00, "Cinnamon"),
        (0x301f1e, "Cocoa"),
        (0x706555, "Coffee"),
        (0x796989, "Rum"),
        (0x4e0606, "Mahogany"),
        (0x782d19, "Mocha"),
        (0xc2b280, "Sand"),
        (0x882d17, "Sienna"),
        (0x780109, "Maple"),
        (0xf0e68c, "Khaki"),
        (0xb87333, "Copper"),
        (0xb94e48, "Chestnut"),
        (0xeed9c4, "Almond"),
        (0xfffdd0, "Cream"),
        (0xb9f2ff, "Diamond"),
        (0xa98307, "Honey"),
        (0xfffff0, "Ivory"),
        (0xeae0c8, "Pearl"),
        (0xeff2f3, "Porcelain"),
        (0xd1bea8, "Vanilla"),
        (0xffffff, "White"),
        (0x808080, "Gray"),
        (0x000000, "Black"),
        (0xe8f1d4, "Chrome"),
        (0x36454f, "Charcoal"),
        (0x0c0b1d, "Ebony"),
        (0xc0c0c0, "Silver"),
        (0xf5f5f5, "Smoke"),
        (0x262335, "Steel"),
        (0x4fa83d, "Apple"),
        (0x80b3c4, "Glacier"),
        (0xfebaad, "Melon"),
        (0xc54b8c, "Mulberry"),
        (0xa9c6c2, "Opal"),
        (0x54a5f8, "Blue"),
    ];
    let r1 = i32::from(accent.r);
    let g1 = i32::from(accent.g);
    let b1 = i32::from(accent.b);
    COLORS
        .iter()
        .min_by_key(|&&(hex, _)| {
            let r2 = ((hex >> 16) & 0xff) as i32;
            let g2 = ((hex >> 8) & 0xff) as i32;
            let b2 = (hex & 0xff) as i32;
            let r_mean = (r1 + r2) / 2;
            let r = r1 - r2;
            let g = g1 - g2;
            let b = b1 - b2;
            (((512 + r_mean) * r * r) >> 8) + (4 * g * g) + (((767 - r_mean) * b * b) >> 8)
        })
        .map_or("Blue", |&(_, name)| name)
}

/// Generates a random-style theme name from `accent` and a caller-supplied `seed`
/// (`Window::Theme::GenerateName` in `window_themes_generate_name.cpp`).
#[must_use]
pub fn generate_theme_name(accent: Color, seed: u64) -> String {
    const ADJECTIVES: &[&str] = &[
        "Ancient",
        "Antique",
        "Autumn",
        "Baby",
        "Barely",
        "Baroque",
        "Blazing",
        "Blushing",
        "Bohemian",
        "Bubbly",
        "Burning",
        "Buttered",
        "Classic",
        "Clear",
        "Cool",
        "Cosmic",
        "Cotton",
        "Cozy",
        "Crystal",
        "Dark",
        "Daring",
        "Darling",
        "Dawn",
        "Dazzling",
        "Deep",
        "Deepest",
        "Delicate",
        "Delightful",
        "Divine",
        "Double",
        "Downtown",
        "Dreamy",
        "Dusky",
        "Dusty",
        "Electric",
        "Enchanted",
        "Endless",
        "Evening",
        "Fantastic",
        "Flirty",
        "Forever",
        "Frigid",
        "Frosty",
        "Frozen",
        "Gentle",
        "Heavenly",
        "Hyper",
        "Icy",
        "Infinite",
        "Innocent",
        "Instant",
        "Luscious",
        "Lunar",
        "Lustrous",
        "Magic",
        "Majestic",
        "Mambo",
        "Midnight",
        "Millennium",
        "Morning",
        "Mystic",
        "Natural",
        "Neon",
        "Night",
        "Opaque",
        "Paradise",
        "Perfect",
        "Perky",
        "Polished",
        "Powerful",
        "Rich",
        "Royal",
        "Sheer",
        "Simply",
        "Sizzling",
        "Solar",
        "Sparkling",
        "Splendid",
        "Spicy",
        "Spring",
        "Stellar",
        "Sugared",
        "Summer",
        "Sunny",
        "Super",
        "Sweet",
        "Tender",
        "Tenacious",
        "Tidal",
        "Toasted",
        "Totally",
        "Tranquil",
        "Tropical",
        "True",
        "Twilight",
        "Twinkling",
        "Ultimate",
        "Ultra",
        "Velvety",
        "Vibrant",
        "Vintage",
        "Virtual",
        "Warm",
        "Warmest",
        "Whipped",
        "Wild",
        "Winsome",
    ];
    const SUBJECTIVES: &[&str] = &[
        "Ambrosia",
        "Attack",
        "Avalanche",
        "Blast",
        "Bliss",
        "Blossom",
        "Blush",
        "Burst",
        "Butter",
        "Candy",
        "Carnival",
        "Charm",
        "Chiffon",
        "Cloud",
        "Comet",
        "Delight",
        "Dream",
        "Dust",
        "Fantasy",
        "Flame",
        "Flash",
        "Fire",
        "Freeze",
        "Frost",
        "Glade",
        "Glaze",
        "Gleam",
        "Glimmer",
        "Glitter",
        "Glow",
        "Grande",
        "Haze",
        "Highlight",
        "Ice",
        "Illusion",
        "Intrigue",
        "Jewel",
        "Jubilee",
        "Kiss",
        "Lights",
        "Lollypop",
        "Love",
        "Luster",
        "Madness",
        "Matte",
        "Mirage",
        "Mist",
        "Moon",
        "Muse",
        "Myth",
        "Nectar",
        "Nova",
        "Parfait",
        "Passion",
        "Pop",
        "Rain",
        "Reflection",
        "Rhapsody",
        "Romance",
        "Satin",
        "Sensation",
        "Silk",
        "Shine",
        "Shadow",
        "Shimmer",
        "Sky",
        "Spice",
        "Star",
        "Sugar",
        "Sunrise",
        "Sunset",
        "Sun",
        "Twist",
        "Unbound",
        "Velvet",
        "Vibrant",
        "Waters",
        "Wine",
        "Wink",
        "Wonder",
        "Zone",
    ];
    let color = nearest_theme_color_name(accent);
    if (seed & 1) == 0 {
        let adj = ADJECTIVES[((seed >> 1) as usize) % ADJECTIVES.len()];
        format!("{adj} {color}")
    } else {
        let subj = SUBJECTIVES[((seed >> 1) as usize) % SUBJECTIVES.len()];
        format!("{color} {subj}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_parsing_comments_overrides_and_error_tolerance() {
        let scheme = br#"
            // Line comment
            /* Multi-line
               comment */
            customUnknownKey: #123456;
            windowBg: customUnknownKey; // references unsupported key!
            windowFg: #aabbccdd;
            windowFg: #112233; // duplicate overwrites
            badHex: #zzzzzz; // skipped with error tolerance
            badRef: nonexistentKey; // skipped with error tolerance
        "#;

        let mut pal = Palette::new_uninitialized();
        load_color_scheme(scheme, &Colorizer::default(), &mut pal).unwrap();
        pal.finalize();

        assert_eq!(pal.window_bg, Color::rgb(0x12, 0x34, 0x56));
        assert_eq!(pal.window_fg, Color::rgb(0x11, 0x22, 0x33));
        // Fallback propagated from windowBg
        assert_eq!(pal.menu_bg, Color::rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn theme_zip_round_trip_with_background_and_cache() {
        // Create a tiny 4x4 PNG background
        let bg_img = RgbaImage::from_pixel(4, 4, image::Rgba([0x20, 0x40, 0x60, 255]));
        let mut png_bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(bg_img.clone())
            .write_to(&mut png_bytes, ImageFormat::Png)
            .unwrap();

        let parsed = ParsedTheme {
            palette: b"windowBg: #102030;\nwindowBgActive: #ff5500;\n".to_vec(),
            background: png_bytes.into_inner(),
            is_png: true,
            tiled: true,
        };

        let packed = pack_theme(&parsed).unwrap();

        // Parse back as ParsedTheme
        let unpacked = parse_theme(&packed, false, None).unwrap();
        assert_eq!(unpacked.palette, parsed.palette);
        assert_eq!(unpacked.background, parsed.background);
        assert!(unpacked.is_png);
        assert!(unpacked.tiled);

        // Load full ThemeInstance
        let inst = load_theme_from_bytes(&packed, &Colorizer::default(), None, false).unwrap();
        assert_eq!(inst.palette.window_bg, Color::rgb(0x10, 0x20, 0x30));
        assert_eq!(inst.palette.window_bg_active, Color::rgb(0xff, 0x55, 0x00));
        assert!(inst.tiled);
        let loaded_bg = inst.background.as_ref().unwrap();
        assert_eq!(loaded_bg.dimensions(), (4, 4));
        assert_eq!(loaded_bg.get_pixel(0, 0).0, [0x20, 0x40, 0x60, 255]);

        // Load from cache matches
        let from_cache = load_from_cache(&packed, &inst.cached).unwrap();
        assert_eq!(from_cache.palette, inst.palette);
        assert!(from_cache.tiled);
        assert_eq!(
            from_cache.background.as_ref().unwrap().get_pixel(0, 0).0,
            [0x20, 0x40, 0x60, 255]
        );
    }

    #[test]
    fn editor_export_replace_and_cloud_header_round_trip() {
        let exported = generate_default_palette();
        let mut pal = Palette::new_uninitialized();
        load_color_scheme(&exported, &Colorizer::default(), &mut pal).unwrap();
        pal.finalize();
        assert_eq!(pal, Palette::default());

        let edited = replace_value_in_palette_content(&exported, "windowBg", "#121212").unwrap();
        let mut pal2 = Palette::new_uninitialized();
        load_color_scheme(&edited, &Colorizer::default(), &mut pal2).unwrap();
        pal2.finalize();
        assert_eq!(pal2.window_bg, Color::rgb(0x12, 0x12, 0x12));

        let cloud = CloudTheme {
            id: 987_654_321,
            access_hash: 111_222_333_444,
            ..CloudTheme::default()
        };
        let mut with_cloud = write_cloud_to_text(&cloud);
        with_cloud.extend_from_slice(&exported);
        let read_back = read_cloud_from_text(&with_cloud);
        assert_eq!(read_back.id, cloud.id);
        assert_eq!(read_back.access_hash, cloud.access_hash);
        assert_eq!(strip_cloud_text_fields(&with_cloud), exported);
    }

    #[test]
    fn web_view_params_and_theme_name_generation() {
        let p = Palette::default();
        let wv = web_view_params(&p);
        assert_eq!(wv.body_bg, Color::WHITE);
        assert!(wv.json.contains("\"bg_color\":\"#ffffff\""));
        assert!(wv.json.contains("\"button_color\":\"#40a7e3\""));
        assert!(wv.json.contains("\"section_separator_color\":\"#"));

        assert_eq!(
            nearest_theme_color_name(Color::rgb(0x54, 0xa5, 0xf8)),
            "Blue"
        );
        assert_eq!(
            nearest_theme_color_name(Color::rgb(0x50, 0xc8, 0x78)),
            "Emerald"
        );
        let name = generate_theme_name(Color::rgb(0x50, 0xc8, 0x78), 0);
        assert!(name.ends_with(" Emerald"));
        assert!(is_good_slug("my_theme_123"));
        assert!(!is_good_slug("no"));
    }
}
