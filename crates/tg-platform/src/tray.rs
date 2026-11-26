//! System tray icon, unread counter badge drawing, taskbar overlay icon, and window flashing.
//!
//! Port of `Telegram/SourceFiles/tray.cpp`,
//! `Telegram/SourceFiles/platform/win/tray_win.cpp`,
//! `Telegram/SourceFiles/window/main_window.cpp` (`GenerateCounterLayer`, `WithSmallCounter`,
//! `ConvertIconToBlack`), and `Telegram/SourceFiles/platform/win/main_window_win.cpp`
//! (`ITaskbarList3::SetOverlayIcon`, `FlashWindowEx`).

#[cfg(windows)]
use std::sync::Arc;

use crate::error::{PlatformError, Result};
use crate::system_info::RgbaColor;

/// Default badge colors taken from `resources/colors.palette`:
/// - `trayCounterBg: #f23c34`
/// - `trayCounterBgMute: #888888`
/// - `trayCounterFg: #ffffff`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayCounterColors {
    /// Background color for unmuted unread counter badge.
    pub bg: RgbaColor,
    /// Background color when all unread messages are muted.
    pub bg_muted: RgbaColor,
    /// Foreground text color inside the counter badge.
    pub fg: RgbaColor,
}

impl Default for TrayCounterColors {
    fn default() -> Self {
        Self {
            bg: RgbaColor::rgb(0xF2, 0x3C, 0x34),
            bg_muted: RgbaColor::rgb(0x88, 0x88, 0x88),
            fg: RgbaColor::rgb(0xFF, 0xFF, 0xFF),
        }
    }
}

/// Straight-alpha RGBA bitmap used for tray icons, counter badges, and taskbar overlays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Row-major `width * height * 4` RGBA bytes.
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// Create a fully transparent RGBA image.
    pub fn transparent(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; (width as usize) * (height as usize) * 4],
        }
    }

    /// Create an RGBA image from existing bytes after validating the buffer length.
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or_else(|| PlatformError::InvalidArgument("image dimensions overflow".into()))?;
        if pixels.len() != expected {
            return Err(PlatformError::InvalidArgument(format!(
                "expected {expected} RGBA bytes, got {}",
                pixels.len()
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Alpha-composite a single pixel `(r, g, b, a)` onto `(x, y)`.
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: RgbaColor) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 || color.a == 0 {
            return;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        let sa = f32::from(color.a) / 255.0;
        let da = f32::from(self.pixels[idx + 3]) / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.0 {
            return;
        }
        let blend = |s: u8, d: u8| -> u8 {
            let v = (f32::from(s) * sa + f32::from(d) * da * (1.0 - sa)) / out_a;
            v.round().clamp(0.0, 255.0) as u8
        };
        self.pixels[idx] = blend(color.r, self.pixels[idx]);
        self.pixels[idx + 1] = blend(color.g, self.pixels[idx + 1]);
        self.pixels[idx + 2] = blend(color.b, self.pixels[idx + 2]);
        self.pixels[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    /// Composite `overlay` onto `self` at `(dst_x, dst_y)`.
    pub fn blit(&mut self, overlay: &RgbaImage, dst_x: i32, dst_y: i32) {
        for oy in 0..overlay.height {
            for ox in 0..overlay.width {
                let idx = ((oy as usize) * (overlay.width as usize) + (ox as usize)) * 4;
                let c = RgbaColor::rgba(
                    overlay.pixels[idx],
                    overlay.pixels[idx + 1],
                    overlay.pixels[idx + 2],
                    overlay.pixels[idx + 3],
                );
                self.blend_pixel(dst_x + ox as i32, dst_y + oy as i32, c);
            }
        }
    }

    /// Encode this image as a 32-bit RGBA PNG byte buffer.
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| PlatformError::Image(e.to_string()))?;
            writer
                .write_image_data(&self.pixels)
                .map_err(|e| PlatformError::Image(e.to_string()))?;
        }
        Ok(buf)
    }
}

/// Format an unread count for full counter badges (`FormatUnreadCounterShort` in `unread_counter_format.cpp`).
pub fn format_unread_counter_short(count: u32) -> String {
    if count < 1_000 {
        count.to_string()
    } else if count < 1_000_000 {
        format!("{}K", count / 1_000)
    } else if count < 1_000_000_000 {
        format!("{}M", count / 1_000_000)
    } else {
        format!("{}B", count / 1_000_000_000)
    }
}

/// Format an unread count for small tray icons (`WithSmallCounter` in `window/main_window.cpp`):
/// `< 100` -> `"N"`, `>= 100` -> `"..D"` where `D = count % 10`.
pub fn format_small_counter(count: u32) -> String {
    if count < 100 {
        count.to_string()
    } else {
        format!("..{}", count % 10)
    }
}

/// Format the tray tooltip (`TrayIconToolTip` in `tray.cpp`).
pub fn format_tray_tooltip(app_name: &str, unread_count: u32) -> String {
    if unread_count > 0 {
        format!("{app_name} ({unread_count})")
    } else {
        app_name.to_string()
    }
}

/// Convert a colored icon to monochrome dark grayscale (`Window::ConvertIconToBlack` in `main_window.cpp`).
pub fn convert_icon_to_black(image: &mut RgbaImage) {
    const SCALE: f64 = 255.0 / 155.0;
    const THRESHOLD: f64 = 100.0;
    for px in image.pixels.as_chunks_mut::<4>().0 {
        let gray = 0.299 * f64::from(px[0]) + 0.587 * f64::from(px[1]) + 0.114 * f64::from(px[2]);
        let comp = if gray < THRESHOLD {
            0u8
        } else {
            ((gray - THRESHOLD) * SCALE).round().clamp(0.0, 255.0) as u8
        };
        px[0] = comp;
        px[1] = comp;
        px[2] = comp;
    }
}

/// Draw a filled anti-aliased rounded rectangle into `img`.
fn draw_rounded_rect(
    img: &mut RgbaImage,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    radius: f32,
    color: RgbaColor,
) {
    let r = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    let x0 = (left.floor() as i32 - 1).max(0);
    let y0 = (top.floor() as i32 - 1).max(0);
    let x1 = ((left + width).ceil() as i32 + 1).min(img.width as i32);
    let y1 = ((top + height).ceil() as i32 + 1).min(img.height as i32);

    let cx = left + width * 0.5;
    let cy = top + height * 0.5;
    let hx = width * 0.5 - r;
    let hy = height * 0.5 - r;

    for y in y0..y1 {
        for x in x0..x1 {
            let px = (x as f32 + 0.5 - cx).abs() - hx;
            let py = (y as f32 + 0.5 - cy).abs() - hy;
            let dist = (px.max(0.0).hypot(py.max(0.0)) + px.max(py).min(0.0)) - r;
            let coverage = (0.5 - dist).clamp(0.0, 1.0);
            if coverage > 0.0 {
                let a = (f32::from(color.a) * coverage).round().clamp(0.0, 255.0) as u8;
                img.blend_pixel(x, y, RgbaColor::rgba(color.r, color.g, color.b, a));
            }
        }
    }
}

/// Draw a filled anti-aliased circle into `img`.
fn draw_circle(img: &mut RgbaImage, cx: f32, cy: f32, radius: f32, color: RgbaColor) {
    draw_rounded_rect(
        img,
        cx - radius,
        cy - radius,
        radius * 2.0,
        radius * 2.0,
        radius,
        color,
    );
}

/// Clean 4x6 pixel bitmap glyphs for compact badge numbers (`0-9`, `.`, `K`, `M`, `B`).
fn glyph_4x6(ch: char) -> &'static [u8; 6] {
    match ch {
        '0' => &[0b0110, 0b1001, 0b1001, 0b1001, 0b1001, 0b0110],
        '1' => &[0b0010, 0b0110, 0b0010, 0b0010, 0b0010, 0b0111],
        '2' => &[0b0110, 0b1001, 0b0010, 0b0100, 0b1000, 0b1111],
        '3' => &[0b1110, 0b0001, 0b0110, 0b0001, 0b0001, 0b1110],
        '4' => &[0b1001, 0b1001, 0b1111, 0b0001, 0b0001, 0b0001],
        '5' => &[0b1111, 0b1000, 0b1110, 0b0001, 0b0001, 0b1110],
        '6' => &[0b0111, 0b1000, 0b1110, 0b1001, 0b1001, 0b0110],
        '7' => &[0b1111, 0b0001, 0b0010, 0b0100, 0b0100, 0b0100],
        '8' => &[0b0110, 0b1001, 0b0110, 0b1001, 0b1001, 0b0110],
        '9' => &[0b0110, 0b1001, 0b0111, 0b0001, 0b0010, 0b1100],
        'K' => &[0b1001, 0b1010, 0b1100, 0b1010, 0b1001, 0b1001],
        'M' => &[0b1001, 0b1111, 0b1111, 0b1001, 0b1001, 0b1001],
        'B' => &[0b1110, 0b1001, 0b1110, 0b1001, 0b1001, 0b1110],
        '.' => &[0b0000, 0b0000, 0b0000, 0b0000, 0b0110, 0b0110],
        _ => &[0b0000, 0b0000, 0b0000, 0b0000, 0b0000, 0b0000],
    }
}

fn draw_badge_text(
    img: &mut RgbaImage,
    text: &str,
    center_x: i32,
    center_y: i32,
    scale: i32,
    color: RgbaColor,
) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return;
    }
    let scale = scale.max(1);
    let glyph_w = 4 * scale;
    let glyph_h = 6 * scale;
    let spacing = scale;
    let total_w = (chars.len() as i32) * glyph_w + ((chars.len() as i32) - 1) * spacing;
    let start_x = center_x - total_w / 2;
    let start_y = center_y - glyph_h / 2;

    for (idx, ch) in chars.iter().enumerate() {
        let rows = glyph_4x6(*ch);
        let gx = start_x + (idx as i32) * (glyph_w + spacing);
        for (ry, &bits) in rows.iter().enumerate() {
            for rx in 0..4 {
                if (bits & (1 << (3 - rx))) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            img.blend_pixel(
                                gx + rx * scale + sx,
                                start_y + (ry as i32) * scale + sy,
                                color,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Render an original geometric Rustgram tray/app icon (rounded chat bubble with clean stripes;
/// does not use Telegram's trademarked paper-plane logo).
pub fn default_app_icon(size: u32, monochrome: bool, dark_taskbar: bool) -> RgbaImage {
    let mut img = RgbaImage::transparent(size, size);
    let s = size as f32;
    let bg = if monochrome {
        if dark_taskbar {
            RgbaColor::rgba(255, 255, 255, 255)
        } else {
            RgbaColor::rgba(0, 0, 0, 228)
        }
    } else {
        RgbaColor::rgb(0x2B, 0x8F, 0xD8)
    };
    let fg = if monochrome {
        if dark_taskbar {
            RgbaColor::rgba(0, 0, 0, 220)
        } else {
            RgbaColor::rgba(255, 255, 255, 255)
        }
    } else {
        RgbaColor::rgb(255, 255, 255)
    };

    // Main circular badge.
    draw_circle(&mut img, s * 0.5, s * 0.48, s * 0.44, bg);
    // Small speech tail at bottom-left.
    draw_rounded_rect(
        &mut img,
        s * 0.14,
        s * 0.68,
        s * 0.24,
        s * 0.24,
        s * 0.06,
        bg,
    );
    // Clean geometric chat lines inside the bubble.
    let bar_h = (s * 0.10).max(1.5);
    draw_rounded_rect(
        &mut img,
        s * 0.26,
        s * 0.34,
        s * 0.48,
        bar_h,
        bar_h * 0.5,
        fg,
    );
    draw_rounded_rect(
        &mut img,
        s * 0.26,
        s * 0.52,
        s * 0.34,
        bar_h,
        bar_h * 0.5,
        fg,
    );
    img
}

/// Generate a standalone unread counter overlay image (used for `ITaskbarList3::SetOverlayIcon`
/// and bottom-right corner badge compositing).
///
/// Mirrors `Window::GenerateCounterLayer` in `window/main_window.cpp`.
pub fn generate_counter_layer(
    size: u32,
    count: u32,
    muted: bool,
    colors: TrayCounterColors,
) -> RgbaImage {
    let dim = match size {
        16 => 16u32,
        20 => 20,
        24 => 24,
        _ => 32,
    };
    let mut img = RgbaImage::transparent(dim, dim);
    if count == 0 {
        return img;
    }

    let text = format_unread_counter_short(count);
    let len = text.len() as i32;
    let bg = if muted { colors.bg_muted } else { colors.bg };

    // Choose crisp integer glyph scale that fits inside `dim`.
    let mut scale = match dim {
        16 => 1,
        20 | 24 => 2,
        _ => 3,
    };
    while scale > 1 && (len * 4 * scale + (len - 1) * scale + 4) > dim as i32 {
        scale -= 1;
    }

    let text_w = len * 4 * scale + (len - 1) * scale;
    let text_h = 6 * scale;
    let pad_x = (scale * 2).max(2);
    let pad_y = (scale * 2).max(2);

    let pill_w = (text_w + pad_x * 2).min(dim as i32).max(text_h + pad_y * 2);
    let pill_h = (text_h + pad_y * 2).min(dim as i32);
    let left = (dim as i32 - pill_w) / 2;
    let top = (dim as i32 - pill_h) / 2;
    let radius = pill_h as f32 * 0.5;

    draw_rounded_rect(
        &mut img,
        left as f32,
        top as f32,
        pill_w as f32,
        pill_h as f32,
        radius,
        bg,
    );
    draw_badge_text(
        &mut img,
        &text,
        left + pill_w / 2,
        top + pill_h / 2,
        scale,
        colors.fg,
    );
    img
}

/// Draw the monochrome unread indicator dot at `(cx=3.9, cy=12.7, r=2.2)` scaled to `16x16`
/// (exact coordinates from `MonochromeWithDot` in `tray_win.cpp` lines 89-102).
fn monochrome_with_dot(mut image: RgbaImage, color: RgbaColor) -> RgbaImage {
    let xm = image.width as f32 / 16.0;
    let ym = image.height as f32 / 16.0;
    draw_circle(&mut image, 3.9 * xm, 12.7 * ym, 2.2 * xm.min(ym), color);
    image
}

/// Overlay a compact unread counter onto a small tray icon (`WithSmallCounter` in `main_window.cpp`).
fn with_small_counter(
    mut image: RgbaImage,
    count: u32,
    muted: bool,
    colors: TrayCounterColors,
) -> RgbaImage {
    let text = format_small_counter(count);
    let len = text.len() as i32;
    let bg = if muted { colors.bg_muted } else { colors.bg };
    let size = image.width as i32;
    let scale = if size >= 32 {
        if len <= 2 { 2 } else { 1 }
    } else {
        1
    };
    let text_w = len * 4 * scale + (len - 1) * scale;
    let text_h = 6 * scale;
    let pad_x = (size / 16).max(1);
    let pad_y = (size / 16).max(1);
    let pill_w = (text_w + pad_x * 2).min(size);
    let pill_h = (text_h + pad_y * 2).min(size);
    let left = size - pill_w;
    let top = size - pill_h;
    let radius = (pill_h as f32 * 0.45).max(2.0);

    draw_rounded_rect(
        &mut image,
        left as f32,
        top as f32,
        pill_w as f32,
        pill_h as f32,
        radius,
        bg,
    );
    draw_badge_text(
        &mut image,
        &text,
        left + pill_w / 2,
        top + pill_h / 2,
        scale,
        colors.fg,
    );
    image
}

/// Compose a base icon with an unread counter overlay (`ImageIconWithCounter` in `tray_win.cpp`).
pub fn compose_icon_with_counter(
    base: &RgbaImage,
    count: u32,
    muted: bool,
    small_icon: bool,
    monochrome: bool,
    dark_taskbar: Option<bool>,
    colors: TrayCounterColors,
) -> RgbaImage {
    let mut result = base.clone();
    if count == 0 {
        return result;
    }
    let bg = if muted { colors.bg_muted } else { colors.bg };
    if small_icon {
        if monochrome && dark_taskbar.is_some() {
            return monochrome_with_dot(result, bg);
        }
        return with_small_counter(result, count, muted, colors);
    }
    let half = (result.width / 2).max(16);
    let layer = generate_counter_layer(half, count, muted, colors);
    let x = result.width.saturating_sub(layer.width) as i32;
    let y = result.height.saturating_sub(layer.height) as i32;
    result.blit(&layer, x, y);
    result
}

/// Encode one or more RGBA images (up to 256x256 each) into a Windows `.ico` file
/// with PNG-compressed entries (`WriteIco` in `tray_win.cpp` lines 336-403).
pub fn encode_ico(images: &[RgbaImage]) -> Result<Vec<u8>> {
    if images.is_empty() {
        return Err(PlatformError::InvalidArgument(
            "encode_ico requires at least one image".into(),
        ));
    }
    let mut pngs = Vec::with_capacity(images.len());
    for img in images {
        if img.width == 0 || img.height == 0 || img.width > 256 || img.height > 256 {
            return Err(PlatformError::InvalidArgument(format!(
                "ICO image dimensions must be 1..=256, got {}x{}",
                img.width, img.height
            )));
        }
        pngs.push(img.encode_png()?);
    }

    let count = images.len() as u16;
    let header_size = 6usize + (images.len() * 16);
    let total_size = header_size + pngs.iter().map(Vec::len).sum::<usize>();
    let mut out = Vec::with_capacity(total_size);

    // ICONDIR header: reserved=0, type=1 (ICO), count
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());

    let mut offset = header_size as u32;
    for (img, png_bytes) in images.iter().zip(&pngs) {
        out.push(if img.width == 256 { 0 } else { img.width as u8 });
        out.push(if img.height == 256 {
            0
        } else {
            img.height as u8
        });
        out.push(0); // palette size
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits-per-pixel
        out.extend_from_slice(&(png_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png_bytes.len() as u32;
    }

    for png_bytes in pngs {
        out.extend_from_slice(&png_bytes);
    }
    Ok(out)
}

/// Entry inside a system tray context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayMenuItem {
    /// Clickable action item.
    Action {
        /// Application-defined command ID returned in [`TrayEvent::MenuItem`].
        id: u32,
        /// Menu item label text.
        label: String,
        /// Whether the item can be clicked.
        enabled: bool,
        /// Whether a checkmark is shown beside the item.
        checked: bool,
    },
    /// Horizontal separator line.
    Separator,
}

impl TrayMenuItem {
    /// Convenience constructor for an enabled, unchecked action item.
    pub fn action(id: u32, label: impl Into<String>) -> Self {
        Self::Action {
            id,
            label: label.into(),
            enabled: true,
            checked: false,
        }
    }
}

/// Event emitted by [`TrayIcon`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// User left-clicked the tray icon.
    LeftClick,
    /// User double-clicked the tray icon.
    DoubleClick,
    /// Context menu is about to open (right-click on tray icon).
    ContextMenuOpening,
    /// User selected a context menu item by its `id`.
    MenuItem(u32),
}

/// Initial configuration for creating a [`TrayIcon`].
#[derive(Debug, Clone)]
pub struct TrayConfig {
    /// Application name used in tooltips (e.g. `"Rustgram"`).
    pub app_name: String,
    /// Optional custom tooltip override; defaults to `format_tray_tooltip(&app_name, unread_count)`.
    pub tooltip: Option<String>,
    /// Optional custom base icon; defaults to [`default_app_icon`].
    pub base_icon: Option<RgbaImage>,
    /// Initial unread badge count (`0` hides the badge).
    pub unread_count: u32,
    /// Whether all unread chats are muted (renders gray badge instead of red).
    pub muted: bool,
    /// Whether monochrome taskbar-adaptive icon rendering is enabled.
    pub monochrome: bool,
    /// Badge colors.
    pub colors: TrayCounterColors,
    /// Context menu items.
    pub menu: Vec<TrayMenuItem>,
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            app_name: "Rustgram".to_string(),
            tooltip: None,
            base_icon: None,
            unread_count: 0,
            muted: false,
            monochrome: false,
            colors: TrayCounterColors::default(),
            menu: Vec::new(),
        }
    }
}

/// Native Windows system tray icon with context menu and live unread counter overlay.
///
/// Runs its own hidden Win32 message window on a dedicated background thread so it works
/// cleanly alongside any GUI event loop (`egui`/`eframe`, console demo, etc.).
pub struct TrayIcon {
    #[cfg(windows)]
    inner: win_tray::WinTrayIcon,
}

impl std::fmt::Debug for TrayIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrayIcon").finish()
    }
}

impl TrayIcon {
    /// Create and show a native tray icon.
    pub fn new<F>(config: TrayConfig, on_event: F) -> Result<Self>
    where
        F: Fn(TrayEvent) + Send + Sync + 'static,
    {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: win_tray::WinTrayIcon::new(config, Arc::new(on_event))?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (config, on_event);
            Ok(Self {})
        }
    }

    /// Update the unread badge count, muted state, and default tooltip.
    pub fn set_unread(&self, count: u32, muted: bool) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.set_unread(count, muted)
        }
        #[cfg(not(windows))]
        {
            let _ = (count, muted);
            Ok(())
        }
    }

    /// Toggle monochrome tray icon mode.
    pub fn set_monochrome(&self, monochrome: bool) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.set_monochrome(monochrome)
        }
        #[cfg(not(windows))]
        {
            let _ = monochrome;
            Ok(())
        }
    }

    /// Update the hover tooltip string (truncated to 127 UTF-16 code units on Windows).
    pub fn set_tooltip(&self, tooltip: impl Into<String>) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.set_tooltip(tooltip.into())
        }
        #[cfg(not(windows))]
        {
            let _ = tooltip;
            Ok(())
        }
    }

    /// Replace the tray context menu items.
    pub fn set_menu(&self, items: Vec<TrayMenuItem>) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.set_menu(items)
        }
        #[cfg(not(windows))]
        {
            let _ = items;
            Ok(())
        }
    }

    /// Replace the base tray icon image.
    pub fn set_base_icon(&self, icon: RgbaImage) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.set_base_icon(icon)
        }
        #[cfg(not(windows))]
        {
            let _ = icon;
            Ok(())
        }
    }

    /// Display a tray balloon notification (`showTrayMessage` in `tray_win.cpp`).
    pub fn show_balloon(&self, title: &str, body: &str) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.show_balloon(title, body)
        }
        #[cfg(not(windows))]
        {
            let _ = (title, body);
            Ok(())
        }
    }
}

/// Taskbar button progress indicator state (`TBPFLAG`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskbarProgressState {
    /// Hide progress bar.
    NoProgress,
    /// Marquee / indeterminate progress.
    Indeterminate,
    /// Green normal progress bar.
    Normal,
    /// Red error progress bar.
    Error,
    /// Yellow paused progress bar.
    Paused,
}

/// Wrapper around Windows `ITaskbarList3` for taskbar overlay badges (`SetOverlayIcon`)
/// and progress indicators.
pub struct TaskbarOverlay {
    #[cfg(windows)]
    inner: win_tray::WinTaskbarOverlay,
}

impl std::fmt::Debug for TaskbarOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskbarOverlay").finish()
    }
}

impl TaskbarOverlay {
    /// Initialize `ITaskbarList3` via COM.
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: win_tray::WinTaskbarOverlay::new()?,
            })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    /// Render and apply the unread counter badge overlay icon to taskbar button `hwnd`.
    ///
    /// Passing `count == 0` clears the overlay icon (matching `MainWindow::updateTaskbarAndIconCounters`).
    pub fn set_unread_badge(
        &mut self,
        hwnd: isize,
        count: u32,
        muted: bool,
        description: &str,
        colors: TrayCounterColors,
    ) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner
                .set_unread_badge(hwnd, count, muted, description, colors)
        }
        #[cfg(not(windows))]
        {
            let _ = (hwnd, count, muted, description, colors);
            Ok(())
        }
    }

    /// Clear any overlay icon currently displayed on taskbar button `hwnd`.
    pub fn clear_overlay(&mut self, hwnd: isize) -> Result<()> {
        self.set_unread_badge(hwnd, 0, false, "", TrayCounterColors::default())
    }

    /// Set the taskbar progress bar state on `hwnd`.
    pub fn set_progress_state(&self, hwnd: isize, state: TaskbarProgressState) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.set_progress_state(hwnd, state)
        }
        #[cfg(not(windows))]
        {
            let _ = (hwnd, state);
            Ok(())
        }
    }

    /// Set the taskbar progress bar value (`completed / total`) on `hwnd`.
    pub fn set_progress_value(&self, hwnd: isize, completed: u64, total: u64) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.set_progress_value(hwnd, completed, total)
        }
        #[cfg(not(windows))]
        {
            let _ = (hwnd, completed, total);
            Ok(())
        }
    }
}

/// Flash the taskbar button and caption of window `hwnd` (`FlashWindowEx`).
///
/// If `count == 0`, flashes continuously until the window comes to the foreground
/// (`FLASHW_ALL | FLASHW_TIMERNOFG`). Otherwise flashes `count` times.
pub fn flash_window(hwnd: isize, count: u32, timeout_ms: u32) -> Result<()> {
    #[cfg(windows)]
    {
        win_tray::flash_window(hwnd, count, timeout_ms)
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, count, timeout_ms);
        Ok(())
    }
}

/// Stop any active flashing on window `hwnd` (`FLASHW_STOP`).
pub fn stop_flash_window(hwnd: isize) -> Result<()> {
    #[cfg(windows)]
    {
        win_tray::stop_flash_window(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) fn rgba_to_hicon_for_platform(
    img: &RgbaImage,
) -> Result<windows::Win32::UI::WindowsAndMessaging::HICON> {
    win_tray::rgba_to_hicon(img)
}

#[cfg(windows)]
mod win_tray {
    use super::*;
    use crate::app_id::win_impl::{ComGuard, to_wide};
    use std::cell::Cell;
    use std::sync::Mutex;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject, HGDIOBJ,
    };
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        ITaskbarList3, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
        NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
        TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CREATESTRUCTW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW,
        DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, FLASHW_ALL,
        FLASHW_STOP, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GWLP_USERDATA, GetCursorPos,
        GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HICON, ICONINFO, MF_CHECKED, MF_GRAYED,
        MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, PostMessageW, PostQuitMessage,
        RegisterClassExW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow,
        SetWindowLongPtrW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, TrackPopupMenuEx, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
        WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NCCREATE,
        WM_NCDESTROY, WM_NULL, WM_RBUTTONUP, WNDCLASSEXW,
    };
    use windows::core::PCWSTR;

    const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
    const WM_TRAY_UPDATE: u32 = WM_APP + 2;
    const WM_TRAY_BALLOON: u32 = WM_APP + 3;
    const TRAY_ICON_UID: u32 = 1;

    /// Convert an [`RgbaImage`] into a Win32 32-bit alpha `HICON`.
    pub(crate) fn rgba_to_hicon(img: &RgbaImage) -> Result<HICON> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: img.width as i32,
                biHeight: -(img.height as i32), // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: Creating a top-down 32-bit BGRA DIB section.
        let hbm_color = unsafe { CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) }
            .map_err(|e| PlatformError::from_win("CreateDIBSection", e))?;

        if bits.is_null() {
            // SAFETY: Cleaning up bitmap on null pixel pointer.
            let _ = unsafe { DeleteObject(HGDIOBJ(hbm_color.0)) };
            return Err(PlatformError::Image(
                "CreateDIBSection returned null bits".into(),
            ));
        }

        // Convert straight RGBA -> BGRA (Windows 32-bit icon format).
        let byte_count = (img.width as usize) * (img.height as usize) * 4;
        // SAFETY: `bits` points to `byte_count` writable bytes allocated by CreateDIBSection.
        let dst = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), byte_count) };
        for (src_px, dst_px) in img
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<4>().0.iter_mut())
        {
            dst_px[0] = src_px[2]; // B
            dst_px[1] = src_px[1]; // G
            dst_px[2] = src_px[0]; // R
            dst_px[3] = src_px[3]; // A
        }

        // 1-bpp empty mask bitmap (alpha channel in `hbm_color` drives transparency).
        // SAFETY: Creating monochrome mask bitmap of the same dimensions.
        let hbm_mask = unsafe { CreateBitmap(img.width as i32, img.height as i32, 1, 1, None) };
        if hbm_mask.is_invalid() {
            let _ = unsafe { DeleteObject(HGDIOBJ(hbm_color.0)) };
            return Err(PlatformError::last_win32("CreateBitmap"));
        }

        let icon_info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        // SAFETY: CreateIconIndirect copies both bitmaps into the returned HICON.
        let hicon = unsafe { CreateIconIndirect(&icon_info) };
        // SAFETY: Always free the temporary GDI bitmaps after CreateIconIndirect.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(hbm_color.0));
            let _ = DeleteObject(HGDIOBJ(hbm_mask.0));
        }

        hicon.map_err(|e| PlatformError::from_win("CreateIconIndirect", e))
    }

    fn copy_wide_trunc(dst: &mut [u16], text: &str) {
        dst.fill(0);
        if dst.is_empty() {
            return;
        }
        let max = dst.len() - 1;
        for (slot, unit) in dst.iter_mut().take(max).zip(text.encode_utf16()) {
            *slot = unit;
        }
    }

    struct SharedTrayState {
        config: TrayConfig,
        pending_balloon: Option<(String, String)>,
    }

    struct WindowContext {
        shared: Arc<Mutex<SharedTrayState>>,
        on_event: Arc<dyn Fn(TrayEvent) + Send + Sync>,
        taskbar_created_msg: u32,
        current_hicon: Cell<HICON>,
    }

    impl WindowContext {
        fn build_current_icon(&self) -> Option<HICON> {
            let (base, count, muted, monochrome, colors) = {
                let guard = self.shared.lock().ok()?;
                let c = &guard.config;
                let sm = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
                let dark = crate::system_info::is_dark_taskbar();
                let base = c
                    .base_icon
                    .clone()
                    .unwrap_or_else(|| default_app_icon(sm, c.monochrome, dark.unwrap_or(true)));
                (base, c.unread_count, c.muted, c.monochrome, c.colors)
            };
            let dark = crate::system_info::is_dark_taskbar();
            let composed =
                compose_icon_with_counter(&base, count, muted, true, monochrome, dark, colors);
            let new_hicon = rgba_to_hicon(&composed).ok()?;
            let prev = self.current_hicon.replace(new_hicon);
            if !prev.is_invalid() {
                // SAFETY: Destroying our previous HICON after creating the replacement.
                let _ = unsafe { DestroyIcon(prev) };
            }
            Some(new_hicon)
        }

        fn build_nid(&self, hwnd: HWND) -> NOTIFYICONDATAW {
            let tooltip = self
                .shared
                .lock()
                .map(|g| {
                    g.config.tooltip.clone().unwrap_or_else(|| {
                        format_tray_tooltip(&g.config.app_name, g.config.unread_count)
                    })
                })
                .unwrap_or_else(|_| "Rustgram".to_string());

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ICON_UID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAY_CALLBACK,
                hIcon: self.current_hicon.get(),
                ..Default::default()
            };
            copy_wide_trunc(&mut nid.szTip, &tooltip);
            nid
        }

        fn add_icon(&self, hwnd: HWND) {
            let _ = self.build_current_icon();
            let mut nid = self.build_nid(hwnd);
            // SAFETY: Adding tray icon to shell notification area and setting version 4.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_ADD, &nid);
                nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
            }
        }

        fn modify_icon(&self, hwnd: HWND) {
            let _ = self.build_current_icon();
            let nid = self.build_nid(hwnd);
            // SAFETY: Updating tray icon and tooltip.
            let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
        }

        fn show_pending_balloon(&self, hwnd: HWND) {
            let balloon = self
                .shared
                .lock()
                .ok()
                .and_then(|mut g| g.pending_balloon.take());
            let Some((title, body)) = balloon else {
                return;
            };
            let mut nid = self.build_nid(hwnd);
            nid.uFlags |= NIF_INFO;
            nid.dwInfoFlags = NIIF_INFO;
            copy_wide_trunc(&mut nid.szInfoTitle, &title);
            copy_wide_trunc(&mut nid.szInfo, &body);
            // SAFETY: Displaying a tray balloon notification.
            let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
        }

        fn show_context_menu(&self, hwnd: HWND) {
            (self.on_event)(TrayEvent::ContextMenuOpening);
            let items = self
                .shared
                .lock()
                .map(|g| g.config.menu.clone())
                .unwrap_or_default();
            if items.is_empty() {
                return;
            }
            // SAFETY: Creating a Win32 popup menu populated from `items`.
            let Ok(hmenu) = (unsafe { CreatePopupMenu() }) else {
                return;
            };
            for item in &items {
                match item {
                    TrayMenuItem::Separator => {
                        // SAFETY: Appending separator to valid `hmenu`.
                        let _ = unsafe { AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()) };
                    }
                    TrayMenuItem::Action {
                        id,
                        label,
                        enabled,
                        checked,
                    } => {
                        let mut flags = MF_STRING;
                        if !*enabled {
                            flags |= MF_GRAYED;
                        }
                        flags |= if *checked { MF_CHECKED } else { MF_UNCHECKED };
                        let label_w = to_wide(label);
                        // SAFETY: Appending menu action item with non-zero command ID `id + 1`.
                        let _ = unsafe {
                            AppendMenuW(hmenu, flags, (*id as usize) + 1, PCWSTR(label_w.as_ptr()))
                        };
                    }
                }
            }

            let mut pt = POINT::default();
            // SAFETY: Querying cursor position and showing popup menu with standard Win32 pattern.
            unsafe {
                let _ = GetCursorPos(&mut pt);
                let _ = SetForegroundWindow(hwnd);
                let cmd = TrackPopupMenuEx(
                    hmenu,
                    (TPM_RIGHTBUTTON
                        | TPM_RETURNCMD
                        | TPM_NONOTIFY
                        | TPM_LEFTALIGN
                        | TPM_BOTTOMALIGN)
                        .0,
                    pt.x,
                    pt.y,
                    hwnd,
                    None,
                );
                let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                let _ = DestroyMenu(hmenu);
                if cmd.0 > 0 {
                    let chosen_id = (cmd.0 as u32) - 1;
                    (self.on_event)(TrayEvent::MenuItem(chosen_id));
                }
            }
        }
    }

    impl Drop for WindowContext {
        fn drop(&mut self) {
            let hicon = self.current_hicon.replace(HICON::default());
            if !hicon.is_invalid() {
                // SAFETY: Destroying our owned HICON on thread exit.
                let _ = unsafe { DestroyIcon(hicon) };
            }
        }
    }

    unsafe extern "system" fn tray_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_NCCREATE {
            // SAFETY: `lparam` points to CREATESTRUCTW during WM_NCCREATE.
            let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }

        if msg == WM_NCDESTROY {
            // SAFETY: Clear pointer before the window is fully torn down so no subsequent message
            // can dereference `WindowContext` after the message loop reclaims it.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }

        // SAFETY: Reading `WindowContext` pointer stored during WM_NCCREATE. Using a shared
        // reference (`&WindowContext`) avoids mutable aliasing UB during re-entrant Win32 calls
        // such as `Shell_NotifyIconW`, `TrackPopupMenuEx`, and `DestroyWindow`.
        let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowContext;
        if ptr.is_null() {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        let ctx = unsafe { &*ptr };

        if msg != 0 && msg == ctx.taskbar_created_msg {
            ctx.add_icon(hwnd);
            return LRESULT(0);
        }

        match msg {
            WM_TRAY_CALLBACK => {
                // Under NOTIFYICON_VERSION_4, the event ID is in LOWORD(lparam).
                let event = (lparam.0 & 0xFFFF) as u32;
                match event {
                    WM_LBUTTONUP => (ctx.on_event)(TrayEvent::LeftClick),
                    WM_LBUTTONDBLCLK => (ctx.on_event)(TrayEvent::DoubleClick),
                    WM_RBUTTONUP | WM_CONTEXTMENU => ctx.show_context_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TRAY_UPDATE => {
                ctx.modify_icon(hwnd);
                LRESULT(0)
            }
            WM_TRAY_BALLOON => {
                ctx.show_pending_balloon(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                let nid = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: TRAY_ICON_UID,
                    ..Default::default()
                };
                // SAFETY: Removing tray icon before destroying hidden window.
                unsafe {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: Exiting tray thread message loop.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    pub(crate) struct WinTrayIcon {
        shared: Arc<Mutex<SharedTrayState>>,
        hwnd_raw: isize,
        thread: Option<JoinHandle<()>>,
    }

    impl WinTrayIcon {
        pub(crate) fn new(
            config: TrayConfig,
            on_event: Arc<dyn Fn(TrayEvent) + Send + Sync>,
        ) -> Result<Self> {
            let shared = Arc::new(Mutex::new(SharedTrayState {
                config,
                pending_balloon: None,
            }));
            let shared_thread = Arc::clone(&shared);
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<isize>>(1);

            let thread = std::thread::Builder::new()
                .name("tg-tray-icon".into())
                .spawn(move || {
                    let _com = ComGuard::init_sta();
                    let class_name = to_wide("RustgramTrayIconWindow");
                    let taskbar_msg_w = to_wide("TaskbarCreated");
                    // SAFETY: Registering window class and TaskbarCreated broadcast message.
                    let taskbar_created_msg =
                        unsafe { RegisterWindowMessageW(PCWSTR(taskbar_msg_w.as_ptr())) };
                    let hinst: HMODULE = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
                    let wc = WNDCLASSEXW {
                        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                        lpfnWndProc: Some(tray_wnd_proc),
                        hInstance: hinst.into(),
                        lpszClassName: PCWSTR(class_name.as_ptr()),
                        ..Default::default()
                    };
                    unsafe { RegisterClassExW(&wc) };

                    let ctx = Box::new(WindowContext {
                        shared: shared_thread,
                        on_event,
                        taskbar_created_msg,
                        current_hicon: Cell::new(HICON::default()),
                    });
                    let ctx_ptr = Box::into_raw(ctx);

                    // SAFETY: Creating hidden top-level window (required to receive TaskbarCreated broadcasts).
                    let hwnd = match unsafe {
                        CreateWindowExW(
                            WINDOW_EX_STYLE(0),
                            PCWSTR(class_name.as_ptr()),
                            PCWSTR::null(),
                            WINDOW_STYLE(0),
                            0,
                            0,
                            0,
                            0,
                            None,
                            None,
                            Some(hinst.into()),
                            Some(ctx_ptr.cast()),
                        )
                    } {
                        Ok(h) if !h.0.is_null() => h,
                        Err(e) => {
                            let _ = unsafe { Box::from_raw(ctx_ptr) };
                            let _ = tx.send(Err(PlatformError::from_win("CreateWindowExW", e)));
                            return;
                        }
                        Ok(_) => {
                            let _ = unsafe { Box::from_raw(ctx_ptr) };
                            let _ = tx.send(Err(PlatformError::last_win32("CreateWindowExW")));
                            return;
                        }
                    };

                    // SAFETY: `ctx_ptr` is owned by this thread until after the message loop ends;
                    // shared borrow avoids aliasing UB if `Shell_NotifyIconW` re-enters `tray_wnd_proc`.
                    unsafe { (&*ctx_ptr).add_icon(hwnd) };
                    let _ = tx.send(Ok(hwnd.0 as isize));

                    let mut msg = MSG::default();
                    // SAFETY: Standard Win32 message loop for tray callbacks (`> 0` exits on both
                    // `WM_QUIT` (`0`) and error (`-1`)).
                    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
                        unsafe {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }

                    // SAFETY: Window is destroyed; reclaim context and destroy owned HICON.
                    let _ = unsafe { Box::from_raw(ctx_ptr) };
                })?;

            let hwnd_raw = rx.recv().map_err(|_| PlatformError::Win32 {
                context: "TrayIcon",
                code: 0,
                message: "tray thread exited prematurely".into(),
            })??;

            Ok(Self {
                shared,
                hwnd_raw,
                thread: Some(thread),
            })
        }

        fn notify_update(&self, msg: u32) -> Result<()> {
            // SAFETY: Posting custom update message to our tray window thread.
            unsafe {
                PostMessageW(
                    Some(HWND(self.hwnd_raw as *mut _)),
                    msg,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .map_err(|e| PlatformError::from_win("PostMessageW(tray)", e))
        }

        // The app sets the unread state and tooltip every frame; the tray thread rebuilds the
        // icon only when one of them changed.
        pub(crate) fn set_unread(&self, count: u32, muted: bool) -> Result<()> {
            if let Ok(mut g) = self.shared.lock() {
                if g.config.unread_count == count && g.config.muted == muted {
                    return Ok(());
                }
                g.config.unread_count = count;
                g.config.muted = muted;
            }
            self.notify_update(WM_TRAY_UPDATE)
        }

        pub(crate) fn set_monochrome(&self, monochrome: bool) -> Result<()> {
            if let Ok(mut g) = self.shared.lock() {
                if g.config.monochrome == monochrome {
                    return Ok(());
                }
                g.config.monochrome = monochrome;
            }
            self.notify_update(WM_TRAY_UPDATE)
        }

        pub(crate) fn set_tooltip(&self, tooltip: String) -> Result<()> {
            if let Ok(mut g) = self.shared.lock() {
                if g.config.tooltip.as_deref() == Some(tooltip.as_str()) {
                    return Ok(());
                }
                g.config.tooltip = Some(tooltip);
            }
            self.notify_update(WM_TRAY_UPDATE)
        }

        pub(crate) fn set_menu(&self, items: Vec<TrayMenuItem>) -> Result<()> {
            if let Ok(mut g) = self.shared.lock() {
                g.config.menu = items;
            }
            Ok(())
        }

        pub(crate) fn set_base_icon(&self, icon: RgbaImage) -> Result<()> {
            if let Ok(mut g) = self.shared.lock() {
                g.config.base_icon = Some(icon);
            }
            self.notify_update(WM_TRAY_UPDATE)
        }

        pub(crate) fn show_balloon(&self, title: &str, body: &str) -> Result<()> {
            if let Ok(mut g) = self.shared.lock() {
                g.pending_balloon = Some((title.to_string(), body.to_string()));
            }
            self.notify_update(WM_TRAY_BALLOON)
        }
    }

    impl Drop for WinTrayIcon {
        fn drop(&mut self) {
            if self.hwnd_raw != 0 {
                // SAFETY: Signalling tray window to remove its icon and exit its message loop.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(self.hwnd_raw as *mut _)),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    pub(crate) struct WinTaskbarOverlay {
        taskbar: ITaskbarList3,
        last_overlay: HICON,
        // IMPORTANT: Must remain the last field so `taskbar` (COM interface) is released BEFORE
        // `ComGuard::drop` calls `CoUninitialize()`. Struct fields drop in declaration order.
        _com: ComGuard,
    }

    impl WinTaskbarOverlay {
        pub(crate) fn new() -> Result<Self> {
            let com = ComGuard::init_sta();
            // SAFETY: Creating standard Windows Shell TaskbarList COM object and calling HrInit.
            let taskbar: ITaskbarList3 = unsafe {
                CoCreateInstance(&TaskbarList, None, CLSCTX_ALL)
                    .map_err(|e| PlatformError::from_win("CoCreateInstance(TaskbarList)", e))?
            };
            // SAFETY: Required initialization before calling ITaskbarList3 methods.
            // Returns E_NOTIMPL (0x80004001) when no Explorer taskbar is running (e.g. headless CI).
            unsafe { taskbar.HrInit() }.map_err(|e| {
                if e.code() == windows::Win32::Foundation::E_NOTIMPL {
                    PlatformError::Unsupported
                } else {
                    PlatformError::from_win("ITaskbarList3::HrInit", e)
                }
            })?;

            Ok(Self {
                taskbar,
                last_overlay: HICON::default(),
                _com: com,
            })
        }

        pub(crate) fn set_unread_badge(
            &mut self,
            hwnd: isize,
            count: u32,
            muted: bool,
            description: &str,
            colors: TrayCounterColors,
        ) -> Result<()> {
            let new_icon = if count > 0 {
                let sm = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
                let badge = generate_counter_layer(sm, count, muted, colors);
                rgba_to_hicon(&badge)?
            } else {
                HICON::default()
            };

            let desc_w = to_wide(description);
            // SAFETY: Setting overlay icon on taskbar button for `hwnd`.
            let res = unsafe {
                self.taskbar
                    .SetOverlayIcon(HWND(hwnd as *mut _), new_icon, PCWSTR(desc_w.as_ptr()))
            };

            if !self.last_overlay.is_invalid() {
                // SAFETY: Destroying previously owned overlay HICON.
                let _ = unsafe { DestroyIcon(self.last_overlay) };
            }
            self.last_overlay = new_icon;

            res.map_err(|e| PlatformError::from_win("ITaskbarList3::SetOverlayIcon", e))
        }

        pub(crate) fn set_progress_state(
            &self,
            hwnd: isize,
            state: TaskbarProgressState,
        ) -> Result<()> {
            let flag = match state {
                TaskbarProgressState::NoProgress => TBPF_NOPROGRESS,
                TaskbarProgressState::Indeterminate => TBPF_INDETERMINATE,
                TaskbarProgressState::Normal => TBPF_NORMAL,
                TaskbarProgressState::Error => TBPF_ERROR,
                TaskbarProgressState::Paused => TBPF_PAUSED,
            };
            // SAFETY: Setting taskbar button progress state.
            unsafe { self.taskbar.SetProgressState(HWND(hwnd as *mut _), flag) }
                .map_err(|e| PlatformError::from_win("ITaskbarList3::SetProgressState", e))
        }

        pub(crate) fn set_progress_value(
            &self,
            hwnd: isize,
            completed: u64,
            total: u64,
        ) -> Result<()> {
            // SAFETY: Setting taskbar button progress fraction.
            unsafe {
                self.taskbar
                    .SetProgressValue(HWND(hwnd as *mut _), completed, total)
            }
            .map_err(|e| PlatformError::from_win("ITaskbarList3::SetProgressValue", e))
        }
    }

    impl Drop for WinTaskbarOverlay {
        fn drop(&mut self) {
            if !self.last_overlay.is_invalid() {
                // SAFETY: Releasing owned overlay HICON.
                let _ = unsafe { DestroyIcon(self.last_overlay) };
            }
        }
    }

    pub(crate) fn flash_window(hwnd: isize, count: u32, timeout_ms: u32) -> Result<()> {
        let flags = if count == 0 {
            FLASHW_ALL | FLASHW_TIMERNOFG
        } else {
            FLASHW_ALL
        };
        let info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: HWND(hwnd as *mut _),
            dwFlags: flags,
            uCount: count,
            dwTimeout: timeout_ms,
        };
        // SAFETY: `info.cbSize` is initialized accurately for FlashWindowEx.
        let _ = unsafe { FlashWindowEx(&info) };
        Ok(())
    }

    pub(crate) fn stop_flash_window(hwnd: isize) -> Result<()> {
        let info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: HWND(hwnd as *mut _),
            dwFlags: FLASHW_STOP,
            uCount: 0,
            dwTimeout: 0,
        };
        // SAFETY: Stopping any active flash on `hwnd`.
        let _ = unsafe { FlashWindowEx(&info) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_counter_formatting_matches_tdesktop() {
        assert_eq!(format_unread_counter_short(1), "1");
        assert_eq!(format_unread_counter_short(999), "999");
        assert_eq!(format_unread_counter_short(1_000), "1K");
        assert_eq!(format_unread_counter_short(25_400), "25K");
        assert_eq!(format_unread_counter_short(1_500_000), "1M");
        assert_eq!(format_unread_counter_short(2_000_000_000), "2B");

        assert_eq!(format_small_counter(7), "7");
        assert_eq!(format_small_counter(99), "99");
        assert_eq!(format_small_counter(100), "..0");
        assert_eq!(format_small_counter(237), "..7");

        assert_eq!(format_tray_tooltip("Rustgram", 0), "Rustgram");
        assert_eq!(format_tray_tooltip("Rustgram", 4), "Rustgram (4)");
    }

    #[test]
    fn counter_badge_and_ico_encoding() {
        let colors = TrayCounterColors::default();
        let layer16 = generate_counter_layer(16, 5, false, colors);
        assert_eq!(layer16.width, 16);
        assert_eq!(layer16.height, 16);
        // Must contain non-transparent red badge pixels and white text pixels.
        let has_red = layer16
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 200 && p[1] < 100 && p[3] > 200);
        let has_white = layer16
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 240 && p[1] > 240 && p[2] > 240 && p[3] > 240);
        assert!(has_red);
        assert!(has_white);

        let muted = generate_counter_layer(16, 5, true, colors);
        let has_gray = muted
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] == 0x88 && p[1] == 0x88 && p[2] == 0x88 && p[3] == 255);
        assert!(has_gray);

        let base = default_app_icon(32, false, true);
        let composed = compose_icon_with_counter(&base, 12, false, true, false, Some(true), colors);
        assert_ne!(base.pixels, composed.pixels);

        let ico = encode_ico(&[
            default_app_icon(16, false, true),
            default_app_icon(32, false, true),
        ])
        .unwrap();
        // ICO header: 00 00 01 00 02 00
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 2, 0]);
        // First PNG signature starts at offset 6 + 2 * 16 = 38.
        assert_eq!(&ico[38..46], b"\x89PNG\r\n\x1a\n");
    }

    #[cfg(windows)]
    #[test]
    fn tray_icon_and_taskbar_overlay_lifecycle() {
        let tray = TrayIcon::new(
            TrayConfig {
                app_name: "Rustgram Test".into(),
                unread_count: 3,
                menu: vec![TrayMenuItem::action(1, "Quit")],
                ..Default::default()
            },
            |_ev| {},
        )
        .unwrap();
        tray.set_unread(12, true).unwrap();
        tray.set_tooltip("Rustgram Test (12)").unwrap();
        drop(tray);

        // Initializing ITaskbarList3 succeeds in an interactive desktop session and returns
        // PlatformError::Unsupported when no Explorer taskbar shell is running (e.g. headless CI).
        match TaskbarOverlay::new() {
            Ok(_) | Err(PlatformError::Unsupported) => {}
            Err(e) => panic!("unexpected TaskbarOverlay::new error: {e}"),
        }
    }
}
