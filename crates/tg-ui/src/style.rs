//! Real Telegram Desktop style adapters backed by `tg-style` (`tg_style::st::*`,
//! `tg_style::structs::*`, DPI scaling) and `tg-theme` (`Colors` / `ColorResolver`).
//!
//! Port of `Telegram/lib_ui/ui/basic.style`,
//! `Telegram/lib_ui/ui/widgets/widgets.style`,
//! `Telegram/lib_ui/ui/layers/layers.style`,
//! `Telegram/SourceFiles/chat_helpers/chat_helpers.style`, and
//! `Telegram/SourceFiles/dialogs/dialogs.style`.

use egui::{Color32, FontId, Vec2};
use tg_style::{
    Align as TgAlign, ColorRef, Font as TgFont, FontFlags, Icon as TgIcon, Margins as TgMargins,
    Pixels, Point as TgPoint, Size as TgSize, st, structs as tg,
};

use crate::fonts::{font_id_for_flags, font_id_from_style};
use crate::theme::{Colors, resolve_color_ref};

/// Helper: convert DPI-scaled `tg_style::Pixels` to `f32`.
#[inline]
#[must_use]
pub fn px(p: Pixels) -> f32 {
    p.scaled() as f32
}

/// Helper: convert `tg_style::ColorRef` to `egui::Color32` via `colors`.
#[inline]
#[must_use]
pub fn resolve_color(c: ColorRef, colors: &dyn Colors) -> Color32 {
    if c.name().is_empty() {
        Color32::TRANSPARENT
    } else {
        resolve_color_ref(c, colors)
    }
}

/// Standard tdesktop spacing / duration constants derived from `tg_style::st::*`.
///
/// The `const` items reflect 100%-scale design values from `basic.style` / `widgets.style` /
/// `layers.style` for backward compatibility; use the `scaled_*` functions or `tg_style::st::*`
/// directly when DPI scaling is active.
pub mod constants {
    use tg_style::st;

    /// Default base font size (`st::fsize` = 13px).
    pub const FSIZE: f32 = st::fsize.raw() as f32;
    /// Default box body font size (`st::box_font_size` = 14px).
    pub const BOX_FONT_SIZE: f32 = st::box_font_size.raw() as f32;
    /// Default box title font size (`st::box_title_font` = 16px).
    pub const BOX_TITLE_FONT_SIZE: f32 = st::box_title_font.size.raw() as f32;
    /// 1px line thickness (`st::line_width`).
    pub const LINE_WIDTH: f32 = st::line_width.raw() as f32;
    /// Large rounded corner radius (`st::round_radius_large` = 6px).
    pub const ROUND_RADIUS_LARGE: f32 = st::round_radius_large.raw() as f32;
    /// Small rounded corner radius (`st::round_radius_small` = 3px).
    pub const ROUND_RADIUS_SMALL: f32 = st::round_radius_small.raw() as f32;
    /// Default button corner radius (`st::button_radius` = 4px).
    pub const BUTTON_RADIUS: f32 = st::button_radius.raw() as f32;
    /// Universal short animation duration in ms (`st::universal_duration` = 120ms).
    pub const UNIVERSAL_DURATION_MS: u32 = st::universal_duration as u32;
    /// Slide transition duration in ms (`st::slide_duration` = 240ms).
    pub const SLIDE_DURATION_MS: u32 = st::slide_duration as u32;
    /// Slide shift in logical pixels (`st::slide_shift` = 100px).
    pub const SLIDE_SHIFT: f32 = st::slide_shift.raw() as f32;
    /// Fade wrap duration in ms (`st::fade_wrap_duration` = 200ms).
    pub const FADE_WRAP_DURATION_MS: u32 = st::fade_wrap_duration as u32;
    /// Widget fade duration in ms (`st::widget_fade_duration` = 200ms).
    pub const WIDGET_FADE_DURATION_MS: u32 = st::widget_fade_duration as u32;
    /// Shake error animation duration in ms (`st::shake_duration` = 300ms).
    pub const SHAKE_DURATION_MS: u32 = st::shake_duration as u32;
    /// Shake error horizontal shift in px (`st::shake_shift` = 4px).
    pub const SHAKE_SHIFT: f32 = st::shake_shift.raw() as f32;
    /// Box show/hide animation duration in ms (`st::box_duration` = 200ms).
    pub const BOX_DURATION_MS: u32 = st::box_duration as u32;
    /// Box corner radius (`st::box_radius` = 8px).
    pub const BOX_RADIUS: f32 = st::box_radius.raw() as f32;
    /// Default box width (`st::box_width` = 320px).
    pub const BOX_WIDTH: f32 = st::box_width.raw() as f32;
    /// Wide box width (`st::box_wide_width` = 364px).
    pub const BOX_WIDE_WIDTH: f32 = st::box_wide_width.raw() as f32;
    /// Box title height (`st::box_title_height` = 48px).
    pub const BOX_TITLE_HEIGHT: f32 = st::box_title_height.raw() as f32;
    /// Box divider height (`st::box_divider_height` = 8px).
    pub const BOX_DIVIDER_HEIGHT: f32 = st::box_divider_height.raw() as f32;
    /// Windows custom title bar height (`st::window_title_height` = 24px).
    pub const WINDOW_TITLE_HEIGHT: f32 = st::window_title_height.raw() as f32;
    /// Windows title bar control button width (`st::window_title_button_width` = 36px).
    pub const WINDOW_TITLE_BUTTON_WIDTH: f32 = st::window_title_button_width.raw() as f32;
}

/// Four-sided margins mirroring tdesktop `style::margins`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Margins {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Margins {
    /// Construct margins `(left, top, right, bottom)`.
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Convert DPI-scaled `tg_style::Margins`.
    #[must_use]
    pub fn from_tg(m: TgMargins) -> Self {
        let s = m.scaled();
        Self::new(s.left as f32, s.top as f32, s.right as f32, s.bottom as f32)
    }

    /// Equal margins on all sides.
    pub const fn uniform(v: f32) -> Self {
        Self::new(v, v, v, v)
    }

    /// Horizontal sum (`left + right`).
    pub const fn horizontal(self) -> f32 {
        self.left + self.right
    }

    /// Vertical sum (`top + bottom`).
    pub const fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

impl From<TgMargins> for Margins {
    fn from(m: TgMargins) -> Self {
        Self::from_tg(m)
    }
}

/// 2D point mirroring tdesktop `style::point`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    /// Construct a point `(x, y)`.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Convert DPI-scaled `tg_style::Point`.
    #[must_use]
    pub fn from_tg(p: TgPoint) -> Self {
        let s = p.scaled();
        Self::new(s.x as f32, s.y as f32)
    }
}

impl From<TgPoint> for Point {
    fn from(p: TgPoint) -> Self {
        Self::from_tg(p)
    }
}

/// 2D size mirroring tdesktop `style::size`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    /// Construct a size `(width, height)`.
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// Convert DPI-scaled `tg_style::Size`.
    #[must_use]
    pub fn from_tg(sz: TgSize) -> Self {
        let s = sz.scaled();
        Self::new(s.width as f32, s.height as f32)
    }

    /// Convert to `egui::Vec2`.
    pub const fn to_vec2(self) -> Vec2 {
        Vec2::new(self.width, self.height)
    }
}

impl From<TgSize> for Size {
    fn from(sz: TgSize) -> Self {
        Self::from_tg(sz)
    }
}

/// Text/widget alignment mirroring tdesktop `style::align`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl From<TgAlign> for Align {
    fn from(a: TgAlign) -> Self {
        match a {
            TgAlign::TopLeft | TgAlign::Justify => Self::TopLeft,
            TgAlign::Top => Self::Top,
            TgAlign::TopRight => Self::TopRight,
            TgAlign::Left => Self::Left,
            TgAlign::Center => Self::Center,
            TgAlign::Right => Self::Right,
            TgAlign::BottomLeft => Self::BottomLeft,
            TgAlign::Bottom => Self::Bottom,
            TgAlign::BottomRight => Self::BottomRight,
        }
    }
}

/// Font description mirroring tdesktop `style::font`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontSpec {
    pub size: f32,
    pub semibold: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub monospace: bool,
}

impl FontSpec {
    /// Convert a generated `tg_style::Font` at the current global DPI scale.
    #[must_use]
    pub fn from_tg(font: TgFont) -> Self {
        let size = font.scaled_size().max(1) as f32;
        Self {
            size,
            semibold: font.flags.is_semibold(),
            bold: font.flags.is_bold(),
            italic: font.flags.is_italic(),
            underline: font.flags.is_underline(),
            monospace: font.flags.is_monospace(),
        }
    }

    /// Regular proportional font at `size` logical pixels.
    pub const fn regular(size: f32) -> Self {
        Self {
            size,
            semibold: false,
            bold: false,
            italic: false,
            underline: false,
            monospace: false,
        }
    }

    /// Semibold proportional font at `size` logical pixels.
    pub const fn semibold(size: f32) -> Self {
        Self {
            semibold: true,
            ..Self::regular(size)
        }
    }

    /// Bold proportional font at `size` logical pixels.
    pub const fn bold(size: f32) -> Self {
        Self {
            bold: true,
            ..Self::regular(size)
        }
    }

    /// Italic proportional font at `size` logical pixels.
    pub const fn italic(size: f32) -> Self {
        Self {
            italic: true,
            ..Self::regular(size)
        }
    }

    /// Underlined proportional font at `size` logical pixels.
    pub const fn underline(size: f32) -> Self {
        Self {
            underline: true,
            ..Self::regular(size)
        }
    }

    /// Monospace font at `size` logical pixels.
    pub const fn monospace(size: f32) -> Self {
        Self {
            monospace: true,
            ..Self::regular(size)
        }
    }

    /// Convert flags to `tg_style::FontFlags`.
    #[must_use]
    pub const fn to_tg_flags(self) -> FontFlags {
        let mut bits = 0u8;
        if self.semibold || self.bold {
            bits |= FontFlags::BOLD.bits();
        }
        if self.italic {
            bits |= FontFlags::ITALIC.bits();
        }
        if self.underline {
            bits |= FontFlags::UNDERLINE.bits();
        }
        if self.monospace {
            bits |= FontFlags::MONOSPACE.bits();
        }
        FontFlags::from_bits(bits)
    }

    /// Convert to an `egui::FontId` selecting the real Open Sans / Hack face.
    #[must_use]
    pub fn font_id(self) -> FontId {
        font_id_for_flags(self.size, self.to_tg_flags())
    }

    /// Approximate line height matching Qt/Open Sans font metrics.
    #[must_use]
    pub fn line_height(self) -> f32 {
        (self.size * 1.35).round()
    }
}

impl From<TgFont> for FontSpec {
    fn from(font: TgFont) -> Self {
        Self::from_tg(font)
    }
}

/// Link underline mode mirroring `kLinkUnderline*` in `basic.style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkUnderline {
    Never = 0,
    #[default]
    Active = 1,
    Always = 2,
}

impl LinkUnderline {
    /// Convert tdesktop's integer constant (`kLinkUnderline*`).
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Never,
            2 => Self::Always,
            _ => Self::Active,
        }
    }
}

/// Quote/pre block visual geometry resolved from `tg_style::structs::QuoteStyle`.
#[derive(Debug, Clone, Copy)]
pub struct QuoteStyle {
    pub padding: Margins,
    pub vertical_skip: f32,
    pub header: f32,
    pub header_position: Point,
    pub icon: TgIcon,
    pub icon_position: Point,
    pub expand: TgIcon,
    pub expand_position: Point,
    pub collapse: TgIcon,
    pub collapse_position: Point,
    pub outline: f32,
    pub outline_shift: f32,
    pub radius: f32,
    pub scrollable: bool,
}

impl QuoteStyle {
    /// Resolve from a generated `tg_style::structs::QuoteStyle`.
    #[must_use]
    pub fn from_tg(s: &tg::QuoteStyle) -> Self {
        Self {
            padding: Margins::from_tg(s.padding),
            vertical_skip: px(s.vertical_skip),
            header: px(s.header),
            header_position: Point::from_tg(s.header_position),
            icon: s.icon,
            icon_position: Point::from_tg(s.icon_position),
            expand: s.expand,
            expand_position: Point::from_tg(s.expand_position),
            collapse: s.collapse,
            collapse_position: Point::from_tg(s.collapse_position),
            outline: px(s.outline),
            outline_shift: px(s.outline_shift),
            radius: px(s.radius),
            scrollable: s.scrollable,
        }
    }

    /// Blockquote style from `tg_style::st::history_text_style.blockquote`.
    #[must_use]
    pub fn history_blockquote() -> Self {
        Self::from_tg(&st::history_text_style.blockquote)
    }

    /// Preformatted code block style from `tg_style::st::history_text_style.pre`.
    #[must_use]
    pub fn history_pre() -> Self {
        Self::from_tg(&st::history_text_style.pre)
    }
}

/// Text color palette mirroring `TextPalette` in `basic.style`.
#[derive(Debug, Clone, Copy)]
pub struct TextPalette {
    pub link_fg: Color32,
    pub mono_fg: Color32,
    pub mark_bg: Color32,
    pub spoiler_fg: Color32,
    pub select_bg: Color32,
    pub select_fg: Color32,
    pub select_link_fg: Color32,
    pub select_mono_fg: Color32,
    pub select_spoiler_fg: Color32,
    pub select_overlay: Color32,
    pub link_always_active: bool,
}

impl TextPalette {
    /// Resolve from any generated `tg_style::structs::TextPalette`.
    #[must_use]
    pub fn from_tg(s: &tg::TextPalette, c: &dyn Colors) -> Self {
        Self {
            link_fg: resolve_color(s.link_fg, c),
            mono_fg: resolve_color(s.mono_fg, c),
            mark_bg: resolve_color(s.mark_bg, c),
            spoiler_fg: resolve_color(s.spoiler_fg, c),
            select_bg: resolve_color(s.select_bg, c),
            select_fg: resolve_color(s.select_fg, c),
            select_link_fg: resolve_color(s.select_link_fg, c),
            select_mono_fg: resolve_color(s.select_mono_fg, c),
            select_spoiler_fg: resolve_color(s.select_spoiler_fg, c),
            select_overlay: resolve_color(s.select_overlay, c),
            link_always_active: s.link_always_active,
        }
    }

    /// `st::default_text_palette`.
    #[must_use]
    pub fn default_palette(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_text_palette, c)
    }

    /// `st::default_toast_palette`.
    #[must_use]
    pub fn toast_palette(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_toast_palette, c)
    }

    /// `st::in_text_palette` (incoming message bubble text palette).
    #[must_use]
    pub fn in_message(c: &dyn Colors) -> Self {
        Self::from_tg(&st::in_text_palette, c)
    }

    /// `st::out_text_palette` (outgoing message bubble text palette).
    #[must_use]
    pub fn out_message(c: &dyn Colors) -> Self {
        Self::from_tg(&st::out_text_palette, c)
    }
}

/// Text style mirroring `TextStyle` in `basic.style`.
#[derive(Debug, Clone, Copy)]
pub struct TextStyle {
    pub font: FontSpec,
    pub link_underline: LinkUnderline,
    pub line_height: f32,
}

impl TextStyle {
    /// Resolve from any generated `tg_style::structs::TextStyle`.
    #[must_use]
    pub fn from_tg(s: &tg::TextStyle) -> Self {
        Self {
            font: FontSpec::from_tg(s.font),
            link_underline: LinkUnderline::from_i32(s.link_underline),
            line_height: px(s.line_height),
        }
    }

    /// `st::default_text_style` (13px regular at 100%).
    #[must_use]
    pub fn default_style() -> Self {
        Self::from_tg(&st::default_text_style)
    }

    /// `st::semibold_text_style` (13px semibold at 100%).
    #[must_use]
    pub fn semibold() -> Self {
        Self::from_tg(&st::semibold_text_style)
    }

    /// `st::box_text_style` (14px regular at 100%).
    #[must_use]
    pub fn box_text() -> Self {
        Self::from_tg(&st::box_text_style)
    }

    /// `st::box_label_style` (14px regular, 22px lineHeight at 100%).
    #[must_use]
    pub fn box_label() -> Self {
        Self::from_tg(&st::box_label_style)
    }

    /// `st::default_box_button_text_style` (14px semibold at 100%).
    #[must_use]
    pub fn box_button() -> Self {
        Self::from_tg(&st::default_box_button_text_style)
    }

    /// `st::box_title.style` (16px semibold at 100%).
    #[must_use]
    pub fn box_title() -> Self {
        Self::from_tg(&st::box_title.style)
    }

    /// `st::history_text_style`.
    #[must_use]
    pub fn history_text() -> Self {
        Self::from_tg(&st::history_text_style)
    }
}

/// Ripple animation parameters mirroring `RippleAnimation` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct RippleAnimationStyle {
    pub color: Color32,
    pub show_duration_ms: u32,
    pub hide_duration_ms: u32,
}

impl RippleAnimationStyle {
    /// Resolve from a generated `tg_style::structs::RippleAnimation`.
    #[must_use]
    pub fn from_tg(s: &tg::RippleAnimation, c: &dyn Colors) -> Self {
        Self {
            color: resolve_color(s.color, c),
            show_duration_ms: s.show_duration.max(0) as u32,
            hide_duration_ms: s.hide_duration.max(0) as u32,
        }
    }

    /// `st::default_ripple_animation` (650ms show, 200ms hide).
    #[must_use]
    pub fn default_ripple(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_ripple_animation, c)
    }

    /// `st::default_ripple_animation_bg_over`.
    #[must_use]
    pub fn bg_over(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_ripple_animation_bg_over, c)
    }

    /// `st::empty_ripple_animation` (disabled).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            color: Color32::TRANSPARENT,
            show_duration_ms: st::empty_ripple_animation.show_duration as u32,
            hide_duration_ms: st::empty_ripple_animation.hide_duration as u32,
        }
    }
}

/// Infinite radial spinner style mirroring `InfiniteRadialAnimation` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct InfiniteRadialAnimationStyle {
    pub color: Color32,
    pub thickness: f32,
    pub size: Size,
    pub linear_period_ms: u32,
    pub sine_period_ms: u32,
    pub sine_duration_ms: u32,
    pub sine_shift_ms: u32,
    pub arc_min: f64,
    pub arc_max: f64,
}

impl InfiniteRadialAnimationStyle {
    /// Resolve from a generated `tg_style::structs::InfiniteRadialAnimation`.
    #[must_use]
    pub fn from_tg(s: &tg::InfiniteRadialAnimation, c: &dyn Colors) -> Self {
        let color = if s.color.name().is_empty() {
            c.color("windowBgActive")
        } else {
            resolve_color(s.color, c)
        };
        let thickness = if s.thickness.raw() > 0 {
            px(s.thickness)
        } else {
            px(st::radial_line)
        };
        let size = if s.size.width.raw() > 0 && s.size.height.raw() > 0 {
            Size::from_tg(s.size)
        } else {
            Size::new(px(Pixels::new(36)), px(Pixels::new(36)))
        };
        Self {
            color,
            thickness,
            size,
            linear_period_ms: s.linear_period.max(0) as u32,
            sine_period_ms: s.sine_period.max(0) as u32,
            sine_duration_ms: s.sine_duration.max(0) as u32,
            sine_shift_ms: s.sine_shift.max(0) as u32,
            arc_min: s.arc_min,
            arc_max: s.arc_max,
        }
    }

    /// `st::default_infinite_radial_animation`.
    #[must_use]
    pub fn default_radial(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_infinite_radial_animation, c)
    }

    /// `st::box_loading_animation` (`st::box_loading_size` = 20px, 2px stroke, `windowSubTextFg`).
    #[must_use]
    pub fn box_loading(c: &dyn Colors) -> Self {
        let mut s = Self::from_tg(&st::box_loading_animation, c);
        let sz = px(st::box_loading_size);
        s.size = Size::new(sz, sz);
        s
    }
}

/// Flat label style mirroring `FlatLabel` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct FlatLabelStyle {
    pub margin: Margins,
    pub min_width: f32,
    pub align: Align,
    pub text_fg: Color32,
    pub max_height: f32,
    pub style: TextStyle,
    pub palette: TextPalette,
}

impl FlatLabelStyle {
    /// Resolve from any generated `tg_style::structs::FlatLabel`.
    #[must_use]
    pub fn from_tg(s: &tg::FlatLabel, c: &dyn Colors) -> Self {
        Self {
            margin: Margins::from_tg(s.margin),
            min_width: px(s.min_width),
            align: s.align.into(),
            text_fg: resolve_color(s.text_fg, c),
            max_height: px(s.max_height),
            style: TextStyle::from_tg(&s.style),
            palette: TextPalette::from_tg(&s.palette, c),
        }
    }

    /// `st::default_flat_label`.
    #[must_use]
    pub fn default_label(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_flat_label, c)
    }

    /// `st::default_sub_text_label`.
    #[must_use]
    pub fn sub_text(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_sub_text_label, c)
    }

    /// `st::default_subsection_title`.
    #[must_use]
    pub fn subsection_title(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_subsection_title, c)
    }

    /// `st::box_title`.
    #[must_use]
    pub fn box_title(c: &dyn Colors) -> Self {
        Self::from_tg(&st::box_title, c)
    }

    /// `st::box_label`.
    #[must_use]
    pub fn box_label(c: &dyn Colors) -> Self {
        Self::from_tg(&st::box_label, c)
    }
}

/// Link button style mirroring `LinkButton` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct LinkButtonStyle {
    pub color: Color32,
    pub over_color: Color32,
    pub font: FontSpec,
    pub over_font: FontSpec,
    pub padding: Margins,
}

impl LinkButtonStyle {
    /// Resolve from any generated `tg_style::structs::LinkButton`.
    #[must_use]
    pub fn from_tg(s: &tg::LinkButton, c: &dyn Colors) -> Self {
        Self {
            color: resolve_color(s.color, c),
            over_color: resolve_color(s.over_color, c),
            font: FontSpec::from_tg(s.font),
            over_font: FontSpec::from_tg(s.over_font),
            padding: Margins::from_tg(s.padding),
        }
    }

    /// `st::default_link_button`.
    #[must_use]
    pub fn default_link(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_link_button, c)
    }

    /// `st::box_link_button`.
    #[must_use]
    pub fn box_link(c: &dyn Colors) -> Self {
        Self::from_tg(&st::box_link_button, c)
    }
}

/// Flat button style mirroring `FlatButton` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct FlatButtonStyle {
    pub color: Color32,
    pub over_color: Color32,
    pub bg_color: Color32,
    pub over_bg_color: Color32,
    pub width: f32,
    pub height: f32,
    pub text_top: f32,
    pub font: FontSpec,
    pub over_font: FontSpec,
    pub ripple: RippleAnimationStyle,
}

impl FlatButtonStyle {
    /// Resolve from any generated `tg_style::structs::FlatButton`.
    #[must_use]
    pub fn from_tg(s: &tg::FlatButton, c: &dyn Colors) -> Self {
        Self {
            color: resolve_color(s.color, c),
            over_color: resolve_color(s.over_color, c),
            bg_color: resolve_color(s.bg_color, c),
            over_bg_color: resolve_color(s.over_bg_color, c),
            width: px(s.width),
            height: px(s.height),
            text_top: px(s.text_top),
            font: FontSpec::from_tg(s.font),
            over_font: FontSpec::from_tg(s.over_font),
            ripple: RippleAnimationStyle::from_tg(&s.ripple, c),
        }
    }

    /// Resolve from `st::history_compose_button` (tdesktop's primary flat action button).
    #[must_use]
    pub fn default_flat(c: &dyn Colors) -> Self {
        let mut s = Self::from_tg(&st::history_compose_button, c);
        if s.width == 0.0 {
            s.width = -px(Pixels::new(36));
        }
        s
    }
}

/// Round button style mirroring `RoundButton` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct RoundButtonStyle {
    pub text_fg: Color32,
    pub text_fg_over: Color32,
    pub text_bg: Color32,
    pub text_bg_over: Color32,
    pub numbers_text_fg: Color32,
    pub numbers_text_fg_over: Color32,
    pub numbers_skip: f32,
    /// If `< 0`, button width = `text_width + (-width)`. If `> 0`, fixed width.
    pub width: f32,
    pub height: f32,
    /// Corner radius (`0.0` falls back to `st::button_radius`).
    pub radius: f32,
    pub full_radius: bool,
    pub padding: Margins,
    pub text_top: f32,
    pub style: TextStyle,
    pub ripple: RippleAnimationStyle,
}

impl RoundButtonStyle {
    /// Resolve from any generated `tg_style::structs::RoundButton`.
    #[must_use]
    pub fn from_tg(s: &tg::RoundButton, c: &dyn Colors) -> Self {
        let radius = if s.radius.raw() > 0 {
            px(s.radius)
        } else {
            px(st::button_radius)
        };
        Self {
            text_fg: resolve_color(s.text_fg, c),
            text_fg_over: resolve_color(s.text_fg_over, c),
            text_bg: resolve_color(s.text_bg, c),
            text_bg_over: resolve_color(s.text_bg_over, c),
            numbers_text_fg: resolve_color(s.numbers_text_fg, c),
            numbers_text_fg_over: resolve_color(s.numbers_text_fg_over, c),
            numbers_skip: px(s.numbers_skip),
            width: px(s.width),
            height: px(s.height),
            radius,
            full_radius: s.full_radius,
            padding: Margins::from_tg(s.padding),
            text_top: px(s.text_top),
            style: TextStyle::from_tg(&s.style),
            ripple: RippleAnimationStyle::from_tg(&s.ripple, c),
        }
    }

    /// `st::default_active_button`.
    #[must_use]
    pub fn default_active(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_active_button, c)
    }

    /// `st::default_light_button`.
    #[must_use]
    pub fn default_light(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_light_button, c)
    }

    /// `st::default_box_button`.
    #[must_use]
    pub fn default_box(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_box_button, c)
    }

    /// `st::attention_box_button`.
    #[must_use]
    pub fn attention_box(c: &dyn Colors) -> Self {
        Self::from_tg(&st::attention_box_button, c)
    }
}

/// Icon button style mirroring `IconButton` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct IconButtonStyle {
    pub width: f32,
    pub height: f32,
    pub icon_fg: Color32,
    pub icon_fg_over: Color32,
    pub icon_size: f32,
    pub duration_ms: u32,
    pub ripple_area_position: Point,
    pub ripple_area_size: f32,
    pub ripple: RippleAnimationStyle,
}

impl IconButtonStyle {
    /// Resolve from any generated `tg_style::structs::IconButton`.
    #[must_use]
    pub fn from_tg(s: &tg::IconButton, c: &dyn Colors) -> Self {
        let icon_fg = s
            .icon
            .parts
            .first()
            .map_or_else(|| c.color("menuIconFg"), |p| resolve_color(p.color, c));
        let icon_fg_over = s
            .icon_over
            .parts
            .first()
            .map_or_else(|| c.color("menuIconFgOver"), |p| resolve_color(p.color, c));
        let icon_size = s
            .icon
            .scaled_size()
            .map(|sz| sz.width.max(sz.height) as f32)
            .ok()
            .filter(|&v| v > 0.0)
            .unwrap_or_else(|| px(Pixels::new(18)));
        let duration_ms = if s.duration > 0 {
            s.duration as u32
        } else {
            st::universal_duration as u32
        };
        let ripple_area_size = if s.ripple_area_size.raw() > 0 {
            px(s.ripple_area_size)
        } else {
            px(s.width).min(px(s.height)).max(px(Pixels::new(36)))
        };
        Self {
            width: px(s.width).max(px(Pixels::new(24))),
            height: px(s.height).max(px(Pixels::new(24))),
            icon_fg,
            icon_fg_over,
            icon_size,
            duration_ms,
            ripple_area_position: Point::from_tg(s.ripple_area_position),
            ripple_area_size,
            ripple: RippleAnimationStyle::from_tg(&s.ripple, c),
        }
    }

    /// `st::menu_toggle` (40x40 circular-ripple icon button).
    #[must_use]
    pub fn default_icon_button(c: &dyn Colors) -> Self {
        Self::from_tg(&st::menu_toggle, c)
    }

    /// `st::box_title_close` (48x48 with 40px ripple centered at (4, 4)).
    #[must_use]
    pub fn box_title_close(c: &dyn Colors) -> Self {
        Self::from_tg(&st::box_title_close, c)
    }
}

/// Checkbox check-box view style mirroring `Check` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct CheckStyle {
    pub bg: Color32,
    pub untoggled_fg: Color32,
    pub toggled_fg: Color32,
    pub check_fg: Color32,
    pub diameter: f32,
    pub thickness: f32,
    pub duration_ms: u32,
    pub ripple_area_padding: f32,
}

impl CheckStyle {
    /// Resolve from a generated `tg_style::structs::Check`.
    #[must_use]
    pub fn from_tg(s: &tg::Check, c: &dyn Colors) -> Self {
        let check_fg = s.icon.parts.first().map_or_else(
            || c.color("overviewCheckFgActive"),
            |p| resolve_color(p.color, c),
        );
        Self {
            bg: resolve_color(s.bg, c),
            untoggled_fg: resolve_color(s.untoggled_fg, c),
            toggled_fg: resolve_color(s.toggled_fg, c),
            check_fg,
            diameter: px(s.diameter),
            thickness: px(s.thickness),
            duration_ms: s.duration.max(0) as u32,
            ripple_area_padding: px(s.ripple_area_padding),
        }
    }

    /// `st::default_check`.
    #[must_use]
    pub fn default_check(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_check, c)
    }
}

/// Radio view style mirroring `Radio` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct RadioStyle {
    pub bg: Color32,
    pub untoggled_fg: Color32,
    pub toggled_fg: Color32,
    pub diameter: f32,
    pub thickness: f32,
    /// `outerSkip` in tenths of a pixel (tdesktop default `10.0` -> 1.0px).
    pub outer_skip_tenths: f32,
    /// `skip` in tenths of a pixel (tdesktop default `60.0` -> 6.0px).
    pub skip_tenths: f32,
    pub duration_ms: u32,
    pub ripple_area_padding: f32,
}

impl RadioStyle {
    /// Resolve from a generated `tg_style::structs::Radio`.
    #[must_use]
    pub fn from_tg(s: &tg::Radio, c: &dyn Colors) -> Self {
        Self {
            bg: resolve_color(s.bg, c),
            untoggled_fg: resolve_color(s.untoggled_fg, c),
            toggled_fg: resolve_color(s.toggled_fg, c),
            diameter: px(s.diameter),
            thickness: px(s.thickness),
            outer_skip_tenths: px(s.outer_skip),
            skip_tenths: px(s.skip),
            duration_ms: s.duration.max(0) as u32,
            ripple_area_padding: px(s.ripple_area_padding),
        }
    }

    /// `st::default_radio`.
    #[must_use]
    pub fn default_radio(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_radio, c)
    }
}

/// Toggle switch style mirroring `Toggle` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct ToggleStyle {
    pub toggled_bg: Color32,
    pub toggled_fg: Color32,
    pub untoggled_bg: Color32,
    pub untoggled_fg: Color32,
    pub duration_ms: u32,
    pub border: f32,
    pub shift: f32,
    pub diameter: f32,
    pub width: f32,
    /// Optional X/V morph glyph size inside the thumb (`0.0` disables it).
    pub xsize: f32,
    pub vsize: f32,
    pub vshift: f32,
    pub stroke: f32,
    pub ripple_area_padding: f32,
}

impl ToggleStyle {
    /// Resolve from a generated `tg_style::structs::Toggle`.
    #[must_use]
    pub fn from_tg(s: &tg::Toggle, c: &dyn Colors) -> Self {
        Self {
            toggled_bg: resolve_color(s.toggled_bg, c),
            toggled_fg: resolve_color(s.toggled_fg, c),
            untoggled_bg: resolve_color(s.untoggled_bg, c),
            untoggled_fg: resolve_color(s.untoggled_fg, c),
            duration_ms: s.duration.max(0) as u32,
            border: px(s.border),
            shift: px(s.shift),
            diameter: px(s.diameter),
            width: px(s.width),
            xsize: px(s.xsize),
            vsize: px(s.vsize),
            vshift: px(s.vshift),
            stroke: px(s.stroke),
            ripple_area_padding: px(s.ripple_area_padding),
        }
    }

    /// `st::default_toggle`.
    #[must_use]
    pub fn default_toggle(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_toggle, c)
    }

    /// Variant with tdesktop's X -> checkmark morph inside the circular thumb.
    #[must_use]
    pub fn with_xv(c: &dyn Colors) -> Self {
        Self {
            diameter: px(Pixels::new(20)),
            width: px(Pixels::new(16)),
            xsize: px(Pixels::new(8)),
            vsize: px(Pixels::new(4)),
            vshift: px(Pixels::new(1)),
            stroke: 1.8,
            ..Self::default_toggle(c)
        }
    }
}

/// Checkbox container style mirroring `Checkbox` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct CheckboxStyle {
    pub text_fg: Color32,
    pub text_fg_active: Color32,
    pub margin: Margins,
    pub text_position: Point,
    pub check_position: Point,
    pub style: TextStyle,
    pub ripple_bg: Color32,
    pub ripple_bg_active: Color32,
    pub ripple: RippleAnimationStyle,
    pub disabled_opacity: f32,
}

impl CheckboxStyle {
    /// Resolve from a generated `tg_style::structs::Checkbox`.
    #[must_use]
    pub fn from_tg(s: &tg::Checkbox, c: &dyn Colors) -> Self {
        Self {
            text_fg: resolve_color(s.text_fg, c),
            text_fg_active: resolve_color(s.text_fg_active, c),
            margin: Margins::from_tg(s.margin),
            text_position: Point::from_tg(s.text_position),
            check_position: Point::from_tg(s.check_position),
            style: TextStyle::from_tg(&s.style),
            ripple_bg: resolve_color(s.ripple_bg, c),
            ripple_bg_active: resolve_color(s.ripple_bg_active, c),
            ripple: RippleAnimationStyle::from_tg(&s.ripple, c),
            disabled_opacity: s.disabled_opacity as f32,
        }
    }

    /// `st::default_checkbox`.
    #[must_use]
    pub fn default_checkbox(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_checkbox, c)
    }

    /// `st::default_box_checkbox`.
    #[must_use]
    pub fn box_checkbox(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_box_checkbox, c)
    }
}

/// Input field style mirroring `InputField` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct InputFieldStyle {
    pub text_bg: Color32,
    pub text_bg_active: Color32,
    pub text_fg: Color32,
    pub text_margins: Margins,
    pub placeholder_fg: Color32,
    pub placeholder_fg_active: Color32,
    pub placeholder_fg_error: Color32,
    pub placeholder_margins: Margins,
    pub placeholder_scale: f32,
    pub placeholder_shift: f32,
    pub placeholder_shift_left: f32,
    pub placeholder_font: FontSpec,
    pub duration_ms: u32,
    pub border_fg: Color32,
    pub border_fg_active: Color32,
    pub border_fg_error: Color32,
    pub border: f32,
    pub border_active: f32,
    pub border_radius: f32,
    pub style: TextStyle,
    pub height_min: f32,
    pub height_max: f32,
    pub width_min: f32,
}

impl InputFieldStyle {
    /// Resolve from any generated `tg_style::structs::InputField`.
    #[must_use]
    pub fn from_tg(s: &tg::InputField, c: &dyn Colors) -> Self {
        let denom = s.border_denominator.max(1) as f32;
        Self {
            text_bg: resolve_color(s.text_bg, c),
            text_bg_active: resolve_color(s.text_bg_active, c),
            text_fg: resolve_color(s.text_fg, c),
            text_margins: Margins::from_tg(s.text_margins),
            placeholder_fg: resolve_color(s.placeholder_fg, c),
            placeholder_fg_active: resolve_color(s.placeholder_fg_active, c),
            placeholder_fg_error: resolve_color(s.placeholder_fg_error, c),
            placeholder_margins: Margins::from_tg(s.placeholder_margins),
            placeholder_scale: s.placeholder_scale as f32,
            placeholder_shift: px(s.placeholder_shift),
            placeholder_shift_left: px(s.placeholder_shift_left),
            placeholder_font: FontSpec::from_tg(s.placeholder_font),
            duration_ms: s.duration.max(0) as u32,
            border_fg: resolve_color(s.border_fg, c),
            border_fg_active: resolve_color(s.border_fg_active, c),
            border_fg_error: resolve_color(s.border_fg_error, c),
            border: px(s.border) / denom,
            border_active: px(s.border_active) / denom,
            border_radius: px(s.border_radius),
            style: TextStyle::from_tg(&s.style),
            height_min: px(s.height_min),
            height_max: px(s.height_max).max(px(s.height_min)),
            width_min: px(s.width_min),
        }
    }

    /// `st::default_input_field` (floating placeholder + bottom animated border).
    #[must_use]
    pub fn default_input(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_input_field, c)
    }

    /// `st::dialogs_filter` (rounded search/filter input field).
    #[must_use]
    pub fn rounded_search(c: &dyn Colors) -> Self {
        Self::from_tg(&st::dialogs_filter, c)
    }
}

/// Scroll area style mirroring `ScrollArea` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct ScrollAreaStyle {
    pub bg: Color32,
    pub bg_over: Color32,
    pub bar_bg: Color32,
    pub bar_bg_over: Color32,
    pub bar_hidden: bool,
    pub round: f32,
    pub width: f32,
    pub min_height: f32,
    pub deltax: f32,
    pub deltat: f32,
    pub deltab: f32,
    pub topsh: f32,
    pub bottomsh: f32,
    pub sh_color: Color32,
    pub duration_ms: u32,
    /// Delay in ms before auto-hiding (`0` means always visible).
    pub hiding_ms: u32,
}

impl ScrollAreaStyle {
    /// Resolve from any generated `tg_style::structs::ScrollArea`.
    #[must_use]
    pub fn from_tg(s: &tg::ScrollArea, c: &dyn Colors) -> Self {
        Self {
            bg: resolve_color(s.bg, c),
            bg_over: resolve_color(s.bg_over, c),
            bar_bg: resolve_color(s.bar_bg, c),
            bar_bg_over: resolve_color(s.bar_bg_over, c),
            bar_hidden: s.bar_hidden,
            round: px(s.round),
            width: px(s.width),
            min_height: px(s.min_height),
            deltax: px(s.deltax),
            deltat: px(s.deltat),
            deltab: px(s.deltab),
            topsh: px(s.topsh),
            bottomsh: px(s.bottomsh),
            sh_color: resolve_color(s.sh_color, c),
            duration_ms: s.duration.max(0) as u32,
            hiding_ms: s.hiding.max(0) as u32,
        }
    }

    /// `st::default_scroll_area` (auto-hides after 1000ms).
    #[must_use]
    pub fn default_scroll(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_scroll_area, c)
    }

    /// `st::default_solid_scroll` (always visible, used in boxes).
    #[must_use]
    pub fn solid(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_solid_scroll, c)
    }
}

/// Box shadow style mirroring `BoxShadow` in `widgets.style`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadowStyle {
    pub blur_radius: f32,
    pub offset: Point,
    pub opacity: f32,
}

impl BoxShadowStyle {
    /// Resolve from any generated `tg_style::structs::BoxShadow`.
    #[must_use]
    pub fn from_tg(s: &tg::BoxShadow) -> Self {
        Self {
            blur_radius: px(s.blur_radius),
            offset: Point::from_tg(s.offset),
            opacity: s.opacity as f32,
        }
    }

    /// `st::default_box_shadow` (blur 5px, offset (0, 1), opacity 0.25 at 100%).
    #[must_use]
    pub fn default_shadow() -> Self {
        Self::from_tg(&st::default_box_shadow)
    }

    /// Deeper shadow for modal boxes (`boxRoundShadow`-like).
    #[must_use]
    pub const fn modal_shadow() -> Self {
        Self {
            blur_radius: 12.0,
            offset: Point::new(0.0, 3.0),
            opacity: 0.28,
        }
    }
}

/// Media slider style mirroring `MediaSlider` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct MediaSliderStyle {
    pub width: f32,
    pub active_fg: Color32,
    pub inactive_fg: Color32,
    pub active_fg_over: Color32,
    pub inactive_fg_over: Color32,
    pub active_fg_disabled: Color32,
    pub inactive_fg_disabled: Color32,
    pub received_till_fg: Color32,
    pub seek_size: Size,
    pub duration_ms: u32,
    pub border_fg: Color32,
    pub border_width: f32,
}

impl MediaSliderStyle {
    /// Resolve from any generated `tg_style::structs::MediaSlider`.
    #[must_use]
    pub fn from_tg(s: &tg::MediaSlider, c: &dyn Colors) -> Self {
        Self {
            width: px(s.width),
            active_fg: resolve_color(s.active_fg, c),
            inactive_fg: resolve_color(s.inactive_fg, c),
            active_fg_over: resolve_color(s.active_fg_over, c),
            inactive_fg_over: resolve_color(s.inactive_fg_over, c),
            active_fg_disabled: resolve_color(s.active_fg_disabled, c),
            inactive_fg_disabled: resolve_color(s.inactive_fg_disabled, c),
            received_till_fg: resolve_color(s.received_till_fg, c),
            seek_size: Size::from_tg(s.seek_size),
            duration_ms: s.duration.max(0) as u32,
            border_fg: resolve_color(s.border_fg, c),
            border_width: px(s.border_width),
        }
    }

    /// `st::default_continuous_slider`.
    #[must_use]
    pub fn default_media_slider(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_continuous_slider, c)
    }
}

/// Filled slider style mirroring `FilledSlider` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct FilledSliderStyle {
    pub full_width: f32,
    pub line_width: f32,
    pub active_fg: Color32,
    pub inactive_fg: Color32,
    pub disabled_fg: Color32,
    pub duration_ms: u32,
}

impl FilledSliderStyle {
    /// Resolve from any generated `tg_style::structs::FilledSlider`.
    #[must_use]
    pub fn from_tg(s: &tg::FilledSlider, c: &dyn Colors) -> Self {
        Self {
            full_width: px(s.full_width),
            line_width: px(s.line_width),
            active_fg: resolve_color(s.active_fg, c),
            inactive_fg: resolve_color(s.inactive_fg, c),
            disabled_fg: resolve_color(s.disabled_fg, c),
            duration_ms: s.duration.max(0) as u32,
        }
    }

    /// `st::media_player_playback` (expands on hover).
    #[must_use]
    pub fn default_filled_slider(c: &dyn Colors) -> Self {
        Self::from_tg(&st::media_player_playback, c)
    }
}

/// Settings / tabs discrete slider style mirroring `SettingsSlider` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct SettingsSliderStyle {
    pub padding: f32,
    pub height: f32,
    pub bar_top: f32,
    pub bar_skip: f32,
    pub bar_stroke: f32,
    pub bar_radius: f32,
    pub bar_fg: Color32,
    pub bar_fg_active: Color32,
    pub bar_snap_to_label: bool,
    pub label_top: f32,
    pub label_style: TextStyle,
    pub label_fg: Color32,
    pub label_fg_active: Color32,
    pub duration_ms: u32,
    pub ripple_bg: Color32,
    pub ripple_bg_active: Color32,
    pub ripple: RippleAnimationStyle,
}

impl SettingsSliderStyle {
    /// Resolve from any generated `tg_style::structs::SettingsSlider`.
    #[must_use]
    pub fn from_tg(s: &tg::SettingsSlider, c: &dyn Colors) -> Self {
        Self {
            padding: px(s.padding),
            height: px(s.height),
            bar_top: px(s.bar_top),
            bar_skip: px(s.bar_skip),
            bar_stroke: px(s.bar_stroke),
            bar_radius: px(s.bar_radius),
            bar_fg: resolve_color(s.bar_fg, c),
            bar_fg_active: resolve_color(s.bar_fg_active, c),
            bar_snap_to_label: s.bar_snap_to_label,
            label_top: px(s.label_top),
            label_style: TextStyle::from_tg(&s.label_style),
            label_fg: resolve_color(s.label_fg, c),
            label_fg_active: resolve_color(s.label_fg_active, c),
            duration_ms: s.duration.max(0) as u32,
            ripple_bg: resolve_color(s.ripple_bg, c),
            ripple_bg_active: resolve_color(s.ripple_bg_active, c),
            ripple: RippleAnimationStyle::from_tg(&s.ripple, c),
        }
    }

    /// `st::default_settings_slider`.
    #[must_use]
    pub fn default_settings_slider(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_settings_slider, c)
    }

    /// `st::default_tabs_slider`.
    #[must_use]
    pub fn default_tabs_slider(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_tabs_slider, c)
    }
}

/// Menu separator style mirroring `MenuSeparator` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct MenuSeparatorStyle {
    pub padding: Margins,
    pub width: f32,
    pub fg: Color32,
}

impl MenuSeparatorStyle {
    /// Resolve from any generated `tg_style::structs::MenuSeparator`.
    #[must_use]
    pub fn from_tg(s: &tg::MenuSeparator, c: &dyn Colors) -> Self {
        Self {
            padding: Margins::from_tg(s.padding),
            width: px(s.width),
            fg: resolve_color(s.fg, c),
        }
    }

    /// `st::default_menu_separator`.
    #[must_use]
    pub fn default_separator(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_menu_separator, c)
    }
}

/// Menu style mirroring `Menu` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct MenuStyle {
    pub skip: f32,
    pub item_bg: Color32,
    pub item_bg_over: Color32,
    pub item_fg: Color32,
    pub item_fg_over: Color32,
    pub item_fg_disabled: Color32,
    pub item_fg_attention: Color32,
    /// `boxTextFgGood`: the green "Unmute" row of the mute menu (`MuteItem`).
    pub item_fg_good: Color32,
    pub item_fg_shortcut: Color32,
    pub item_fg_shortcut_over: Color32,
    pub item_fg_shortcut_disabled: Color32,
    pub item_icon_fg: Color32,
    pub item_icon_fg_over: Color32,
    pub item_padding: Margins,
    pub item_right_skip: f32,
    pub item_style: TextStyle,
    pub separator: MenuSeparatorStyle,
    pub width_min: f32,
    pub width_max: f32,
    pub ripple: RippleAnimationStyle,
}

impl MenuStyle {
    /// Resolve from any generated `tg_style::structs::Menu`.
    #[must_use]
    pub fn from_tg(s: &tg::Menu, c: &dyn Colors) -> Self {
        Self {
            skip: px(s.skip),
            item_bg: resolve_color(s.item_bg, c),
            item_bg_over: resolve_color(s.item_bg_over, c),
            item_fg: resolve_color(s.item_fg, c),
            item_fg_over: resolve_color(s.item_fg_over, c),
            item_fg_disabled: resolve_color(s.item_fg_disabled, c),
            item_fg_attention: resolve_color(st::menu_icon_attention_color, c),
            item_fg_good: c.color("boxTextFgGood"),
            item_fg_shortcut: resolve_color(s.item_fg_shortcut, c),
            item_fg_shortcut_over: resolve_color(s.item_fg_shortcut_over, c),
            item_fg_shortcut_disabled: resolve_color(s.item_fg_shortcut_disabled, c),
            item_icon_fg: c.color("menuIconFg"),
            item_icon_fg_over: c.color("menuIconFgOver"),
            item_padding: Margins::from_tg(s.item_padding),
            item_right_skip: px(s.item_right_skip),
            item_style: TextStyle::from_tg(&s.item_style),
            separator: MenuSeparatorStyle::from_tg(&s.separator, c),
            width_min: px(s.width_min),
            width_max: px(s.width_max),
            ripple: RippleAnimationStyle::from_tg(&s.ripple, c),
        }
    }

    /// `st::default_menu`.
    #[must_use]
    pub fn default_menu(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_menu, c)
    }
}

/// Popup menu style mirroring `PopupMenu` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct PopupMenuStyle {
    pub shadow: BoxShadowStyle,
    pub scroll_padding: Margins,
    pub max_height: f32,
    pub menu: MenuStyle,
    pub radius: f32,
    pub duration_ms: u32,
    pub show_duration_ms: u32,
}

impl PopupMenuStyle {
    /// Resolve from any generated `tg_style::structs::PopupMenu`.
    #[must_use]
    pub fn from_tg(s: &tg::PopupMenu, c: &dyn Colors) -> Self {
        Self {
            shadow: BoxShadowStyle::from_tg(&s.shadow),
            scroll_padding: Margins::from_tg(s.scroll_padding),
            max_height: px(s.max_height),
            menu: MenuStyle::from_tg(&s.menu, c),
            radius: px(s.radius),
            duration_ms: s.duration.max(0) as u32,
            show_duration_ms: s.show_duration.max(0) as u32,
        }
    }

    /// `st::default_popup_menu`.
    #[must_use]
    pub fn default_popup(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_popup_menu, c)
    }
}

/// Toast notification style mirroring `Toast` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct ToastStyle {
    pub bg: Color32,
    pub fg: Color32,
    pub style: TextStyle,
    pub palette: TextPalette,
    pub padding: Margins,
    pub margin: Margins,
    pub min_width: f32,
    pub max_width: f32,
    pub radius: f32,
    pub duration_fade_in_ms: u32,
    pub duration_fade_out_ms: u32,
    pub duration_slide_ms: u32,
}

impl ToastStyle {
    /// Resolve from any generated `tg_style::structs::Toast`.
    #[must_use]
    pub fn from_tg(s: &tg::Toast, c: &dyn Colors) -> Self {
        Self {
            bg: c.color("toastBg"),
            fg: c.color("toastFg"),
            style: TextStyle::from_tg(&s.style),
            palette: TextPalette::from_tg(&s.palette, c),
            padding: Margins::from_tg(s.padding),
            margin: Margins::from_tg(s.margin),
            min_width: px(s.min_width),
            max_width: px(s.max_width),
            radius: px(s.radius),
            duration_fade_in_ms: s.duration_fade_in.max(0) as u32,
            duration_fade_out_ms: s.duration_fade_out.max(0) as u32,
            duration_slide_ms: s.duration_slide.max(0) as u32,
        }
    }

    /// `st::default_toast`.
    #[must_use]
    pub fn default_toast(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_toast, c)
    }
}

/// Classic hover tooltip style mirroring `Tooltip` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct TooltipStyle {
    pub text_bg: Color32,
    pub text_fg: Color32,
    pub text_style: TextStyle,
    pub text_border: Color32,
    pub text_padding: Margins,
    pub shift: Point,
    pub skip: f32,
    pub width_max: f32,
    pub lines_max: usize,
}

impl TooltipStyle {
    /// Resolve from any generated `tg_style::structs::Tooltip`.
    #[must_use]
    pub fn from_tg(s: &tg::Tooltip, c: &dyn Colors) -> Self {
        Self {
            text_bg: resolve_color(s.text_bg, c),
            text_fg: resolve_color(s.text_fg, c),
            text_style: TextStyle::from_tg(&s.text_style),
            text_border: resolve_color(s.text_border, c),
            text_padding: Margins::from_tg(s.text_padding),
            shift: Point::from_tg(s.shift),
            skip: px(s.skip),
            width_max: px(s.width_max),
            lines_max: s.lines_max.max(1) as usize,
        }
    }

    /// `st::default_tooltip`.
    #[must_use]
    pub fn default_tooltip(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_tooltip, c)
    }
}

/// Callout tooltip style mirroring `ImportantTooltip` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct ImportantTooltipStyle {
    pub bg: Color32,
    pub fg: Color32,
    pub margin: Margins,
    pub padding: Margins,
    pub radius: f32,
    pub arrow: f32,
    pub shift: f32,
    pub duration_ms: u32,
}

impl ImportantTooltipStyle {
    /// Resolve from any generated `tg_style::structs::ImportantTooltip`.
    #[must_use]
    pub fn from_tg(s: &tg::ImportantTooltip, c: &dyn Colors) -> Self {
        Self {
            bg: resolve_color(s.bg, c),
            fg: resolve_color(st::default_important_tooltip_label.text_fg, c),
            margin: Margins::from_tg(s.margin),
            padding: Margins::from_tg(s.padding),
            radius: px(s.radius),
            arrow: px(s.arrow),
            shift: px(s.shift),
            duration_ms: s.duration.max(0) as u32,
        }
    }

    /// `st::default_important_tooltip`.
    #[must_use]
    pub fn default_important(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_important_tooltip, c)
    }
}

/// Modal dialog box style mirroring `Box` in `layers.style`.
#[derive(Debug, Clone, Copy)]
pub struct BoxStyle {
    pub button_padding: Margins,
    pub button_height: f32,
    pub button: RoundButtonStyle,
    pub attention_button: RoundButtonStyle,
    pub margin: Margins,
    pub content_padding: Margins,
    pub title: FlatLabelStyle,
    pub bg: Color32,
    pub layer_bg: Color32,
    pub title_additional_fg: Color32,
    pub title_height: f32,
    pub radius: f32,
    pub duration_ms: u32,
    pub shadow: BoxShadowStyle,
}

impl BoxStyle {
    /// Resolve from any generated `tg_style::structs::Box`.
    #[must_use]
    pub fn from_tg(s: &tg::Box, c: &dyn Colors) -> Self {
        Self {
            button_padding: Margins::from_tg(s.button_padding),
            button_height: px(s.button_height),
            button: RoundButtonStyle::from_tg(&s.button, c),
            attention_button: RoundButtonStyle::attention_box(c),
            margin: Margins::from_tg(s.margin),
            content_padding: Margins::from_tg(st::box_padding),
            title: FlatLabelStyle::from_tg(&s.title, c),
            bg: resolve_color(s.bg, c),
            layer_bg: c.color("layerBg"),
            title_additional_fg: resolve_color(s.title_additional_fg, c),
            title_height: px(st::box_title_height),
            radius: px(st::box_radius),
            duration_ms: st::box_duration.max(0) as u32,
            shadow: BoxShadowStyle::modal_shadow(),
        }
    }

    /// `st::default_box`.
    #[must_use]
    pub fn default_box(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_box, c)
    }
}

/// Custom Windows title bar style mirroring `WindowTitle` in `widgets.style`.
#[derive(Debug, Clone, Copy)]
pub struct WindowTitleStyle {
    pub height: f32,
    pub button_width: f32,
    pub bg: Color32,
    pub bg_active: Color32,
    pub fg: Color32,
    pub fg_active: Color32,
    pub shadow: bool,
    pub shadow_fg: Color32,
    pub button_bg: Color32,
    pub button_bg_over: Color32,
    pub button_fg: Color32,
    pub button_fg_over: Color32,
    pub close_bg_over: Color32,
    pub close_fg_over: Color32,
    pub font: FontSpec,
}

impl WindowTitleStyle {
    /// Resolve from any generated `tg_style::structs::WindowTitle`.
    #[must_use]
    pub fn from_tg(s: &tg::WindowTitle, c: &dyn Colors) -> Self {
        Self {
            height: px(s.height),
            button_width: px(s.minimize.width),
            bg: resolve_color(s.bg, c),
            bg_active: resolve_color(s.bg_active, c),
            fg: resolve_color(s.fg, c),
            fg_active: resolve_color(s.fg_active, c),
            shadow: s.shadow,
            shadow_fg: c.color("titleShadow"),
            button_bg: c.color("titleButtonBg"),
            button_bg_over: c.color("titleButtonBgOver"),
            button_fg: c.color("titleButtonFg"),
            button_fg_over: c.color("titleButtonFgOver"),
            close_bg_over: c.color("titleButtonCloseBgOver"),
            close_fg_over: c.color("titleButtonCloseFgOver"),
            font: FontSpec::from_tg(s.style.font),
        }
    }

    /// `st::default_window_title`.
    #[must_use]
    pub fn default_title(c: &dyn Colors) -> Self {
        Self::from_tg(&st::default_window_title, c)
    }
}

/// Unread counter badge style mirroring `UnreadBadgeStyle` in `unread_badge_paint.h`
/// and `dialogs.style`.
#[derive(Debug, Clone, Copy)]
pub struct UnreadBadgeStyle {
    pub align: Align,
    pub active: bool,
    pub selected: bool,
    pub muted: bool,
    pub size: f32,
    pub padding: f32,
    pub font: FontSpec,
}

impl Default for UnreadBadgeStyle {
    fn default() -> Self {
        Self {
            align: Align::Right,
            active: false,
            selected: false,
            muted: false,
            size: px(st::dialogs_unread_height),
            padding: px(st::dialogs_unread_padding),
            font: FontSpec::from_tg(st::dialogs_unread_font),
        }
    }
}

/// Convenience helper for obtaining a scaled `egui::FontId` from any `tg_style::Font`.
#[inline]
#[must_use]
pub fn tg_font_id(font: TgFont) -> FontId {
    font_id_from_style(font)
}
