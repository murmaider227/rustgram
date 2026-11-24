//! Stickers picker widget with animated `tg-lottie` previews, recent/favorite sections,
//! emoji/keyword search, premium-locked badges, and bottom pack icon strip.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/stickers_list_widget.h`,
//! `Telegram/SourceFiles/chat_helpers/stickers_list_widget.cpp`, and `stickers_list_footer.cpp`.

use std::collections::HashMap;

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Key, Margin, Pos2, Rect, ScrollArea, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_chat::stickers::{FAVED_SET_ID, RECENT_SET_ID};
use tg_chat::{DocumentId, StickerSetId};
use tg_lang::tr;
use tg_lottie::{Composition, FrameBuffer, Size as LottieSize, render_composition_into};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::chat_helpers::backend::{ChatHelpersBackend, StickerMedia};
use crate::chat_helpers::emoji_list_widget::{EMOJI_FOOTER_HEIGHT, render_search_bar};
use crate::chat_helpers::gift_box_pack::{GiftBoxPack, GiftBoxPackKind, GiftBoxSticker};
use crate::chat_helpers::paint_helper_icon;
use tg_app_core::stand_in::theme::Palette;

/// Sticker grid columns in the 345px panel (`floor((345 - 22) / 64) = 5`).
pub const STICKER_COLUMNS: usize = 5;
/// Sticker cell side matching `st::stickerPanSize`.
pub const STICKER_CELL_SIZE: f32 = st::sticker_pan_size.width.0 as f32;
/// Rasterized preview resolution for animated Lottie stickers in the grid.
pub const LOTTIE_PREVIEW_PX: u32 = 96;

/// Events emitted by [`StickersListWidget::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StickersListEvent {
    /// User selected a sticker to send.
    Chosen {
        /// Selected sticker document ID.
        document_id: DocumentId,
        /// Owning sticker set ID, if known.
        set_id: Option<StickerSetId>,
    },
    /// User clicked a Premium-locked sticker without an active Premium subscription.
    PremiumRequired(DocumentId),
    /// User installed a featured/trending sticker pack.
    SetInstalled(StickerSetId),
    /// User uninstalled a sticker pack.
    SetRemoved(StickerSetId),
}

struct LottiePreviewSlot {
    composition: Composition,
    last_frame_idx: Option<u32>,
    buffer: FrameBuffer,
    texture: Option<TextureHandle>,
}

/// Shared texture & `tg-lottie` software rasterizer cache for sticker thumbnails and grid items.
#[derive(Default)]
pub struct StickerMediaCache {
    static_textures: HashMap<DocumentId, TextureHandle>,
    lottie_slots: HashMap<DocumentId, LottiePreviewSlot>,
    failed: HashMap<DocumentId, ()>,
}

impl std::fmt::Debug for StickerMediaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StickerMediaCache")
            .field("static_count", &self.static_textures.len())
            .field("lottie_count", &self.lottie_slots.len())
            .finish()
    }
}

impl StickerMediaCache {
    /// Creates an empty media cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns an `egui::TextureId` for `doc_id` at animation timestamp `time_secs`,
    /// rasterizing Lottie compositions live via `tg-lottie` when needed.
    ///
    /// Returns `(texture_id, is_animated)` so the caller knows whether to request repaint.
    pub fn texture_for(
        &mut self,
        ctx: &egui::Context,
        doc_id: DocumentId,
        time_secs: f64,
        backend: &dyn ChatHelpersBackend,
    ) -> Option<(egui::TextureId, bool)> {
        if let Some(tex) = self.static_textures.get(&doc_id) {
            return Some((tex.id(), false));
        }
        if let Some(slot) = self.lottie_slots.get_mut(&doc_id) {
            return Self::update_lottie_slot(ctx, doc_id, slot, time_secs).map(|id| (id, true));
        }
        if self.failed.contains_key(&doc_id) {
            return None;
        }

        let media = backend.sticker_media(doc_id)?;
        match media {
            StickerMedia::StaticRgba {
                width,
                height,
                rgba,
            } => {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    &rgba,
                );
                let handle = ctx.load_texture(
                    format!("tg_sticker_static_{}", doc_id.0),
                    img,
                    TextureOptions::LINEAR,
                );
                let id = handle.id();
                self.static_textures.insert(doc_id, handle);
                Some((id, false))
            }
            StickerMedia::LottieJson(json) => match Composition::from_json(&json) {
                Ok(comp) => {
                    let mut slot = LottiePreviewSlot {
                        composition: comp,
                        last_frame_idx: None,
                        buffer: FrameBuffer::new(LottieSize::new(
                            LOTTIE_PREVIEW_PX,
                            LOTTIE_PREVIEW_PX,
                        )),
                        texture: None,
                    };
                    let tex_id = Self::update_lottie_slot(ctx, doc_id, &mut slot, time_secs);
                    self.lottie_slots.insert(doc_id, slot);
                    tex_id.map(|id| (id, true))
                }
                Err(_) => {
                    self.failed.insert(doc_id, ());
                    None
                }
            },
            StickerMedia::LottieTgs(bytes) => {
                let unpacked = tg_lottie::unpack_gzip(&bytes).ok().and_then(|raw| {
                    let s = std::str::from_utf8(&raw).ok()?;
                    Composition::from_json(s).ok()
                });
                if let Some(comp) = unpacked {
                    let mut slot = LottiePreviewSlot {
                        composition: comp,
                        last_frame_idx: None,
                        buffer: FrameBuffer::new(LottieSize::new(
                            LOTTIE_PREVIEW_PX,
                            LOTTIE_PREVIEW_PX,
                        )),
                        texture: None,
                    };
                    let tex_id = Self::update_lottie_slot(ctx, doc_id, &mut slot, time_secs);
                    self.lottie_slots.insert(doc_id, slot);
                    tex_id.map(|id| (id, true))
                } else {
                    self.failed.insert(doc_id, ());
                    None
                }
            }
        }
    }

    fn update_lottie_slot(
        ctx: &egui::Context,
        doc_id: DocumentId,
        slot: &mut LottiePreviewSlot,
        time_secs: f64,
    ) -> Option<egui::TextureId> {
        let total_frames = (slot.composition.out_point - slot.composition.in_point).max(1.0);
        let fps = slot.composition.frame_rate.clamp(1.0, 60.0);
        let frame_idx = ((time_secs * fps) as u64 % (total_frames.round() as u64).max(1)) as u32;

        if slot.last_frame_idx != Some(frame_idx) || slot.texture.is_none() {
            let frame_f = slot.composition.in_point + f64::from(frame_idx);
            render_composition_into(&slot.composition, frame_f, &mut slot.buffer);
            // `tg-lottie` renders premultiplied RGBA8 (via tiny-skia).
            let img = egui::ColorImage::from_rgba_premultiplied(
                [slot.buffer.width as usize, slot.buffer.height as usize],
                &slot.buffer.data,
            );
            if let Some(handle) = slot.texture.as_mut() {
                handle.set(img, TextureOptions::LINEAR);
            } else {
                slot.texture = Some(ctx.load_texture(
                    format!("tg_sticker_lottie_{}", doc_id.0),
                    img,
                    TextureOptions::LINEAR,
                ));
            }
            slot.last_frame_idx = Some(frame_idx);
        }

        slot.texture.as_ref().map(TextureHandle::id)
    }
}

/// Stateful Stickers tab widget (`StickersListWidget`).
#[derive(Debug, Default)]
pub struct StickersListWidget {
    search_query: String,
    active_set_id: Option<StickerSetId>,
    scroll_to_set: Option<StickerSetId>,
    focused_index: Option<usize>,
    media_cache: StickerMediaCache,
    gift_box_pack: GiftBoxPack,
    gift_box_kind: GiftBoxPackKind,
}

impl StickersListWidget {
    /// Creates a new [`StickersListWidget`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the gift-box sticker pack resolver (`Stickers::GiftBoxPack`).
    #[must_use]
    pub fn gift_box_pack(&self) -> &GiftBoxPack {
        &self.gift_box_pack
    }

    /// Returns mutable access to the gift-box sticker pack resolver (`Stickers::GiftBoxPack`).
    pub fn gift_box_pack_mut(&mut self) -> &mut GiftBoxPack {
        &mut self.gift_box_pack
    }

    /// Returns the active gift-box ribbon mode (Premium/Stars vs TON).
    #[must_use]
    pub const fn gift_box_kind(&self) -> GiftBoxPackKind {
        self.gift_box_kind
    }

    /// Switches the gift-box sticker ribbon between Premium/Stars and TON tiers.
    pub fn set_gift_box_kind(&mut self, kind: GiftBoxPackKind) {
        self.gift_box_kind = kind;
    }

    /// Returns the current search query.
    #[must_use]
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Sets the search query programmatically.
    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.search_query = query.into();
        self.focused_index = None;
    }

    /// Jumps scroll to a specific sticker set section.
    pub fn jump_to_set(&mut self, set_id: StickerSetId) {
        self.search_query.clear();
        self.active_set_id = Some(set_id);
        self.scroll_to_set = Some(set_id);
    }

    /// Provides mutable access to the sticker media cache (shared with autocomplete sticker previews).
    pub fn media_cache_mut(&mut self) -> &mut StickerMediaCache {
        &mut self.media_cache
    }

    /// Handles keyboard navigation (`Arrow` keys, `Enter`, `Escape`).
    pub fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn ChatHelpersBackend,
    ) -> (bool, Option<StickersListEvent>) {
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if !self.search_query.is_empty() {
                self.search_query.clear();
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

        let flat = self.collect_visible_docs(backend);
        if flat.is_empty() {
            return (false, None);
        }
        let max_idx = flat.len() - 1;

        if enter {
            if let Some(idx) = self.focused_index
                && let Some(&doc_id) = flat.get(idx)
            {
                return (true, Some(self.activate_sticker(doc_id, backend)));
            }
            return (false, None);
        }

        let cur = self.focused_index.unwrap_or(0);
        let next = if right {
            (cur + 1).min(max_idx)
        } else if left {
            cur.saturating_sub(1)
        } else if down {
            (cur + STICKER_COLUMNS).min(max_idx)
        } else if up {
            cur.saturating_sub(STICKER_COLUMNS)
        } else {
            cur
        };
        self.focused_index = Some(next);
        (true, None)
    }

    fn collect_visible_docs(&self, backend: &dyn ChatHelpersBackend) -> Vec<DocumentId> {
        if !self.search_query.trim().is_empty() {
            return backend.search_stickers(self.search_query.trim());
        }
        let mut docs = Vec::new();
        for sec in backend.sticker_sections() {
            docs.extend(sec.stickers);
        }
        for sec in backend.featured_sticker_sections() {
            docs.extend(sec.stickers);
        }
        docs
    }

    fn activate_sticker(
        &self,
        doc_id: DocumentId,
        backend: &mut dyn ChatHelpersBackend,
    ) -> StickersListEvent {
        let is_locked = backend
            .sticker_document(doc_id)
            .is_some_and(|d| d.is_premium_sticker)
            && !backend.is_premium();
        if is_locked {
            return StickersListEvent::PremiumRequired(doc_id);
        }
        let set_id = backend.sticker_document(doc_id).and_then(|d| d.set_id);
        backend.record_sticker_used(doc_id);
        StickersListEvent::Chosen {
            document_id: doc_id,
            set_id,
        }
    }

    /// Renders the Stickers tab: search bar, Favorites/Recent/Installed/Trending grids, and pack footer.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
    ) -> Option<StickersListEvent> {
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

        let my_sections = backend.sticker_sections();
        let featured_sections = backend.featured_sticker_sections();
        let time_secs = ui.input(|i| i.time);
        let mut any_animated_visible = false;

        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            // 1. Search bar (search by emoji or pack keyword).
            let search_ph = tr::lng_dlg_filter();
            Frame::NONE
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    render_search_bar(
                        ui,
                        "tg_stickers_search_field",
                        &mut self.search_query,
                        &search_ph,
                        None,
                        palette,
                    );
                });

            // 2. Scrollable stickers list.
            let scroll_rect = ui.available_rect_before_wrap();
            ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt("tg_stickers_list_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::NONE
                            .inner_margin(Margin::symmetric(8, 2))
                            .show(ui, |ui| {
                                if !self.search_query.trim().is_empty() {
                                    let hits = backend.search_stickers(self.search_query.trim());
                                    if hits.is_empty() {
                                        ui.add_space(36.0);
                                        ui.vertical_centered(|ui| {
                                            ui.label(
                                                egui::RichText::new(
                                                    tr::lng_stickers_nothing_found(),
                                                )
                                                .size(13.5)
                                                .color(palette.window_sub_text_fg),
                                            );
                                        });
                                    } else {
                                        let mut g_idx = 0usize;
                                        if let Some(ev) = self.paint_sticker_grid(
                                            ui,
                                            &hits,
                                            &mut g_idx,
                                            time_secs,
                                            &mut any_animated_visible,
                                            backend,
                                            palette,
                                        ) {
                                            event = Some(ev);
                                        }
                                    }
                                } else {
                                    // Gift-box sticker pack ribbon (`stickers_gift_box_pack.cpp`).
                                    if let Some(ev) =
                                        self.paint_gift_box_pack_ribbon(ui, backend, palette)
                                    {
                                        event = Some(ev);
                                    }

                                    let mut g_idx = 0usize;
                                    for sec in &my_sections {
                                        let hdr_r = self.paint_section_header(
                                            ui,
                                            sec.id,
                                            &sec.title,
                                            sec.stickers.len(),
                                            false,
                                            backend,
                                            palette,
                                            &mut event,
                                        );
                                        if self.scroll_to_set == Some(sec.id) {
                                            ui.scroll_to_rect(hdr_r, Some(egui::Align::TOP));
                                            self.scroll_to_set = None;
                                        }
                                        if hdr_r.top() <= scroll_rect.top() + 36.0 {
                                            self.active_set_id = Some(sec.id);
                                        }
                                        if let Some(ev) = self.paint_sticker_grid(
                                            ui,
                                            &sec.stickers,
                                            &mut g_idx,
                                            time_secs,
                                            &mut any_animated_visible,
                                            backend,
                                            palette,
                                        ) {
                                            event = Some(ev);
                                        }
                                    }

                                    // Featured / Trending packs section
                                    if !featured_sections.is_empty() {
                                        ui.add_space(6.0);
                                        ui.painter().line_segment(
                                            [
                                                Pos2::new(
                                                    ui.available_rect_before_wrap().left(),
                                                    ui.cursor().top(),
                                                ),
                                                Pos2::new(
                                                    ui.available_rect_before_wrap().right(),
                                                    ui.cursor().top(),
                                                ),
                                            ],
                                            Stroke::new(1.0, palette.divider),
                                        );
                                        ui.add_space(4.0);
                                        for sec in &featured_sections {
                                            let _ = self.paint_section_header(
                                                ui,
                                                sec.id,
                                                &sec.title,
                                                sec.stickers.len(),
                                                true,
                                                backend,
                                                palette,
                                                &mut event,
                                            );
                                            if let Some(ev) = self.paint_sticker_grid(
                                                ui,
                                                &sec.stickers,
                                                &mut g_idx,
                                                time_secs,
                                                &mut any_animated_visible,
                                                backend,
                                                palette,
                                            ) {
                                                event = Some(ev);
                                            }
                                        }
                                    }
                                }
                            });
                    });
            });
        });

        // 3. Bottom pack icon footer (`StickersListFooter`).
        let icons = backend.sticker_tab_icons(&my_sections, 24);
        self.paint_footer(ui, footer_rect, &icons, time_secs, backend, palette);

        if any_animated_visible {
            // Smooth 30fps playback for visible Lottie animated stickers.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }

        event
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_section_header(
        &self,
        ui: &mut Ui,
        set_id: StickerSetId,
        title: &str,
        count: usize,
        is_featured: bool,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
        event_out: &mut Option<StickersListEvent>,
    ) -> Rect {
        let width = ui.available_width().max(120.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());

        ui.painter().text(
            Pos2::new(rect.left() + 4.0, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            font_id_from_style(st::stickers_trending_header_font),
            if is_featured {
                palette.window_bold_fg
            } else {
                palette.window_sub_text_fg
            },
        );

        if is_featured {
            let installed = backend.is_set_installed(set_id);
            let label = if installed {
                tr::lng_stickers_featured_installed()
            } else {
                tr::lng_stickers_featured_add()
            };
            let btn_rect = Rect::from_min_size(
                Pos2::new(rect.right() - 56.0, rect.center().y - 11.0),
                Vec2::new(52.0, 22.0),
            );
            let resp = ui.interact(
                btn_rect,
                ui.make_persistent_id(("tg_featured_add_btn", set_id.0)),
                Sense::click(),
            );
            let fill = if installed {
                palette.window_bg_over
            } else {
                palette.window_bg_active
            };
            let fg = if installed {
                palette.window_active_text_fg
            } else {
                palette.window_fg_active
            };
            ui.painter()
                .rect_filled(btn_rect, CornerRadius::same(11), fill);
            ui.painter().text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.5),
                fg,
            );
            if resp.clicked() {
                if installed {
                    if backend.remove_sticker_set(set_id) {
                        *event_out = Some(StickersListEvent::SetRemoved(set_id));
                    }
                } else if backend.install_sticker_set(set_id) {
                    *event_out = Some(StickersListEvent::SetInstalled(set_id));
                }
            }
        } else if set_id != FAVED_SET_ID && set_id != RECENT_SET_ID {
            ui.painter().text(
                Pos2::new(rect.right() - 6.0, rect.center().y),
                Align2::RIGHT_CENTER,
                format!("{count}"),
                FontId::proportional(11.5),
                palette.window_sub_text_fg,
            );
        }

        rect
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_sticker_grid(
        &mut self,
        ui: &mut Ui,
        docs: &[DocumentId],
        global_idx: &mut usize,
        time_secs: f64,
        any_animated: &mut bool,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
    ) -> Option<StickersListEvent> {
        let mut chosen = None;
        let avail_w = ui.available_width().max(STICKER_CELL_SIZE);
        let cols = STICKER_COLUMNS.min(((avail_w / 60.0).floor() as usize).max(1));
        let cell_w = avail_w / cols as f32;
        let cell_h = STICKER_CELL_SIZE;
        let rows = docs.len().div_ceil(cols);
        let total_h = rows as f32 * cell_h;

        let (grid_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());
        let clip = ui.clip_rect();
        let first_row =
            (((clip.top() - grid_rect.top()) / cell_h).floor().max(0.0) as usize).min(rows);
        let last_row =
            (((clip.bottom() - grid_rect.top()) / cell_h).ceil().max(0.0) as usize).min(rows);

        let base_idx = *global_idx;
        *global_idx += docs.len();
        let is_premium_user = backend.is_premium();

        for row in first_row..last_row {
            for col in 0..cols {
                let local_i = row * cols + col;
                let Some(&doc_id) = docs.get(local_i) else {
                    break;
                };
                let item_idx = base_idx + local_i;
                let cell_rect = Rect::from_min_size(
                    Pos2::new(
                        grid_rect.left() + col as f32 * cell_w,
                        grid_rect.top() + row as f32 * cell_h,
                    ),
                    Vec2::new(cell_w, cell_h),
                );
                let id = ui.make_persistent_id(("tg_sticker_cell", item_idx, doc_id.0));
                let resp = ui.interact(cell_rect, id, Sense::click());

                let is_focused = self.focused_index == Some(item_idx);
                if resp.hovered() || is_focused {
                    ui.painter().rect_filled(
                        cell_rect.shrink(2.0),
                        CornerRadius::same(8),
                        palette.window_bg_over,
                    );
                }
                if is_focused {
                    ui.painter().rect_stroke(
                        cell_rect.shrink(2.0),
                        CornerRadius::same(8),
                        Stroke::new(1.5, palette.window_bg_active),
                        StrokeKind::Inside,
                    );
                }

                let scale = if resp.is_pointer_button_down_on() {
                    0.88
                } else if resp.hovered() {
                    1.04
                } else {
                    1.0
                };
                let draw_side = (cell_w.min(cell_h) - 10.0) * scale;
                let draw_rect = Rect::from_center_size(cell_rect.center(), Vec2::splat(draw_side));

                if let Some((tex_id, animated)) =
                    self.media_cache
                        .texture_for(ui.ctx(), doc_id, time_secs, backend)
                {
                    if animated {
                        *any_animated = true;
                    }
                    ui.painter().image(
                        tex_id,
                        draw_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().circle_filled(
                        cell_rect.center(),
                        draw_side * 0.35,
                        palette.divider,
                    );
                }

                let is_premium_sticker = backend
                    .sticker_document(doc_id)
                    .is_some_and(|d| d.is_premium_sticker);
                let is_locked = is_premium_sticker && !is_premium_user;

                // Premium-locked marker pill (`StickerPremiumMark` in bottom-right corner).
                if is_locked {
                    paint_premium_lock_badge(ui.painter(), cell_rect);
                } else if is_premium_sticker {
                    paint_premium_star_badge(ui.painter(), cell_rect);
                }

                // Hover Favorite star toggle in top-right corner.
                let is_faved = backend.is_sticker_faved(doc_id);
                let mut faved_clicked = false;
                if resp.hovered() || is_faved {
                    let star_rect = Rect::from_min_size(
                        Pos2::new(cell_rect.right() - 20.0, cell_rect.top() + 3.0),
                        Vec2::splat(17.0),
                    );
                    let star_resp = ui.interact(
                        star_rect,
                        ui.make_persistent_id(("tg_sticker_fave", item_idx, doc_id.0)),
                        Sense::click(),
                    );
                    if resp.hovered() || star_resp.hovered() {
                        ui.painter().circle_filled(
                            star_rect.center(),
                            8.5,
                            Color32::from_rgba_unmultiplied(0, 0, 0, 125),
                        );
                        let star_color = if is_faved {
                            Color32::from_rgb(0xFF, 0xD2, 0x3F)
                        } else {
                            Color32::WHITE
                        };
                        paint_star_icon(
                            ui.painter(),
                            star_rect.center(),
                            5.2,
                            star_color,
                            is_faved,
                        );
                    }
                    if star_resp
                        .on_hover_text(if is_faved {
                            tr::lng_faved_stickers_remove()
                        } else {
                            tr::lng_faved_stickers_add()
                        })
                        .clicked()
                    {
                        backend.toggle_faved_sticker(doc_id);
                        faved_clicked = true;
                    }
                }

                if !faved_clicked && resp.clicked() {
                    self.focused_index = Some(item_idx);
                    chosen = Some(self.activate_sticker(doc_id, backend));
                }
            }
        }

        chosen
    }

    fn paint_footer(
        &mut self,
        ui: &mut Ui,
        footer_rect: Rect,
        icons: &[tg_chat::stickers::StickerTabIcon],
        time_secs: f64,
        backend: &dyn ChatHelpersBackend,
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

        let slot_w = 36.0f32;
        let mut x = footer_rect.left() + 6.0;
        for (idx, icon) in icons.iter().enumerate() {
            if x + slot_w > footer_rect.right() - 4.0 {
                break;
            }
            let slot_rect = Rect::from_min_size(
                Pos2::new(x, footer_rect.top()),
                Vec2::new(slot_w, footer_rect.height()),
            );
            let resp = ui.interact(
                slot_rect,
                ui.make_persistent_id(("tg_sticker_footer", idx, icon.set_id.0)),
                Sense::click(),
            );
            let active = self.search_query.is_empty() && self.active_set_id == Some(icon.set_id);

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
                        Pos2::new(slot_rect.left() + 5.0, slot_rect.bottom() - 2.5),
                        Vec2::new(slot_rect.width() - 10.0, 2.5),
                    ),
                    CornerRadius::same(1),
                    palette.window_bg_active,
                );
            }

            if icon.set_id == FAVED_SET_ID {
                paint_star_icon(
                    ui.painter(),
                    slot_rect.center(),
                    8.0,
                    if active {
                        palette.window_bg_active
                    } else {
                        palette.window_sub_text_fg
                    },
                    active,
                );
            } else if icon.set_id == RECENT_SET_ID {
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
            } else if let Some(doc_id) = icon.thumbnail_document_id
                && let Some((tex_id, _)) =
                    self.media_cache
                        .texture_for(ui.ctx(), doc_id, time_secs, backend)
            {
                let thumb = Rect::from_center_size(slot_rect.center(), Vec2::splat(24.0));
                ui.painter().image(
                    tex_id,
                    thumb,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.painter()
                    .circle_filled(slot_rect.center(), 8.0, palette.divider);
            }

            if resp.clicked() {
                self.jump_to_set(icon.set_id);
            }
            x += slot_w + 2.0;
        }
    }

    /// Renders the compact `stickers_gift_box_pack` ribbon (Premium / TON gift-box sticker tiers).
    fn paint_gift_box_pack_ribbon(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
    ) -> Option<StickersListEvent> {
        let slot = self.gift_box_pack.slot(self.gift_box_kind);
        let items: Vec<GiftBoxSticker> = slot.items().into_iter().cloned().collect();
        let set_id = slot.set_id;
        if items.is_empty() {
            return None;
        }

        let mut chosen = None;
        let width = ui.available_width().max(180.0);

        // Header row: "Gift Box Pack" + Premium / TON toggle pills.
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Gift Boxes")
                    .font(font_id_from_style(st::stickers_trending_header_font))
                    .color(palette.window_sub_text_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (kind, lbl) in [
                    (GiftBoxPackKind::Ton, "TON"),
                    (GiftBoxPackKind::Premium, "Premium"),
                ] {
                    let active = self.gift_box_kind == kind;
                    let galley = ui.painter().layout_no_wrap(
                        lbl.to_owned(),
                        FontId::proportional(10.5),
                        if active {
                            palette.window_fg_active
                        } else {
                            palette.window_sub_text_fg
                        },
                    );
                    let (r, resp) = ui.allocate_exact_size(
                        Vec2::new(galley.size().x + 14.0, 22.0),
                        Sense::click(),
                    );
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::Button,
                            true,
                            active,
                            format!("Gift Boxes: {lbl}"),
                        )
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let bg = if active {
                        palette.window_bg_active
                    } else if resp.hovered() {
                        palette.window_bg_over
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(r, CornerRadius::same(11), bg);
                    ui.painter().galley(
                        Pos2::new(
                            r.center().x - galley.size().x * 0.5,
                            r.center().y - galley.size().y * 0.5,
                        ),
                        galley,
                        Color32::WHITE,
                    );
                    if resp.clicked() {
                        self.gift_box_kind = kind;
                    }
                }
            });
        });

        ui.add_space(4.0);

        // Horizontal ribbon of gift-box sticker tiles (adapts smoothly to narrow widths).
        let gap = 6.0;
        let cols = items.len().max(1);
        let tile_w =
            ((width - gap * (cols.saturating_sub(1) as f32)) / cols as f32).clamp(46.0, 64.0);
        let tile_h = 56.0;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(gap, gap);
            for item in &items {
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());
                let acc = format!("Gift Box sticker ({})", item.tier_label);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &acc));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let accent =
                    Color32::from_rgb(item.accent_rgb[0], item.accent_rgb[1], item.accent_rgb[2]);
                let fill = if resp.hovered() {
                    palette.window_bg_over
                } else {
                    Color32::from_rgba_unmultiplied(
                        item.accent_rgb[0],
                        item.accent_rgb[1],
                        item.accent_rgb[2],
                        24,
                    )
                };
                ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(8),
                    Stroke::new(
                        if resp.hovered() { 1.5 } else { 0.75 },
                        if resp.hovered() {
                            palette.window_bg_active
                        } else {
                            palette.divider
                        },
                    ),
                    StrokeKind::Inside,
                );

                // Crisp vector gift-box emblem (box body + lid + vertical/horizontal ribbon).
                let box_rect = Rect::from_center_size(
                    Pos2::new(rect.center().x, rect.top() + 22.0),
                    Vec2::new(22.0, 20.0),
                );
                let lid_rect = Rect::from_min_size(
                    Pos2::new(box_rect.left() - 2.0, box_rect.top()),
                    Vec2::new(box_rect.width() + 4.0, 6.0),
                );
                let body_rect = Rect::from_min_max(
                    Pos2::new(box_rect.left(), box_rect.top() + 6.0),
                    box_rect.max,
                );
                ui.painter()
                    .rect_filled(body_rect, CornerRadius::same(3), accent);
                ui.painter()
                    .rect_filled(lid_rect, CornerRadius::same(2), accent);
                // White vertical & horizontal ribbon bands
                ui.painter().rect_filled(
                    Rect::from_center_size(
                        Pos2::new(box_rect.center().x, box_rect.center().y),
                        Vec2::new(3.5, box_rect.height()),
                    ),
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 225),
                );
                ui.painter().rect_filled(
                    Rect::from_center_size(
                        Pos2::new(box_rect.center().x, body_rect.center().y),
                        Vec2::new(body_rect.width(), 3.0),
                    ),
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 190),
                );

                // Tier label ("1m", "3m", "12m", "10 TON")
                ui.painter().text(
                    Pos2::new(rect.center().x, rect.bottom() - 10.0),
                    Align2::CENTER_CENTER,
                    &item.tier_label,
                    FontId::proportional(10.0),
                    palette.window_bold_fg,
                );

                if resp.clicked() {
                    backend.record_sticker_used(item.document_id);
                    chosen = Some(StickersListEvent::Chosen {
                        document_id: item.document_id,
                        set_id: Some(set_id),
                    });
                }
            }
        });

        ui.add_space(6.0);
        ui.painter().line_segment(
            [
                Pos2::new(ui.available_rect_before_wrap().left(), ui.cursor().top()),
                Pos2::new(ui.available_rect_before_wrap().right(), ui.cursor().top()),
            ],
            Stroke::new(1.0, palette.divider),
        );
        ui.add_space(4.0);

        chosen
    }
}

/// Paints Telegram's bottom-right circular Premium lock marker on a sticker cell (`st::stickers_premium_lock`).
pub(crate) fn paint_premium_lock_badge(painter: &egui::Painter, cell_rect: Rect) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let center = Pos2::new(cell_rect.right() - 12.0, cell_rect.bottom() - 12.0);
    painter.circle_filled(center, 9.5, Color32::from_rgb(0x7B, 0x5C, 0xF5));
    painter.circle_stroke(center, 9.5, Stroke::new(1.2, Color32::WHITE));
    let icon_rect = Rect::from_center_size(center, Vec2::splat(13.0));
    if !paint_helper_icon(
        painter,
        icon_rect,
        st::stickers_premium_lock,
        dark,
        Some(Color32::WHITE),
    ) {
        let shackle_c = Pos2::new(center.x, center.y - 2.0);
        painter.circle_stroke(shackle_c, 2.6, Stroke::new(1.3, Color32::WHITE));
        painter.rect_filled(
            Rect::from_center_size(Pos2::new(center.x, center.y + 2.0), Vec2::new(7.5, 5.5)),
            CornerRadius::same(1),
            Color32::WHITE,
        );
    }
}

fn paint_premium_star_badge(painter: &egui::Painter, cell_rect: Rect) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let center = Pos2::new(cell_rect.right() - 11.0, cell_rect.bottom() - 11.0);
    painter.circle_filled(center, 8.0, Color32::from_rgb(0x88, 0x62, 0xF8));
    let icon_rect = Rect::from_center_size(center, Vec2::splat(12.0));
    if !paint_helper_icon(
        painter,
        icon_rect,
        st::info_premium_star,
        dark,
        Some(Color32::WHITE),
    ) {
        paint_star_icon(painter, center, 4.8, Color32::WHITE, true);
    }
}

/// Paints Telegram's favorite star icon (`st::menu_icon_unfave` / `st::menu_icon_fave`),
/// falling back to a 5-point vector star if the icon mask is unavailable.
pub(crate) fn paint_star_icon(
    painter: &egui::Painter,
    center: Pos2,
    outer_r: f32,
    color: Color32,
    filled: bool,
) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let icon = if filled {
        st::info_premium_star
    } else {
        st::menu_icon_fave
    };
    let icon_rect = Rect::from_center_size(center, Vec2::splat((outer_r * 2.1).max(12.0)));
    if paint_helper_icon(painter, icon_rect, icon, dark, Some(color)) {
        return;
    }
    let inner_r = outer_r * 0.44;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        pts.push(Pos2::new(
            center.x + r * angle.cos(),
            center.y + r * angle.sin(),
        ));
    }
    if filled {
        for i in 0..10 {
            let next = (i + 1) % 10;
            painter.add(egui::Shape::convex_polygon(
                vec![center, pts[i], pts[next]],
                color,
                Stroke::NONE,
            ));
        }
    } else {
        painter.add(egui::Shape::closed_line(pts, Stroke::new(1.5, color)));
    }
}

/// Paints Telegram's recent clock icon (`st::info_icon_media_stories_recent`) used in Recent footers.
pub(crate) fn paint_clock_icon(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let icon_rect = Rect::from_center_size(center, Vec2::splat((radius * 2.2).max(16.0)));
    if paint_helper_icon(
        painter,
        icon_rect,
        st::info_icon_media_stories_recent,
        dark,
        Some(color),
    ) {
        return;
    }
    painter.circle_stroke(center, radius, Stroke::new(1.5, color));
    painter.line_segment(
        [center, Pos2::new(center.x, center.y - radius * 0.55)],
        Stroke::new(1.5, color),
    );
    painter.line_segment(
        [
            center,
            Pos2::new(center.x + radius * 0.45, center.y + radius * 0.15),
        ],
        Stroke::new(1.5, color),
    );
}
