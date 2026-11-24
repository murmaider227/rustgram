//! GIFs picker widget with mosaic grid layout, inline-bot `@gif` search hook,
//! animated multi-frame previews, saved GIFs toggling, and category footer.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/gifs_list_widget.h`,
//! `Telegram/SourceFiles/chat_helpers/gifs_list_widget.cpp`, and `layout/layout_mosaic.cpp`.

use std::collections::HashMap;

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Key, Margin, Pos2, Rect, ScrollArea, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_chat::DocumentId;
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_ui::tg_style::st;

use crate::chat_helpers::backend::{ChatHelpersBackend, GifItem};
use crate::chat_helpers::emoji_list_widget::{EMOJI_FOOTER_HEIGHT, render_search_bar};
use crate::chat_helpers::paint_helper_icon;
use crate::chat_helpers::stickers_list_widget::paint_clock_icon;
use crate::chat_helpers::strings;
use tg_app_core::stand_in::theme::Palette;

/// Gap between mosaic tiles (`inlineResultsSkip = 2px`).
pub const MOSAIC_GAP: f32 = 2.0;
/// Target row height for the GIF mosaic grid.
pub const MOSAIC_TARGET_ROW_HEIGHT: f32 = 90.0;

/// Single laid-out mosaic tile rectangle paired with its item index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MosaicTile {
    /// Index into the input items slice.
    pub index: usize,
    /// Tile rectangle relative to `(0, 0)` of the mosaic grid.
    pub rect: Rect,
}

/// Computes a balanced multi-row mosaic layout filling `available_width` with `MOSAIC_GAP` spacing,
/// matching tdesktop's `Mosaic::Layout::Layout` row-justification algorithm.
#[must_use]
pub fn layout_mosaic_rows(
    items: &[GifItem],
    available_width: f32,
    target_row_height: f32,
) -> (Vec<MosaicTile>, f32) {
    let width = available_width.max(120.0);
    let mut tiles = Vec::with_capacity(items.len());
    let mut y = 0.0f32;
    let mut i = 0usize;

    while i < items.len() {
        let mut row_end = i;
        let mut aspect_sum = 0.0f32;
        while row_end < items.len() {
            let item = &items[row_end];
            let aspect = (item.width.max(1) as f32 / item.height.max(1) as f32).clamp(0.55, 2.2);
            let next_count = (row_end - i + 1) as f32;
            let gaps = (next_count - 1.0) * MOSAIC_GAP;
            let row_w_at_target = (aspect_sum + aspect) * target_row_height + gaps;
            aspect_sum += aspect;
            row_end += 1;
            if row_w_at_target >= width * 0.92 || (row_end - i) >= 3 {
                break;
            }
        }

        let count = row_end - i;
        let gaps = ((count.saturating_sub(1)) as f32) * MOSAIC_GAP;
        let is_last_sparse =
            row_end == items.len() && aspect_sum * target_row_height + gaps < width * 0.72;
        let row_h = if is_last_sparse {
            target_row_height
        } else {
            ((width - gaps) / aspect_sum.max(0.1)).clamp(64.0, 126.0)
        };

        let mut x = 0.0f32;
        for (offset, item) in items[i..row_end].iter().enumerate() {
            let idx = i + offset;
            let aspect = (item.width.max(1) as f32 / item.height.max(1) as f32).clamp(0.55, 2.2);
            let tile_w = if !is_last_sparse && idx + 1 == row_end {
                (width - x).max(32.0)
            } else {
                (aspect * row_h).round()
            };
            tiles.push(MosaicTile {
                index: idx,
                rect: Rect::from_min_size(Pos2::new(x, y), Vec2::new(tile_w, row_h)),
            });
            x += tile_w + MOSAIC_GAP;
        }
        y += row_h + MOSAIC_GAP;
        i = row_end;
    }

    (tiles, (y - MOSAIC_GAP).max(0.0))
}

/// Events emitted by [`GifsListWidget::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GifsListEvent {
    /// User chose a GIF to send.
    Chosen {
        /// Selected GIF document ID.
        gif_id: DocumentId,
        /// Display title of the GIF.
        title: String,
    },
}

struct GifTextureSlot {
    last_frame: usize,
    texture: TextureHandle,
}

/// Stateful GIFs tab widget (`GifsListWidget`).
pub struct GifsListWidget {
    search_query: String,
    active_category_id: String,
    focused_index: Option<usize>,
    textures: HashMap<DocumentId, GifTextureSlot>,
}

impl std::fmt::Debug for GifsListWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GifsListWidget")
            .field("search_query", &self.search_query)
            .field("active_category_id", &self.active_category_id)
            .field("focused_index", &self.focused_index)
            .field("cached_textures", &self.textures.len())
            .finish()
    }
}

impl Default for GifsListWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl GifsListWidget {
    /// Creates a new [`GifsListWidget`] starting on the Saved/Recent GIFs category.
    #[must_use]
    pub fn new() -> Self {
        Self {
            search_query: String::new(),
            active_category_id: "recent".into(),
            focused_index: None,
            textures: HashMap::new(),
        }
    }

    /// Returns the active GIF search query.
    #[must_use]
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Sets the GIF search query programmatically (dispatched through the inline-bot query hook).
    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.search_query = query.into();
        self.focused_index = None;
    }

    /// Selects a footer category by id (and updates the search query if applicable).
    pub fn select_category(&mut self, category_id: &str, query: &str) {
        self.active_category_id = category_id.to_string();
        self.search_query = query.to_string();
        self.focused_index = None;
    }

    /// Resolves the current list of GIFs to render (either saved GIFs or inline-bot search results).
    #[must_use]
    pub fn current_items(&self, backend: &dyn ChatHelpersBackend) -> Vec<GifItem> {
        let q = self.search_query.trim();
        if q.is_empty() && self.active_category_id == "recent" {
            backend.saved_gifs()
        } else {
            backend.search_gifs(q)
        }
    }

    /// Handles keyboard navigation (`Arrow` keys, `Enter`, `Escape`).
    pub fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        backend: &dyn ChatHelpersBackend,
    ) -> (bool, Option<GifsListEvent>) {
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if !self.search_query.is_empty() || self.active_category_id != "recent" {
                self.search_query.clear();
                self.active_category_id = "recent".into();
                self.focused_index = None;
                return (true, None);
            }
            return (false, None);
        }

        let left = ctx.input(|i| i.key_pressed(Key::ArrowLeft));
        let right = ctx.input(|i| i.key_pressed(Key::ArrowRight));
        let up = ctx.input(|i| i.key_pressed(Key::ArrowUp));
        let down = ctx.input(|i| i.key_pressed(Key::ArrowDown));
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));

        if !(left || right || up || down || enter) {
            return (false, None);
        }

        let items = self.current_items(backend);
        if items.is_empty() {
            return (false, None);
        }
        let max_idx = items.len() - 1;

        if enter {
            if let Some(idx) = self.focused_index
                && let Some(item) = items.get(idx)
            {
                return (
                    true,
                    Some(GifsListEvent::Chosen {
                        gif_id: item.id,
                        title: item.title.clone(),
                    }),
                );
            }
            return (false, None);
        }

        let cur = self.focused_index.unwrap_or(0);
        let next = if right {
            (cur + 1).min(max_idx)
        } else if left {
            cur.saturating_sub(1)
        } else if down {
            (cur + 2).min(max_idx)
        } else if up {
            cur.saturating_sub(2)
        } else {
            cur
        };
        self.focused_index = Some(next);
        (true, None)
    }

    /// Renders the GIFs tab: `@gif` inline search bar, mosaic grid, animated previews, and footer.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) -> Option<GifsListEvent> {
        let mut event = None;
        let full_rect = ui.available_rect_before_wrap();
        let footer_rect = Rect::from_min_max(
            Pos2::new(full_rect.left(), full_rect.bottom() - EMOJI_FOOTER_HEIGHT),
            full_rect.max,
        );
        let body_rect = Rect::from_min_max(
            full_rect.min,
            Pos2::new(full_rect.right(), footer_rect.top()),
        );

        let time_secs = ui.input(|i| i.time);
        let mut any_visible = false;

        let dark = ui.ctx().global_style().visuals.dark_mode;
        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            // 1. Inline-bot `@gif` search bar.
            let search_ph = tr::lng_dlg_filter();
            Frame::NONE
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    if render_search_bar(
                        ui,
                        "tg_gifs_search_field",
                        &mut self.search_query,
                        &search_ph,
                        Some(strings::INLINE_GIF_BOT_PREFIX),
                        palette,
                    ) {
                        if self.search_query.is_empty() {
                            self.active_category_id = "recent".into();
                        } else {
                            self.active_category_id = "search".into();
                        }
                        self.focused_index = None;
                    }
                });

            let items = self.current_items(backend);

            // 2. Scrollable mosaic grid.
            let scroll_rect = ui.available_rect_before_wrap();
            ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt("tg_gifs_list_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::NONE
                            .inner_margin(Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                if items.is_empty() {
                                    ui.add_space(36.0);
                                    ui.vertical_centered(|ui| {
                                        let empty_label = if self.search_query.trim().is_empty() {
                                            tr::lng_gifs_no_saved()
                                        } else {
                                            tr::lng_search_messages_none()
                                        };
                                        ui.label(
                                            egui::RichText::new(empty_label)
                                                .size(13.5)
                                                .color(palette.window_sub_text_fg),
                                        );
                                    });
                                    return;
                                }

                                let avail_w = ui.available_width().max(120.0);
                                let (tiles, total_h) =
                                    layout_mosaic_rows(&items, avail_w, MOSAIC_TARGET_ROW_HEIGHT);
                                let (grid_rect, _) = ui.allocate_exact_size(
                                    Vec2::new(avail_w, total_h.max(60.0)),
                                    Sense::hover(),
                                );
                                let clip = ui.clip_rect();

                                for tile in tiles {
                                    let screen_rect = tile.rect.translate(grid_rect.min.to_vec2());
                                    if !screen_rect.intersects(clip) {
                                        continue;
                                    }
                                    any_visible = true;
                                    let item = &items[tile.index];
                                    let resp = ui.interact(
                                        screen_rect,
                                        ui.make_persistent_id((
                                            "tg_gif_tile",
                                            tile.index,
                                            item.id.0,
                                        )),
                                        Sense::click(),
                                    );

                                    let frame_idx = ((time_secs * f64::from(item.fps)) as usize)
                                        % item.frame_count.max(1);
                                    if let Some(tex_id) =
                                        self.frame_texture(ui.ctx(), item.id, frame_idx, backend)
                                    {
                                        ui.painter().image(
                                            tex_id,
                                            screen_rect,
                                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                            Color32::WHITE,
                                        );
                                    } else {
                                        ui.painter().rect_filled(
                                            screen_rect,
                                            CornerRadius::same(4),
                                            palette.input_bg,
                                        );
                                    }

                                    // Focus ring or hover tint
                                    let is_focused = self.focused_index == Some(tile.index);
                                    if resp.hovered() {
                                        ui.painter().rect_filled(
                                            screen_rect,
                                            CornerRadius::same(3),
                                            Color32::from_rgba_unmultiplied(0, 0, 0, 48),
                                        );
                                    }
                                    if is_focused {
                                        ui.painter().rect_stroke(
                                            screen_rect.shrink(1.0),
                                            CornerRadius::same(4),
                                            Stroke::new(2.0, palette.window_bg_active),
                                            StrokeKind::Inside,
                                        );
                                    }

                                    // Compact GIF badge / hover title bar
                                    let label_rect = Rect::from_min_max(
                                        Pos2::new(
                                            screen_rect.left() + 4.0,
                                            screen_rect.bottom() - 18.0,
                                        ),
                                        Pos2::new(
                                            screen_rect.right() - 4.0,
                                            screen_rect.bottom() - 3.0,
                                        ),
                                    );
                                    if resp.hovered() {
                                        ui.painter().rect_filled(
                                            label_rect,
                                            CornerRadius::same(4),
                                            Color32::from_rgba_unmultiplied(0, 0, 0, 145),
                                        );
                                        ui.painter().text(
                                            Pos2::new(
                                                label_rect.left() + 5.0,
                                                label_rect.center().y,
                                            ),
                                            Align2::LEFT_CENTER,
                                            &item.title,
                                            FontId::proportional(10.5),
                                            Color32::WHITE,
                                        );
                                    } else {
                                        let badge = Rect::from_min_size(
                                            Pos2::new(
                                                screen_rect.left() + 4.0,
                                                screen_rect.top() + 4.0,
                                            ),
                                            Vec2::new(26.0, 14.0),
                                        );
                                        ui.painter().rect_filled(
                                            badge,
                                            CornerRadius::same(4),
                                            Color32::from_rgba_unmultiplied(0, 0, 0, 120),
                                        );
                                        ui.painter().text(
                                            badge.center(),
                                            Align2::CENTER_CENTER,
                                            strings::GIF_BADGE,
                                            FontId::proportional(9.5),
                                            Color32::WHITE,
                                        );
                                    }

                                    // Hover Save / Remove button in top-right corner (`st::sticker_pan_delete_icon_fg` / `st::craft_add_icon`).
                                    let mut save_clicked = false;
                                    if resp.hovered() {
                                        let btn_r = Rect::from_min_size(
                                            Pos2::new(
                                                screen_rect.right() - 22.0,
                                                screen_rect.top() + 4.0,
                                            ),
                                            Vec2::splat(18.0),
                                        );
                                        let btn_resp = ui.interact(
                                            btn_r,
                                            ui.make_persistent_id((
                                                "tg_gif_save_btn",
                                                tile.index,
                                                item.id.0,
                                            )),
                                            Sense::click(),
                                        );
                                        let bg_alpha = if btn_resp.hovered() { 180 } else { 125 };
                                        ui.painter().circle_filled(
                                            btn_r.center(),
                                            9.0,
                                            Color32::from_rgba_unmultiplied(0, 0, 0, bg_alpha),
                                        );
                                        let action_icon = if item.saved {
                                            st::sticker_pan_delete_icon_fg
                                        } else {
                                            st::craft_add_icon
                                        };
                                        if !paint_helper_icon(
                                            ui.painter(),
                                            btn_r.shrink(3.0),
                                            action_icon,
                                            dark,
                                            Some(Color32::WHITE),
                                        ) {
                                            ui.painter().text(
                                                btn_r.center(),
                                                Align2::CENTER_CENTER,
                                                if item.saved { "×" } else { "+" },
                                                FontId::proportional(12.5),
                                                Color32::WHITE,
                                            );
                                        }
                                        if btn_resp
                                            .on_hover_text(if item.saved {
                                                tr::lng_context_delete_gif()
                                            } else {
                                                tr::lng_context_save_gif()
                                            })
                                            .clicked()
                                        {
                                            backend.toggle_saved_gif(item.id);
                                            save_clicked = true;
                                        }
                                    }

                                    if !save_clicked && resp.clicked() {
                                        self.focused_index = Some(tile.index);
                                        event = Some(GifsListEvent::Chosen {
                                            gif_id: item.id,
                                            title: item.title.clone(),
                                        });
                                    }
                                }
                            });
                    });
            });
        });

        // 3. Bottom category footer.
        let categories = backend.gif_categories();
        self.paint_footer(ui, footer_rect, &categories, emoji_cache, palette);

        if any_visible {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(75));
        }

        event
    }

    fn frame_texture(
        &mut self,
        ctx: &egui::Context,
        id: DocumentId,
        frame_idx: usize,
        backend: &dyn ChatHelpersBackend,
    ) -> Option<egui::TextureId> {
        if let Some(slot) = self.textures.get_mut(&id) {
            if slot.last_frame != frame_idx
                && let Some((w, h, rgba)) = backend.gif_frame_rgba(id, frame_idx)
            {
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                slot.texture.set(img, TextureOptions::LINEAR);
                slot.last_frame = frame_idx;
            }
            return Some(slot.texture.id());
        }
        let (w, h, rgba) = backend.gif_frame_rgba(id, frame_idx)?;
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let handle = ctx.load_texture(format!("tg_gif_{}", id.0), img, TextureOptions::LINEAR);
        let tex_id = handle.id();
        self.textures.insert(
            id,
            GifTextureSlot {
                last_frame: frame_idx,
                texture: handle,
            },
        );
        Some(tex_id)
    }

    fn paint_footer(
        &mut self,
        ui: &mut Ui,
        footer_rect: Rect,
        categories: &[crate::chat_helpers::backend::GifCategory],
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) {
        ui.painter().rect_filled(
            footer_rect,
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: 8,
                se: 8,
            },
            palette.window_bg,
        );
        ui.painter().line_segment(
            [
                Pos2::new(footer_rect.left(), footer_rect.top()),
                Pos2::new(footer_rect.right(), footer_rect.top()),
            ],
            Stroke::new(1.0, palette.divider),
        );

        let count = categories.len().max(1);
        let slot_w = (footer_rect.width() / count as f32).min(46.0);
        let total_w = slot_w * count as f32;
        let start_x = footer_rect.left() + (footer_rect.width() - total_w) * 0.5;

        for (idx, cat) in categories.iter().enumerate() {
            let slot_rect = Rect::from_min_size(
                Pos2::new(start_x + idx as f32 * slot_w, footer_rect.top()),
                Vec2::new(slot_w, footer_rect.height()),
            );
            let resp = ui.interact(
                slot_rect,
                ui.make_persistent_id(("tg_gif_footer_cat", idx, &cat.id)),
                Sense::click(),
            );
            let active = self.active_category_id == cat.id;

            if resp.hovered() {
                ui.painter().rect_filled(
                    slot_rect.shrink(3.0),
                    CornerRadius::same(6),
                    palette.window_bg_over,
                );
            }
            if active {
                ui.painter().rect_filled(
                    Rect::from_min_size(
                        Pos2::new(slot_rect.left() + 6.0, slot_rect.bottom() - 2.5),
                        Vec2::new(slot_rect.width() - 12.0, 2.5),
                    ),
                    CornerRadius::same(1),
                    palette.window_bg_active,
                );
            }

            if cat.emoji.is_empty() {
                paint_clock_icon(
                    ui.painter(),
                    slot_rect.center(),
                    7.5,
                    if active {
                        palette.window_bg_active
                    } else {
                        palette.window_sub_text_fg
                    },
                );
            } else {
                let icon_rect = Rect::from_center_size(slot_rect.center(), Vec2::splat(19.0));
                emoji_cache.paint_emoji_str(
                    ui.painter(),
                    ui.ctx(),
                    &cat.emoji,
                    icon_rect,
                    Color32::WHITE,
                );
            }

            if resp.on_hover_text(&cat.label).clicked() {
                self.select_category(&cat.id, &cat.query);
            }
        }
    }
}
