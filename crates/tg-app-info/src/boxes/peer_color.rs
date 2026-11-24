//! Peer Accent & Profile Color + Background Emoji editor (`EditPeerColorBox`).
//!
//! Port origin: `Telegram/SourceFiles/boxes/peers/edit_peer_color_box.cpp`
//! and `Telegram/SourceFiles/api/api_peer_colors.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::info::backend::{InfoBackend, PeerColorSettings, UpdatePeerColorRequest};
use tg_app_core::stand_in::types::PeerId;

/// Telegram peer accent color swatch descriptor (index + primary/secondary/tertiary preview RGB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerColorSwatch {
    /// Telegram `PeerColor.color` index (`0..21`).
    pub color_id: u8,
    /// Primary accent RGB.
    pub primary: [u8; 3],
    /// Optional secondary stripe RGB (for two-tone & three-tone accents).
    pub secondary: Option<[u8; 3]>,
    /// Optional tertiary stripe RGB.
    pub tertiary: Option<[u8; 3]>,
}

/// Built-in Telegram peer color palette (7 classic + 14 multi-tone swatches matching Layer 233).
pub const PEER_COLOR_SWATCHES: [PeerColorSwatch; 21] = [
    // 0..7 classic single-tone peer colors
    PeerColorSwatch {
        color_id: 0,
        primary: [0xE1, 0x57, 0x59],
        secondary: None,
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 1,
        primary: [0xF2, 0x8E, 0x2B],
        secondary: None,
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 2,
        primary: [0x9C, 0x6B, 0xDF],
        secondary: None,
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 3,
        primary: [0x4E, 0xAE, 0x51],
        secondary: None,
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 4,
        primary: [0x39, 0xA8, 0xBD],
        secondary: None,
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 5,
        primary: [0x40, 0xA7, 0xE3],
        secondary: None,
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 6,
        primary: [0xE3, 0x5A, 0x92],
        secondary: None,
        tertiary: None,
    },
    // 7..14 two-tone peer colors
    PeerColorSwatch {
        color_id: 7,
        primary: [0xE1, 0x57, 0x59],
        secondary: Some([0xFF, 0x9F, 0x68]),
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 8,
        primary: [0xF2, 0x8E, 0x2B],
        secondary: Some([0xF6, 0xC8, 0x45]),
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 9,
        primary: [0x8E, 0x54, 0xE9],
        secondary: Some([0x47, 0x76, 0xE6]),
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 10,
        primary: [0x2B, 0xB6, 0x73],
        secondary: Some([0x8C, 0xD8, 0x67]),
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 11,
        primary: [0x25, 0xA0, 0xC9],
        secondary: Some([0x6D, 0xD5, 0xFA]),
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 12,
        primary: [0x3B, 0x82, 0xF6],
        secondary: Some([0x93, 0xC5, 0xFD]),
        tertiary: None,
    },
    PeerColorSwatch {
        color_id: 13,
        primary: [0xEC, 0x48, 0x99],
        secondary: Some([0xF9, 0xA8, 0xD4]),
        tertiary: None,
    },
    // 14..21 three-tone peer colors
    PeerColorSwatch {
        color_id: 14,
        primary: [0xCC, 0x33, 0x44],
        secondary: Some([0xF6, 0x81, 0x36]),
        tertiary: Some([0xFF, 0xD1, 0x66]),
    },
    PeerColorSwatch {
        color_id: 15,
        primary: [0x2B, 0x8A, 0x3E],
        secondary: Some([0x66, 0xBB, 0x6A]),
        tertiary: Some([0xB2, 0xF2, 0xBB]),
    },
    PeerColorSwatch {
        color_id: 16,
        primary: [0x1C, 0x7E, 0xD6],
        secondary: Some([0x4D, 0xAB, 0xF7]),
        tertiary: Some([0xA5, 0xD8, 0xFF]),
    },
    PeerColorSwatch {
        color_id: 17,
        primary: [0x70, 0x48, 0xE8],
        secondary: Some([0xB1, 0x97, 0xFC]),
        tertiary: Some([0xF3, 0xD9, 0xFA]),
    },
    PeerColorSwatch {
        color_id: 18,
        primary: [0x0C, 0x85, 0x99],
        secondary: Some([0x3B, 0xC9, 0xDB]),
        tertiary: Some([0xC5, 0xF6, 0xFA]),
    },
    PeerColorSwatch {
        color_id: 19,
        primary: [0xD6, 0x33, 0x6C],
        secondary: Some([0xF7, 0x83, 0xAC]),
        tertiary: Some([0xFF, 0xDE, 0xEB]),
    },
    PeerColorSwatch {
        color_id: 20,
        primary: [0xE8, 0x59, 0x0C],
        secondary: Some([0xFF, 0xA9, 0x4D]),
        tertiary: Some([0xFF, 0xEC, 0x99]),
    },
];

/// Curated synthetic custom-emoji background pattern presets `(document_id, glyph, label)`.
pub const BACKGROUND_EMOJI_PRESETS: [(u64, &str, &str); 6] = [
    (0, "∅", "None"),
    (5368324170671202286, "★", "Star"),
    (5368324170671202287, "♦", "Gem"),
    (5368324170671202288, "⚡", "Bolt"),
    (5368324170671202289, "✿", "Bloom"),
    (5368324170671202290, "☁", "Cloud"),
];

/// Active tab inside [`EditPeerColorBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerColorTab {
    /// Reply/name accent color (`for_profile = false`).
    Name,
    /// Profile header banner color (`for_profile = true`).
    Profile,
}

/// Modal box state for editing peer name/reply and profile colors (`EditPeerColorBox`).
#[derive(Debug, Clone)]
pub struct EditPeerColorBoxState {
    /// Target peer identifier (self user or channel/supergroup).
    pub peer_id: PeerId,
    /// Display name/title rendered in the live preview card.
    pub peer_title: String,
    /// Active editing tab (`Name` vs `Profile`).
    pub active_tab: PeerColorTab,
    /// Selected reply/name accent color index.
    pub name_color_index: u8,
    /// Selected reply/quote background custom emoji document ID (`0` = none).
    pub name_background_emoji_id: u64,
    /// Selected profile header color index (`None` = default / reset).
    pub profile_color_index: Option<u8>,
    /// Selected profile header background custom emoji document ID (`0` = none).
    pub profile_background_emoji_id: u64,
    /// Editable numeric input for custom emoji document ID.
    pub custom_emoji_input: String,
    /// Validation or RPC error.
    pub error: Option<String>,
}

impl EditPeerColorBoxState {
    /// Creates an [`EditPeerColorBoxState`] initialized from `settings`.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        settings: PeerColorSettings,
    ) -> Self {
        let name_color_index = settings.color_index.unwrap_or(5);
        let custom_emoji_input = if settings.background_emoji_id != 0 {
            settings.background_emoji_id.to_string()
        } else {
            String::new()
        };
        Self {
            peer_id,
            peer_title: peer_title.into(),
            active_tab: PeerColorTab::Name,
            name_color_index,
            name_background_emoji_id: settings.background_emoji_id,
            profile_color_index: settings.profile_color_index,
            profile_background_emoji_id: settings.profile_background_emoji_id,
            custom_emoji_input,
            error: None,
        }
    }

    /// Switches between `Name` and `Profile` tabs, syncing the custom emoji ID text box.
    pub fn set_tab(&mut self, tab: PeerColorTab) {
        if self.active_tab != tab {
            self.active_tab = tab;
            let eid = self.current_emoji_id();
            self.custom_emoji_input = if eid != 0 {
                eid.to_string()
            } else {
                String::new()
            };
        }
    }

    /// Returns the currently selected background emoji document ID for the active tab.
    #[must_use]
    pub const fn current_emoji_id(&self) -> u64 {
        match self.active_tab {
            PeerColorTab::Name => self.name_background_emoji_id,
            PeerColorTab::Profile => self.profile_background_emoji_id,
        }
    }

    /// Sets the background emoji document ID for the active tab.
    pub fn set_current_emoji_id(&mut self, id: u64) {
        match self.active_tab {
            PeerColorTab::Name => self.name_background_emoji_id = id,
            PeerColorTab::Profile => self.profile_background_emoji_id = id,
        }
        self.custom_emoji_input = if id != 0 {
            id.to_string()
        } else {
            String::new()
        };
    }

    /// Builds the [`UpdatePeerColorRequest`] for the currently active tab.
    #[must_use]
    pub fn build_request(&self) -> UpdatePeerColorRequest {
        match self.active_tab {
            PeerColorTab::Name => UpdatePeerColorRequest {
                for_profile: false,
                color_index: Some(self.name_color_index),
                background_emoji_id: self.name_background_emoji_id,
            },
            PeerColorTab::Profile => UpdatePeerColorRequest {
                for_profile: true,
                color_index: self.profile_color_index,
                background_emoji_id: self.profile_background_emoji_id,
            },
        }
    }

    /// Applies the active tab's color & emoji background via `backend.update_peer_color`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let trimmed = self.custom_emoji_input.trim();
        if !trimmed.is_empty() {
            match trimmed.parse::<u64>() {
                Ok(parsed) => self.set_current_emoji_id(parsed),
                Err(_) => {
                    let msg = "Background emoji ID must be a valid unsigned integer.".to_owned();
                    self.error = Some(msg.clone());
                    return Err(msg);
                }
            }
        }
        let req = self.build_request();
        match backend.update_peer_color(self.peer_id, req) {
            Ok(()) => {
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, "Peer Appearance")
            .width(st::box_wide_width.0 as f32 + 32.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("apply_color", "Apply Color"));
        if self.active_tab == PeerColorTab::Profile && self.profile_color_index.is_some() {
            modal = modal.with_left_button(BoxButton::default_btn("reset_profile_color", "Reset"));
        }
        modal
    }

    /// Renders tabs, live preview card, color swatch palette, and emoji background selector.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        // 1. Name vs Profile mode tabs
        ui.horizontal(|ui| {
            if tab_pill(
                ui,
                "Name & Replies",
                self.active_tab == PeerColorTab::Name,
                colors,
            ) {
                self.set_tab(PeerColorTab::Name);
            }
            ui.add_space(8.0);
            if tab_pill(
                ui,
                "Profile",
                self.active_tab == PeerColorTab::Profile,
                colors,
            ) {
                self.set_tab(PeerColorTab::Profile);
            }
        });

        ui.add_space(10.0);

        // 2. Live Preview Card
        let active_swatch = match self.active_tab {
            PeerColorTab::Name => swatch_for_id(self.name_color_index),
            PeerColorTab::Profile => self
                .profile_color_index
                .map(swatch_for_id)
                .unwrap_or(PEER_COLOR_SWATCHES[5]),
        };
        let active_rgb = Color32::from_rgb(
            active_swatch.primary[0],
            active_swatch.primary[1],
            active_swatch.primary[2],
        );

        let box_w = ui.available_width().max(240.0);
        match self.active_tab {
            PeerColorTab::Name => {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(box_w, 86.0), Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(10),
                    colors.color("windowBgOver"),
                );
                let quote_rect = rect.shrink2(Vec2::new(14.0, 12.0));
                let tint = Color32::from_rgba_unmultiplied(
                    active_rgb.r(),
                    active_rgb.g(),
                    active_rgb.b(),
                    32,
                );
                ui.painter()
                    .rect_filled(quote_rect, CornerRadius::same(6), tint);
                let bar = Rect::from_min_size(quote_rect.min, Vec2::new(3.5, quote_rect.height()));
                ui.painter()
                    .rect_filled(bar, CornerRadius::same(2), active_rgb);
                if self.current_emoji_id() != 0 {
                    paint_pattern_icon(
                        ui.painter(),
                        self.current_emoji_id(),
                        Pos2::new(quote_rect.max.x - 16.0, quote_rect.center().y),
                        20.0,
                        Color32::from_rgba_unmultiplied(
                            active_rgb.r(),
                            active_rgb.g(),
                            active_rgb.b(),
                            110,
                        ),
                    );
                }
                ui.painter().text(
                    Pos2::new(quote_rect.min.x + 12.0, quote_rect.min.y + 8.0),
                    Align2::LEFT_TOP,
                    &self.peer_title,
                    font_id_from_style(st::semibold_font),
                    active_rgb,
                );
                ui.painter().text(
                    Pos2::new(quote_rect.min.x + 12.0, quote_rect.min.y + 30.0),
                    Align2::LEFT_TOP,
                    "This is how replies and link previews appear.",
                    FontId::new(12.5, family_regular()),
                    colors.color("windowFg"),
                );
            }
            PeerColorTab::Profile => {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(box_w, 96.0), Sense::hover());
                let bg = if self.profile_color_index.is_some() {
                    active_rgb
                } else {
                    colors.color("windowBgOver")
                };
                ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
                if self.current_emoji_id() != 0 {
                    let wm = Color32::from_rgba_unmultiplied(255, 255, 255, 70);
                    for (dx, dy) in [(-110.0, -18.0), (110.0, -14.0), (-82.0, 22.0), (88.0, 20.0)] {
                        paint_pattern_icon(
                            ui.painter(),
                            self.current_emoji_id(),
                            rect.center() + Vec2::new(dx, dy),
                            15.0,
                            wm,
                        );
                    }
                }
                let av_rect = Rect::from_center_size(
                    Pos2::new(rect.center().x, rect.min.y + 34.0),
                    Vec2::splat(42.0),
                );
                EmptyUserpic::for_color_index(
                    self.profile_color_index.unwrap_or(self.name_color_index) % 7,
                    &self.peer_title,
                    colors,
                )
                .paint_circle(ui.painter(), av_rect);
                let fg = if self.profile_color_index.is_some() {
                    Color32::WHITE
                } else {
                    colors.color("windowBoldFg")
                };
                ui.painter().text(
                    Pos2::new(rect.center().x, rect.min.y + 72.0),
                    Align2::CENTER_CENTER,
                    &self.peer_title,
                    font_id_from_style(st::semibold_font),
                    fg,
                );
            }
        }

        ui.add_space(10.0);
        FlatLabel::plain("Accent Color", FlatLabelStyle::subsection_title(colors))
            .id_salt("peer_color_palette_hdr")
            .show(ui);
        ui.add_space(6.0);

        // 3. Swatch grid (7 per row, 3 rows = 21 swatches)
        let selected_id = match self.active_tab {
            PeerColorTab::Name => Some(self.name_color_index),
            PeerColorTab::Profile => self.profile_color_index,
        };
        for row in PEER_COLOR_SWATCHES.chunks(7) {
            ui.horizontal(|ui| {
                for swatch in row {
                    let is_sel = selected_id == Some(swatch.color_id);
                    if paint_color_swatch(ui, *swatch, is_sel, colors) {
                        match self.active_tab {
                            PeerColorTab::Name => self.name_color_index = swatch.color_id,
                            PeerColorTab::Profile => {
                                self.profile_color_index = Some(swatch.color_id)
                            }
                        }
                    }
                    ui.add_space(6.0);
                }
            });
            ui.add_space(6.0);
        }

        ui.add_space(6.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(8.0);

        // 4. Background Emoji selector
        FlatLabel::plain(
            "Background Pattern Emoji",
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("peer_color_emoji_hdr")
        .show(ui);
        ui.add_space(6.0);

        let cur_emoji = self.current_emoji_id();
        ui.horizontal_wrapped(|ui| {
            let sel_bg = colors.color("windowBgActive");
            let hover_bg = colors.color("windowBgOver");
            for (doc_id, _glyph, lbl) in BACKGROUND_EMOJI_PRESETS {
                let selected = cur_emoji == doc_id;
                let text_col = if selected {
                    Color32::WHITE
                } else {
                    colors.color("windowFg")
                };
                let icon_col = if selected {
                    Color32::WHITE
                } else {
                    colors.color("windowActiveTextFg")
                };
                let painter_cb = move |p: &egui::Painter, c: Pos2, sz: f32, _col: Color32| {
                    paint_pattern_icon(p, doc_id, c, sz, icon_col);
                };
                if tg_app_core::ui_icons::selectable_icon_chip(
                    ui,
                    ("peer_color_pat", doc_id),
                    selected,
                    lbl,
                    12.0,
                    text_col,
                    sel_bg,
                    hover_bg,
                    icon_col,
                    Some(&painter_cb),
                )
                .clicked()
                {
                    self.set_current_emoji_id(doc_id);
                }
                ui.add_space(4.0);
            }
        });
        ui.add_space(6.0);
        let resp = InputField::single_line(
            "peer_color_custom_emoji_id",
            &mut self.custom_emoji_input,
            "Custom Emoji Document ID (optional)",
            InputFieldStyle::default_input(colors),
        )
        .show(ui);
        if resp.changed() {
            let t = self.custom_emoji_input.trim();
            if t.is_empty() {
                match self.active_tab {
                    PeerColorTab::Name => self.name_background_emoji_id = 0,
                    PeerColorTab::Profile => self.profile_background_emoji_id = 0,
                }
            } else if let Ok(v) = t.parse::<u64>() {
                match self.active_tab {
                    PeerColorTab::Name => self.name_background_emoji_id = v,
                    PeerColorTab::Profile => self.profile_background_emoji_id = v,
                }
            }
        }

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

fn swatch_for_id(id: u8) -> PeerColorSwatch {
    PEER_COLOR_SWATCHES
        .get(usize::from(id) % PEER_COLOR_SWATCHES.len())
        .copied()
        .unwrap_or(PEER_COLOR_SWATCHES[5])
}

fn paint_pattern_icon(
    painter: &egui::Painter,
    doc_id: u64,
    center: Pos2,
    size: f32,
    color: Color32,
) {
    match doc_id {
        0 => tg_app_core::ui_icons::paint_none_slash(painter, center, size, color),
        5368324170671202286 => {
            tg_app_core::ui_icons::paint_star(painter, center, size * 0.46, color)
        }
        5368324170671202287 => tg_app_core::ui_icons::paint_diamond(painter, center, size, color),
        5368324170671202288 => tg_app_core::ui_icons::paint_bolt(painter, center, size, color),
        5368324170671202289 => tg_app_core::ui_icons::paint_bloom(painter, center, size, color),
        5368324170671202290 => tg_app_core::ui_icons::paint_cloud(painter, center, size, color),
        _ => tg_app_core::ui_icons::paint_sparkle(painter, center, size, color),
    }
}

fn paint_color_swatch(
    ui: &mut Ui,
    swatch: PeerColorSwatch,
    selected: bool,
    colors: &dyn Colors,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
    resp.widget_info(|| {
        WidgetInfo::selected(
            Role::RadioButton,
            true,
            selected,
            format!("Color {}", swatch.color_id),
        )
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let primary = Color32::from_rgb(swatch.primary[0], swatch.primary[1], swatch.primary[2]);
    ui.painter().circle_filled(rect.center(), 14.0, primary);
    if let Some(sec) = swatch.secondary {
        let sec_col = Color32::from_rgb(sec[0], sec[1], sec[2]);
        ui.painter().circle_filled(
            Pos2::new(rect.center().x + 5.0, rect.center().y + 5.0),
            6.5,
            sec_col,
        );
    }
    if let Some(tert) = swatch.tertiary {
        let tert_col = Color32::from_rgb(tert[0], tert[1], tert[2]);
        ui.painter().circle_filled(
            Pos2::new(rect.center().x - 5.0, rect.center().y + 5.0),
            4.5,
            tert_col,
        );
    }
    if selected {
        ui.painter().circle_stroke(
            rect.center(),
            16.5,
            Stroke::new(2.0, colors.color("activeButtonBg")),
        );
    }
    resp.clicked()
}

fn tab_pill(ui: &mut Ui, label: &str, selected: bool, colors: &dyn Colors) -> bool {
    let fg = if selected {
        Color32::WHITE
    } else {
        colors.color("windowFg")
    };
    let bg = if selected {
        colors.color("activeButtonBg")
    } else {
        colors.color("windowBgOver")
    };
    let font = FontId::new(12.0, family_semibold());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let size = Vec2::new(galley.size().x + 18.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    resp.clicked()
}
