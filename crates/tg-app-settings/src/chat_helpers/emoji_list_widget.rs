//! Emoji picker list widget, search bar, category footer, and skin-tone color picker popup.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/emoji_list_widget.h`,
//! `Telegram/SourceFiles/chat_helpers/emoji_list_widget.cpp`, and `chat_helpers.style`.

use std::collections::HashMap;

use egui::{
    Align2, Area, Color32, CornerRadius, FontId, Frame, Id, Key, Margin, Order, Pos2, Rect,
    ScrollArea, Sense, Stroke, StrokeKind, TextEdit, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_chat::emoji::SkinTone;
use tg_emoji::{EmojiAtlas, EmojiTextureCache};
use tg_lang::tr;
use tg_text::emoji::{self, EmojiRef, Section};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::{Icon, st};

use crate::chat_helpers::backend::{ChatHelpersBackend, EmojiGroupInfo};
use crate::chat_helpers::paint_helper_icon;
use tg_app_core::stand_in::theme::Palette;

/// Number of emoji columns in the 345px panel (`floor((345 - 14) / 37) = 8`).
pub const EMOJI_COLUMNS: usize = 8;
/// Target emoji cell size (`st::defaultEmojiPan.desiredSize`).
pub const EMOJI_CELL_SIZE: f32 = st::default_emoji_pan.desired_size.0 as f32;
/// Emoji sprite draw size inside each cell (`Ui::Emoji::GetSizeLarge()`: `18 * 4 / 3` at 100%).
pub const EMOJI_DRAW_SIZE: f32 = 24.0;
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

/// Footer icon pair (normal, active) for a Unicode emoji category (`defaultComposeIcons`).
fn footer_icons_for(section: Section) -> (Icon, Icon) {
    let icons = &st::default_compose_icons;
    match section {
        Section::Recent => (icons.recent, icons.recent_active),
        Section::People => (icons.people, icons.people_active),
        Section::Nature => (icons.nature, icons.nature_active),
        Section::Food => (icons.food, icons.food_active),
        Section::Activity => (icons.activity, icons.activity_active),
        Section::Travel => (icons.travel, icons.travel_active),
        Section::Objects => (icons.objects, icons.objects_active),
        Section::Symbols => (icons.symbols, icons.symbols_active),
    }
}

/// Inner margin of the scrolling emoji / sticker lists: `defaultEmojiPan.margin` plus
/// `defaultEmojiPan.padding` (left 7 + 7, right 7 + 4, bottom 7).
pub(crate) fn emoji_list_margin() -> Margin {
    let pan = &st::default_emoji_pan;
    Margin {
        left: (pan.margin.left.0 + pan.padding.left.0) as i8,
        right: (pan.margin.right.0 + pan.padding.right.0) as i8,
        top: 0,
        bottom: pan.padding.bottom.0 as i8,
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
    active_group: Option<usize>,
    mood_icons: MoodIconCache,
    mood_groups_polled_at: Option<f64>,
    columns: usize,
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
            active_group: None,
            mood_icons: MoodIconCache::default(),
            mood_groups_polled_at: None,
            columns: EMOJI_COLUMNS,
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
        self.active_group = None;
    }

    /// Returns the selected emoji group of the search row, if any.
    #[must_use]
    pub const fn active_group(&self) -> Option<usize> {
        self.active_group
    }

    /// Selects (or clears) an emoji group of the search row ([`Self::mood_groups`]).
    pub fn set_active_group(&mut self, group: Option<usize>) {
        self.active_group = group.filter(|&g| g < self.mood_icons.groups().len());
        self.search_query.clear();
        self.focused_index = None;
    }

    /// Emoji groups shown right of the search field (`TabbedSearch::_groups`).
    #[must_use]
    pub fn mood_groups(&self) -> &[MoodGroup] {
        self.mood_icons.groups()
    }

    /// Replaces the search-row emoji groups with the server's `messages.getEmojiGroups` list.
    /// An empty list restores the built-in [`EMOJI_MOOD_GROUPS`] fallback.
    pub fn set_mood_groups(&mut self, groups: Vec<MoodGroup>) {
        if self.mood_icons.set_groups(groups) {
            self.active_group = None;
        }
    }

    /// Pulls the search-row groups from `backend.emoji_groups()` (`messages.getEmojiGroups`,
    /// `EmojiStatuses::emojiGroupsValue` in `tabbed_selector.cpp` `MakeSearch`) when the panel is
    /// first shown and then every [`MOOD_GROUPS_POLL_SECS`]. An empty list or an error keeps
    /// the groups already shown (the built-in [`EMOJI_MOOD_GROUPS`] until the server answers),
    /// as tdesktop keeps its last `messages.emojiGroups` on a failed request.
    pub fn refresh_mood_groups(&mut self, backend: &dyn ChatHelpersBackend, now: f64) {
        if self
            .mood_groups_polled_at
            .is_some_and(|at| now >= at && now - at < MOOD_GROUPS_POLL_SECS)
        {
            return;
        }
        self.mood_groups_polled_at = Some(now);
        let groups = backend
            .emoji_groups()
            .map(|list| mood_groups_from_server(&list))
            .unwrap_or_default();
        if !groups.is_empty() {
            self.set_mood_groups(groups);
        }
    }

    fn group_emojis(&self) -> Option<Vec<EmojiRef>> {
        let group = self.mood_icons.groups().get(self.active_group?)?;
        Some(
            group
                .emoticons
                .iter()
                .filter_map(|e| emoji::find_emoji(e, 0).map(|(r, _)| r))
                .collect(),
        )
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
        self.active_group = None;
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
            if !self.search_query.is_empty() || self.active_group.is_some() {
                self.search_query.clear();
                self.active_group = None;
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
            (cur + self.columns).min(max_idx)
        } else if up {
            cur.saturating_sub(self.columns)
        } else {
            cur
        };
        self.focused_index = Some(next);
        (true, None)
    }

    fn collect_visible_emojis(&mut self, backend: &mut dyn ChatHelpersBackend) -> Vec<EmojiRef> {
        if self.search_query.trim().is_empty()
            && let Some(refs) = self.group_emojis()
        {
            return refs;
        }
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
        let now = ui.input(|i| i.time);
        self.refresh_mood_groups(backend, now);

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
            // 1. Search row (`TabbedSearch`, emoji groups on the right).
            let search = render_tabbed_search(
                ui,
                "tg_emoji_search_field",
                &mut self.search_query,
                self.active_group,
                &mut self.mood_icons,
                palette,
            );
            if search.changed {
                self.active_group = None;
                self.focused_index = None;
            }
            if let Some(g) = search.group_clicked {
                let next = (self.active_group != Some(g)).then_some(g);
                self.set_active_group(next);
            }

            // 2. Scrollable emoji grid body.
            let scroll_avail = list_rect_below_search(body_rect, &search);
            ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_avail), |ui| {
                // tdesktop scroll bars overlay the list (`emojiScroll`), they take no width.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::floating();
                ScrollArea::vertical()
                    .id_salt("tg_emoji_list_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::NONE
                            .inner_margin(emoji_list_margin())
                            .show(ui, |ui| {
                                // Headers and rows are stacked with no gaps (`sectionInfo`).
                                ui.spacing_mut().item_spacing.y = 0.0;
                                let now = ui.input(|i| i.time);
                                if self.search_query.trim().is_empty()
                                    && let Some(refs) = self.group_emojis()
                                {
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
                                } else if !self.search_query.trim().is_empty() {
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
                                        // The recent section has no title (`EmojiListWidget::paint`
                                        // skips the header of section 0).
                                        let top = ui.cursor().top();
                                        if self.scroll_to_section == Some(Section::Recent) {
                                            let r = Rect::from_min_size(
                                                Pos2::new(ui.cursor().left(), top),
                                                Vec2::new(1.0, 1.0),
                                            );
                                            ui.scroll_to_rect(r, Some(egui::Align::TOP));
                                            self.scroll_to_section = None;
                                        }
                                        if top >= scroll_avail.top() - 1.0 {
                                            self.active_section = Section::Recent;
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
                                        );
                                        if self.scroll_to_section == Some(sec) {
                                            ui.scroll_to_rect(hdr_rect, Some(egui::Align::TOP));
                                            self.scroll_to_section = None;
                                        }
                                        // Update active section indicator as user scrolls.
                                        if hdr_rect.top() <= scroll_avail.top() + 1.0 {
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
        self.paint_footer(ui, footer_rect, palette);

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
    ) -> Rect {
        let width = ui.available_width().max(120.0);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(width, EMOJI_HEADER_HEIGHT), Sense::hover());
        let pan = &st::default_emoji_pan;
        // `headerLeft` is measured from the panel edge; the list already has `margin.left`.
        let left = (pan.header_left.0 - pan.margin.left.0 - pan.padding.left.0) as f32;
        let font = font_id_from_style(st::emoji_pan_header_font);
        ui.painter().text(
            Pos2::new(
                rect.left() + left.max(0.0),
                rect.top() + pan.header_top.0 as f32,
            ),
            Align2::LEFT_TOP,
            section_title(section),
            font,
            theme_color(ui.ctx(), "emojiPanHeaderFg", palette.window_sub_text_fg),
        );

        // "Choose default skin tone" button in the Smileys & People header (`emojiPanColorAll`).
        if show_skin_all_button {
            let btn = &st::emoji_pan_color_all;
            let side = btn.width.0 as f32;
            let btn_rect = Rect::from_min_size(
                Pos2::new(
                    rect.right() - side,
                    rect.top() + pan.header_top.0 as f32 - 4.0,
                ),
                Vec2::splat(side),
            );
            let resp = ui.interact(
                btn_rect,
                ui.make_persistent_id("tg_emoji_color_all_btn"),
                Sense::click(),
            );
            let dark = ui.ctx().global_style().visuals.dark_mode;
            let icon = if resp.hovered() {
                btn.icon_over
            } else {
                btn.icon
            };
            if !paint_helper_icon(ui.painter(), btn_rect, icon, dark, None) {
                ui.painter().circle_stroke(
                    btn_rect.center(),
                    6.0,
                    Stroke::new(1.4, palette.window_sub_text_fg),
                );
            }
            if resp.on_hover_text(tr::lng_emoji_color_all()).clicked() {
                self.open_skin_tone_popup("✋", btn_rect, true);
            }
        }
        rect
    }

    /// Grid geometry of `EmojiListWidget::countDesiredHeight` for a list content width
    /// `avail_w` (the panel width minus `margin` and `padding` on both sides): returns the column
    /// count, the cell size and the x offset of the first column from the content's left edge.
    fn grid_geometry(avail_w: f32) -> (usize, Vec2, f32) {
        let pan = &st::default_emoji_pan;
        let desired = pan.desired_size.0.max(1);
        // tdesktop's `innerWidth` is the panel width minus `padding` only.
        let inner_w = (avail_w.max(0.0) as i32) + pan.margin.left.0 + pan.margin.right.0;
        let cols = (inner_w / desired).max(1);
        let single = inner_w / cols;
        // `_rowsLeft` is measured from the inner widget, which starts `margin.left` in; the
        // content here starts `margin.left + padding.left` in.
        let left = (inner_w - cols * single) / 2 - pan.margin.left.0;
        let size = Vec2::new(single as f32, (single - 2 * pan.vertical_size_sub.0) as f32);
        (cols as usize, size, left as f32)
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
        let (cols, cell, grid_left) = Self::grid_geometry(avail_w);
        self.columns = cols;
        let (cell_w, cell_h) = (cell.x, cell.y);
        let rows = items.len().div_ceil(cols);
        let total_h = rows as f32 * cell_h;

        let (alloc_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());
        let grid_rect = alloc_rect.translate(Vec2::new(grid_left, 0.0));
        let clip = ui.clip_rect();
        let area = Vec2::new(
            st::emoji_pan_area.width.0 as f32,
            st::emoji_pan_area.height.0 as f32,
        );

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
                // `_overBg`: an `emojiPanArea` rounded rect (`emojiPanRadius`) centred in the cell.
                let area_rect = Rect::from_center_size(cell_rect.center(), area);
                let radius = CornerRadius::same(st::emoji_pan_radius.0 as u8);
                if resp.hovered() || is_focused {
                    ui.painter().rect_filled(
                        area_rect,
                        radius,
                        theme_color(ui.ctx(), "emojiPanHover", palette.window_bg_over),
                    );
                }
                if is_focused {
                    ui.painter().rect_stroke(
                        area_rect,
                        radius,
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

    fn paint_footer(&mut self, ui: &mut Ui, footer_rect: Rect, palette: &Palette) {
        // `StickersListFooter` in emoji mode: Recent, then the Unicode categories as one
        // group icon that expands into all seven category icons while one of them is active.
        paint_footer_background(ui, footer_rect, palette);
        let icons = &st::default_compose_icons;
        let searching = !self.search_query.is_empty() || self.active_group.is_some();
        let expanded = !searching && self.active_section != Section::Recent;
        let categories = &EMOJI_SECTIONS[1..];

        let mut slots: Vec<(Section, Icon, Icon)> =
            vec![(Section::Recent, icons.recent, icons.recent_active)];
        if expanded {
            for &sec in categories {
                let (icon, active) = footer_icons_for(sec);
                slots.push((sec, icon, active));
            }
        } else {
            slots.push((Section::People, icons.people, icons.people_active));
        }

        if !searching {
            if expanded {
                paint_footer_selection(ui, footer_rect, 1, categories.len());
            } else {
                paint_footer_selection(ui, footer_rect, 0, 0);
            }
        }
        for (idx, (sec, icon, icon_active)) in slots.into_iter().enumerate() {
            let slot_rect = footer_slot_rect(footer_rect, idx);
            let resp = ui.interact(
                slot_rect,
                ui.make_persistent_id(("tg_emoji_footer", idx)),
                Sense::click(),
            );
            let active = !searching && self.active_section == sec;
            let icon = if active { icon_active } else { icon };
            if !paint_footer_icon(ui, slot_rect, icon) {
                ui.painter().circle_stroke(
                    slot_rect.center(),
                    8.0,
                    Stroke::new(1.5, palette.window_sub_text_fg),
                );
            }
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

// ---------------------------------------------------------------------------------------------
// Shared 7.4.6 pieces used by the Emoji, Stickers and GIFs tabs: the `TabbedSearch` row with
// emoji-group ("mood") buttons and the bottom `StickersListFooter` icon strip.
// ---------------------------------------------------------------------------------------------

/// egui temp-data key set by [`super::tabbed_panel::TabbedPanel`] while it paints in docked
/// (third column) mode, so footers paint square bottom corners.
pub(crate) const DOCKED_PANEL_FLAG: &str = "tg_tabbed_panel_docked";

/// Whether the panel is being painted as a docked column this frame.
pub(crate) fn is_docked_panel(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(Id::unique(DOCKED_PANEL_FLAG)))
        .unwrap_or(false)
}

/// Resolves a `tg-theme` palette color by its tdesktop name for the current Day/Night mode.
pub(crate) fn theme_color(ctx: &egui::Context, name: &str, fallback: Color32) -> Color32 {
    let dark = ctx.global_style().visuals.dark_mode;
    crate::chat_helpers::theme_palette_for_dark(dark)
        .color_by_name(name)
        .map_or(fallback, |c| {
            Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
        })
}

/// One emoji group ("mood") button of the search row (`Data::EmojiGroup`). tdesktop receives
/// these from `messages.getEmojiGroups`; the list below mirrors the server's default groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiMoodGroup {
    /// Emoji whose silhouette is drawn as the group icon.
    pub icon: &'static str,
    /// Group title (tooltip / accessible name).
    pub title: &'static str,
    /// Emoticons the group filters by.
    pub emoticons: &'static [&'static str],
}

/// Default emoji groups shown right of the search field (`TabbedSearch::_groups`).
pub const EMOJI_MOOD_GROUPS: [EmojiMoodGroup; 11] = [
    EmojiMoodGroup {
        icon: "❤️",
        title: "Love",
        emoticons: &["❤️", "😍", "🥰", "😘", "💕", "💖", "💘", "😻"],
    },
    EmojiMoodGroup {
        icon: "👍",
        title: "Approval",
        emoticons: &["👍", "👌", "👏", "🙌", "✅", "💯", "🤝"],
    },
    EmojiMoodGroup {
        icon: "👎",
        title: "Disapproval",
        emoticons: &["👎", "🙅", "❌", "🚫", "🙄"],
    },
    EmojiMoodGroup {
        icon: "🎉",
        title: "Cheers",
        emoticons: &["🎉", "🥳", "🎊", "🥂", "🍻", "🍾"],
    },
    EmojiMoodGroup {
        icon: "😂",
        title: "Laughter",
        emoticons: &["😂", "🤣", "😆", "😁", "😹"],
    },
    EmojiMoodGroup {
        icon: "😮",
        title: "Astonishment",
        emoticons: &["😮", "😲", "😱", "🤯", "😯"],
    },
    EmojiMoodGroup {
        icon: "😢",
        title: "Sadness",
        emoticons: &["😢", "😭", "😞", "😔", "🥺"],
    },
    EmojiMoodGroup {
        icon: "😡",
        title: "Anger",
        emoticons: &["😡", "😠", "🤬", "👿", "💢"],
    },
    EmojiMoodGroup {
        icon: "😐",
        title: "Neutral",
        emoticons: &["😐", "😑", "😶", "🫤"],
    },
    EmojiMoodGroup {
        icon: "🤔",
        title: "Doubt",
        emoticons: &["🤔", "🤨", "🧐", "😕"],
    },
    EmojiMoodGroup {
        icon: "🤪",
        title: "Silly",
        emoticons: &["🤪", "😜", "😝", "😛"],
    },
];

/// One emoji group of the search row as shown: either a server `Data::EmojiGroup`
/// (`messages.getEmojiGroups`) or an entry of the built-in [`EMOJI_MOOD_GROUPS`] fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoodGroup {
    /// Group title (tooltip / accessible name).
    pub title: String,
    /// Emoji whose silhouette is drawn as the group icon.
    pub icon: String,
    /// Emoticons the group filters by.
    pub emoticons: Vec<String>,
}

impl MoodGroup {
    /// Builds a group from server data. tdesktop draws the group's `icon_emoji_id` custom
    /// emoji; until that document is available the first emoticon stands in as the icon.
    #[must_use]
    pub fn new(title: impl Into<String>, emoticons: Vec<String>) -> Self {
        let icon = emoticons.first().cloned().unwrap_or_default();
        Self {
            title: title.into(),
            icon,
            emoticons,
        }
    }
}

impl From<&EmojiMoodGroup> for MoodGroup {
    fn from(group: &EmojiMoodGroup) -> Self {
        Self {
            title: group.title.to_owned(),
            icon: group.icon.to_owned(),
            emoticons: group.emoticons.iter().map(|e| (*e).to_owned()).collect(),
        }
    }
}

/// Seconds between `backend.emoji_groups()` polls while the emoji tab is shown; the live
/// backend answers from its hash-checked cache in between server refreshes.
pub const MOOD_GROUPS_POLL_SECS: f64 = 60.0;

/// Converts `messages.getEmojiGroups` entries into search-row groups (`GroupsFromTL` in
/// `data_emoji_statuses.cpp`). The `emojiGroupPremium` entry is dropped: its `*premium` query
/// lists premium custom emoji, which this emoji tab does not render.
#[must_use]
pub fn mood_groups_from_server(groups: &[EmojiGroupInfo]) -> Vec<MoodGroup> {
    groups
        .iter()
        .filter(|g| !g.is_premium && !g.emoticons.is_empty())
        .map(|g| MoodGroup::new(g.title.clone(), g.emoticons.clone()))
        .collect()
}

/// The built-in [`EMOJI_MOOD_GROUPS`] as owned [`MoodGroup`]s.
#[must_use]
pub fn default_mood_groups() -> Vec<MoodGroup> {
    EMOJI_MOOD_GROUPS.iter().map(MoodGroup::from).collect()
}

/// The search-row emoji groups plus their single-color icons: tdesktop paints the server's
/// group icons as text-colored custom emoji. Each icon is an outline alpha mask made from the
/// emoji sprite, tinted at paint time.
pub struct MoodIconCache {
    groups: Vec<MoodGroup>,
    textures: HashMap<usize, Option<TextureHandle>>,
}

impl Default for MoodIconCache {
    fn default() -> Self {
        Self {
            groups: default_mood_groups(),
            textures: HashMap::new(),
        }
    }
}

impl std::fmt::Debug for MoodIconCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoodIconCache")
            .field("groups", &self.groups.len())
            .field("count", &self.textures.len())
            .finish()
    }
}

impl MoodIconCache {
    /// Groups currently shown in the search row.
    #[must_use]
    pub fn groups(&self) -> &[MoodGroup] {
        &self.groups
    }

    /// Replaces the groups (an empty list restores the defaults). Returns `true` if they changed.
    pub fn set_groups(&mut self, groups: Vec<MoodGroup>) -> bool {
        let groups = if groups.is_empty() {
            default_mood_groups()
        } else {
            groups
        };
        if groups == self.groups {
            return false;
        }
        self.groups = groups;
        self.textures.clear();
        true
    }

    fn texture(&mut self, ctx: &egui::Context, index: usize) -> Option<egui::TextureId> {
        let group = self.groups.get(index)?;
        let name = format!("tg_emoji_mood_{index}_{}", group.icon);
        let icon = group.icon.clone();
        self.textures
            .entry(index)
            .or_insert_with(|| {
                let image = mood_icon_mask(&icon)?;
                Some(ctx.load_texture(name, image, TextureOptions::LINEAR))
            })
            .as_ref()
            .map(TextureHandle::id)
    }
}

fn mood_icon_mask(seq: &str) -> Option<egui::ColorImage> {
    let (er, _) = emoji::find_emoji(seq, 0)?;
    let atlas = EmojiAtlas::global();
    let sprite = atlas.sprite(er)?;
    let rgba = atlas.emoji_rgba(er).ok()??;
    let [_, _, w, h] = sprite.pixel_rect;
    let (w, h) = (w as usize, h as usize);
    if rgba.len() != w * h * 4 {
        return None;
    }
    let alpha: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
    let mut out = Vec::with_capacity(rgba.len());
    for a in outline_alpha(&alpha, w, h) {
        out.extend_from_slice(&[255, 255, 255, a]);
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &out))
}

/// The server's emoji-group icons are thin line drawings painted in the text colour. Without
/// them the built-in groups use the emoji's silhouette as an outline: the shape minus its
/// erosion, a stroke of about 1/12 of the sprite (1.5 px at the 18 px icon size).
fn outline_alpha(alpha: &[u8], w: usize, h: usize) -> Vec<u8> {
    let radius = (w.min(h) / 12).max(1);
    let eroded = erode(alpha, w, h, radius);
    alpha
        .iter()
        .zip(&eroded)
        .map(|(&a, &e)| a.saturating_sub(e))
        .collect()
}

/// Separable min filter (box erosion) of `radius` pixels; outside the image counts as empty.
fn erode(alpha: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
    let mut rows = vec![0_u8; alpha.len()];
    for y in 0..h {
        for x in 0..w {
            let from = x.saturating_sub(radius);
            let to = x + radius;
            rows[y * w + x] = if x < radius || to >= w {
                0
            } else {
                alpha[y * w + from..=y * w + to]
                    .iter()
                    .copied()
                    .min()
                    .unwrap_or(0)
            };
        }
    }
    let mut out = vec![0_u8; alpha.len()];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = if y < radius || y + radius >= h {
                0
            } else {
                (y - radius..=y + radius)
                    .map(|yy| rows[yy * w + x])
                    .min()
                    .unwrap_or(0)
            };
        }
    }
    out
}

/// Result of [`render_tabbed_search`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct TabbedSearchResponse {
    /// The typed query changed this frame.
    pub changed: bool,
    /// An emoji-group button was clicked (index into [`MoodIconCache::groups`]).
    pub group_clicked: Option<usize>,
    /// Bottom of the search row (`TabbedSearch::height`: `searchMargin` + `height`); the list
    /// rows start here (`_rowsTop`).
    pub bottom: f32,
}

/// The list area below the search row: from the row's bottom to the bottom of `body`.
pub(crate) fn list_rect_below_search(body: Rect, search: &TabbedSearchResponse) -> Rect {
    Rect::from_min_max(
        Pos2::new(body.left(), search.bottom.clamp(body.top(), body.bottom())),
        body.max,
    )
}

/// Paints tdesktop's `TabbedSearch` row (`defaultTabbedSearch`): a rounded `windowBgOver` pill
/// with the search icon, a `Search` field of `defaultFieldWidth` and the emoji-group buttons.
/// Margins follow `defaultEmojiPan.margin` + `searchMargin`.
pub(crate) fn render_tabbed_search(
    ui: &mut Ui,
    id_salt: &str,
    query: &mut String,
    active_group: Option<usize>,
    icons: &mut MoodIconCache,
    palette: &Palette,
) -> TabbedSearchResponse {
    let ts = &st::default_tabbed_search;
    let pan = &st::default_emoji_pan;
    let height = ts.height.0 as f32;
    let outer = ui.available_rect_before_wrap();
    let margin_left = (pan.margin.left.0 + pan.search_margin.left.0) as f32;
    let margin_right = (pan.margin.right.0 + pan.search_margin.right.0) as f32;
    let margin_top = pan.search_margin.top.0 as f32;
    let margin_bottom = pan.search_margin.bottom.0 as f32;
    let bar = Rect::from_min_max(
        Pos2::new(outer.left() + margin_left, outer.top() + margin_top),
        Pos2::new(
            (outer.right() - margin_right).max(outer.left() + margin_left + 120.0),
            outer.top() + margin_top + height,
        ),
    );
    let row_bottom = bar.bottom() + margin_bottom;

    let ctx = ui.ctx().clone();
    let dark = ctx.global_style().visuals.dark_mode;
    let bg = theme_color(&ctx, "windowBgOver", palette.window_bg_over);
    let bg_active = theme_color(&ctx, "windowBgRipple", palette.window_bg_over);
    let fg = theme_color(&ctx, "emojiIconFg", palette.window_sub_text_fg);
    let fg_active = theme_color(&ctx, "emojiSubIconFgActive", palette.window_fg);
    let placeholder_fg = theme_color(&ctx, "placeholderFg", palette.window_sub_text_fg);
    let text_fg = theme_color(&ctx, "windowFg", palette.window_fg);
    ui.painter()
        .rect_filled(bar, CornerRadius::same((height / 2.0) as u8), bg);

    let mut out = TabbedSearchResponse {
        bottom: row_bottom,
        ..TabbedSearchResponse::default()
    };

    // Search icon button (`defaultTabbedSearchButton`, icon at x = 7, centered vertically).
    let search_btn = Rect::from_min_size(bar.min, Vec2::new(ts.search.width.0 as f32, height));
    let icon_rect = Rect::from_min_size(
        Pos2::new(
            search_btn.left() + ts.search.icon_position.x.0 as f32,
            search_btn.center().y - 12.0,
        ),
        Vec2::splat(24.0),
    );
    if !paint_helper_icon(ui.painter(), icon_rect, ts.search.icon, dark, Some(fg)) {
        let c = icon_rect.center() - Vec2::new(1.5, 1.5);
        ui.painter().circle_stroke(c, 5.0, Stroke::new(1.5, fg));
        ui.painter().line_segment(
            [c + Vec2::new(3.6, 3.6), c + Vec2::new(7.0, 7.0)],
            Stroke::new(1.6, fg),
        );
    }

    let edit_id = ui.make_persistent_id(("tg_tabbed_search_field", id_salt));
    let focused = ui.memory(|m| m.has_focus(edit_id));
    let show_groups = query.is_empty() && !focused;
    let cancel_w = if query.is_empty() {
        0.0
    } else {
        ts.cancel.width.0 as f32
    };
    let field_left = search_btn.right();
    // Groups start at `FadeLeftStart` (search icon x + its width) + `defaultFieldWidth`.
    let groups_left = bar.left()
        + ts.search.icon_position.x.0 as f32
        + SEARCH_ICON_WIDTH
        + ts.default_field_width.0 as f32;
    let field_right = if show_groups {
        groups_left.min(bar.right() - 4.0)
    } else {
        bar.right() - cancel_w - 4.0
    };
    let field_rect = Rect::from_min_max(
        Pos2::new(field_left, bar.top()),
        Pos2::new(field_right.max(field_left + 20.0), bar.bottom()),
    );
    let font = FontId::proportional(ts.field.style.font.size.0 as f32);
    // `defaultTabbedSearchField`: `textMargins` + `placeholderMargins` = 4 px before the text.
    let text_rect = Rect::from_min_max(
        Pos2::new(field_rect.left() + 4.0, field_rect.top() + 5.0),
        Pos2::new(field_rect.right() - 2.0, field_rect.bottom() - 5.0),
    );
    let edit_resp = ui.put(
        text_rect,
        TextEdit::singleline(query)
            .id(edit_id)
            .frame(Frame::NONE)
            .font(font.clone())
            .text_color(text_fg)
            .align(Align2::LEFT_CENTER)
            .hint_text(
                egui::RichText::new(tr::lng_dlg_filter())
                    .font(font)
                    .color(placeholder_fg),
            ),
    );
    if edit_resp.changed() {
        out.changed = true;
    }

    if show_groups {
        // `GroupsStrip`: `groupWidth` slots with no gaps, running to the pill's right end where
        // the pill's rounded clip path cuts them (no right fade).
        let groups_rect = Rect::from_min_max(
            Pos2::new(field_rect.right(), bar.top()),
            Pos2::new(bar.right(), bar.bottom()),
        );
        let painter = ui.painter_at(groups_rect);
        let group_w = ts.group_width.0 as f32;
        let skip = ts.group_skip.0 as f32;
        let group_titles: Vec<String> = icons.groups().iter().map(|g| g.title.clone()).collect();
        for (idx, title) in group_titles.iter().enumerate() {
            let x = groups_rect.left() + idx as f32 * group_w;
            if x >= groups_rect.right() {
                break;
            }
            let slot = Rect::from_min_size(
                Pos2::new(x, groups_rect.top()),
                Vec2::new(group_w, groups_rect.height()),
            );
            let resp = ui.interact(
                slot.intersect(groups_rect),
                ui.make_persistent_id(("tg_tabbed_search_group", id_salt, idx)),
                Sense::click(),
            );
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::Role::Button,
                    true,
                    active_group == Some(idx),
                    title,
                )
            });
            let active = active_group == Some(idx);
            if active {
                painter.circle_filled(slot.center(), group_w / 2.0 - skip, bg_active);
            }
            if let Some(tex) = icons.texture(&ctx, idx) {
                painter.image(
                    tex,
                    Rect::from_center_size(slot.center(), Vec2::splat(GROUP_ICON_DRAW_SIZE)),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    if active { fg_active } else { fg },
                );
            }
            if resp.clicked() {
                out.group_clicked = Some(idx);
            }
        }
        let outer_bg = theme_color(&ctx, "emojiPanBg", palette.window_bg);
        paint_round_end_mask(ui.painter(), bar, outer_bg);
    }

    if !query.is_empty() {
        let cancel = Rect::from_min_max(Pos2::new(bar.right() - cancel_w, bar.top()), bar.max);
        let resp = ui.interact(
            cancel,
            ui.make_persistent_id(("tg_tabbed_search_cancel", id_salt)),
            Sense::click(),
        );
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tr::lng_cancel()));
        let cross_fg = theme_color(&ctx, "menuIconFg", palette.window_sub_text_fg);
        if !paint_helper_icon(
            ui.painter(),
            Rect::from_center_size(cancel.center(), Vec2::splat(12.0)),
            st::small_close_icon,
            dark,
            Some(cross_fg),
        ) {
            let cc = cancel.center();
            ui.painter().line_segment(
                [cc + Vec2::new(-4.0, -4.0), cc + Vec2::new(4.0, 4.0)],
                Stroke::new(1.5, cross_fg),
            );
            ui.painter().line_segment(
                [cc + Vec2::new(4.0, -4.0), cc + Vec2::new(-4.0, 4.0)],
                Stroke::new(1.5, cross_fg),
            );
        }
        if resp.clicked() {
            query.clear();
            out.changed = true;
        }
    }
    // Reserve the whole row last: `ui.put` for the text field moved the cursor to the field.
    ui.allocate_rect(
        Rect::from_min_max(outer.min, Pos2::new(outer.right(), row_bottom)),
        Sense::hover(),
    );
    out
}

/// Drawn size of a search-row group icon. tdesktop draws the server's icons at 22 px
/// (`SearchWithGroups::IconSizeOverride`) with about 2 px of padding inside; the outlines made
/// from emoji sprites fill their box, so they are drawn at the visible size instead.
const GROUP_ICON_DRAW_SIZE: f32 = 18.0;

/// Width of the `emoji/emoji_search_input` icon (`FadeLeftStart` adds it to `iconPosition.x`).
const SEARCH_ICON_WIDTH: f32 = 24.0;

/// Covers the outside of `bar`'s rounded right end with `outer`, so whatever was painted up to
/// the bar's edge is cut along the pill's curve (tdesktop clips the strip to the pill's path).
fn paint_round_end_mask(painter: &egui::Painter, bar: Rect, outer: Color32) {
    let r = bar.height() / 2.0;
    let center = Pos2::new(bar.right() - r, bar.center().y);
    const STEPS: usize = 24;
    let mut mesh = egui::Mesh::default();
    for (corner, from, to) in [
        (bar.right_top(), -std::f32::consts::FRAC_PI_2, 0.0_f32),
        (bar.right_bottom(), 0.0, std::f32::consts::FRAC_PI_2),
    ] {
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(corner, outer);
        for i in 0..=STEPS {
            let a = from + (to - from) * i as f32 / STEPS as f32;
            mesh.colored_vertex(center + r * Vec2::new(a.cos(), a.sin()), outer);
        }
        for i in 0..STEPS as u32 {
            mesh.add_triangle(base, base + 1 + i, base + 2 + i);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Footer icon slot width (`defaultEmojiPan.iconWidth`).
pub(crate) const FOOTER_ICON_WIDTH: f32 = st::default_emoji_pan.icon_width.0 as f32;
/// Footer selection square (`defaultEmojiPan.iconArea`).
pub(crate) const FOOTER_ICON_AREA: f32 = st::default_emoji_pan.icon_area.0 as f32;
/// Left / right inset of the footer icons (`defaultEmojiPan.iconSkip`).
pub(crate) const FOOTER_ICON_SKIP: f32 = st::default_emoji_pan.icon_skip.0 as f32;

/// Paints the footer strip background (`categoriesBg`). No line above it: tdesktop shows
/// `TabbedSelector::_bottomShadow` only in the emoji-status mode.
pub(crate) fn paint_footer_background(ui: &Ui, rect: Rect, palette: &Palette) {
    let ctx = ui.ctx();
    let bg = theme_color(ctx, "emojiPanCategories", palette.window_bg);
    let r = if is_docked_panel(ctx) {
        0
    } else {
        st::emoji_pan_radius.0 as u8
    };
    ui.painter().rect_filled(
        rect,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: r,
            se: r,
        },
        bg,
    );
}

/// Rect of footer icon slot `index` (left aligned, `iconSkip` inset, `iconWidth` each).
pub(crate) fn footer_slot_rect(footer: Rect, index: usize) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            footer.left() + FOOTER_ICON_SKIP + index as f32 * FOOTER_ICON_WIDTH,
            footer.top(),
        ),
        Vec2::new(FOOTER_ICON_WIDTH, footer.height()),
    )
}

/// Paints the rounded `categoriesBgOver` selection behind footer slots `from..=to`.
pub(crate) fn paint_footer_selection(ui: &Ui, footer: Rect, from: usize, to: usize) {
    let first = footer_slot_rect(footer, from);
    let last = footer_slot_rect(footer, to.max(from));
    let inset = (FOOTER_ICON_WIDTH - FOOTER_ICON_AREA) / 2.0;
    let rect = Rect::from_min_max(
        Pos2::new(
            first.left() + inset,
            first.center().y - FOOTER_ICON_AREA / 2.0,
        ),
        Pos2::new(
            last.right() - inset,
            first.center().y + FOOTER_ICON_AREA / 2.0,
        ),
    );
    let radius = if to > from {
        FOOTER_ICON_AREA / 2.0
    } else {
        st::emoji_pan_radius.0 as f32
    };
    let ctx = ui.ctx();
    let fallback = ui.visuals().widgets.hovered.bg_fill;
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius as u8),
        theme_color(ctx, "windowBgRipple", fallback),
    );
}

/// Paints a 24 px footer category icon (`defaultComposeIcons`) centered in `slot`.
pub(crate) fn paint_footer_icon(ui: &Ui, slot: Rect, icon: Icon) -> bool {
    let dark = ui.ctx().global_style().visuals.dark_mode;
    paint_helper_icon(
        ui.painter(),
        Rect::from_center_size(slot.center(), Vec2::splat(24.0)),
        icon,
        dark,
        None,
    )
}

#[cfg(test)]
mod outline_tests {
    use super::outline_alpha;

    #[test]
    fn mood_icon_outline_keeps_the_edge_and_hollows_the_inside() {
        let (w, h) = (24, 24);
        let mut alpha = vec![0_u8; w * h];
        for y in 4..20 {
            for x in 4..20 {
                alpha[y * w + x] = 255;
            }
        }
        let out = outline_alpha(&alpha, w, h);
        assert_eq!(out[12 * w + 12], 0, "centre is hollow");
        assert_eq!(out[12 * w + 4], 255, "left edge stays");
        assert_eq!(out[4 * w + 12], 255, "top edge stays");
        assert_eq!(out[0], 0, "outside stays empty");
    }
}
