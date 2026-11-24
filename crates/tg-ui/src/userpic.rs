//! Userpic circles with vertical gradients, peer color index mapping, and initials.
//!
//! Port of `Telegram/SourceFiles/ui/empty_userpic.h`,
//! `Telegram/SourceFiles/ui/empty_userpic.cpp`,
//! `DecideColorIndex` / `ColorIndexToPaletteIndex` in
//! `Telegram/SourceFiles/ui/chat/chat_style.cpp` and `data/data_peer.cpp`.

use std::f32::consts::TAU;

use egui::epaint::{Vertex, WHITE_UV};
use egui::{Align2, Color32, FontFamily, FontId, Mesh, Painter, Pos2, Rect, Shape, Stroke, Vec2};

use crate::anim::color_lerp;
use crate::theme::Colors;

/// Number of base peer colors (`kSimpleColorIndexCount` in `chat_style.h`).
pub const SIMPLE_COLOR_INDEX_COUNT: u8 = 7;

/// Total peer color slots including collectible/extended indices (`kColorIndexCount`).
pub const COLOR_INDEX_COUNT: u8 = 64;

/// Map a bare peer id (user/chat/channel id) to its default color index in `0..7`.
///
/// Exact port of `Ui::DecideColorIndex(uint64 id)` in `chat_style.cpp`.
#[inline]
pub const fn decide_color_index(peer_id: u64) -> u8 {
    (peer_id % SIMPLE_COLOR_INDEX_COUNT as u64) as u8
}

/// Map a peer color index (`0..64`) to its palette index (`0..8`, corresponding
/// to `historyPeer{1..8}UserpicBg` — note palette index 2 / yellow is unused by default).
///
/// Exact port of `Ui::ColorIndexToPaletteIndex(uint8 colorIndex)` in `chat_style.cpp`:
/// `const int8 map[] = { 0, 7, 4, 1, 6, 3, 5 };`
#[inline]
pub const fn color_index_to_palette_index(color_index: u8) -> u8 {
    const MAP: [u8; 7] = [0, 7, 4, 1, 6, 3, 5];
    MAP[(color_index % SIMPLE_COLOR_INDEX_COUNT) as usize]
}

/// Top and bottom vertical gradient colors for an empty userpic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserpicBgColors {
    /// Top gradient stop (`historyPeerNUserpicBg`).
    pub color1: Color32,
    /// Bottom gradient stop (`historyPeerNUserpicBg2`).
    pub color2: Color32,
}

/// Look up the gradient colors for `color_index` from `colors`.
///
/// Exact port of `EmptyUserpic::UserpicColor(uint8 colorIndex)` in `empty_userpic.cpp`.
pub fn userpic_colors(color_index: u8, colors: &dyn Colors) -> UserpicBgColors {
    const KEYS: [(&str, &str); 8] = [
        ("historyPeer1UserpicBg", "historyPeer1UserpicBg2"),
        ("historyPeer2UserpicBg", "historyPeer2UserpicBg2"),
        ("historyPeer3UserpicBg", "historyPeer3UserpicBg2"),
        ("historyPeer4UserpicBg", "historyPeer4UserpicBg2"),
        ("historyPeer5UserpicBg", "historyPeer5UserpicBg2"),
        ("historyPeer6UserpicBg", "historyPeer6UserpicBg2"),
        ("historyPeer7UserpicBg", "historyPeer7UserpicBg2"),
        ("historyPeer8UserpicBg", "historyPeer8UserpicBg2"),
    ];
    let idx = color_index_to_palette_index(color_index) as usize;
    let (k1, k2) = KEYS[idx];
    UserpicBgColors {
        color1: colors.color(k1),
        color2: colors.color(k2),
    }
}

/// Look up the peer display name color for `color_index`.
pub fn peer_name_color(color_index: u8, colors: &dyn Colors) -> Color32 {
    const KEYS: [&str; 8] = [
        "historyPeer1NameFg",
        "historyPeer2NameFg",
        "historyPeer3NameFg",
        "historyPeer4NameFg",
        "historyPeer5NameFg",
        "historyPeer6NameFg",
        "historyPeer7NameFg",
        "historyPeer8NameFg",
    ];
    let idx = color_index_to_palette_index(color_index) as usize;
    colors.color(KEYS[idx])
}

/// Extract 1 or 2 uppercase initials from a peer name, matching `EmptyUserpic::fillString`
/// in `Telegram/SourceFiles/ui/empty_userpic.cpp` (lines 582-642).
///
/// Prefers the second initial after `' '`, falling back to a letter after `'-'`.
pub fn extract_initials(name: &str) -> String {
    let mut letters: Vec<String> = Vec::new();
    let mut levels: Vec<u8> = Vec::new();

    let mut level: u8 = 0;
    let mut letter_found = false;
    let mut chars = name.chars().peekable();

    while let Some(ch) = chars.next() {
        if !letter_found && ch.is_alphanumeric() {
            letter_found = true;
            let mut grapheme = String::new();
            grapheme.push(ch);
            // Attach a following combining mark if present (matches IsDiacritic check).
            if let Some(&next) = chars.peek()
                && is_combining_mark(next)
            {
                grapheme.push(next);
                chars.next();
            }
            letters.push(grapheme);
            levels.push(level);
        } else if ch == ' ' {
            level = 0;
            letter_found = false;
        } else if letter_found && ch == '-' {
            level = 1;
            letter_found = false;
        }
    }

    let mut out = String::new();
    if let Some(first) = letters.first() {
        out.push_str(first);
        let mut best_index = 0usize;
        let mut best_level = 2u8;
        let mut i = letters.len();
        while i > 1 {
            i -= 1;
            if levels[i] < best_level {
                best_index = i;
                best_level = levels[i];
            }
        }
        if best_index > 0 {
            out.push_str(&letters[best_index]);
        }
    }
    out.to_uppercase()
}

fn is_combining_mark(ch: char) -> bool {
    matches!(ch as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Avatar placeholder with vertical gradient background and centered initials.
#[derive(Debug, Clone)]
pub struct EmptyUserpic {
    colors: UserpicBgColors,
    initials: String,
    fg: Color32,
}

impl EmptyUserpic {
    /// Create from explicit gradient colors and peer display name.
    pub fn new(colors: UserpicBgColors, name: &str, palette: &dyn Colors) -> Self {
        Self {
            colors,
            initials: extract_initials(name),
            fg: palette.color("historyPeerUserpicFg"),
        }
    }

    /// Create directly from a `peer_id` and peer display name.
    pub fn for_peer(peer_id: u64, name: &str, palette: &dyn Colors) -> Self {
        let index = decide_color_index(peer_id);
        Self::new(userpic_colors(index, palette), name, palette)
    }

    /// Create from an explicit `color_index` (`0..64`) and display name.
    pub fn for_color_index(color_index: u8, name: &str, palette: &dyn Colors) -> Self {
        Self::new(userpic_colors(color_index, palette), name, palette)
    }

    /// Computed initials string.
    pub fn initials(&self) -> &str {
        &self.initials
    }

    /// Gradient background colors.
    pub const fn bg_colors(&self) -> UserpicBgColors {
        self.colors
    }

    /// Paint a circular userpic at `rect` (diameter = `rect.width().min(rect.height())`).
    pub fn paint_circle(&self, painter: &Painter, rect: Rect) {
        paint_vertical_gradient_circle(painter, rect, self.colors.color1, self.colors.color2);
        self.paint_initials(painter, rect);
    }

    /// Paint a rounded-square userpic at `rect` with corner `radius`.
    pub fn paint_rounded(&self, painter: &Painter, rect: Rect, radius: f32) {
        paint_vertical_gradient_round_rect(
            painter,
            rect,
            radius,
            self.colors.color1,
            self.colors.color2,
        );
        self.paint_initials(painter, rect);
    }

    /// Paint a square userpic at `rect`.
    pub fn paint_square(&self, painter: &Painter, rect: Rect) {
        paint_vertical_gradient_round_rect(
            painter,
            rect,
            0.0,
            self.colors.color1,
            self.colors.color2,
        );
        self.paint_initials(painter, rect);
    }

    fn paint_initials(&self, painter: &Painter, rect: Rect) {
        if self.initials.is_empty() {
            return;
        }
        let size = rect.width().min(rect.height());
        // Exact tdesktop ratio: `(size * 13) / 33`.
        let font_size = (size * 13.0 / 33.0).max(8.0);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            &self.initials,
            FontId::new(font_size, FontFamily::Proportional),
            self.fg,
        );
    }

    /// Paint the Telegram "Saved Messages" bookmark avatar inside `rect`.
    ///
    /// Uses the exact bookmark polygon proportions from `PaintSavedMessagesInner`
    /// in `empty_userpic.cpp` (lines 58-110).
    pub fn paint_saved_messages(painter: &Painter, rect: Rect, palette: &dyn Colors) {
        let top = palette.color("historyPeerSavedMessagesBg");
        let bottom = palette.color("historyPeerSavedMessagesBg2");
        let fg = palette.color("historyPeerUserpicFg");

        paint_vertical_gradient_circle(painter, rect, top, bottom);

        let size = rect.width().min(rect.height());
        let thickness = (size * 0.055).round().max(1.5);
        let width = (size * 0.15).round() * 2.0;
        let height = (size * 0.19).round() * 2.0;
        let add = (size * 0.064).round();

        let left = rect.center().x - width * 0.5;
        let top_y = rect.center().y - height * 0.5;
        let right = left + width;
        let bottom_y = top_y + height;
        let middle = (left + right) * 0.5;

        let pts = vec![
            Pos2::new(left, bottom_y),
            Pos2::new(left, top_y),
            Pos2::new(right, top_y),
            Pos2::new(right, bottom_y),
            Pos2::new(middle, bottom_y - add),
            Pos2::new(left, bottom_y),
        ];
        painter.add(Shape::line(pts, Stroke::new(thickness, fg)));
    }
}

/// Paint a smooth vertical-gradient circle using a vertex-colored triangle fan mesh.
pub fn paint_vertical_gradient_circle(
    painter: &Painter,
    rect: Rect,
    top_color: Color32,
    bottom_color: Color32,
) {
    let radius = rect.width().min(rect.height()) * 0.5;
    if radius <= 0.5 {
        return;
    }
    let center = rect.center();
    const SEGMENTS: usize = 48;

    let mut mesh = Mesh::default();
    let mid_color = color_lerp(top_color, bottom_color, 0.5);
    mesh.vertices.push(Vertex {
        pos: center,
        uv: WHITE_UV,
        color: mid_color,
    });

    for i in 0..=SEGMENTS {
        let angle = (i as f32 / SEGMENTS as f32) * TAU;
        let x = center.x + radius * angle.cos();
        let y = center.y + radius * angle.sin();
        let t = ((y - (center.y - radius)) / (2.0 * radius)).clamp(0.0, 1.0);
        let color = color_lerp(top_color, bottom_color, f64::from(t));
        mesh.vertices.push(Vertex {
            pos: Pos2::new(x, y),
            uv: WHITE_UV,
            color,
        });
    }

    for i in 1..=SEGMENTS as u32 {
        mesh.indices.push(0);
        mesh.indices.push(i);
        mesh.indices.push(i + 1);
    }
    painter.add(Shape::mesh(mesh));
}

/// Paint a vertical-gradient rounded rectangle using a vertex-colored mesh.
pub fn paint_vertical_gradient_round_rect(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    top_color: Color32,
    bottom_color: Color32,
) {
    let r = radius.clamp(0.0, rect.width().min(rect.height()) * 0.5);
    let mut perimeter = Vec::new();
    let corners = [
        (
            Pos2::new(rect.max.x - r, rect.min.y + r),
            -std::f32::consts::FRAC_PI_2,
            0.0,
        ),
        (
            Pos2::new(rect.max.x - r, rect.max.y - r),
            0.0,
            std::f32::consts::FRAC_PI_2,
        ),
        (
            Pos2::new(rect.min.x + r, rect.max.y - r),
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::PI,
        ),
        (
            Pos2::new(rect.min.x + r, rect.min.y + r),
            std::f32::consts::PI,
            1.5 * std::f32::consts::PI,
        ),
    ];
    const STEPS: usize = 8;
    for (c, a0, a1) in corners {
        if r <= 0.1 {
            perimeter.push(c);
        } else {
            for s in 0..=STEPS {
                let a = a0 + (a1 - a0) * (s as f32 / STEPS as f32);
                perimeter.push(Pos2::new(c.x + r * a.cos(), c.y + r * a.sin()));
            }
        }
    }

    let h = rect.height().max(1.0);
    let mut mesh = Mesh::default();
    let center = rect.center();
    mesh.vertices.push(Vertex {
        pos: center,
        uv: WHITE_UV,
        color: color_lerp(top_color, bottom_color, 0.5),
    });
    for p in &perimeter {
        let t = ((p.y - rect.min.y) / h).clamp(0.0, 1.0);
        mesh.vertices.push(Vertex {
            pos: *p,
            uv: WHITE_UV,
            color: color_lerp(top_color, bottom_color, f64::from(t)),
        });
    }
    let n = perimeter.len() as u32;
    for i in 0..n {
        let next = (i + 1) % n;
        mesh.indices.push(0);
        mesh.indices.push(1 + i);
        mesh.indices.push(1 + next);
    }
    painter.add(Shape::mesh(mesh));
}

/// Allocate and draw an `EmptyUserpic` widget in an egui `Ui`.
pub fn userpic_widget(
    ui: &mut egui::Ui,
    peer_id: u64,
    name: &str,
    diameter: f32,
    colors: &dyn Colors,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        EmptyUserpic::for_peer(peer_id, name, colors).paint_circle(ui.painter(), rect);
    }
    response
}
