//! Widget message data model, payload decompression, layout sizing, theme tokens, and shared
//! public-data polling registry.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_media.h/.cpp`, `tgw.style`, `tgw_theme.h/.cpp`,
//! `tgw_state.h/.cpp`, `tgw_runtime.h`, and `history/view/media/history_view_html_widget.cpp`.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use flate2::Compression;
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::ZlibEncoder;
use serde::{Deserialize, Serialize};
use tg_theme::Color;

use crate::error::{Result, TgwError};

/// Default placeholder height before the widget reports its height (`tgw.style:10`).
pub const TGW_PLACEHOLDER_HEIGHT: i32 = 160;

/// Minimum widget width in CSS pixels (`tgw_renderer.h:40`).
pub const TGW_MIN_WIDTH: f64 = 200.0;

/// Minimum widget height in CSS pixels (`tgw_renderer.h:41`).
pub const TGW_MIN_HEIGHT: f64 = 50.0;

/// Maximum widget height in CSS pixels (`tgw_renderer.h:42`).
pub const TGW_MAX_HEIGHT: f64 = 600.0;

/// Maximum time the host waits for a gesture claim or menu decision (`tgw_renderer.h:44`).
pub const ANSWER_DEADLINE_MS: u64 = 150;

/// Maximum corner-info safe area height inset (`history_view_html_widget.cpp:35`).
pub const MAX_SAFE_AREA_INSET: i32 = 64;

/// Maximum corner-info safe area width inset (`history_view_html_widget.cpp:36`).
pub const MAX_SAFE_AREA_CORNER: i32 = 200;

/// Default shared public-data poll period in milliseconds (`tgw_state.cpp:23`).
pub const DEFAULT_PUBLIC_DATA_POLL_MS: u64 = 30_000;

/// Minimum shared public-data poll period in seconds (`tgw_state.cpp:24`).
pub const MIN_PUBLIC_DATA_POLL_SECS: i32 = 5;

/// Maximum shared public-data poll period in seconds (`tgw_state.cpp:25`).
pub const MAX_PUBLIC_DATA_POLL_SECS: i32 = 3_600;

/// Safety cap for unpacked HTML payloads (16 MiB) to guard against decompression bombs.
pub const MAX_UNPACKED_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Widget layout anchor dimensions (`Tgw::Anchors` in `tgw_media.h:14-19`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Anchors {
    /// Requested width in CSS pixels (`0` means auto/full bubble width).
    pub width: i32,
    /// Requested height in CSS pixels (`0` means auto-height driven by content).
    pub height: i32,
}

/// Capability flags carried by `messageMediaWidget` (`Tgw::WidgetFlags` in `tgw_media.h:21-27`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WidgetFlags {
    /// Whether this widget message is a forwarded/copied snapshot.
    pub is_copy: bool,
    /// Whether the widget may invoke interactive bot queries (`messages.getBotWidgetQueryAnswer`).
    pub can_query: bool,
    /// Whether the widget may perform background bot queries.
    pub background_query: bool,
}

/// Server-backed state blobs attached to a widget message (`Tgw::WidgetState` in `tgw_media.h:29-36`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WidgetState {
    /// Per-chat widget JSON/text data (`chat_data`).
    pub chat_data: Option<Vec<u8>>,
    /// Shared cross-chat widget JSON/text data (`public_data`).
    pub public_data: Option<Vec<u8>>,
}

/// Complete parsed fields of `messageMediaWidget` (`Tgw::WidgetFields` in `tgw_media.h:38-49`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WidgetFields {
    /// Preferred anchor dimensions (`width`, `height`).
    pub anchors: Anchors,
    /// Compressed (zlib or legacy gzip) HTML payload bytes.
    pub payload: Vec<u8>,
    /// Current per-chat and public data state blobs.
    pub state: WidgetState,
    /// Hash of `state.public_data` for conditional polling (`messages.getWidgetPublicData`).
    pub public_data_hash: Option<i64>,
    /// Shared widget identifier across messages (`widget_id`).
    pub widget_id: Option<i64>,
    /// Message capability flags.
    pub flags: WidgetFlags,
}

/// Incremental update to a widget's server state (`Tgw::WidgetStateEdit` in `tgw_media.h:51-55`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WidgetStateEdit {
    /// Updated per-chat data, if present in the update.
    pub chat_data: Option<Vec<u8>>,
    /// Updated shared public data, if present in the update.
    pub public_data: Option<Vec<u8>>,
    /// Updated public-data hash, if present in the update.
    pub public_data_hash: Option<i64>,
}

/// Chat kind exposed to the sandboxed widget (`Tgw::ChatType` in `tgw_media.h:57-63`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatType {
    /// Hidden (e.g. forwarded copy).
    #[default]
    None,
    /// 1:1 private chat or bot DM.
    Private,
    /// Basic group chat.
    Group,
    /// Supergroup / megagroup.
    Supergroup,
    /// Broadcast channel.
    Channel,
}

impl ChatType {
    /// Canonical string name exposed to widget SDK (`ChatTypeName` in `tgw_runtime.cpp`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Private => "private",
            Self::Group => "group",
            Self::Supergroup => "supergroup",
            Self::Channel => "channel",
        }
    }
}

/// Message execution context passed to the widget runtime (`Tgw::MessageContext` in `tgw_media.h:65-71`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageContext {
    /// Effective capability flags (non-history items have `can_query`/`background_query` cleared).
    pub flags: WidgetFlags,
    /// Chat type (`ChatType::None` when `flags.is_copy` is set).
    pub chat_type: ChatType,
    /// Whether TON wallet integration is available and permitted for this message.
    pub wallet: bool,
}

/// Origin of a widget height change (`Tgw::HeightChange` in `tgw_runtime.h:43-46`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HeightChange {
    /// Triggered by the widget's own content layout.
    Own,
    /// Triggered by client resize / width change.
    #[default]
    Client,
}

/// Anchor edge preserved when a widget changes height (`Tgw::ResizeAnchor` in `tgw_runtime.h:48-51`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResizeAnchor {
    /// Keep top edge fixed (default).
    #[default]
    Top,
    /// Keep bottom edge fixed.
    Bottom,
}

/// Recorded height layout version (`Tgw::HeightLayout` in `tgw_runtime.h:53-58`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeightLayout {
    /// Monotonic serial incremented on each layout update.
    pub serial: u64,
    /// Height in logical pixels.
    pub height: i32,
    /// Origin of this height change.
    pub change: HeightChange,
    /// Anchor edge preserved during resize.
    pub anchor: ResizeAnchor,
}

/// Safe-area insets reserved for the overlaid message timestamp pill (`Tgw::ContentSafeArea`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentSafeArea {
    /// Bottom inset in CSS pixels.
    pub bottom: i32,
    /// Bottom-left corner inset in CSS pixels (RTL).
    pub corner_left: i32,
    /// Bottom-right corner inset in CSS pixels (LTR).
    pub corner_right: i32,
}

/// Unpacks a widget payload compressed with zlib (current server format) or gzip (legacy format).
///
/// Faithful port of `Tgw::UnpackPayload` (`tgw_media.cpp:56-99`), which calls
/// `inflateInit2(&stream, 32 + MAX_WBITS)` and rejects empty, truncated, or corrupt streams.
pub fn unpack_payload(payload: &[u8]) -> Result<Vec<u8>> {
    if payload.is_empty() {
        return Err(TgwError::InvalidPayload("empty widget payload".to_owned()));
    }

    let is_gzip = payload.len() >= 2 && payload[0] == 0x1F && payload[1] == 0x8B;
    let mut out = Vec::with_capacity(16 * 1024);

    if is_gzip {
        let mut decoder = GzDecoder::new(std::io::Cursor::new(payload));
        let mut limited = (&mut decoder).take((MAX_UNPACKED_PAYLOAD_BYTES + 1) as u64);
        limited
            .read_to_end(&mut out)
            .map_err(|e| TgwError::InvalidPayload(format!("gzip inflate failed: {e}")))?;
        if out.len() > MAX_UNPACKED_PAYLOAD_BYTES {
            return Err(TgwError::InvalidPayload(format!(
                "unpacked payload exceeds {MAX_UNPACKED_PAYLOAD_BYTES} bytes"
            )));
        }
        // Verify the entire input buffer was consumed (no trailing garbage).
        if decoder.into_inner().position() as usize != payload.len() {
            return Err(TgwError::InvalidPayload(
                "trailing bytes after gzip stream".to_owned(),
            ));
        }
    } else {
        let mut decoder = ZlibDecoder::new(payload);
        let mut limited = (&mut decoder).take((MAX_UNPACKED_PAYLOAD_BYTES + 1) as u64);
        limited
            .read_to_end(&mut out)
            .map_err(|e| TgwError::InvalidPayload(format!("zlib inflate failed: {e}")))?;
        if out.len() > MAX_UNPACKED_PAYLOAD_BYTES {
            return Err(TgwError::InvalidPayload(format!(
                "unpacked payload exceeds {MAX_UNPACKED_PAYLOAD_BYTES} bytes"
            )));
        }
        if decoder.total_in() as usize != payload.len() {
            return Err(TgwError::InvalidPayload(
                "trailing bytes after zlib stream".to_owned(),
            ));
        }
    }

    if out.is_empty() {
        return Err(TgwError::InvalidPayload(
            "decompressed widget payload is empty".to_owned(),
        ));
    }

    Ok(out)
}

/// Compresses raw HTML bytes into the server's zlib widget payload format.
#[must_use]
pub fn pack_payload_zlib(html: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = enc.write_all(html);
    enc.finish().unwrap_or_default()
}

/// Extracts the `<title>...</title>` text from an HTML widget payload, if present.
#[must_use]
pub fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start_tag = lower.find("<title")?;
    let after_open = html[start_tag..].find('>')? + start_tag + 1;
    let end_tag = lower[after_open..].find("</title>")? + after_open;
    let title = html[after_open..end_tag].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(120).collect())
    }
}

/// Extracts a short plain-text summary from an HTML widget body (stripping `<style>`/`<script>`/tags).
#[must_use]
pub fn extract_html_text_preview(html: &str, max_chars: usize) -> String {
    let mut cleaned = String::with_capacity(html.len().min(max_chars * 2));
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut in_tag = false;
    let mut last_space = true;

    while i < bytes.len() && cleaned.chars().count() < max_chars {
        if !in_tag && lower[i..].starts_with("<script") {
            if let Some(end) = lower[i..].find("</script>") {
                i += end + "</script>".len();
                continue;
            }
            break;
        }
        if !in_tag && lower[i..].starts_with("<style") {
            if let Some(end) = lower[i..].find("</style>") {
                i += end + "</style>".len();
                continue;
            }
            break;
        }
        if !in_tag && lower[i..].starts_with("<title") {
            if let Some(end) = lower[i..].find("</title>") {
                i += end + "</title>".len();
                continue;
            }
            break;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        i += ch.len_utf8();
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !last_space && !cleaned.is_empty() {
                    cleaned.push(' ');
                    last_space = true;
                }
            }
            _ if in_tag => {}
            c if c.is_whitespace() => {
                if !last_space && !cleaned.is_empty() {
                    cleaned.push(' ');
                    last_space = true;
                }
            }
            c => {
                cleaned.push(c);
                last_space = false;
            }
        }
    }

    cleaned.trim().to_owned()
}

/// Domain wrapper around [`WidgetFields`] with lazy HTML unpacking and state application.
///
/// Port of `Tgw::WidgetMedia` (`tgw_media.h:77-107`, `tgw_media.cpp:126-251`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetMedia {
    fields: WidgetFields,
    unpacked_html: Option<Vec<u8>>,
}

impl WidgetMedia {
    /// Creates a new `WidgetMedia` from parsed fields.
    #[must_use]
    pub fn new(fields: WidgetFields) -> Self {
        Self {
            fields,
            unpacked_html: None,
        }
    }

    /// Returns the underlying widget fields.
    #[must_use]
    pub fn fields(&self) -> &WidgetFields {
        &self.fields
    }

    /// Decompresses (caching on first success) and returns the raw HTML bytes.
    pub fn html(&mut self) -> Result<&[u8]> {
        if self.unpacked_html.is_none() {
            let decoded = unpack_payload(&self.fields.payload)?;
            self.unpacked_html = Some(decoded);
        }
        Ok(self.unpacked_html.as_deref().unwrap_or(&[]))
    }

    /// Builds the execution [`MessageContext`] for a message in `chat_type`.
    #[must_use]
    pub fn context(
        &self,
        is_history_entry: bool,
        chat_type: ChatType,
        wallet_available: bool,
    ) -> MessageContext {
        let mut flags = self.fields.flags;
        if !is_history_entry {
            flags.can_query = false;
            flags.background_query = false;
        }
        MessageContext {
            flags,
            chat_type: if flags.is_copy {
                ChatType::None
            } else {
                chat_type
            },
            wallet: flags.can_query && wallet_available,
        }
    }

    /// Applies an incremental [`WidgetStateEdit`], returning `true` if any field changed.
    ///
    /// Port of `WidgetMedia::applyState` (`tgw_media.cpp:176-191`).
    pub fn apply_state(&mut self, edit: &WidgetStateEdit) -> bool {
        let mut changed = false;
        if let Some(ref chat_data) = edit.chat_data
            && self.fields.state.chat_data.as_deref() != Some(chat_data.as_slice())
        {
            self.fields.state.chat_data = Some(chat_data.clone());
            changed = true;
        }
        if let Some(ref public_data) = edit.public_data
            && (self.fields.state.public_data.as_deref() != Some(public_data.as_slice())
                || self.fields.public_data_hash != edit.public_data_hash)
        {
            self.fields.state.public_data = Some(public_data.clone());
            self.fields.public_data_hash = edit.public_data_hash;
            changed = true;
        }
        changed
    }

    /// Computes optimal card width clamped to `[TGW_MIN_WIDTH, max_bubble_width]`
    /// (`HtmlWidget::optimalCardWidth` in `history_view_html_widget.cpp:276-285`).
    #[must_use]
    pub fn optimal_card_width(&self, max_bubble_width: i32) -> i32 {
        if self.fields.anchors.width > 0 {
            let max_css = f64::from(max_bubble_width.max(TGW_MIN_WIDTH as i32));
            let clamped = f64::from(self.fields.anchors.width).clamp(TGW_MIN_WIDTH, max_css);
            (clamped.round() as i32).min(max_bubble_width)
        } else {
            max_bubble_width
        }
    }

    /// Computes maximum CSS height (`HtmlWidget::maxCssHeight` in `history_view_html_widget.cpp:295-302`).
    #[must_use]
    pub fn max_css_height(&self) -> f64 {
        if self.fields.anchors.height > 0 {
            f64::from(self.fields.anchors.height).clamp(TGW_MIN_HEIGHT, TGW_MAX_HEIGHT)
        } else {
            TGW_MAX_HEIGHT
        }
    }

    /// Computes logical card height (`HtmlWidget::cardHeight` in `history_view_html_widget.cpp:287-293`).
    #[must_use]
    pub fn card_height(&self, runtime_height: Option<i32>) -> i32 {
        if self.fields.anchors.height > 0 {
            self.max_css_height().round() as i32
        } else {
            runtime_height.unwrap_or(TGW_PLACEHOLDER_HEIGHT)
        }
    }
}

// =============================================================================
// Theme tokens (`tgw_theme.h/.cpp`)
// =============================================================================

/// Button styling tokens passed into the widget sandbox (`twidget::WidgetButtonTokens`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetButtonTokens {
    /// Background color hex (`#rrggbb`).
    pub bg: String,
    /// Text color hex (`#rrggbb`).
    pub text: String,
    /// Border radius CSS length (e.g. `"6px"`).
    pub radius: String,
    /// CSS shorthand font token (e.g. `"600 14px/19px system-ui, sans-serif"`).
    pub font: String,
    /// CSS padding shorthand.
    pub padding: String,
    /// CSS height length.
    pub height: String,
    /// Pressed opacity (`1.0` in tdesktop).
    pub pressed_opacity: f64,
    /// Disabled opacity (`1.0` in tdesktop).
    pub disabled_opacity: f64,
}

/// Text styling tokens passed into the widget sandbox (`twidget::WidgetTextTokens`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetTextTokens {
    /// Base font size in CSS pixels, clamped to `[8.0, 64.0]`.
    pub font_size: f64,
    /// Unitless line height, clamped to `[1.0, 3.0]`.
    pub line_height: f64,
    /// Message bubble padding CSS shorthand.
    pub padding: String,
}

/// Complete theme payload injected into a sandboxed widget (`tgw_theme.cpp:183-252`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetTheme {
    /// `"dark"` or `"light"`.
    pub color_scheme: String,
    /// 15 standard Telegram theme color parameters (`bg_color`, `text_color`, ...).
    pub params: BTreeMap<String, String>,
    /// Active button tokens.
    pub button: WidgetButtonTokens,
    /// Message text tokens.
    pub text: WidgetTextTokens,
}

/// Bubble color inputs for [`build_bubble_theme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BubbleThemeColors {
    /// Bubble background color (possibly translucent).
    pub bubble_bg: Color,
    /// Underlying window/wallpaper color used to flatten translucent colors.
    pub under_bg: Color,
    /// Primary message text color.
    pub text_fg: Color,
    /// Muted hint / date text color.
    pub hint_fg: Color,
    /// Link text color.
    pub link_fg: Color,
    /// Active button fill color.
    pub button_bg: Color,
    /// Active button label color.
    pub button_fg: Color,
    /// Reply bar / secondary background tint color.
    pub reply_bar: Color,
    /// Accent / service text color.
    pub accent_fg: Color,
    /// Subtitle / media secondary text color.
    pub subtitle_fg: Color,
    /// Destructive action text color.
    pub destructive_fg: Color,
}

impl BubbleThemeColors {
    /// Standard light-mode bubble theme colors.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            bubble_bg: Color::rgb(0xFF, 0xFF, 0xFF),
            under_bg: Color::rgb(0xE7, 0xEB, 0xF0),
            text_fg: Color::rgb(0x00, 0x00, 0x00),
            hint_fg: Color::rgb(0x99, 0x99, 0x99),
            link_fg: Color::rgb(0x16, 0x8A, 0xCD),
            button_bg: Color::rgb(0x40, 0xA7, 0xE3),
            button_fg: Color::rgb(0xFF, 0xFF, 0xFF),
            reply_bar: Color::rgb(0x40, 0xA7, 0xE3),
            accent_fg: Color::rgb(0x16, 0x8A, 0xCD),
            subtitle_fg: Color::rgb(0x70, 0x75, 0x79),
            destructive_fg: Color::rgb(0xE5, 0x39, 0x35),
        }
    }

    /// Standard dark-mode bubble theme colors.
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            bubble_bg: Color::rgb(0x18, 0x25, 0x33),
            under_bg: Color::rgb(0x0E, 0x16, 0x21),
            text_fg: Color::rgb(0xF5, 0xF5, 0xF5),
            hint_fg: Color::rgb(0x70, 0x84, 0x99),
            link_fg: Color::rgb(0x6A, 0xB3, 0xF3),
            button_bg: Color::rgb(0x52, 0x88, 0xC1),
            button_fg: Color::rgb(0xFF, 0xFF, 0xFF),
            reply_bar: Color::rgb(0x6A, 0xB3, 0xF3),
            accent_fg: Color::rgb(0x6A, 0xB3, 0xF3),
            subtitle_fg: Color::rgb(0x7F, 0x91, 0xA4),
            destructive_fg: Color::rgb(0xEC, 0x39, 0x42),
        }
    }
}

/// Flattens a possibly translucent `fg` over an opaque `bg` (`Flatten` in `tgw_theme.cpp:36-42`).
#[must_use]
pub fn flatten_color(fg: Color, bg: Color) -> Color {
    let alpha = f64::from(fg.a) / 255.0;
    let lerp = |a: u8, b: u8| -> u8 {
        let v = f64::from(a) + (f64::from(b) - f64::from(a)) * alpha;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color::rgb(lerp(bg.r, fg.r), lerp(bg.g, fg.g), lerp(bg.b, fg.b))
}

fn srgb_luminance(c: Color) -> f64 {
    let channel = |v: u8| -> f64 {
        let x = f64::from(v) / 255.0;
        if x <= 0.03928 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

fn contrast_ratio(a: Color, b: Color) -> f64 {
    let la = srgb_luminance(a);
    let lb = srgb_luminance(b);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Returns `true` when `color` has higher WCAG contrast with white than black (`IsDark` in `tgw_theme.cpp:44-47`).
#[must_use]
pub fn is_dark_color(color: Color) -> bool {
    contrast_ratio(color, Color::rgb(255, 255, 255)) > contrast_ratio(color, Color::rgb(0, 0, 0))
}

fn hex_rgb(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// Builds a full [`WidgetTheme`] from bubble colors (`Tgw::BubbleTheme` in `tgw_theme.cpp:183-228`).
#[must_use]
pub fn build_bubble_theme(colors: &BubbleThemeColors) -> WidgetTheme {
    const SEPARATOR_OPACITY: f64 = 0.3;
    const DEFAULT_BG_OPACITY: u8 = 26; // ~0.10 * 255 (`Ui::kDefaultBgOpacity`)

    let bg = flatten_color(colors.bubble_bg, colors.under_bg);
    let over = |c: Color| hex_rgb(flatten_color(c, bg));
    let button = flatten_color(colors.button_bg, bg);
    let button_text = flatten_color(colors.button_fg, button);

    let secondary = Color::rgba(
        colors.reply_bar.r,
        colors.reply_bar.g,
        colors.reply_bar.b,
        DEFAULT_BG_OPACITY,
    );
    let sep_alpha = (f64::from(colors.hint_fg.a) * SEPARATOR_OPACITY)
        .round()
        .clamp(0.0, 255.0) as u8;
    let separator = Color::rgba(
        colors.hint_fg.r,
        colors.hint_fg.g,
        colors.hint_fg.b,
        sep_alpha,
    );

    let surface = hex_rgb(bg);
    let accent = over(colors.accent_fg);

    let mut params = BTreeMap::new();
    params.insert("bg_color".to_owned(), surface.clone());
    params.insert("text_color".to_owned(), over(colors.text_fg));
    params.insert("hint_color".to_owned(), over(colors.hint_fg));
    params.insert("link_color".to_owned(), over(colors.link_fg));
    params.insert("button_color".to_owned(), hex_rgb(button));
    params.insert("button_text_color".to_owned(), hex_rgb(button_text));
    params.insert("secondary_bg_color".to_owned(), over(secondary));
    params.insert("accent_text_color".to_owned(), accent.clone());
    params.insert("subtitle_text_color".to_owned(), over(colors.subtitle_fg));
    params.insert(
        "destructive_text_color".to_owned(),
        over(colors.destructive_fg),
    );
    params.insert("section_bg_color".to_owned(), surface.clone());
    params.insert("section_header_text_color".to_owned(), accent);
    params.insert("section_separator_color".to_owned(), over(separator));
    params.insert("header_bg_color".to_owned(), surface.clone());
    params.insert("bottom_bar_bg_color".to_owned(), surface);

    WidgetTheme {
        color_scheme: if is_dark_color(bg) {
            "dark".to_owned()
        } else {
            "light".to_owned()
        },
        params,
        button: WidgetButtonTokens {
            bg: hex_rgb(button),
            text: hex_rgb(button_text),
            radius: "6px".to_owned(),
            font: "600 14px/19px system-ui, sans-serif".to_owned(),
            padding: "8px 16px 8px 16px".to_owned(),
            height: "36px".to_owned(),
            pressed_opacity: 1.0,
            disabled_opacity: 1.0,
        },
        text: WidgetTextTokens {
            font_size: 14.0,
            line_height: 1.357,
            padding: "8px 12px 8px 12px".to_owned(),
        },
    }
}

/// Checks whether two [`WidgetTheme`]s are identical (`Tgw::SameTheme` in `tgw_theme.cpp:230-252`).
#[must_use]
pub fn same_theme(a: &WidgetTheme, b: &WidgetTheme) -> bool {
    a == b
}

// =============================================================================
// Shared public-data poller (`tgw_state.h/.cpp`)
// =============================================================================

/// Result of polling `messages.getWidgetPublicData` (`tgw_state.cpp:108-129`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicDataPollResult {
    /// Server returned `messages.widgetPublicDataNotModified`.
    NotModified {
        /// Clamped poll interval in milliseconds.
        period_ms: u64,
    },
    /// Server returned new `messages.widgetPublicData`.
    Updated {
        /// New data hash.
        hash: i64,
        /// New public data payload.
        data: String,
        /// Clamped poll interval in milliseconds.
        period_ms: u64,
    },
}

/// Clamps a server-supplied poll interval in seconds into `[5s, 3600s]` and converts to ms
/// (`Period` in `tgw_state.cpp:90-93`).
#[must_use]
pub fn clamp_poll_period_ms(seconds: i32) -> u64 {
    let clamped = seconds.clamp(MIN_PUBLIC_DATA_POLL_SECS, MAX_PUBLIC_DATA_POLL_SECS);
    (clamped as u64) * 1_000
}

/// Tracks shared `widget_id` poll schedules, hashes, and carriers (`tgw_state.cpp`).
#[derive(Debug, Default)]
pub struct PublicDataPoller {
    entries: HashMap<i64, PolledWidgetEntry>,
}

#[derive(Debug, Clone)]
struct PolledWidgetEntry {
    hash: Option<i64>,
    data: Option<String>,
    period_ms: u64,
    next_due_ms: u64,
    in_flight: bool,
}

impl PublicDataPoller {
    /// Creates an empty poller.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a watched shared widget if not yet present, scheduling its initial poll.
    pub fn watch(&mut self, widget_id: i64, initial_hash: Option<i64>, now_ms: u64) {
        self.entries
            .entry(widget_id)
            .or_insert_with(|| PolledWidgetEntry {
                hash: initial_hash,
                data: None,
                period_ms: DEFAULT_PUBLIC_DATA_POLL_MS,
                next_due_ms: now_ms.saturating_add(DEFAULT_PUBLIC_DATA_POLL_MS),
                in_flight: false,
            });
    }

    /// Returns shared widget IDs due for polling at `now_ms`, marking them in-flight.
    pub fn take_due(&mut self, now_ms: u64) -> Vec<(i64, i64)> {
        let mut due = Vec::new();
        for (&widget_id, entry) in &mut self.entries {
            if !entry.in_flight && now_ms >= entry.next_due_ms {
                entry.in_flight = true;
                entry.next_due_ms = now_ms.saturating_add(entry.period_ms);
                due.push((widget_id, entry.hash.unwrap_or(0)));
            }
        }
        due
    }

    /// Records a successful poll response for `widget_id`.
    pub fn apply_poll_result(
        &mut self,
        widget_id: i64,
        result: &PublicDataPollResult,
        now_ms: u64,
    ) {
        let entry = self
            .entries
            .entry(widget_id)
            .or_insert_with(|| PolledWidgetEntry {
                hash: None,
                data: None,
                period_ms: DEFAULT_PUBLIC_DATA_POLL_MS,
                next_due_ms: now_ms,
                in_flight: false,
            });
        entry.in_flight = false;
        match result {
            PublicDataPollResult::NotModified { period_ms } => {
                entry.period_ms = *period_ms;
                entry.next_due_ms = now_ms.saturating_add(*period_ms);
            }
            PublicDataPollResult::Updated {
                hash,
                data,
                period_ms,
            } => {
                entry.hash = Some(*hash);
                entry.data = Some(data.clone());
                entry.period_ms = *period_ms;
                entry.next_due_ms = now_ms.saturating_add(*period_ms);
            }
        }
    }

    /// Marks an in-flight poll as failed, keeping the next poll at `now_ms + period_ms`.
    pub fn mark_failed(&mut self, widget_id: i64, now_ms: u64) {
        if let Some(entry) = self.entries.get_mut(&widget_id) {
            entry.in_flight = false;
            entry.next_due_ms = now_ms.saturating_add(entry.period_ms);
        }
    }

    /// Returns the latest cached `(hash, data)` for `widget_id`, if any.
    #[must_use]
    pub fn latest_public_data(&self, widget_id: i64) -> Option<(Option<i64>, &str)> {
        let entry = self.entries.get(&widget_id)?;
        Some((entry.hash, entry.data.as_deref()?))
    }

    /// Returns the earliest next poll timestamp across all watched widgets.
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.entries
            .values()
            .filter(|e| !e.in_flight)
            .map(|e| e.next_due_ms)
            .min()
    }
}
