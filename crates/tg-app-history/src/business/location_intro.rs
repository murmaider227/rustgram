//! Business Location and Chat Intro (Start Page) editors.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/business/settings_location.cpp`
//! - `Telegram/SourceFiles/settings/business/settings_chat_intro.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::business::backend::BusinessBackend;
use crate::business::model::{
    BusinessLocation, CHAT_INTRO_DESCRIPTION_LIMIT, CHAT_INTRO_TITLE_LIMIT, ChatIntro, GeoPoint,
    IntroStickerOption,
};
use crate::business::ui_helpers::{
    BUSINESS_GOLD, BUSINESS_TEAL, PREMIUM_PURPLE, business_section_intro, divider_caption,
    paint_star_emblem, pill_button, primary_button, section_divider, settings_toggle_row,
    subsection_title, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

// -----------------------------------------------------------------------------
// Business Location (`settings_location.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for Business Location (`settings_location.cpp`).
pub struct LocationView {
    /// Draft location address and optional coordinates.
    pub location: BusinessLocation,
    /// Cached last map coordinates so toggling map off/on restores a convenient pin.
    last_point: GeoPoint,
}

impl LocationView {
    /// Creates a [`LocationView`] from the saved `location`.
    #[must_use]
    pub fn new(location: BusinessLocation) -> Self {
        let last_point = location.point.unwrap_or(GeoPoint {
            lat: 45.5231,
            lon: -122.6765,
        });
        Self {
            location,
            last_point,
        }
    }

    /// Renders the Business Location editor and returns an optional toast notification.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        business_section_intro(
            ui,
            "PIN",
            Color32::from_rgb(0xE5, 0x53, 0x4B),
            "Business Location",
            "Add your physical address and optional map pin so customers can find you easily.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                subsection_title(ui, "Address", palette);
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.location.address)
                        .desired_rows(2)
                        .desired_width(ui.available_width())
                        .hint_text("Enter your business address"),
                );
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Presets:")
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                    let presets = [
                        (
                            "Studio HQ",
                            "420 Cedar Workshop Ave, Suite 3B, Portland, OR",
                            45.5231,
                            -122.6765,
                        ),
                        (
                            "Midtown Loft",
                            "118 Mercer St, Floor 4, New York, NY",
                            40.7235,
                            -73.9997,
                        ),
                        (
                            "Harbor Lab",
                            "75 Pier Walk, Unit 12, Seattle, WA",
                            47.6062,
                            -122.3321,
                        ),
                    ];
                    for (name, addr, lat, lon) in presets {
                        if pill_button(
                            ui,
                            name,
                            palette.window_bg_over,
                            palette.window_active_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.location.address = addr.to_owned();
                            let pt = GeoPoint { lat, lon };
                            self.location.point = Some(pt);
                            self.last_point = pt;
                        }
                    }
                });
            });

        section_divider(ui, palette);

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 6))
            .show(ui, |ui| {
                let mut map_enabled = self.location.point.is_some();
                if settings_toggle_row(
                    ui,
                    "Set Location on Map",
                    Some("Click anywhere on the map preview below to reposition the pin."),
                    &mut map_enabled,
                    palette,
                ) {
                    if map_enabled {
                        self.location.point = Some(self.last_point);
                    } else {
                        if let Some(pt) = self.location.point {
                            self.last_point = pt;
                        }
                        self.location.point = None;
                    }
                }

                if let Some(mut pt) = self.location.point {
                    ui.add_space(8.0);
                    let map_w = ui.available_width().max(220.0);
                    let (map_rect, resp) =
                        ui.allocate_exact_size(Vec2::new(map_w, 168.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                    }
                    if resp.clicked()
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        let nx = ((pos.x - map_rect.left()) / map_rect.width()).clamp(0.0, 1.0);
                        let ny = ((pos.y - map_rect.top()) / map_rect.height()).clamp(0.0, 1.0);
                        pt.lon += f64::from(nx - 0.5) * 0.04;
                        pt.lat -= f64::from(ny - 0.5) * 0.03;
                        pt.lat = pt.lat.clamp(-85.0, 85.0);
                        pt.lon = pt.lon.clamp(-180.0, 180.0);
                        self.location.point = Some(pt);
                        self.last_point = pt;
                    }
                    paint_stylized_map_preview(ui.painter(), map_rect, pt);
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Coordinates: {:.4}, {:.4}  (click map to adjust pin)",
                            pt.lat, pt.lon
                        ))
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                    );
                }

                ui.add_space(12.0);
                if primary_button(
                    ui,
                    "Save Location",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                {
                    match backend.save_location(self.location.clone()) {
                        Ok(()) => toast = Some("Business location saved.".to_owned()),
                        Err(e) => toast = Some(e),
                    }
                }
                if self.location.is_set() {
                    ui.add_space(6.0);
                    if primary_button(
                        ui,
                        "Remove Location",
                        with_alpha(palette.attention_fg, 0.14),
                        palette.attention_fg,
                        34.0,
                    )
                    .clicked()
                    {
                        self.location = BusinessLocation::default();
                        match backend.save_location(BusinessLocation::default()) {
                            Ok(()) => toast = Some("Business location cleared.".to_owned()),
                            Err(e) => toast = Some(e),
                        }
                    }
                }
            });

        toast
    }
}

fn paint_stylized_map_preview(painter: &egui::Painter, rect: Rect, pt: GeoPoint) {
    // Soft pastel map canvas with park block, river strip, street grid, and red location pin.
    painter.rect_filled(
        rect,
        CornerRadius::same(10),
        Color32::from_rgb(0xE9, 0xF0, 0xE6),
    );
    let park = Rect::from_min_size(
        Pos2::new(rect.left() + 14.0, rect.top() + 14.0),
        Vec2::new(rect.width() * 0.28, rect.height() * 0.42),
    );
    painter.rect_filled(
        park,
        CornerRadius::same(8),
        Color32::from_rgb(0xC9, 0xE5, 0xC6),
    );
    let river = Rect::from_min_size(
        Pos2::new(rect.right() - rect.width() * 0.24, rect.top()),
        Vec2::new(rect.width() * 0.14, rect.height()),
    );
    painter.rect_filled(
        river,
        CornerRadius::ZERO,
        Color32::from_rgb(0xB8, 0xDC, 0xF5),
    );

    // Street grid
    let road_col = Color32::WHITE;
    for i in 1..6 {
        let x = rect.left() + rect.width() * (i as f32 / 6.0);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(if i == 3 { 4.0 } else { 2.0 }, road_col),
        );
    }
    for j in 1..4 {
        let y = rect.top() + rect.height() * (j as f32 / 4.0);
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(if j == 2 { 4.0 } else { 2.0 }, road_col),
        );
    }

    // Center marker (`historyMapPoint` + `historyMapPointInner`)
    let pin_tip = rect.center();
    let pin_head = Pos2::new(pin_tip.x, pin_tip.y - 16.0);
    painter.circle_filled(
        Pos2::new(pin_tip.x, pin_tip.y + 2.0),
        7.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, 45),
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(pin_head.x - 9.0, pin_head.y + 4.0),
            Pos2::new(pin_head.x + 9.0, pin_head.y + 4.0),
            pin_tip,
        ],
        Color32::from_rgb(0xE5, 0x48, 0x4D),
        Stroke::NONE,
    ));
    painter.circle_filled(pin_head, 11.0, Color32::from_rgb(0xE5, 0x48, 0x4D));
    painter.circle_filled(pin_head, 4.5, Color32::WHITE);

    // Coordinate pill at bottom-left
    let pill = Rect::from_min_size(
        Pos2::new(rect.left() + 10.0, rect.bottom() - 28.0),
        Vec2::new(152.0, 20.0),
    );
    painter.rect_filled(
        pill,
        CornerRadius::same(10),
        Color32::from_rgba_unmultiplied(0x1E, 0x29, 0x3B, 200),
    );
    painter.text(
        pill.center(),
        Align2::CENTER_CENTER,
        format!("{:.3}°, {:.3}°", pt.lat, pt.lon),
        FontId::proportional(10.5),
        Color32::WHITE,
    );

    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, Color32::from_rgb(0xCB, 0xD5, 0xE1)),
        StrokeKind::Inside,
    );
}

// -----------------------------------------------------------------------------
// Chat Intro / Start Page (`settings_chat_intro.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for Business Start Page / Chat Intro (`settings_chat_intro.cpp`).
pub struct ChatIntroView {
    /// Draft intro values.
    pub intro: ChatIntro,
    /// Available sticker choices.
    pub stickers: Vec<IntroStickerOption>,
    /// Whether the inline sticker picker grid is expanded.
    pub sticker_picker_open: bool,
}

impl ChatIntroView {
    /// Creates a [`ChatIntroView`] from saved `intro` and preset `stickers`.
    #[must_use]
    pub fn new(intro: ChatIntro, stickers: Vec<IntroStickerOption>) -> Self {
        Self {
            intro,
            stickers,
            sticker_picker_open: false,
        }
    }

    fn selected_sticker(&self) -> Option<&IntroStickerOption> {
        let id = self.intro.sticker_id?;
        self.stickers.iter().find(|s| s.id == id)
    }

    /// Renders the Start Page editor with live chat-canvas preview card.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        // Live empty-chat preview (`PreviewWrap` / `AboutView` in `settings_chat_intro.cpp`)
        let preview_w = ui.available_width().max(260.0);
        let (canvas_rect, _) = ui.allocate_exact_size(Vec2::new(preview_w, 196.0), Sense::hover());
        ui.painter()
            .rect_filled(canvas_rect, CornerRadius::ZERO, palette.chat_canvas_bg);

        let card_w = (preview_w - 64.0).clamp(220.0, 320.0);
        let card_rect = Rect::from_center_size(canvas_rect.center(), Vec2::new(card_w, 162.0));
        ui.painter().rect_filled(
            card_rect,
            CornerRadius::same(14),
            with_alpha(palette.window_bg, 0.94),
        );
        ui.painter().rect_stroke(
            card_rect,
            CornerRadius::same(14),
            Stroke::new(1.0, with_alpha(palette.divider, 0.8)),
            StrokeKind::Inside,
        );

        let sticker_opt = self.selected_sticker().or_else(|| self.stickers.first());
        let sticker_center = Pos2::new(card_rect.center().x, card_rect.top() + 44.0);
        if let Some(st) = sticker_opt {
            paint_intro_sticker_art(
                ui.painter(),
                sticker_center,
                26.0,
                &st.art_kind,
                Color32::from_rgb(st.accent_rgb[0], st.accent_rgb[1], st.accent_rgb[2]),
            );
        }
        let preview_title = if self.intro.title.trim().is_empty() {
            "No messages here yet..."
        } else {
            self.intro.title.as_str()
        };
        let preview_desc = if self.intro.description.trim().is_empty() {
            "Send a message or tap the greeting below."
        } else {
            self.intro.description.as_str()
        };
        ui.painter().text(
            Pos2::new(card_rect.center().x, card_rect.top() + 92.0),
            Align2::CENTER_CENTER,
            preview_title,
            FontId::proportional(14.5),
            palette.window_bold_fg,
        );
        // Word-wrapped preview description inside card
        let desc_rect = Rect::from_min_max(
            Pos2::new(card_rect.left() + 16.0, card_rect.top() + 106.0),
            Pos2::new(card_rect.right() - 16.0, card_rect.bottom() - 10.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(desc_rect), |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(preview_desc)
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                );
            });
        });

        // Title, Description & Sticker controls
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                // Title input (max 32 chars)
                ui.horizontal(|ui| {
                    subsection_title(ui, "Title", palette);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let len = self.intro.title.chars().count();
                        ui.label(
                            egui::RichText::new(format!("{len}/{CHAT_INTRO_TITLE_LIMIT}"))
                                .size(11.0)
                                .color(if len >= CHAT_INTRO_TITLE_LIMIT {
                                    palette.attention_fg
                                } else {
                                    palette.window_sub_text_fg
                                }),
                        );
                    });
                });
                ui.add_space(4.0);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.intro.title)
                            .hint_text("Enter title (e.g. Welcome to our studio)")
                            .desired_width(ui.available_width()),
                    )
                    .changed()
                {
                    truncate_chars(&mut self.intro.title, CHAT_INTRO_TITLE_LIMIT);
                }

                ui.add_space(10.0);

                // Description input (max 70 chars)
                ui.horizontal(|ui| {
                    subsection_title(ui, "Message", palette);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let len = self.intro.description.chars().count();
                        ui.label(
                            egui::RichText::new(format!("{len}/{CHAT_INTRO_DESCRIPTION_LIMIT}"))
                                .size(11.0)
                                .color(if len >= CHAT_INTRO_DESCRIPTION_LIMIT {
                                    palette.attention_fg
                                } else {
                                    palette.window_sub_text_fg
                                }),
                        );
                    });
                });
                ui.add_space(4.0);
                if ui
                    .add(
                        egui::TextEdit::multiline(&mut self.intro.description)
                            .desired_rows(2)
                            .hint_text("Enter short greeting description")
                            .desired_width(ui.available_width()),
                    )
                    .changed()
                {
                    truncate_chars(&mut self.intro.description, CHAT_INTRO_DESCRIPTION_LIMIT);
                }

                ui.add_space(10.0);

                // Sticker picker button (`CreateIntroStickerButton`)
                let current_sticker_name = self
                    .selected_sticker()
                    .map_or("Random", |s| s.name.as_str());
                let width = ui.available_width().max(200.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 6.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    "Sticker",
                    FontId::proportional(13.5),
                    palette.window_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 18.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    current_sticker_name,
                    FontId::proportional(12.5),
                    palette.window_active_text_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 6.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    if self.sticker_picker_open { "v" } else { ">" },
                    FontId::proportional(12.5),
                    palette.window_sub_text_fg,
                );
                if resp.clicked() {
                    self.sticker_picker_open = !self.sticker_picker_open;
                }

                if self.sticker_picker_open {
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                                let is_random = self.intro.sticker_id.is_none();
                                if pill_button(
                                    ui,
                                    "Random",
                                    if is_random {
                                        palette.window_bg_active
                                    } else {
                                        palette.window_bg
                                    },
                                    if is_random {
                                        Color32::WHITE
                                    } else {
                                        palette.window_fg
                                    },
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    self.intro.sticker_id = None;
                                }
                                for st in &self.stickers {
                                    let active = self.intro.sticker_id == Some(st.id);
                                    let accent = Color32::from_rgb(
                                        st.accent_rgb[0],
                                        st.accent_rgb[1],
                                        st.accent_rgb[2],
                                    );
                                    if pill_button(
                                        ui,
                                        &st.name,
                                        if active { accent } else { palette.window_bg },
                                        if active {
                                            Color32::WHITE
                                        } else {
                                            palette.window_fg
                                        },
                                        Some(if active { accent } else { palette.divider }),
                                    )
                                    .clicked()
                                    {
                                        self.intro.sticker_id = Some(st.id);
                                    }
                                }
                            });
                        });
                }
            });

        divider_caption(
            ui,
            "Customize the title, text, and sticker that new customers see before sending their first message.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                if primary_button(
                    ui,
                    "Save Start Page",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                {
                    match backend.save_chat_intro(self.intro.clone()) {
                        Ok(()) => toast = Some("Start page saved.".to_owned()),
                        Err(e) => toast = Some(e),
                    }
                }
                if self.intro.is_set() {
                    ui.add_space(6.0);
                    if primary_button(
                        ui,
                        "Reset Start Page",
                        with_alpha(palette.attention_fg, 0.14),
                        palette.attention_fg,
                        34.0,
                    )
                    .clicked()
                    {
                        self.intro = ChatIntro::default();
                        match backend.save_chat_intro(ChatIntro::default()) {
                            Ok(()) => toast = Some("Start page reset to default.".to_owned()),
                            Err(e) => toast = Some(e),
                        }
                    }
                }
            });

        toast
    }
}

fn truncate_chars(s: &mut String, max_chars: usize) {
    if s.chars().count() > max_chars {
        *s = s.chars().take(max_chars).collect();
    }
}

fn paint_intro_sticker_art(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    kind: &str,
    accent: Color32,
) {
    painter.circle_filled(center, radius * 1.12, with_alpha(accent, 0.16));
    match kind {
        "star" => paint_star_emblem(painter, center, radius * 0.9, accent),
        "spark" => {
            let pts = vec![
                Pos2::new(center.x, center.y - radius),
                Pos2::new(center.x + radius * 0.72, center.y),
                Pos2::new(center.x, center.y + radius),
                Pos2::new(center.x - radius * 0.72, center.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, accent, Stroke::NONE));
            painter.circle_filled(center, radius * 0.28, Color32::WHITE);
        }
        "cup" => {
            let body = Rect::from_center_size(
                Pos2::new(center.x, center.y + 3.0),
                Vec2::new(radius * 1.15, radius * 0.95),
            );
            painter.rect_filled(body, CornerRadius::same(6), BUSINESS_TEAL);
            painter.circle_stroke(
                Pos2::new(body.right() + 2.0, body.center().y),
                radius * 0.28,
                Stroke::new(2.5, BUSINESS_TEAL),
            );
        }
        "rocket" => {
            let hull = vec![
                Pos2::new(center.x, center.y - radius),
                Pos2::new(center.x + radius * 0.45, center.y),
                Pos2::new(center.x + radius * 0.35, center.y + radius * 0.65),
                Pos2::new(center.x - radius * 0.35, center.y + radius * 0.65),
                Pos2::new(center.x - radius * 0.45, center.y),
            ];
            painter.add(egui::Shape::convex_polygon(hull, accent, Stroke::NONE));
            painter.circle_filled(
                Pos2::new(center.x, center.y - 2.0),
                radius * 0.2,
                Color32::WHITE,
            );
        }
        "sign" => {
            let board = Rect::from_center_size(center, Vec2::new(radius * 1.55, radius * 0.95));
            painter.rect_filled(board, CornerRadius::same(6), accent);
            painter.text(
                board.center(),
                Align2::CENTER_CENTER,
                "OPEN",
                FontId::proportional(11.0),
                Color32::WHITE,
            );
        }
        _ => {
            let base = Rect::from_center_size(
                Pos2::new(center.x, center.y + radius * 0.35),
                Vec2::new(radius * 1.3, radius * 0.55),
            );
            painter.rect_filled(base, CornerRadius::same(4), PREMIUM_PURPLE);
            painter.circle_filled(
                Pos2::new(center.x, center.y - radius * 0.3),
                5.0,
                BUSINESS_GOLD,
            );
        }
    }
}
