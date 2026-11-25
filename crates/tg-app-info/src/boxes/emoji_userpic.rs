//! Emoji Userpic Builder modal box (`info/userpic/*`).
//!
//! Lets users craft a profile or chat avatar by choosing an emoji/sticker and
//! a 1..=4 color gradient background, encoded as TL `videoSizeEmojiMarkup`.
//!
//! Port origin: `Telegram/SourceFiles/info/userpic/info_userpic_emoji_builder_widget.cpp`,
//! `Telegram/SourceFiles/info/userpic/info_userpic_colors_editor.cpp`,
//! `Telegram/SourceFiles/info/userpic/info_userpic_emoji_builder_preview.cpp`,
//! and `Telegram/SourceFiles/info/userpic/info_userpic_emoji_builder_common.cpp`.

use std::sync::{Arc, Mutex};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Role, Sense, Stroke, Ui,
    Vec2, WidgetInfo,
};
use tg_app_core::chat_helpers::backend::{ChatHelpersBackend, DefaultEmojiCatalogKind};
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::input_field::InputField;

use crate::info::backend::{EmojiUserpicRequest, InfoBackend};
use tg_app_core::stand_in::types::PeerId;

/// Maximum gradient color stops matching `UserpicBuilder::kMaxColors` (`info_userpic_colors_editor.cpp:27`).
pub const EMOJI_USERPIC_MAX_COLORS: usize = 4;

/// Preset emoji entry shown in the Emoji Userpic Builder picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiUserpicPreset {
    /// Synthetic or custom emoji document ID sent in `videoSizeEmojiMarkup.emoji_id`.
    pub emoji_id: i64,
    /// Unicode glyph painted inside the avatar preview.
    pub glyph: &'static str,
    /// Accessible label.
    pub name: &'static str,
}

/// Curated emoji presets for the userpic builder.
pub const DEFAULT_EMOJI_PRESETS: [EmojiUserpicPreset; 12] = [
    EmojiUserpicPreset {
        emoji_id: 5368324170671202286,
        glyph: "🚀",
        name: "Rocket",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202287,
        glyph: "🔥",
        name: "Fire",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202288,
        glyph: "⭐",
        name: "Star",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202289,
        glyph: "💎",
        name: "Gem",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202290,
        glyph: "🎨",
        name: "Palette",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202291,
        glyph: "🎧",
        name: "Headphones",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202292,
        glyph: "⚡",
        name: "Lightning",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202293,
        glyph: "🌿",
        name: "Leaf",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202294,
        glyph: "🦊",
        name: "Fox",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202295,
        glyph: "🛸",
        name: "UFO",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202296,
        glyph: "🏆",
        name: "Trophy",
    },
    EmojiUserpicPreset {
        emoji_id: 5368324170671202297,
        glyph: "❄",
        name: "Snowflake",
    },
];

/// Preset multi-stop gradients matching Telegram's userpic builder palettes (`0xRRGGBB`).
pub const DEFAULT_BG_PALETTES: &[(&str, &[u32])] = &[
    ("Azure", &[0x54A9EB, 0x2B7DE9, 0x7A6FF0]),
    ("Sunrise", &[0xFF9A5A, 0xF55E7C, 0xC850C0]),
    ("Forest", &[0x43CE88, 0x1E9C7B]),
    ("Gold", &[0xF7C948, 0xF08C2E]),
    ("Berry", &[0xB56BFF, 0x6B4EF5, 0x4A7CF7, 0x39B5E0]),
    ("Slate", &[0x52657A, 0x2B394A]),
];

/// Swatch colors available when editing a single color stop in [`EmojiUserpicBoxState`].
pub const STOP_COLOR_SWATCHES: [u32; 8] = [
    0x40A7E3, 0x2EBD6B, 0xF5A623, 0xE55353, 0x9B59B6, 0x1ABC9C, 0xFF6B81, 0x34495E,
];

/// Server-suggested default custom-emoji entry loaded via
/// `account.getDefaultProfilePhotoEmojis` or `account.getDefaultGroupPhotoEmojis`
/// (`api/api_peer_photo.cpp:854-905`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedEmojiEntry {
    /// Custom emoji document ID returned in `emojiList.document_id`.
    pub emoji_id: i64,
    /// Visual Unicode glyph painted via `tg_emoji::EmojiTextureCache`.
    pub glyph: String,
    /// Accessible name and tooltip label.
    pub label: String,
}

/// Curated fallback glyphs for server custom-emoji document IDs whose local sticker metadata
/// is not yet hydrated.
const SUGGESTED_PROFILE_GLYPHS: [&str; 8] = ["🌟", "🎯", "👑", "🦋", "🎧", "🔥", "💎", "🚀"];
const SUGGESTED_GROUP_GLYPHS: [&str; 8] = ["💬", "🌍", "⚡", "🎨", "🏆", "📚", "🎵", "🌿"];

/// Stateful Emoji Userpic Builder dialog (`UserpicBuilder::EmojiBuilderWidget`).
#[derive(Debug, Clone)]
pub struct EmojiUserpicBoxState {
    /// Target peer (self user or group/channel) receiving the emoji avatar.
    pub peer_id: PeerId,
    /// Display title of the peer (for header context).
    pub peer_title: String,
    /// Which server catalog to query (`ProfilePhoto` for users, `GroupPhoto` for groups/channels).
    pub catalog_kind: DefaultEmojiCatalogKind,
    /// Server-suggested default emojis loaded via `ChatHelpersBackend::default_emoji_catalog`.
    pub suggested_emojis: Vec<SuggestedEmojiEntry>,
    /// Whether `default_emoji_catalog` has been queried at least once.
    pub catalog_loaded: bool,
    /// Optional non-fatal error message if fetching the server catalog failed.
    pub catalog_error: Option<String>,
    /// Selected emoji document ID (`videoSizeEmojiMarkup.emoji_id`).
    pub emoji_id: i64,
    /// Selected emoji glyph for live preview.
    pub emoji_glyph: String,
    /// Active background gradient stops (`1..=4` entries, packed `0xRRGGBB`).
    pub background_colors: Vec<u32>,
    /// Currently selected color stop index inside [`Self::background_colors`].
    pub selected_stop: usize,
    /// Optional custom emoji document ID text override.
    pub custom_emoji_id_input: String,
    /// Submission error message, if any.
    pub error: Option<String>,
}

impl EmojiUserpicBoxState {
    /// Creates an Emoji Userpic Builder initialized with the default Rocket + Azure gradient.
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>) -> Self {
        Self::with_catalog_kind(peer_id, peer_title, DefaultEmojiCatalogKind::ProfilePhoto)
    }

    /// Creates an Emoji Userpic Builder configured for a specific [`DefaultEmojiCatalogKind`]
    /// (`ProfilePhoto` for users, `GroupPhoto` for groups/channels).
    #[must_use]
    pub fn with_catalog_kind(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        catalog_kind: DefaultEmojiCatalogKind,
    ) -> Self {
        let first = DEFAULT_EMOJI_PRESETS[0];
        Self {
            peer_id,
            peer_title: peer_title.into(),
            catalog_kind,
            suggested_emojis: Vec::new(),
            catalog_loaded: false,
            catalog_error: None,
            emoji_id: first.emoji_id,
            emoji_glyph: first.glyph.to_owned(),
            background_colors: DEFAULT_BG_PALETTES[0].1.to_vec(),
            selected_stop: 0,
            custom_emoji_id_input: String::new(),
            error: None,
        }
    }

    /// Creates an Emoji Userpic Builder and immediately loads the server default emoji catalog
    /// via [`ChatHelpersBackend::default_emoji_catalog`].
    #[must_use]
    pub fn with_default_catalog(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        catalog_kind: DefaultEmojiCatalogKind,
        chat_helpers: &dyn ChatHelpersBackend,
    ) -> Self {
        let mut state = Self::with_catalog_kind(peer_id, peer_title, catalog_kind);
        state.load_default_catalog(chat_helpers, false);
        state
    }

    /// Loads default suggested emojis from `chat_helpers` (`account.getDefaultProfilePhotoEmojis`
    /// or `account.getDefaultGroupPhotoEmojis`, matching `info_profile_top_bar.cpp:1798-1800`).
    pub fn load_default_catalog(&mut self, chat_helpers: &dyn ChatHelpersBackend, force: bool) {
        self.catalog_loaded = true;
        match chat_helpers.default_emoji_catalog(self.catalog_kind, force) {
            Ok(catalog) => {
                let fallback_table = match self.catalog_kind {
                    DefaultEmojiCatalogKind::GroupPhoto => &SUGGESTED_GROUP_GLYPHS,
                    _ => &SUGGESTED_PROFILE_GLYPHS,
                };
                self.suggested_emojis = catalog
                    .document_ids
                    .iter()
                    .enumerate()
                    .map(|(idx, doc_id)| {
                        let raw_id = doc_id.0 as i64;
                        if let Some(preset) =
                            DEFAULT_EMOJI_PRESETS.iter().find(|p| p.emoji_id == raw_id)
                        {
                            SuggestedEmojiEntry {
                                emoji_id: raw_id,
                                glyph: preset.glyph.to_owned(),
                                label: format!("Suggested {}", preset.name),
                            }
                        } else {
                            let alt = chat_helpers
                                .sticker_document(*doc_id)
                                .map(|d| d.alt_emoji.as_str())
                                .filter(|s| !s.is_empty());
                            let glyph = alt
                                .unwrap_or(fallback_table[idx % fallback_table.len()])
                                .to_owned();
                            SuggestedEmojiEntry {
                                emoji_id: raw_id,
                                glyph,
                                label: format!("Suggested Emoji #{}", doc_id.0),
                            }
                        }
                    })
                    .collect();
                self.catalog_error = None;
                if let Some(first) = self.suggested_emojis.first()
                    && self.emoji_id == DEFAULT_EMOJI_PRESETS[0].emoji_id
                    && self.custom_emoji_id_input.is_empty()
                {
                    self.emoji_id = first.emoji_id;
                    self.emoji_glyph = first.glyph.clone();
                }
            }
            Err(err) => {
                self.catalog_error = Some(err);
            }
        }
    }

    /// Selects a server-suggested default emoji entry.
    pub fn select_suggested(&mut self, entry: &SuggestedEmojiEntry) {
        self.emoji_id = entry.emoji_id;
        self.emoji_glyph = entry.glyph.clone();
        self.custom_emoji_id_input.clear();
        self.error = None;
    }

    /// Selects a built-in emoji preset.
    pub fn select_preset(&mut self, preset: EmojiUserpicPreset) {
        self.emoji_id = preset.emoji_id;
        self.emoji_glyph = preset.glyph.to_owned();
        self.custom_emoji_id_input.clear();
        self.error = None;
    }

    /// Applies one of the built-in background palettes.
    pub fn select_palette(&mut self, colors: &[u32]) {
        if !colors.is_empty() {
            self.background_colors = colors
                .iter()
                .copied()
                .take(EMOJI_USERPIC_MAX_COLORS)
                .collect();
            self.selected_stop = self.selected_stop.min(self.background_colors.len() - 1);
            self.error = None;
        }
    }

    /// Adds a color stop (up to [`EMOJI_USERPIC_MAX_COLORS`]).
    pub fn add_color_stop(&mut self) {
        if self.background_colors.len() < EMOJI_USERPIC_MAX_COLORS {
            let next =
                STOP_COLOR_SWATCHES[self.background_colors.len() % STOP_COLOR_SWATCHES.len()];
            self.background_colors.push(next);
            self.selected_stop = self.background_colors.len() - 1;
        }
    }

    /// Removes the last color stop (keeping at least 1 color).
    pub fn remove_color_stop(&mut self) {
        if self.background_colors.len() > 1 {
            self.background_colors.pop();
            self.selected_stop = self.selected_stop.min(self.background_colors.len() - 1);
        }
    }

    /// Builds the [`EmojiUserpicRequest`] payload from current state.
    pub fn build_request(&self) -> Result<EmojiUserpicRequest, String> {
        let emoji_id = if self.custom_emoji_id_input.trim().is_empty() {
            self.emoji_id
        } else {
            self.custom_emoji_id_input
                .trim()
                .parse::<i64>()
                .map_err(|_| "Custom emoji ID must be a valid integer.".to_owned())?
        };
        if self.background_colors.is_empty()
            || self.background_colors.len() > EMOJI_USERPIC_MAX_COLORS
        {
            return Err("Background requires between 1 and 4 colors.".to_owned());
        }
        Ok(EmojiUserpicRequest {
            emoji_id,
            emoji_glyph: self.emoji_glyph.clone(),
            background_colors: self.background_colors.clone(),
        })
    }

    /// Submits the crafted emoji userpic to `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let req = match self.build_request() {
            Ok(r) => r,
            Err(err) => {
                self.error = Some(err.clone());
                return Err(err);
            }
        };
        match backend.upload_emoji_userpic(self.peer_id, req) {
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
        BoxModal::new(id, "Emoji Avatar Builder")
            .width(st::box_wide_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save_emoji_userpic", "Set Photo"))
    }

    /// Renders the circular avatar preview, gradient stop editor, and emoji picker grid.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let width = (ui.available_width() - 14.0).max(240.0);

        // 1. Live circular preview (`EmojiPreview`)
        let preview_h = 124.0;
        let (prev_rect, _) = ui.allocate_exact_size(Vec2::new(width, preview_h), Sense::hover());
        let center = prev_rect.center();
        let radius = 52.0;
        paint_gradient_avatar_circle(ui.painter(), center, radius, &self.background_colors);
        paint_emoji_glyph(
            ui.painter(),
            ui.ctx(),
            &self.emoji_glyph,
            Rect::from_center_size(center, Vec2::splat(52.0)),
            Color32::WHITE,
        );

        ui.add_space(6.0);

        // 2. Background preset strip
        ui.label(
            egui::RichText::new("Background Presets")
                .font(FontId::new(12.5, family_semibold()))
                .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (name, stops) in DEFAULT_BG_PALETTES {
                let selected = self.background_colors.as_slice() == *stops;
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                resp.widget_info(|| {
                    WidgetInfo::selected(Role::Button, true, selected, format!("Palette {name}"))
                });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                paint_gradient_avatar_circle(ui.painter(), rect.center(), 12.0, stops);
                if selected {
                    ui.painter().circle_stroke(
                        rect.center(),
                        14.0,
                        Stroke::new(2.0, colors.color("windowActiveTextFg")),
                    );
                }
                if resp.on_hover_text(*name).clicked() {
                    self.select_palette(stops);
                }
            }
        });

        ui.add_space(8.0);

        // 3. Multi-stop color line (`ColorsLine`: `-` | Stop 1..4 | `+`)
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Gradient Colors ({}/{})",
                    self.background_colors.len(),
                    EMOJI_USERPIC_MAX_COLORS
                ))
                .font(FontId::new(12.0, family_semibold()))
                .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(6.0);
            if ui
                .add_enabled(self.background_colors.len() > 1, egui::Button::new("−"))
                .on_hover_text("Remove color stop")
                .clicked()
            {
                self.remove_color_stop();
            }
            for idx in 0..self.background_colors.len() {
                let rgb = self.background_colors[idx];
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                resp.widget_info(|| {
                    WidgetInfo::selected(
                        Role::Button,
                        true,
                        self.selected_stop == idx,
                        format!("Color stop {}", idx + 1),
                    )
                });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter()
                    .circle_filled(r.center(), 9.0, rgb_u32_to_color32(rgb));
                if self.selected_stop == idx {
                    ui.painter().circle_stroke(
                        r.center(),
                        11.5,
                        Stroke::new(2.0, colors.color("windowActiveTextFg")),
                    );
                }
                if resp.clicked() {
                    self.selected_stop = idx;
                }
            }
            if ui
                .add_enabled(
                    self.background_colors.len() < EMOJI_USERPIC_MAX_COLORS,
                    egui::Button::new("+"),
                )
                .on_hover_text("Add color stop")
                .clicked()
            {
                self.add_color_stop();
            }
        });

        // Swatches to recolor the currently selected stop
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for &swatch_rgb in &STOP_COLOR_SWATCHES {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter()
                    .circle_filled(r.center(), 8.5, rgb_u32_to_color32(swatch_rgb));
                if resp.clicked()
                    && let Some(slot) = self.background_colors.get_mut(self.selected_stop)
                {
                    *slot = swatch_rgb;
                }
            }
        });

        ui.add_space(10.0);

        // 4. Server-suggested default emoji catalog (`account.getDefaultProfilePhotoEmojis` /
        //    `account.getDefaultGroupPhotoEmojis`)
        if !self.suggested_emojis.is_empty() {
            let section_lbl = match self.catalog_kind {
                DefaultEmojiCatalogKind::GroupPhoto => "Suggested for Group",
                _ => "Suggested for Profile",
            };
            ui.label(
                egui::RichText::new(section_lbl)
                    .font(FontId::new(12.5, family_semibold()))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(4.0);
            let mut chosen_suggested: Option<SuggestedEmojiEntry> = None;
            ui.horizontal_wrapped(|ui| {
                for entry in &self.suggested_emojis {
                    let active =
                        self.emoji_id == entry.emoji_id && self.custom_emoji_id_input.is_empty();
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
                    resp.widget_info(|| {
                        WidgetInfo::selected(Role::Button, true, active, &entry.label)
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let bg = if active {
                        colors.color("windowBgActive")
                    } else if resp.hovered() {
                        colors.color("windowBgOver")
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(r, CornerRadius::same(8), bg);
                    paint_emoji_glyph(
                        ui.painter(),
                        ui.ctx(),
                        &entry.glyph,
                        Rect::from_center_size(r.center(), Vec2::splat(24.0)),
                        colors.color("windowFg"),
                    );
                    if resp.on_hover_text(&entry.label).clicked() {
                        chosen_suggested = Some(entry.clone());
                    }
                }
            });
            if let Some(entry) = chosen_suggested {
                self.select_suggested(&entry);
            }
            ui.add_space(8.0);
        }

        // 5. Built-in emoji presets grid
        ui.label(
            egui::RichText::new("Choose Emoji")
                .font(FontId::new(12.5, family_semibold()))
                .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for preset in DEFAULT_EMOJI_PRESETS {
                let active =
                    self.emoji_id == preset.emoji_id && self.custom_emoji_id_input.is_empty();
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
                resp.widget_info(|| WidgetInfo::selected(Role::Button, true, active, preset.name));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let bg = if active {
                    colors.color("windowBgActive")
                } else if resp.hovered() {
                    colors.color("windowBgOver")
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(r, CornerRadius::same(8), bg);
                paint_emoji_glyph(
                    ui.painter(),
                    ui.ctx(),
                    preset.glyph,
                    Rect::from_center_size(r.center(), Vec2::splat(24.0)),
                    colors.color("windowFg"),
                );
                if resp.on_hover_text(preset.name).clicked() {
                    self.select_preset(preset);
                }
            }
        });

        ui.add_space(8.0);
        InputField::single_line(
            "emoji_userpic_custom_id",
            &mut self.custom_emoji_id_input,
            "Optional custom emoji document ID",
            InputFieldStyle::default_input(colors),
        )
        .show(ui);

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        } else {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("Target: {}", self.peer_title))
                    .font(FontId::new(11.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        }
    }
}

/// Converts a packed `0xRRGGBB` integer into an [`egui::Color32`].
#[must_use]
pub const fn rgb_u32_to_color32(rgb: u32) -> Color32 {
    Color32::from_rgb(
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    )
}

fn paint_gradient_avatar_circle(painter: &egui::Painter, center: Pos2, radius: f32, stops: &[u32]) {
    let base = stops.first().copied().unwrap_or(0x40A7E3);
    painter.circle_filled(center, radius, rgb_u32_to_color32(base));
    if stops.len() >= 2 {
        let second = rgb_u32_to_color32(stops[1]);
        painter.circle_filled(
            Pos2::new(center.x + radius * 0.22, center.y + radius * 0.22),
            radius * 0.72,
            Color32::from_rgba_unmultiplied(second.r(), second.g(), second.b(), 150),
        );
    }
    if stops.len() >= 3 {
        let third = rgb_u32_to_color32(stops[2]);
        painter.circle_filled(
            Pos2::new(center.x - radius * 0.24, center.y - radius * 0.18),
            radius * 0.56,
            Color32::from_rgba_unmultiplied(third.r(), third.g(), third.b(), 120),
        );
    }
    if stops.len() >= 4 {
        let fourth = rgb_u32_to_color32(stops[3]);
        painter.circle_filled(
            Pos2::new(center.x + radius * 0.18, center.y - radius * 0.24),
            radius * 0.44,
            Color32::from_rgba_unmultiplied(fourth.r(), fourth.g(), fourth.b(), 110),
        );
    }
}

/// Paints a Unicode emoji glyph via per-context `tg_emoji::EmojiTextureCache` with text fallback.
fn paint_emoji_glyph(
    painter: &egui::Painter,
    ctx: &egui::Context,
    emoji: &str,
    rect: Rect,
    fallback_color: Color32,
) {
    let cache = ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(Id::unique("tg_emoji_userpic_emoji_cache"), || {
            Arc::new(Mutex::new(EmojiTextureCache::new()))
        })
        .clone()
    });
    let painted = cache
        .lock()
        .map(|mut guard| guard.paint_emoji_str(painter, ctx, emoji, rect, Color32::WHITE))
        .unwrap_or(false);
    if !painted {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            emoji,
            FontId::proportional((rect.height() * 0.8).clamp(16.0, 44.0)),
            fallback_color,
        );
    }
}
