//! Tabbed selector hosting the Emoji, Stickers, and GIFs tabs with animated tab switching.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/tabbed_selector.h` and
//! `Telegram/SourceFiles/chat_helpers/tabbed_selector.cpp`.

use egui::{CornerRadius, Key, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_chat::{DocumentId, StickerSetId};
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::chat_helpers::backend::ChatHelpersBackend;
use crate::chat_helpers::emoji_list_widget::{
    EmojiListEvent, EmojiListWidget, is_docked_panel, theme_color,
};
use crate::chat_helpers::gifs_list_widget::{GifsListEvent, GifsListWidget};
use crate::chat_helpers::stickers_list_widget::{
    StickerMediaCache, StickersListEvent, StickersListWidget,
};
use tg_app_core::stand_in::theme::Palette;

/// Height of the top tab slider bar (`st::defaultTabsSlider.height`).
pub const SELECTOR_TAB_BAR_HEIGHT: f32 = st::default_tabs_slider.height.0 as f32;
/// Duration of the horizontal tab slide & indicator animation (`st::emojiPanSlideDuration`).
pub const TAB_SLIDE_DURATION_SECS: f64 = st::emoji_pan_slide_duration as f64 / 1000.0;

/// Active tab inside [`TabbedSelector`], matching `ChatHelpers::SelectorTab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SelectorTab {
    /// Unicode emoji picker tab.
    #[default]
    Emoji,
    /// Stickers packs & search tab.
    Stickers,
    /// Saved & inline-search GIFs tab.
    Gifs,
}

impl SelectorTab {
    /// All tabs in visual left-to-right order.
    pub const ALL: [Self; 3] = [Self::Emoji, Self::Stickers, Self::Gifs];

    /// Zero-based index of this tab (`0..3`).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Emoji => 0,
            Self::Stickers => 1,
            Self::Gifs => 2,
        }
    }

    /// Returns the localized display label for the tab header (`tr::lng_switch_*`).
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Emoji => tr::lng_switch_emoji(),
            Self::Stickers => tr::lng_switch_stickers(),
            Self::Gifs => tr::lng_switch_gifs(),
        }
    }

    /// Returns the next tab cyclically (for Ctrl+Tab navigation).
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Emoji => Self::Stickers,
            Self::Stickers => Self::Gifs,
            Self::Gifs => Self::Emoji,
        }
    }

    /// Returns the previous tab cyclically (for Ctrl+Shift+Tab navigation).
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Emoji => Self::Gifs,
            Self::Stickers => Self::Emoji,
            Self::Gifs => Self::Stickers,
        }
    }
}

/// High-level events emitted when the user interacts with [`TabbedSelector`] or [`super::TabbedPanel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatHelpersEvent {
    /// User chose an emoji to insert into the compose input field.
    EmojiChosen {
        /// Full Unicode emoji text (including skin tone modifier or `U+FE0F`).
        emoji: String,
    },
    /// User chose a sticker to send immediately.
    StickerChosen {
        /// Selected sticker document ID.
        document_id: DocumentId,
        /// Owning sticker set ID, if known.
        set_id: Option<StickerSetId>,
    },
    /// User chose a GIF to send immediately.
    GifChosen {
        /// Selected GIF document ID.
        gif_id: DocumentId,
        /// Descriptive title of the GIF.
        title: String,
    },
    /// User clicked a Premium-locked sticker while not subscribed to Telegram Premium.
    PremiumRequired {
        /// Human-readable reason / prompt message.
        reason: String,
        /// Locked sticker document ID.
        document_id: DocumentId,
    },
    /// User installed a trending/featured sticker set.
    StickerSetInstalled(StickerSetId),
    /// User uninstalled a sticker set.
    StickerSetRemoved(StickerSetId),
    /// User cleared recent stickers (`messages.clearRecentStickers`).
    RecentStickersCleared,
    /// User requested closing the panel (e.g. via Escape).
    CloseRequested,
}

/// Composite tabbed selector widget containing Emoji, Stickers, and GIFs views.
#[derive(Debug)]
pub struct TabbedSelector {
    active_tab: SelectorTab,
    indicator_from: f32,
    indicator_to: f32,
    slide_start_time: Option<f64>,
    slide_direction: f32,
    emoji_list: EmojiListWidget,
    stickers_list: StickersListWidget,
    gifs_list: GifsListWidget,
}

impl Default for TabbedSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl TabbedSelector {
    /// Creates a new [`TabbedSelector`] initialized on the Emoji tab.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_tab: SelectorTab::Emoji,
            indicator_from: 0.0,
            indicator_to: 0.0,
            slide_start_time: None,
            slide_direction: 0.0,
            emoji_list: EmojiListWidget::new(),
            stickers_list: StickersListWidget::new(),
            gifs_list: GifsListWidget::new(),
        }
    }

    /// Returns the currently selected tab.
    #[must_use]
    pub const fn active_tab(&self) -> SelectorTab {
        self.active_tab
    }

    /// Switches to `tab` immediately without animation.
    pub fn set_tab_immediate(&mut self, tab: SelectorTab) {
        self.active_tab = tab;
        self.indicator_from = tab.index() as f32;
        self.indicator_to = tab.index() as f32;
        self.slide_start_time = None;
        self.slide_direction = 0.0;
    }

    /// Switches to `tab` with tdesktop's 200ms indicator + horizontal slide animation.
    pub fn switch_tab(&mut self, tab: SelectorTab, now_secs: f64) {
        if self.active_tab == tab {
            return;
        }
        let current_pos = self.indicator_position(now_secs);
        self.slide_direction = if tab.index() > self.active_tab.index() {
            1.0
        } else {
            -1.0
        };
        self.indicator_from = current_pos;
        self.indicator_to = tab.index() as f32;
        self.slide_start_time = Some(now_secs);
        self.active_tab = tab;
        self.emoji_list.close_skin_tone_popup();
    }

    /// Returns `true` when an overlay popup (e.g. skin-tone picker) should prevent auto-hiding.
    #[must_use]
    pub fn prevent_auto_hide(&self) -> bool {
        self.emoji_list.has_open_popup()
    }

    /// Immutable access to the inner [`EmojiListWidget`].
    #[must_use]
    pub const fn emoji_list(&self) -> &EmojiListWidget {
        &self.emoji_list
    }

    /// Mutable access to the inner [`EmojiListWidget`].
    pub fn emoji_list_mut(&mut self) -> &mut EmojiListWidget {
        &mut self.emoji_list
    }

    /// Mutable access to the inner [`StickersListWidget`].
    pub fn stickers_list_mut(&mut self) -> &mut StickersListWidget {
        &mut self.stickers_list
    }

    /// Mutable access to the inner [`GifsListWidget`].
    pub fn gifs_list_mut(&mut self) -> &mut GifsListWidget {
        &mut self.gifs_list
    }

    /// Returns `true` once after the Stickers footer settings button was clicked; the host
    /// then opens the installed sticker sets box (`StickersBox`).
    pub fn take_sticker_settings_requested(&mut self) -> bool {
        self.stickers_list.take_settings_requested()
    }

    /// Mutable access to the shared [`StickerMediaCache`] (useful for autocomplete previews).
    pub fn sticker_media_cache_mut(&mut self) -> &mut StickerMediaCache {
        self.stickers_list.media_cache_mut()
    }

    fn slide_progress(&self, now_secs: f64) -> f32 {
        let Some(start) = self.slide_start_time else {
            return 1.0;
        };
        let t = ((now_secs - start) / TAB_SLIDE_DURATION_SECS).clamp(0.0, 1.0) as f32;
        // Ease-out cubic matching tdesktop `anim::easeOutCirc` / smooth deceleration.
        1.0 - (1.0 - t).powi(3)
    }

    fn indicator_position(&self, now_secs: f64) -> f32 {
        let p = self.slide_progress(now_secs);
        self.indicator_from + (self.indicator_to - self.indicator_from) * p
    }

    /// Processes keyboard shortcuts for tab switching (`Alt+1/2/3`, `Ctrl+Tab`) and delegates
    /// grid navigation to the active tab widget.
    pub fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn ChatHelpersBackend,
    ) -> Vec<ChatHelpersEvent> {
        let mut events = Vec::new();
        let now = ctx.input(|i| i.time);

        // Tab shortcuts: Alt+1/2/3 or Ctrl+Tab
        let (alt_1, alt_2, alt_3, ctrl_tab, shift_down) = ctx.input(|i| {
            (
                i.modifiers.alt && i.key_pressed(Key::Num1),
                i.modifiers.alt && i.key_pressed(Key::Num2),
                i.modifiers.alt && i.key_pressed(Key::Num3),
                i.modifiers.ctrl && i.key_pressed(Key::Tab),
                i.modifiers.shift,
            )
        });
        if alt_1 {
            self.switch_tab(SelectorTab::Emoji, now);
            return events;
        } else if alt_2 {
            self.switch_tab(SelectorTab::Stickers, now);
            return events;
        } else if alt_3 {
            self.switch_tab(SelectorTab::Gifs, now);
            return events;
        } else if ctrl_tab {
            let next = if shift_down {
                self.active_tab.prev()
            } else {
                self.active_tab.next()
            };
            self.switch_tab(next, now);
            return events;
        }

        let consumed = match self.active_tab {
            SelectorTab::Emoji => {
                let (used, ev) = self.emoji_list.handle_keyboard(ctx, backend);
                if let Some(EmojiListEvent::Chosen(emoji)) = ev {
                    events.push(ChatHelpersEvent::EmojiChosen { emoji });
                }
                used
            }
            SelectorTab::Stickers => {
                let (used, ev) = self.stickers_list.handle_keyboard(ctx, backend);
                if let Some(ev) = ev {
                    events.push(map_sticker_event(ev));
                }
                used
            }
            SelectorTab::Gifs => {
                let (used, ev) = self.gifs_list.handle_keyboard(ctx, backend);
                if let Some(GifsListEvent::Chosen { gif_id, title }) = ev {
                    events.push(ChatHelpersEvent::GifChosen { gif_id, title });
                }
                used
            }
        };

        if !consumed && ctx.input(|i| i.key_pressed(Key::Escape)) {
            events.push(ChatHelpersEvent::CloseRequested);
        }

        events
    }

    /// Renders the tab bar and active tab content inside `ui`'s available rect.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) -> Vec<ChatHelpersEvent> {
        let mut events = Vec::new();
        let rect = ui.available_rect_before_wrap();
        let now = ui.input(|i| i.time);

        // 1. Top tab bar (`SELECTOR_TAB_BAR_HEIGHT`).
        let tab_bar_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), SELECTOR_TAB_BAR_HEIGHT));
        self.paint_tab_bar(ui, tab_bar_rect, now, palette);

        // 2. Content area with horizontal slide offset during tab transitions.
        let p = self.slide_progress(now);
        if p < 1.0 {
            ui.ctx().request_repaint();
        } else {
            self.slide_start_time = None;
        }
        let slide_offset_x = if p < 1.0 {
            self.slide_direction * (1.0 - p) * 24.0
        } else {
            0.0
        };

        let content_rect = Rect::from_min_max(
            Pos2::new(rect.left() + slide_offset_x, tab_bar_rect.bottom()),
            Pos2::new(rect.right() + slide_offset_x, rect.bottom()),
        );
        let clip_rect = Rect::from_min_max(Pos2::new(rect.left(), tab_bar_rect.bottom()), rect.max);

        ui.scope_builder(egui::UiBuilder::new().max_rect(content_rect), |ui| {
            ui.set_clip_rect(clip_rect.intersect(ui.clip_rect()));
            match self.active_tab {
                SelectorTab::Emoji => {
                    if let Some(EmojiListEvent::Chosen(emoji)) =
                        self.emoji_list.show(ui, backend, emoji_cache, palette)
                    {
                        events.push(ChatHelpersEvent::EmojiChosen { emoji });
                    }
                }
                SelectorTab::Stickers => {
                    if let Some(ev) = self.stickers_list.show(ui, backend, palette) {
                        events.push(map_sticker_event(ev));
                    }
                }
                SelectorTab::Gifs => {
                    if let Some(GifsListEvent::Chosen { gif_id, title }) =
                        self.gifs_list.show(ui, backend, emoji_cache, palette)
                    {
                        events.push(ChatHelpersEvent::GifChosen { gif_id, title });
                    }
                }
            }
        });

        events
    }

    fn paint_tab_bar(&mut self, ui: &mut Ui, tab_bar_rect: Rect, now: f64, palette: &Palette) {
        // `emojiTabs` = `defaultTabsSlider`: equal sections, semibold labels at `labelTop`,
        // `lightButtonFg` for the active label and a `barStroke` bar from `barTop` snapped to
        // the active label width (`barSnapToLabel`).
        let slider = &st::default_tabs_slider;
        let docked = is_docked_panel(ui.ctx());
        let r = if docked {
            0
        } else {
            st::emoji_pan_radius.0 as u8
        };
        ui.painter().rect_filled(
            tab_bar_rect,
            CornerRadius {
                nw: r,
                ne: r,
                sw: 0,
                se: 0,
            },
            palette.window_bg,
        );

        let font = font_id_from_style(slider.label_style.font);
        let label_fg = theme_color(ui.ctx(), "windowSubTextFg", palette.window_sub_text_fg);
        let label_fg_active = theme_color(ui.ctx(), "lightButtonFg", palette.window_active_text_fg);
        let bar_fg = theme_color(ui.ctx(), "sliderBgActive", palette.window_bg_active);

        let tab_w = tab_bar_rect.width() / SelectorTab::ALL.len() as f32;
        let mut label_widths = [0.0f32; 3];
        for (idx, tab) in SelectorTab::ALL.iter().copied().enumerate() {
            let slot = Rect::from_min_size(
                Pos2::new(tab_bar_rect.left() + idx as f32 * tab_w, tab_bar_rect.top()),
                Vec2::new(tab_w, tab_bar_rect.height()),
            );
            let resp = ui.interact(
                slot,
                ui.make_persistent_id(("tg_selector_tab", idx)),
                Sense::click(),
            );
            let active = self.active_tab == tab;
            let galley = ui.painter().layout_no_wrap(
                tab.label(),
                font.clone(),
                if active { label_fg_active } else { label_fg },
            );
            label_widths[idx] = galley.size().x;
            ui.painter().galley(
                Pos2::new(
                    slot.center().x - galley.size().x / 2.0,
                    slot.top() + slider.label_top.0 as f32,
                ),
                galley,
                label_fg,
            );
            if resp.clicked() {
                self.switch_tab(tab, now);
            }
        }

        // Animated active bar, interpolated between label positions and widths.
        let ind_pos = self.indicator_position(now).clamp(0.0, 2.0);
        let lo = ind_pos.floor() as usize;
        let hi = (lo + 1).min(2);
        let t = ind_pos - lo as f32;
        let width = label_widths[lo] + (label_widths[hi] - label_widths[lo]) * t;
        let center_x = tab_bar_rect.left() + (ind_pos + 0.5) * tab_w;
        let bar_top = tab_bar_rect.top() + slider.bar_top.0 as f32;
        let bar_bottom = (bar_top + slider.bar_stroke.0 as f32).min(tab_bar_rect.bottom());
        ui.painter().rect_filled(
            Rect::from_min_max(
                Pos2::new(center_x - width / 2.0, bar_top),
                Pos2::new(center_x + width / 2.0, bar_bottom),
            ),
            CornerRadius::same(slider.bar_radius.0 as u8),
            bar_fg,
        );
        ui.painter().line_segment(
            [
                Pos2::new(tab_bar_rect.left(), tab_bar_rect.bottom()),
                Pos2::new(tab_bar_rect.right(), tab_bar_rect.bottom()),
            ],
            Stroke::new(1.0, theme_color(ui.ctx(), "shadowFg", palette.divider)),
        );
    }
}

fn map_sticker_event(ev: StickersListEvent) -> ChatHelpersEvent {
    match ev {
        StickersListEvent::Chosen {
            document_id,
            set_id,
        } => ChatHelpersEvent::StickerChosen {
            document_id,
            set_id,
        },
        StickersListEvent::PremiumRequired(document_id) => ChatHelpersEvent::PremiumRequired {
            reason: tr::lng_premium_unlock_stickers(),
            document_id,
        },
        StickersListEvent::SetInstalled(id) => ChatHelpersEvent::StickerSetInstalled(id),
        StickersListEvent::SetRemoved(id) => ChatHelpersEvent::StickerSetRemoved(id),
        StickersListEvent::RecentCleared => ChatHelpersEvent::RecentStickersCleared,
    }
}
