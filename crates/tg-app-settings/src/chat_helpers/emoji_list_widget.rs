//! Emoji picker list widget, search bar, category footer, and skin-tone color picker popup.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/emoji_list_widget.h`,
//! `Telegram/SourceFiles/chat_helpers/emoji_list_widget.cpp`, and `chat_helpers.style`.

use egui::{
    Align2, Area, Color32, CornerRadius, Frame, Id, Key, Margin, Order, Pos2, Rect, ScrollArea,
    Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
};
use tg_chat::emoji::{SkinTone, strip_skin_tone};
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_text::emoji::{self, EmojiRef, Section};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::chat_helpers::backend::ChatHelpersBackend;
use crate::chat_helpers::paint_helper_icon;
use tg_app_core::stand_in::theme::Palette;

/// Number of emoji columns in the 345px panel (`floor((345 - 14) / 37) = 8`).
pub const EMOJI_COLUMNS: usize = 8;
/// Target emoji cell size (`st::defaultEmojiPan.desiredSize`).
pub const EMOJI_CELL_SIZE: f32 = st::default_emoji_pan.desired_size.0 as f32;
/// Emoji sprite draw size inside each cell.
pub const EMOJI_DRAW_SIZE: f32 = 26.0;
/// Section header height (`st::defaultEmojiPan.header`).
pub const EMOJI_HEADER_HEIGHT: f32 = st::default_emoji_pan.header.0 as f32;
/// Category footer height (`st::defaultEmojiPan.footer`).
pub const EMOJI_FOOTER_HEIGHT: f32 = st::default_emoji_pan.footer.0 as f32;
/// Long-press duration before opening the skin-tone popup (`kColorPickerDelay = 500ms`).
pub const SKIN_TONE_LONG_PRESS_SECS: f64 = 0.45;

/// Ordered picker sections matching `Ui::Emoji::Section`.
pub const EMOJI_SECTIONS: [Section; 8] = [
    Section::Recent,
    Section::People,
    Section::Nature,
    Section::Food,
    Section::Activity,
    Section::Travel,
    Section::Objects,
    Section::Symbols,
];

/// Returns the localized title for an emoji [`Section`] (`tr::lng_recent_stickers` / `tr::lng_emoji_category1..7`).
#[must_use]
pub fn section_title(section: Section) -> String {
    match section {
        Section::Recent => tr::lng_recent_stickers(),
        Section::People => tr::lng_emoji_category1(),
        Section::Nature => tr::lng_emoji_category2(),
        Section::Food => tr::lng_emoji_category3(),
        Section::Activity => tr::lng_emoji_category4(),
        Section::Travel => tr::lng_emoji_category5(),
        Section::Objects => tr::lng_emoji_category6(),
        Section::Symbols => tr::lng_emoji_category7(),
    }
}

/// Representative emoji glyph used on the category footer strip for `section`.
#[must_use]
pub const fn section_footer_glyph(section: Section) -> &'static str {
    match section {
        Section::Recent => "🕒",
        Section::People => "😀",
        Section::Nature => "🐻",
        Section::Food => "🍔",
        Section::Activity => "⚽",
        Section::Travel => "🚗",
        Section::Objects => "💡",
        Section::Symbols => "❤️",
    }
}

/// Active skin-tone variant popup state (`EmojiColorPicker` in `emoji_list_widget.cpp`).
#[derive(Debug, Clone)]
pub struct SkinTonePopup {
    /// Base colorizable emoji whose 6 variants (`0..=5`) are displayed.
    pub base_emoji: EmojiRef,
    /// Anchor rect (the pressed emoji cell) in screen coordinates.
    pub anchor_rect: Rect,
    /// When `true`, choosing a tone applies globally via `save_all_emoji_variants`.
    pub apply_to_all: bool,
    /// Currently highlighted variant index (`0..=5`).
    pub selected_variant: usize,
}

/// Output produced by [`EmojiListWidget::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmojiListEvent {
    /// User chose an emoji to insert into the compose field.
    Chosen(String),
}

/// Stateful emoji picker tab with keyword search, recent section, 7 Unicode categories,
/// skin-tone long-press popup, keyboard grid navigation, and bottom category footer.
#[derive(Debug)]
pub struct EmojiListWidget {
    search_query: String,
    active_section: Section,
    scroll_to_section: Option<Section>,
    focused_index: Option<usize>,
    press_hold: Option<(EmojiRef, Rect, f64)>,
    color_picker: Option<SkinTonePopup>,
    section_cache: Vec<(Section, Vec<EmojiRef>)>,
}

impl Default for EmojiListWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiListWidget {
    /// Creates a new [`EmojiListWidget`] with static `tg-text` sections pre-indexed.
    #[must_use]
    pub fn new() -> Self {
        let mut section_cache = Vec::with_capacity(7);
        for &sec in &EMOJI_SECTIONS[1..] {
            section_cache.push((sec, emoji::section(sec)));
        }
        Self {
            search_query: String::new(),
            active_section: Section::Recent,
            scroll_to_section: None,
            focused_index: None,
            press_hold: None,
            color_picker: None,
            section_cache,
        }
    }

    /// Returns the current search query string.
    #[must_use]
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Sets the search query programmatically (e.g. for tests or screenshots).
    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.search_query = query.into();
        self.focused_index = None;
    }

    /// Returns whether the skin-tone popup is currently open (used to prevent auto-hiding panel).
    #[must_use]
    pub fn has_open_popup(&self) -> bool {
        self.color_picker.is_some()
    }

    /// Opens the skin-tone popup for a specific emoji programmatically (for tests/screenshots).
    pub fn open_skin_tone_popup(&mut self, emoji_seq: &str, anchor_rect: Rect, apply_to_all: bool) {
        if let Some((er, _)) = emoji::find_emoji(emoji_seq, 0)
            && er.has_variants()
        {
            self.color_picker = Some(SkinTonePopup {
                base_emoji: er.original(),
                anchor_rect,
                apply_to_all,
                selected_variant: 0,
            });
        }
    }

    /// Closes any open skin-tone popup.
    pub fn close_skin_tone_popup(&mut self) {
        self.color_picker = None;
        self.press_hold = None;
    }

    /// Returns the currently highlighted section in the footer.
    #[must_use]
    pub const fn active_section(&self) -> Section {
        self.active_section
    }

    /// Jumps to a specific emoji section in the scroll list.
    pub fn jump_to_section(&mut self, section: Section) {
        self.search_query.clear();
        self.active_section = section;
        self.scroll_to_section = Some(section);
        self.color_picker = None;
    }

    /// Handles keyboard navigation (`Arrow` keys, `Enter`, `Escape`) when the Emoji tab is active.
    ///
    /// Returns `(consumed, optional_event)`.
    pub fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn ChatHelpersBackend,
    ) -> (bool, Option<EmojiListEvent>) {
        // Escape closes skin tone popup first, then clears search.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.color_picker.is_some() {
                self.close_skin_tone_popup();
                return (true, None);
            }
            if !self.search_query.is_empty() {
                self.search_query.clear();
                self.focused_index = None;
                return (true, None);
            }
            return (false, None);
        }

        // Skin tone popup keyboard selection (Left/Right/Enter).
        if let Some(mut popup) = self.color_picker.clone() {
            let mut changed = false;
            if ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
                popup.selected_variant = popup.selected_variant.saturating_sub(1);
                changed = true;
            } else if ctx.input(|i| i.key_pressed(Key::ArrowRight)) {
                popup.selected_variant = (popup.selected_variant + 1).min(5);
                changed = true;
            } else if ctx.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space)) {
                let chosen = self.commit_skin_tone_choice(&popup, popup.selected_variant, backend);
                self.color_picker = None;
                return (true, Some(EmojiListEvent::Chosen(chosen)));
            }
            if changed {
                self.color_picker = Some(popup);
                return (true, None);
            }
            return (false, None);
        }

        // Only move grid focus when the search input isn't consuming arrow keys.
        let left = ctx.input(|i| i.key_pressed(Key::ArrowLeft));
        let right = ctx.input(|i| i.key_pressed(Key::ArrowRight));
        let up = ctx.input(|i| i.key_pressed(Key::ArrowUp));
        let down = ctx.input(|i| i.key_pressed(Key::ArrowDown));
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));

        if !(left || right || up || down || enter) {
            return (false, None);
        }

        let flat = self.collect_visible_emojis(backend);
        if flat.is_empty() {
            return (false, None);
        }
        let max_idx = flat.len() - 1;

        if enter {
            if let Some(idx) = self.focused_index
                && let Some(chosen) = flat.get(idx)
            {
                let text = self.resolve_emoji_for_insert(*chosen, backend);
                backend.record_emoji_used(&text);
                return (true, Some(EmojiListEvent::Chosen(text)));
            }
            return (false, None);
        }

        let cur = self.focused_index.unwrap_or(0);
        let next = if right {
            (cur + 1).min(max_idx)
        } else if left {
            cur.saturating_sub(1)
        } else if down {
            (cur + EMOJI_COLUMNS).min(max_idx)
        } else if up {
            cur.saturating_sub(EMOJI_COLUMNS)
        } else {
            cur
        };
        self.focused_index = Some(next);
        (true, None)
    }

    fn collect_visible_emojis(&mut self, backend: &mut dyn ChatHelpersBackend) -> Vec<EmojiRef> {
        if !self.search_query.trim().is_empty() {
            return backend
                .search_emoji(self.search_query.trim())
                .into_iter()
                .filter_map(|r| emoji::find_emoji(&r.emoji, 0).map(|(e, _)| e))
                .collect();
        }
        let mut flat = Vec::new();
        for s in backend.recent_emoji() {
            if let Some((e, _)) = emoji::find_emoji(&s, 0) {
                flat.push(e);
            }
        }
        for (_, items) in &self.section_cache {
            flat.extend_from_slice(items);
        }
        flat
    }

    fn resolve_emoji_for_insert(
        &self,
        emoji_ref: EmojiRef,
        backend: &dyn ChatHelpersBackend,
    ) -> String {
        if emoji_ref.colored() {
            return emoji_ref.text().into_owned();
        }
        if emoji_ref.has_variants() {
            let preferred = backend
                .emoji_variants()
                .lookup_tone(emoji_ref.non_colored_id())
                .unwrap_or(SkinTone::Default);
            return emoji_ref
                .variant(preferred.index() as usize)
                .text()
                .into_owned();
        }
        emoji_ref.text().into_owned()
    }

    fn commit_skin_tone_choice(
        &self,
        popup: &SkinTonePopup,
        variant_idx: usize,
        backend: &mut dyn ChatHelpersBackend,
    ) -> String {
        let tone = SkinTone::from_index(variant_idx as u8).unwrap_or(SkinTone::Default);
        let chosen_ref = popup.base_emoji.variant(variant_idx);
        let text = chosen_ref.text().into_owned();
        if popup.apply_to_all {
            backend.save_all_emoji_variants(tone);
        } else {
            backend.save_emoji_variant(popup.base_emoji.non_colored_id(), tone);
            backend.record_emoji_used(&text);
        }
        text
    }

    /// Renders the Emoji search bar, scrollable emoji sections, skin-tone popup, and category footer.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) -> Option<EmojiListEvent> {
        let mut event = None;
        let full_rect = ui.available_rect_before_wrap();

        // Reserve bottom footer strip (`EMOJI_FOOTER_HEIGHT`).
        let footer_rect = Rect::from_min_max(
            Pos2::new(full_rect.left(), full_rect.bottom() - EMOJI_FOOTER_HEIGHT),
            full_rect.max,
        );
        let body_rect = Rect::from_min_max(
            full_rect.min,
            Pos2::new(full_rect.right(), footer_rect.top()),
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            // 1. Search bar (`TabbedSearch` style, 33px height).
            let search_ph = tr::lng_dlg_filter();
            Frame::NONE
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    render_search_bar(
                        ui,
                        "tg_emoji_search_field",
                        &mut self.search_query,
                        &search_ph,
                        None,
                        palette,
                    );
                });

            // 2. Scrollable emoji grid body.
            let scroll_avail = ui.available_rect_before_wrap();
            ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_avail), |ui| {
                ScrollArea::vertical()
                    .id_salt("tg_emoji_list_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::NONE
                            .inner_margin(Margin::symmetric(8, 2))
                            .show(ui, |ui| {
                                let now = ui.input(|i| i.time);
                                if !self.search_query.trim().is_empty() {
                                    let hits = backend.search_emoji(self.search_query.trim());
                                    if hits.is_empty() {
                                        ui.add_space(36.0);
                                        ui.vertical_centered(|ui| {
                                            ui.label(
                                                egui::RichText::new(tr::lng_emoji_nothing_found())
                                                    .size(13.5)
                                                    .color(palette.window_sub_text_fg),
                                            );
                                        });
                                    } else {
                                        let refs: Vec<EmojiRef> = hits
                                            .iter()
                                            .filter_map(|h| {
                                                emoji::find_emoji(&h.emoji, 0).map(|(e, _)| e)
                                            })
                                            .collect();
                                        let mut global_idx = 0usize;
                                        if let Some(ev) = self.paint_emoji_grid(
                                            ui,
                                            &refs,
                                            &mut global_idx,
                                            now,
                                            backend,
                                            emoji_cache,
                                            palette,
                                        ) {
                                            event = Some(ev);
                                        }
                                    }
                                } else {
                                    let mut global_idx = 0usize;
                                    // Recent section
                                    let recent_strs = backend.recent_emoji();
                                    let recent_refs: Vec<EmojiRef> = recent_strs
                                        .iter()
                                        .filter_map(|s| emoji::find_emoji(s, 0).map(|(e, _)| e))
                                        .collect();
                                    if !recent_refs.is_empty() {
                                        let hdr_rect = self.paint_section_header(
                                            ui,
                                            Section::Recent,
                                            false,
                                            palette,
                                            emoji_cache,
                                            backend,
                                        );
                                        if self.scroll_to_section == Some(Section::Recent) {
                                            ui.scroll_to_rect(hdr_rect, Some(egui::Align::TOP));
                                            self.scroll_to_section = None;
                                        }
                                        if let Some(ev) = self.paint_emoji_grid(
                                            ui,
                                            &recent_refs,
                                            &mut global_idx,
                                            now,
                                            backend,
                                            emoji_cache,
                                            palette,
                                        ) {
                                            event = Some(ev);
                                        }
                                    }

                                    // 7 static Unicode sections
                                    let sections_clone = self.section_cache.clone();
                                    for (sec, items) in sections_clone {
                                        let hdr_rect = self.paint_section_header(
                                            ui,
                                            sec,
                                            sec == Section::People,
                                            palette,
                                            emoji_cache,
                                            backend,
                                        );
                                        if self.scroll_to_section == Some(sec) {
                                            ui.scroll_to_rect(hdr_rect, Some(egui::Align::TOP));
                                            self.scroll_to_section = None;
                                        }
                                        // Update active section indicator as user scrolls.
                                        if hdr_rect.top() <= scroll_avail.top() + 40.0
                                            && hdr_rect.bottom() >= scroll_avail.top() - 600.0
                                        {
                                            self.active_section = sec;
                                        }
                                        if let Some(ev) = self.paint_emoji_grid(
                                            ui,
                                            &items,
                                            &mut global_idx,
                                            now,
                                            backend,
                                            emoji_cache,
                                            palette,
                                        ) {
                                            event = Some(ev);
                                        }
                                    }
                                }
                            });
                    });
            });
        });

        // 3. Bottom category footer (`36px`).
        self.paint_footer(ui, footer_rect, emoji_cache, palette);

        // 4. Long-press check for skin-tone popup.
        if let Some((er, rect, pressed_at)) = self.press_hold {
            let now = ui.input(|i| i.time);
            let still_down = ui.input(|i| i.pointer.primary_down());
            if !still_down {
                self.press_hold = None;
            } else if now - pressed_at >= SKIN_TONE_LONG_PRESS_SECS {
                self.color_picker = Some(SkinTonePopup {
                    base_emoji: er.original(),
                    anchor_rect: rect,
                    apply_to_all: false,
                    selected_variant: 0,
                });
                self.press_hold = None;
            } else {
                ui.ctx().request_repaint();
            }
        }

        // 5. Floating Skin Tone Picker Popup (`EmojiColorPicker`).
        if let Some(ev) = self.paint_skin_tone_popup(ui.ctx(), backend, emoji_cache, palette) {
            event = Some(ev);
        }

        event
    }

    fn paint_section_header(
        &mut self,
        ui: &mut Ui,
        section: Section,
        show_skin_all_button: bool,
        palette: &Palette,
        emoji_cache: &mut EmojiTextureCache,
        backend: &dyn ChatHelpersBackend,
    ) -> Rect {
        let width = ui.available_width().max(120.0);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(width, EMOJI_HEADER_HEIGHT), Sense::hover());
        ui.painter().text(
            Pos2::new(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            section_title(section),
            font_id_from_style(st::emoji_pan_header_font),
            palette.window_sub_text_fg,
        );

        // "Choose default skin tone" hand button in Smileys & People header (`emojiPanColorAll`).
        if show_skin_all_button {
            let btn_rect = Rect::from_min_size(
                Pos2::new(rect.right() - 28.0, rect.center().y - 12.0),
                Vec2::new(24.0, 24.0),
            );
            let resp = ui.interact(
                btn_rect,
                ui.make_persistent_id("tg_emoji_color_all_btn"),
                Sense::click(),
            );
            if resp.hovered() {
                ui.painter()
                    .rect_filled(btn_rect, CornerRadius::same(6), palette.window_bg_over);
            }
            let global_tone = backend
                .emoji_variants()
                .lookup_tone("")
                .unwrap_or(SkinTone::Default);
            if let Some((hand, _)) = emoji::find_emoji("✋", 0) {
                let toned = hand.variant(global_tone.index() as usize);
                emoji_cache.paint_emoji(
                    ui.painter(),
                    ui.ctx(),
                    toned,
                    btn_rect.shrink(3.0),
                    Color32::WHITE,
                );
            }
            if resp.on_hover_text(tr::lng_emoji_color_all()).clicked() {
                self.open_skin_tone_popup("✋", btn_rect, true);
            }
        }
        rect
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_emoji_grid(
        &mut self,
        ui: &mut Ui,
        items: &[EmojiRef],
        global_idx: &mut usize,
        now: f64,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) -> Option<EmojiListEvent> {
        let mut chosen = None;
        let avail_w = ui.available_width().max(EMOJI_CELL_SIZE);
        let cols = EMOJI_COLUMNS.min(((avail_w / 34.0).floor() as usize).max(1));
        let cell_w = (avail_w / cols as f32).min(42.0);
        let cell_h = EMOJI_CELL_SIZE;
        let rows = items.len().div_ceil(cols);
        let total_h = rows as f32 * cell_h;

        let (grid_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());
        let clip = ui.clip_rect();

        // Virtualized row range: only paint rows intersecting `clip` so all ~1,800 emojis run at 120fps!
        let first_row =
            (((clip.top() - grid_rect.top()) / cell_h).floor().max(0.0) as usize).min(rows);
        let last_row =
            (((clip.bottom() - grid_rect.top()) / cell_h).ceil().max(0.0) as usize).min(rows);

        let base_global = *global_idx;
        *global_idx += items.len();

        let variants = backend.emoji_variants().clone();

        for row in first_row..last_row {
            for col in 0..cols {
                let local_i = row * cols + col;
                let Some(&base_ref) = items.get(local_i) else {
                    break;
                };
                let item_global = base_global + local_i;
                let cell_rect = Rect::from_min_size(
                    Pos2::new(
                        grid_rect.left() + col as f32 * cell_w,
                        grid_rect.top() + row as f32 * cell_h,
                    ),
                    Vec2::new(cell_w, cell_h),
                );
                let id = ui.make_persistent_id(("tg_emoji_cell", item_global, base_ref.index()));
                let resp = ui.interact(cell_rect, id, Sense::click());

                // Apply saved skin-tone preference for display when this is an uncolored base emoji.
                let display_ref = if !base_ref.colored() && base_ref.has_variants() {
                    let tone = variants
                        .lookup_tone(base_ref.non_colored_id())
                        .unwrap_or(SkinTone::Default);
                    base_ref.variant(tone.index() as usize)
                } else {
                    base_ref
                };

                let is_focused = self.focused_index == Some(item_global);
                if resp.hovered() || is_focused {
                    ui.painter().rect_filled(
                        cell_rect.shrink(1.0),
                        CornerRadius::same(6),
                        palette.window_bg_over,
                    );
                }
                if is_focused {
                    ui.painter().rect_stroke(
                        cell_rect.shrink(1.5),
                        CornerRadius::same(6),
                        Stroke::new(1.5, palette.window_bg_active),
                        StrokeKind::Inside,
                    );
                }

                // Subtle press shrink feedback (`0.88x` while pressed).
                let draw_side = if resp.is_pointer_button_down_on() {
                    EMOJI_DRAW_SIZE * 0.88
                } else {
                    EMOJI_DRAW_SIZE
                };
                let draw_rect = Rect::from_center_size(cell_rect.center(), Vec2::splat(draw_side));
                emoji_cache.paint_emoji(
                    ui.painter(),
                    ui.ctx(),
                    display_ref,
                    draw_rect,
                    Color32::WHITE,
                );

                // Subtle corner dot on colorizable emoji so users know long-press/right-click works.
                if base_ref.has_variants() && resp.hovered() {
                    ui.painter().circle_filled(
                        Pos2::new(cell_rect.right() - 5.0, cell_rect.bottom() - 5.0),
                        2.0,
                        palette.window_sub_text_fg,
                    );
                }

                // Track long-press start for skin-tone popup.
                if resp.is_pointer_button_down_on()
                    && base_ref.has_variants()
                    && self.press_hold.is_none()
                    && self.color_picker.is_none()
                {
                    self.press_hold = Some((base_ref, cell_rect, now));
                }

                // Right-click opens skin-tone popup immediately on colorizable emoji.
                if resp.secondary_clicked() && base_ref.has_variants() {
                    self.color_picker = Some(SkinTonePopup {
                        base_emoji: base_ref.original(),
                        anchor_rect: cell_rect,
                        apply_to_all: false,
                        selected_variant: 0,
                    });
                    self.press_hold = None;
                } else if resp.clicked() && self.color_picker.is_none() {
                    self.press_hold = None;
                    self.focused_index = Some(item_global);
                    let text = display_ref.text().into_owned();
                    backend.record_emoji_used(&text);
                    chosen = Some(EmojiListEvent::Chosen(text));
                }
            }
        }

        chosen
    }

    fn paint_footer(
        &mut self,
        ui: &mut Ui,
        footer_rect: Rect,
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

        let count = EMOJI_SECTIONS.len();
        let slot_w = footer_rect.width() / count as f32;
        for (idx, &sec) in EMOJI_SECTIONS.iter().enumerate() {
            let slot_rect = Rect::from_min_size(
                Pos2::new(footer_rect.left() + idx as f32 * slot_w, footer_rect.top()),
                Vec2::new(slot_w, footer_rect.height()),
            );
            let resp = ui.interact(
                slot_rect,
                ui.make_persistent_id(("tg_emoji_footer", idx)),
                Sense::click(),
            );
            let active = self.search_query.is_empty() && self.active_section == sec;

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

            let tint = if active {
                Color32::WHITE
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 190)
            };
            let icon_rect = Rect::from_center_size(slot_rect.center(), Vec2::splat(19.0));
            emoji_cache.paint_emoji_str(
                ui.painter(),
                ui.ctx(),
                section_footer_glyph(sec),
                icon_rect,
                tint,
            );

            if resp.on_hover_text(section_title(sec)).clicked() {
                self.jump_to_section(sec);
            }
        }
    }

    fn paint_skin_tone_popup(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) -> Option<EmojiListEvent> {
        let popup = self.color_picker.clone()?;
        let cell_side = 36.0f32;
        let pad = 8.0f32;
        let sep_w = 9.0f32;
        let header_h = if popup.apply_to_all { 22.0 } else { 0.0 };
        let popup_w = pad * 2.0 + cell_side * 6.0 + sep_w;
        let popup_h = pad * 2.0 + cell_side + header_h;

        let screen = ctx.content_rect();
        let x = (popup.anchor_rect.center().x - popup_w * 0.5).clamp(
            screen.left() + 8.0,
            (screen.right() - popup_w - 8.0).max(8.0),
        );
        let y = (popup.anchor_rect.top() - popup_h - 6.0).max(screen.top() + 8.0);
        let popup_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(popup_w, popup_h));

        let mut chosen_event = None;
        let mut close_popup = false;

        Area::new(Id::unique("tg_emoji_skin_tone_popup"))
            .order(Order::Foreground)
            .fixed_pos(popup_rect.min)
            .show(ctx, |ui| {
                // Soft drop shadow
                ui.painter().rect_filled(
                    popup_rect.expand(2.0).translate(Vec2::new(0.0, 2.0)),
                    CornerRadius::same(10),
                    Color32::from_rgba_unmultiplied(0, 0, 0, 38),
                );
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(pad as i8))
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(popup_w - pad * 2.0, popup_h - pad * 2.0));
                        if popup.apply_to_all {
                            ui.label(
                                egui::RichText::new(tr::lng_emoji_color_all())
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                            );
                            ui.add_space(4.0);
                        }
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for variant_idx in 0..6usize {
                                if variant_idx == 1 {
                                    // 1px vertical separator between default yellow & 5 skin tones (`emojiColorsSep`)
                                    let (sep_r, _) = ui.allocate_exact_size(
                                        Vec2::new(sep_w, cell_side),
                                        Sense::hover(),
                                    );
                                    ui.painter().line_segment(
                                        [
                                            Pos2::new(sep_r.center().x, sep_r.top() + 4.0),
                                            Pos2::new(sep_r.center().x, sep_r.bottom() - 4.0),
                                        ],
                                        Stroke::new(1.0, palette.divider),
                                    );
                                }
                                let (cell_r, resp) =
                                    ui.allocate_exact_size(Vec2::splat(cell_side), Sense::click());
                                if resp.hovered() || popup.selected_variant == variant_idx {
                                    ui.painter().rect_filled(
                                        cell_r.shrink(1.0),
                                        CornerRadius::same(6),
                                        palette.window_bg_over,
                                    );
                                }
                                let variant_ref = popup.base_emoji.variant(variant_idx);
                                emoji_cache.paint_emoji(
                                    ui.painter(),
                                    ui.ctx(),
                                    variant_ref,
                                    cell_r.shrink(4.0),
                                    Color32::WHITE,
                                );
                                if resp.clicked() {
                                    let text =
                                        self.commit_skin_tone_choice(&popup, variant_idx, backend);
                                    if !popup.apply_to_all {
                                        chosen_event = Some(EmojiListEvent::Chosen(text));
                                    }
                                    close_popup = true;
                                }
                            }
                        });
                    });
            });

        // Clicking outside dismisses the skin-tone popup.
        if !close_popup
            && ctx.input(|i| i.pointer.any_click())
            && let Some(pos) = ctx.input(|i| i.pointer.interact_pos())
            && !popup_rect.contains(pos)
            && !popup.anchor_rect.contains(pos)
        {
            close_popup = true;
        }
        if close_popup {
            self.color_picker = None;
        }
        chosen_event
    }
}

/// Shared tdesktop-styled pill search bar (`defaultTabbedSearch`, 33px height) used across
/// Emoji, Stickers, and GIFs tabs.
pub(crate) fn render_search_bar(
    ui: &mut Ui,
    id_salt: &str,
    query: &mut String,
    placeholder: &str,
    prefix_badge: Option<&str>,
    palette: &Palette,
) -> bool {
    let mut changed = false;
    let bar_w = ui.available_width().max(120.0);
    let bar_h = 33.0;
    let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(bar_w, bar_h), Sense::hover());

    ui.painter()
        .rect_filled(bar_rect, CornerRadius::same(16), palette.input_bg);

    let inner_rect = bar_rect.shrink2(Vec2::new(10.0, 4.0));
    let dark = ui.ctx().global_style().visuals.dark_mode;
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner_rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // Search icon (`st::box_field_search_icon`).
            let (icon_r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            if !paint_helper_icon(
                ui.painter(),
                icon_r,
                st::box_field_search_icon,
                dark,
                Some(palette.window_sub_text_fg),
            ) {
                let c = Pos2::new(icon_r.left() + 6.0, icon_r.top() + 6.0);
                ui.painter()
                    .circle_stroke(c, 4.2, Stroke::new(1.4, palette.window_sub_text_fg));
                ui.painter().line_segment(
                    [c + Vec2::new(3.1, 3.1), c + Vec2::new(6.2, 6.2)],
                    Stroke::new(1.5, palette.window_sub_text_fg),
                );
            }

            if let Some(badge) = prefix_badge {
                ui.label(
                    egui::RichText::new(badge)
                        .size(12.5)
                        .strong()
                        .color(palette.window_active_text_fg),
                );
            }

            let clear_w = if query.is_empty() { 0.0 } else { 20.0 };
            let edit_w = (ui.available_width() - clear_w).max(60.0);
            let resp = ui.add_sized(
                Vec2::new(edit_w, 22.0),
                TextEdit::singleline(query)
                    .id_source(id_salt)
                    .frame(Frame::NONE)
                    .hint_text(placeholder),
            );
            if resp.changed() {
                changed = true;
            }

            if !query.is_empty() {
                let (clr_r, clr_resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                let fg = if clr_resp.hovered() {
                    palette.window_fg
                } else {
                    palette.window_sub_text_fg
                };
                if !paint_helper_icon(
                    ui.painter(),
                    clr_r.shrink(2.0),
                    st::small_close_icon,
                    dark,
                    Some(fg),
                ) {
                    let cc = clr_r.center();
                    ui.painter().line_segment(
                        [cc + Vec2::new(-3.5, -3.5), cc + Vec2::new(3.5, 3.5)],
                        Stroke::new(1.4, fg),
                    );
                    ui.painter().line_segment(
                        [cc + Vec2::new(3.5, -3.5), cc + Vec2::new(-3.5, 3.5)],
                        Stroke::new(1.4, fg),
                    );
                }
                if clr_resp.clicked() {
                    query.clear();
                    changed = true;
                }
            }
        });
    });
    let _ = strip_skin_tone("");
    changed
}
