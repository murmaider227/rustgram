//! Theme preview image generator for theme files and theme lists.
//!
//! Port of `Telegram/SourceFiles/window/themes/window_theme_preview.h` and
//! `window_theme_preview.cpp`.
//!
//! Generates crisp software `RgbaImage` previews of a [`ThemeInstance`] (both full desktop
//! mockups with dialogs list + chat history + bubbles + waveform + reply + compose bar, and
//! compact theme cards for horizontal theme strips) without relying on any proprietary assets.

use std::collections::HashMap;

use egui::{Context, TextureHandle, TextureOptions};
use image::{Rgba, RgbaImage};
use tg_theme::svg_pattern::{PatternTileMode, render_tiled_pattern_wallpaper};
use tg_theme::{Color, Palette, ThemeInstance, generate_background_image};

use crate::themes::chat_background::{
    BackgroundConfig, PatternKind, generate_pattern_mask, rgba_to_color_image,
};

/// Preview output format matching `Window::Theme::PreviewType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PreviewType {
    /// Standard preview window contents (`640x480` logical aspect).
    #[default]
    Normal,
    /// Extended preview with outer window frame, title bar, and soft drop shadow.
    Extended,
}

/// Extracts up to 2 uppercase initials from a contact name (`FillLetters` in `window_theme_preview.cpp`).
#[must_use]
pub fn fill_initials(name: &str) -> String {
    let mut letters: Vec<(char, u8)> = Vec::new();
    let mut level = 0u8;
    let mut letter_found = false;

    for ch in name.chars() {
        if !letter_found && ch.is_alphanumeric() {
            letter_found = true;
            letters.push((ch, level));
        } else if ch == ' ' {
            level = 0;
            letter_found = false;
        } else if letter_found && ch == '-' {
            level = 1;
            letter_found = false;
        }
    }

    let mut result = String::new();
    if let Some(&(first, _)) = letters.first() {
        result.extend(first.to_uppercase());
        let mut best_index = 0usize;
        let mut best_level = 2u8;
        for i in (1..letters.len()).rev() {
            if letters[i].1 < best_level {
                best_index = i;
                best_level = letters[i].1;
            }
        }
        if best_index > 0 {
            result.extend(letters[best_index].0.to_uppercase());
        }
    }
    result
}

/// Generates a full Telegram Desktop theme preview image (`Window::Theme::GeneratePreview`)
/// from a [`ThemeInstance`].
///
/// Renders the classic tdesktop preview scene:
/// - Optional extended window chrome and title bar (`PreviewType::Extended`)
/// - Left dialogs column with search field, active pinned chat ("Eva Summer"), unread badges
///   (accent & muted), and colored peer userpics (`historyPeer1..8UserpicBg`)
/// - Right chat pane with top bar, theme background (or generated default gradient + pattern),
///   photo bubble placeholder, outgoing voice waveform bubble, date service pill, incoming &
///   outgoing text bubbles with reply accent bar, and compose bar.
#[must_use]
pub fn generate_theme_preview(
    theme: &ThemeInstance,
    preview_type: PreviewType,
    preview_bg_override: Option<Color>,
) -> RgbaImage {
    let palette = &theme.palette;
    let (total_w, total_h, margin, title_h) = match preview_type {
        PreviewType::Normal => (560u32, 390u32, 0i32, 0i32),
        PreviewType::Extended => (600u32, 446u32, 20i32, 26i32),
    };

    let outer_bg = preview_bg_override
        .or_else(|| palette.color_by_name("themePreviewBg"))
        .unwrap_or(palette.window_bg);
    let mut canvas = RgbaImage::from_pixel(total_w, total_h, to_rgba(outer_bg));

    let inner_x = margin;
    let inner_y = margin;
    let inner_w = total_w as i32 - margin * 2;
    let inner_h = total_h as i32 - margin * 2;

    // Extended window drop shadow + titlebar
    if preview_type == PreviewType::Extended {
        fill_rounded_rect(
            &mut canvas,
            inner_x - 3,
            inner_y + 2,
            inner_w + 6,
            inner_h + 6,
            10,
            Color::rgba(0, 0, 0, 42),
        );
        fill_rect(
            &mut canvas,
            inner_x,
            inner_y,
            inner_w,
            title_h,
            palette.title_bg_active,
        );
        // Window control dots / title bar accent
        let ctrl_y = inner_y + title_h / 2;
        fill_circle(
            &mut canvas,
            inner_x + inner_w - 16,
            ctrl_y,
            4,
            palette.title_button_close_fg,
        );
        fill_circle(
            &mut canvas,
            inner_x + inner_w - 32,
            ctrl_y,
            4,
            palette.title_button_fg,
        );
        fill_circle(
            &mut canvas,
            inner_x + inner_w - 48,
            ctrl_y,
            4,
            palette.title_button_fg,
        );
        fill_rect(
            &mut canvas,
            inner_x + 12,
            ctrl_y - 3,
            64,
            6,
            palette.title_fg_active,
        );
    }

    let body_x = inner_x;
    let body_y = inner_y + title_h;
    let body_w = inner_w;
    let body_h = inner_h - title_h;

    let dialogs_w = (body_w * 36) / 100;
    let history_x = body_x + dialogs_w;
    let history_w = body_w - dialogs_w;
    let top_bar_h = 44;
    let compose_h = 40;
    let chat_y = body_y + top_bar_h;
    let chat_h = body_h - top_bar_h - compose_h;

    // 1. History background
    paint_preview_background(&mut canvas, history_x, chat_y, history_w, chat_h, theme);

    // 2. Chat bubbles inside history area
    paint_preview_bubbles(&mut canvas, history_x, chat_y, history_w, chat_h, palette);

    // 3. Top bar
    fill_rect(
        &mut canvas,
        history_x,
        body_y,
        history_w,
        top_bar_h,
        palette.top_bar_bg,
    );
    fill_rounded_rect(
        &mut canvas,
        history_x + 14,
        body_y + 10,
        96,
        9,
        4,
        palette.window_bold_fg,
    );
    fill_rounded_rect(
        &mut canvas,
        history_x + 14,
        body_y + 25,
        46,
        6,
        3,
        palette.window_active_text_fg,
    );
    // Top bar action icons
    for idx in 0..3 {
        fill_circle(
            &mut canvas,
            history_x + history_w - 18 - idx * 24,
            body_y + top_bar_h / 2,
            5,
            palette.menu_icon_fg,
        );
    }
    fill_rect(
        &mut canvas,
        history_x,
        body_y + top_bar_h - 1,
        history_w,
        1,
        palette.shadow_fg,
    );

    // 4. Compose area
    let compose_y = body_y + body_h - compose_h;
    fill_rect(
        &mut canvas,
        history_x,
        compose_y,
        history_w,
        compose_h,
        palette.history_compose_area_bg,
    );
    fill_rect(
        &mut canvas,
        history_x,
        compose_y,
        history_w,
        1,
        palette.shadow_fg,
    );
    // Attach icon, input placeholder bar, emoji & send icons
    fill_circle(
        &mut canvas,
        history_x + 20,
        compose_y + compose_h / 2,
        6,
        palette.history_compose_icon_fg,
    );
    fill_rounded_rect(
        &mut canvas,
        history_x + 36,
        compose_y + compose_h / 2 - 4,
        history_w - 102,
        8,
        4,
        palette.placeholder_fg,
    );
    fill_circle(
        &mut canvas,
        history_x + history_w - 44,
        compose_y + compose_h / 2,
        6,
        palette.history_compose_icon_fg,
    );
    fill_circle(
        &mut canvas,
        history_x + history_w - 20,
        compose_y + compose_h / 2,
        7,
        palette.history_send_icon_fg,
    );

    // 5. Left dialogs list
    fill_rect(
        &mut canvas,
        body_x,
        body_y,
        dialogs_w,
        body_h,
        palette.dialogs_bg,
    );
    // Hamburger + Search input
    fill_rect(
        &mut canvas,
        body_x + 14,
        body_y + 15,
        14,
        2,
        palette.dialogs_menu_icon_fg,
    );
    fill_rect(
        &mut canvas,
        body_x + 14,
        body_y + 20,
        14,
        2,
        palette.dialogs_menu_icon_fg,
    );
    fill_rect(
        &mut canvas,
        body_x + 14,
        body_y + 25,
        14,
        2,
        palette.dialogs_menu_icon_fg,
    );
    fill_rounded_rect(
        &mut canvas,
        body_x + 38,
        body_y + 10,
        dialogs_w - 48,
        24,
        12,
        palette.filter_input_inactive_bg,
    );

    let peer_colors = [
        palette.history_peer1_userpic_bg,
        palette.history_peer8_userpic_bg,
        palette.history_peer3_userpic_bg,
        palette.history_peer2_userpic_bg,
        palette.history_peer7_userpic_bg,
        palette.history_peer4_userpic_bg,
        palette.history_peer5_userpic_bg,
    ];
    let row_h = 46;
    let rows_start_y = body_y + top_bar_h;
    let row_specs: &[(bool, u8, bool, i32, i32)] = &[
        // (active, unread_count, muted, name_w, text_w)
        (true, 0, false, 72, 102),
        (false, 2, false, 84, 88),
        (false, 2, true, 64, 72),
        (false, 0, false, 78, 92),
        (false, 0, false, 68, 96),
        (false, 0, false, 74, 82),
        (false, 0, false, 80, 76),
    ];

    for (idx, &(active, unread, muted, name_w, text_w)) in row_specs.iter().enumerate() {
        let ry = rows_start_y + (idx as i32) * row_h;
        if ry + row_h > body_y + body_h {
            break;
        }
        if active {
            fill_rect(
                &mut canvas,
                body_x,
                ry,
                dialogs_w,
                row_h,
                palette.dialogs_bg_active,
            );
        }
        let avatar_cx = body_x + 26;
        let avatar_cy = ry + row_h / 2;
        fill_circle(
            &mut canvas,
            avatar_cx,
            avatar_cy,
            16,
            peer_colors[idx % peer_colors.len()],
        );
        let text_x = body_x + 50;
        let max_line_w = (dialogs_w - 64).max(20);
        let name_col = if active {
            palette.dialogs_name_fg_active
        } else {
            palette.dialogs_name_fg
        };
        let msg_col = if active {
            palette.dialogs_text_fg_active
        } else if idx == 2 || idx == 3 {
            palette.dialogs_text_fg_service
        } else {
            palette.dialogs_text_fg
        };
        let date_col = if active {
            palette.dialogs_date_fg_active
        } else {
            palette.dialogs_date_fg
        };
        fill_rounded_rect(
            &mut canvas,
            text_x,
            ry + 11,
            name_w.min(max_line_w - 28),
            8,
            4,
            name_col,
        );
        fill_rounded_rect(
            &mut canvas,
            body_x + dialogs_w - 30,
            ry + 12,
            20,
            6,
            3,
            date_col,
        );
        fill_rounded_rect(
            &mut canvas,
            text_x,
            ry + 27,
            text_w.min(max_line_w - 24),
            6,
            3,
            msg_col,
        );
        if unread > 0 {
            let badge_bg = if muted {
                palette.dialogs_unread_bg_muted
            } else {
                palette.dialogs_unread_bg
            };
            fill_circle(&mut canvas, body_x + dialogs_w - 18, ry + 30, 7, badge_bg);
        }
    }

    // Vertical column divider between dialogs and history
    fill_rect(&mut canvas, history_x, body_y, 1, body_h, palette.shadow_fg);

    canvas
}

/// Generates a compact theme card preview (`120x86`) suitable for horizontal theme strips
/// (port of `Settings::ThemePreview` / `window_themes_cloud_list.cpp` thumbnails).
#[must_use]
pub fn generate_compact_theme_card(theme: &ThemeInstance, width: u32, height: u32) -> RgbaImage {
    let w = width.max(48);
    let h = height.max(36);
    let palette = &theme.palette;
    let mut img = if let Some(ref bg) = theme.background {
        image::imageops::resize(bg, w, h, image::imageops::FilterType::Triangle)
    } else if palette.is_dark_palette() {
        generate_background_image(
            w,
            h,
            &[
                palette.window_bg,
                palette.dialogs_bg_over,
                palette.msg_in_bg,
            ],
            45,
            0.0,
            None,
        )
    } else {
        generate_background_image(
            w,
            h,
            &[
                Color::rgb(0xDB, 0xDD, 0xBB),
                Color::rgb(0x6B, 0xA5, 0x87),
                Color::rgb(0xD5, 0xD8, 0x8D),
            ],
            45,
            0.0,
            None,
        )
    };

    let iw = w as i32;
    let ih = h as i32;
    // Incoming bubble (top-left)
    let in_w = (iw * 58) / 100;
    let bubble_h = (ih * 24) / 100;
    fill_rounded_rect(&mut img, 10, 12, in_w, bubble_h, 6, palette.msg_in_bg);
    fill_rounded_rect(
        &mut img,
        16,
        12 + bubble_h / 2 - 2,
        (in_w - 16).max(8),
        4,
        2,
        palette.history_text_in_fg,
    );

    // Outgoing bubble (middle-right)
    let out_w = (iw * 60) / 100;
    let out_y = 12 + bubble_h + 7;
    fill_rounded_rect(
        &mut img,
        iw - 10 - out_w,
        out_y,
        out_w,
        bubble_h,
        6,
        palette.msg_out_bg,
    );
    fill_rounded_rect(
        &mut img,
        iw - 10 - out_w + 6,
        out_y + bubble_h / 2 - 2,
        (out_w - 16).max(8),
        4,
        2,
        palette.history_text_out_fg,
    );

    // Bottom bar swatch
    let bar_h = (ih * 20) / 100;
    fill_rect(&mut img, 0, ih - bar_h, iw, bar_h, palette.window_bg);
    fill_circle(
        &mut img,
        iw - 12,
        ih - bar_h / 2,
        4,
        palette.window_bg_active,
    );

    img
}

/// Renders an SVG or `.tgv` vector pattern [`BackgroundConfig`] into an `RgbaImage` preview
/// using `tg_theme::svg_pattern::render_tiled_pattern_wallpaper`.
///
/// Returns `None` when `config` does not carry vector pattern bytes or fails rasterization.
#[must_use]
pub fn generate_pattern_wallpaper_preview(
    config: &BackgroundConfig,
    width: u32,
    height: u32,
) -> Option<RgbaImage> {
    let raw = config.pattern_bytes.as_deref()?;
    if raw.is_empty() || !config.wallpaper.is_pattern() {
        return None;
    }
    let w = width.max(1);
    let h = height.max(1);
    let tile_dim = w.min(h).clamp(32, 256);
    render_tiled_pattern_wallpaper(
        raw,
        w,
        h,
        &config.wallpaper.background_colors,
        config.wallpaper.rotation,
        config.wallpaper.intensity,
        Some((tile_dim, tile_dim)),
        if config.tiled {
            PatternTileMode::Centered
        } else {
            PatternTileMode::Repeat
        },
    )
    .ok()
}

/// Caches generated `egui::TextureHandle` previews for theme lists so thumbnails are not
/// re-rasterized every frame.
#[derive(Default)]
pub struct ThemePreviewCache {
    textures: HashMap<String, TextureHandle>,
}

impl ThemePreviewCache {
    /// Returns a cached preview texture or renders and uploads it on first access.
    pub fn get_or_generate(
        &mut self,
        ctx: &Context,
        cache_key: &str,
        theme: &ThemeInstance,
        preview_type: PreviewType,
    ) -> TextureHandle {
        if let Some(existing) = self.textures.get(cache_key) {
            return existing.clone();
        }
        let rgba = generate_theme_preview(theme, preview_type, None);
        let tex = ctx.load_texture(
            format!("tg_theme_preview_{cache_key}"),
            rgba_to_color_image(&rgba),
            TextureOptions::LINEAR,
        );
        self.textures.insert(cache_key.to_owned(), tex.clone());
        tex
    }

    /// Returns a cached compact card texture or renders and uploads it on first access.
    pub fn get_or_generate_card(
        &mut self,
        ctx: &Context,
        cache_key: &str,
        theme: &ThemeInstance,
        width: u32,
        height: u32,
    ) -> TextureHandle {
        if let Some(existing) = self.textures.get(cache_key) {
            return existing.clone();
        }
        let rgba = generate_compact_theme_card(theme, width, height);
        let tex = ctx.load_texture(
            format!("tg_theme_card_{cache_key}"),
            rgba_to_color_image(&rgba),
            TextureOptions::LINEAR,
        );
        self.textures.insert(cache_key.to_owned(), tex.clone());
        tex
    }

    /// Clears all cached preview textures (e.g. after editing a theme).
    pub fn invalidate(&mut self, cache_key: &str) {
        self.textures.remove(cache_key);
    }

    /// Clears the entire preview texture cache.
    pub fn clear(&mut self) {
        self.textures.clear();
    }
}

fn paint_preview_background(
    canvas: &mut RgbaImage,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    theme: &ThemeInstance,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let bg_img = if let Some(ref bg) = theme.background {
        image::imageops::resize(
            bg,
            w as u32,
            h as u32,
            image::imageops::FilterType::Triangle,
        )
    } else if theme.palette.is_dark_palette() {
        let colors = [
            Color::rgb(0x0E, 0x16, 0x21),
            Color::rgb(0x16, 0x26, 0x38),
            Color::rgb(0x11, 0x1D, 0x2B),
        ];
        let mask = generate_pattern_mask(PatternKind::Constellation, 160);
        generate_background_image(w as u32, h as u32, &colors, 45, 0.25, Some(&mask))
    } else {
        let colors = [
            Color::rgb(0xDB, 0xDD, 0xBB),
            Color::rgb(0x6B, 0xA5, 0x87),
            Color::rgb(0xD5, 0xD8, 0x8D),
            Color::rgb(0x88, 0xB8, 0x84),
        ];
        let mask = generate_pattern_mask(PatternKind::Doodles, 160);
        generate_background_image(w as u32, h as u32, &colors, 0, 0.45, Some(&mask))
    };

    for py in 0..h {
        for px in 0..w {
            let dx = x + px;
            let dy = y + py;
            if dx >= 0 && dy >= 0 && (dx as u32) < canvas.width() && (dy as u32) < canvas.height() {
                canvas.put_pixel(
                    dx as u32,
                    dy as u32,
                    *bg_img.get_pixel(px as u32, py as u32),
                );
            }
        }
    }
}

fn paint_preview_bubbles(
    canvas: &mut RgbaImage,
    hx: i32,
    hy: i32,
    hw: i32,
    hh: i32,
    palette: &Palette,
) {
    // 1. Outgoing voice note bubble with waveform (`addAudioBubble` in window_theme_preview.cpp)
    let voice_w = 186;
    let voice_h = 46;
    let voice_x = hx + hw - 14 - voice_w;
    let voice_y = hy + 14;
    fill_rounded_rect(
        canvas,
        voice_x,
        voice_y + 1,
        voice_w,
        voice_h,
        11,
        palette.msg_out_shadow,
    );
    fill_rounded_rect(
        canvas,
        voice_x,
        voice_y,
        voice_w,
        voice_h,
        11,
        palette.msg_out_bg,
    );
    fill_circle(
        canvas,
        voice_x + 22,
        voice_y + 23,
        14,
        palette.msg_file_out_bg,
    );
    // Waveform bars
    let wave = [
        3, 9, 14, 7, 16, 12, 5, 11, 15, 8, 13, 6, 10, 4, 12, 9, 5, 7, 4, 3,
    ];
    for (i, &bh) in wave.iter().enumerate() {
        let bar_col = if i < 10 {
            palette.msg_waveform_out_active
        } else {
            palette.msg_waveform_out_inactive
        };
        fill_rect(
            canvas,
            voice_x + 44 + (i as i32) * 5,
            voice_y + 24 - bh,
            3,
            bh,
            bar_col,
        );
    }
    fill_rounded_rect(
        canvas,
        voice_x + 44,
        voice_y + 30,
        32,
        5,
        2,
        palette.msg_out_date_fg,
    );

    // 2. Date service pill ("December 26")
    let pill_w = 88;
    let pill_h = 20;
    let pill_x = hx + (hw - pill_w) / 2;
    let pill_y = voice_y + voice_h + 14;
    fill_rounded_rect(
        canvas,
        pill_x,
        pill_y,
        pill_w,
        pill_h,
        10,
        palette.msg_service_bg,
    );
    fill_rounded_rect(
        canvas,
        pill_x + 18,
        pill_y + 7,
        pill_w - 36,
        6,
        3,
        palette.msg_service_fg,
    );

    // 3. Outgoing grouped text bubble
    let out_w = (hw - 68).min(230);
    let out_h = 54;
    let out_x = hx + hw - 14 - out_w;
    let out_y = pill_y + pill_h + 14;
    fill_rounded_rect(
        canvas,
        out_x,
        out_y + 1,
        out_w,
        out_h,
        11,
        palette.msg_out_shadow,
    );
    fill_rounded_rect(canvas, out_x, out_y, out_w, out_h, 11, palette.msg_out_bg);
    fill_rounded_rect(
        canvas,
        out_x + 12,
        out_y + 12,
        out_w - 28,
        7,
        3,
        palette.history_text_out_fg,
    );
    fill_rounded_rect(
        canvas,
        out_x + 12,
        out_y + 24,
        out_w - 44,
        7,
        3,
        palette.history_text_out_fg,
    );
    fill_rounded_rect(
        canvas,
        out_x + 12,
        out_y + 36,
        (out_w - 92).max(40),
        7,
        3,
        palette.history_link_out_fg,
    );
    fill_rounded_rect(
        canvas,
        out_x + out_w - 38,
        out_y + out_h - 14,
        24,
        5,
        2,
        palette.msg_out_date_fg,
    );

    // 4. Incoming reply bubble
    let in_w = (hw - 76).min(220);
    let in_h = 64;
    let in_x = hx + 14;
    let in_y = (out_y + out_h + 12).min(hy + hh - in_h - 10);
    if in_y > hy {
        fill_rounded_rect(
            canvas,
            in_x,
            in_y + 1,
            in_w,
            in_h,
            11,
            palette.msg_in_shadow,
        );
        fill_rounded_rect(canvas, in_x, in_y, in_w, in_h, 11, palette.msg_in_bg);
        // Reply accent bar
        fill_rect(
            canvas,
            in_x + 12,
            in_y + 10,
            3,
            22,
            palette.msg_in_reply_bar_color,
        );
        fill_rounded_rect(
            canvas,
            in_x + 20,
            in_y + 11,
            76,
            6,
            3,
            palette.msg_in_service_fg,
        );
        fill_rounded_rect(
            canvas,
            in_x + 20,
            in_y + 22,
            112,
            6,
            3,
            palette.window_sub_text_fg,
        );
        // Message line + timestamp
        fill_rounded_rect(
            canvas,
            in_x + 12,
            in_y + 42,
            in_w - 58,
            7,
            3,
            palette.history_text_in_fg,
        );
        fill_rounded_rect(
            canvas,
            in_x + in_w - 38,
            in_y + in_h - 14,
            24,
            5,
            2,
            palette.msg_in_date_fg,
        );
    }
}

fn to_rgba(c: Color) -> Rgba<u8> {
    Rgba([c.r, c.g, c.b, c.a])
}

fn blend_pixel(canvas: &mut RgbaImage, x: i32, y: i32, color: Color) {
    if x < 0 || y < 0 || (x as u32) >= canvas.width() || (y as u32) >= canvas.height() {
        return;
    }
    if color.a == 0 {
        return;
    }
    let dst = canvas.get_pixel_mut(x as u32, y as u32);
    if color.a == 255 {
        dst.0 = [color.r, color.g, color.b, 255];
        return;
    }
    let a = u16::from(color.a);
    let inv = 255 - a;
    dst.0[0] = ((u16::from(color.r) * a + u16::from(dst.0[0]) * inv + 127) / 255) as u8;
    dst.0[1] = ((u16::from(color.g) * a + u16::from(dst.0[1]) * inv + 127) / 255) as u8;
    dst.0[2] = ((u16::from(color.b) * a + u16::from(dst.0[2]) * inv + 127) / 255) as u8;
    dst.0[3] = 255;
}

fn fill_rect(canvas: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, color: Color) {
    if w <= 0 || h <= 0 {
        return;
    }
    for py in y..(y + h) {
        for px in x..(x + w) {
            blend_pixel(canvas, px, py, color);
        }
    }
}

fn fill_circle(canvas: &mut RgbaImage, cx: i32, cy: i32, r: i32, color: Color) {
    let r_sq = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r_sq {
                blend_pixel(canvas, cx + dx, cy + dy, color);
            }
        }
    }
}

fn fill_rounded_rect(
    canvas: &mut RgbaImage,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: i32,
    color: Color,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let r = radius.min(w / 2).min(h / 2).max(0);
    let r_sq = r * r;
    for py in 0..h {
        for px in 0..w {
            let in_corner = |cx: i32, cy: i32| {
                let dx = px - cx;
                let dy = py - cy;
                dx * dx + dy * dy <= r_sq
            };
            let inside = if px < r && py < r {
                in_corner(r, r)
            } else if px >= w - r && py < r {
                in_corner(w - r - 1, r)
            } else if px < r && py >= h - r {
                in_corner(r, h - r - 1)
            } else if px >= w - r && py >= h - r {
                in_corner(w - r - 1, h - r - 1)
            } else {
                true
            };
            if inside {
                blend_pixel(canvas, x + px, y + py, color);
            }
        }
    }
}
