//! History view theme colors and `tg_ui::stand_in::Colors` adapter.
//!
//! Port origin: `Telegram/Resources/colors.palette` (history section) and
//! `Telegram/SourceFiles/ui/chat/chat_style.cpp`.

use egui::Color32;
use tg_ui::stand_in::{Colors, DayPalette, TextPalette};

use crate::stand_in::theme::Palette;

/// Semantic color set for painting the chat history canvas, bubbles, bars, and overlays.
#[derive(Debug, Clone, Copy)]
pub struct HistoryColors {
    /// True when the Night palette is active.
    pub is_night: bool,
    /// Wallpaper / canvas background behind bubbles.
    pub canvas_bg: Color32,
    /// Subtle decorative wallpaper pattern tint.
    pub canvas_pattern: Color32,
    /// Chrome surface (top bar, pinned bar, compose bar).
    pub bar_bg: Color32,
    /// Hover surface on bars/buttons.
    pub bar_bg_over: Color32,
    /// Primary text on bars and bubbles.
    pub text_fg: Color32,
    /// Bold heading text.
    pub bold_fg: Color32,
    /// Muted / subtitle text.
    pub sub_text_fg: Color32,
    /// Active accent blue.
    pub accent: Color32,
    /// Active link / online text.
    pub active_text_fg: Color32,
    /// Danger / destructive red.
    pub attention_fg: Color32,
    /// 1px divider line.
    pub divider: Color32,
    /// Incoming message bubble fill (`msgInBg`).
    pub msg_in_bg: Color32,
    /// Incoming message bubble selected fill (`msgInBgSelected`).
    pub msg_in_bg_selected: Color32,
    /// Outgoing message bubble fill (`msgOutBg`).
    pub msg_out_bg: Color32,
    /// Outgoing message bubble selected fill (`msgOutBgSelected`).
    pub msg_out_bg_selected: Color32,
    /// Bubble drop shadow (`msgInShadow` / `msgOutShadow`).
    pub msg_in_shadow: Color32,
    /// Outgoing bubble drop shadow.
    pub msg_out_shadow: Color32,
    /// Incoming timestamp/views text (`msgInDateFg`).
    pub msg_in_date_fg: Color32,
    /// Outgoing timestamp/views/ticks text (`msgOutDateFg`).
    pub msg_out_date_fg: Color32,
    /// Incoming reply accent bar (`msgInReplyBarColor`).
    pub msg_in_reply_bar: Color32,
    /// Outgoing reply accent bar (`msgOutReplyBarColor`).
    pub msg_out_reply_bar: Color32,
    /// Incoming reply preview translucent fill.
    pub msg_in_reply_bg: Color32,
    /// Outgoing reply preview translucent fill.
    pub msg_out_reply_bg: Color32,
    /// Incoming link text.
    pub link_in_fg: Color32,
    /// Outgoing link text.
    pub link_out_fg: Color32,
    /// Incoming monospace code foreground.
    pub mono_in_fg: Color32,
    /// Outgoing monospace code foreground.
    pub mono_out_fg: Color32,
    /// Monospace code background pill.
    pub mono_bg: Color32,
    /// Service pill / date separator translucent background (`msgServiceBg`).
    pub service_bg: Color32,
    /// Service pill / date separator text (`msgServiceFg`).
    pub service_fg: Color32,
    /// Floating image/sticker timestamp dark pill (`msgDateImgBg`).
    pub date_img_bg: Color32,
    /// Floating image/sticker timestamp text (`msgDateImgFg`).
    pub date_img_fg: Color32,
    /// Unread messages bar background (`historyUnreadBarBg`).
    pub unread_bar_bg: Color32,
    /// Unread messages bar text (`historyUnreadBarFg`).
    pub unread_bar_fg: Color32,
    /// Selection row overlay tint (`msgSelectOverlay`).
    pub select_overlay: Color32,
    /// Jump-to-message highlight tint.
    pub highlight_tint: Color32,
    /// Voice waveform active bar (incoming).
    pub wave_in_active: Color32,
    /// Voice waveform inactive bar (incoming).
    pub wave_in_inactive: Color32,
    /// Voice waveform active bar (outgoing).
    pub wave_out_active: Color32,
    /// Voice waveform inactive bar (outgoing).
    pub wave_out_inactive: Color32,
    /// Reaction pill unchosen background (incoming).
    pub reaction_in_bg: Color32,
    /// Reaction pill unchosen background (outgoing).
    pub reaction_out_bg: Color32,
    /// Reaction pill chosen background.
    pub reaction_chosen_bg: Color32,
    /// Reaction pill chosen foreground.
    pub reaction_chosen_fg: Color32,
    /// Scroll-to-bottom button fill (`historyToDownBg`).
    pub to_down_bg: Color32,
    /// Scroll-to-bottom button hover fill (`historyToDownBgOver`).
    pub to_down_bg_over: Color32,
    /// Scroll-to-bottom chevron color (`historyToDownFg`).
    pub to_down_fg: Color32,
    /// Text selection highlight fill inside a bubble.
    pub text_select_bg: Color32,
}

impl HistoryColors {
    /// Derives complete history view colors from the shell [`Palette`].
    #[must_use]
    pub fn from_palette(palette: &Palette) -> Self {
        let is_night = palette.window_bg.r() < 128;
        if is_night {
            Self {
                is_night: true,
                canvas_bg: palette.chat_canvas_bg,
                canvas_pattern: Color32::from_rgba_unmultiplied(255, 255, 255, 7),
                bar_bg: palette.window_bg,
                bar_bg_over: palette.window_bg_over,
                text_fg: palette.window_fg,
                bold_fg: palette.window_bold_fg,
                sub_text_fg: palette.window_sub_text_fg,
                accent: palette.window_bg_active,
                active_text_fg: palette.window_active_text_fg,
                attention_fg: palette.attention_fg,
                divider: palette.divider,
                msg_in_bg: palette.msg_in_bg,
                msg_in_bg_selected: Color32::from_rgb(0x2E, 0x70, 0xA5),
                msg_out_bg: palette.msg_out_bg,
                msg_out_bg_selected: Color32::from_rgb(0x3F, 0x7A, 0xB3),
                msg_in_shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 56),
                msg_out_shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 56),
                msg_in_date_fg: Color32::from_rgb(0x6D, 0x7F, 0x8F),
                msg_out_date_fg: Color32::from_rgb(0x7D, 0xA8, 0xD3),
                msg_in_reply_bar: Color32::from_rgb(0x64, 0xB5, 0xEF),
                msg_out_reply_bar: Color32::from_rgb(0x88, 0xC8, 0xF8),
                msg_in_reply_bg: Color32::from_rgba_unmultiplied(0x64, 0xB5, 0xEF, 26),
                msg_out_reply_bg: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 20),
                link_in_fg: Color32::from_rgb(0x6A, 0xB2, 0xF2),
                link_out_fg: Color32::from_rgb(0x8A, 0xC8, 0xFF),
                mono_in_fg: Color32::from_rgb(0x8E, 0xC2, 0xEB),
                mono_out_fg: Color32::from_rgb(0xA6, 0xD4, 0xFA),
                mono_bg: Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 48),
                service_bg: Color32::from_rgba_unmultiplied(0x18, 0x25, 0x33, 196),
                service_fg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                date_img_bg: Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 128),
                date_img_fg: Color32::WHITE,
                unread_bar_bg: Color32::from_rgb(0x1D, 0x2A, 0x3A),
                unread_bar_fg: Color32::from_rgb(0x6A, 0xB2, 0xF2),
                select_overlay: Color32::from_rgba_unmultiplied(0x52, 0x88, 0xC1, 68),
                highlight_tint: Color32::from_rgba_unmultiplied(0x52, 0x88, 0xC1, 110),
                wave_in_active: Color32::from_rgb(0x6A, 0xB2, 0xF2),
                wave_in_inactive: Color32::from_rgb(0x46, 0x5B, 0x70),
                wave_out_active: Color32::from_rgb(0x8A, 0xC8, 0xFF),
                wave_out_inactive: Color32::from_rgb(0x51, 0x7C, 0xA6),
                reaction_in_bg: Color32::from_rgba_unmultiplied(0x6A, 0xB2, 0xF2, 34),
                reaction_out_bg: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 32),
                reaction_chosen_bg: Color32::from_rgb(0x52, 0x88, 0xC1),
                reaction_chosen_fg: Color32::WHITE,
                to_down_bg: Color32::from_rgb(0x1F, 0x2C, 0x3A),
                to_down_bg_over: Color32::from_rgb(0x28, 0x38, 0x4A),
                to_down_fg: Color32::from_rgb(0x8D, 0xA1, 0xB5),
                text_select_bg: Color32::from_rgba_unmultiplied(0x52, 0x88, 0xC1, 95),
            }
        } else {
            Self {
                is_night: false,
                canvas_bg: Color32::from_rgb(0xD5, 0xE2, 0xEC),
                canvas_pattern: Color32::from_rgba_unmultiplied(0x6B, 0x8F, 0xA8, 22),
                bar_bg: palette.window_bg,
                bar_bg_over: palette.window_bg_over,
                text_fg: palette.window_fg,
                bold_fg: palette.window_bold_fg,
                sub_text_fg: palette.window_sub_text_fg,
                accent: palette.window_bg_active,
                active_text_fg: palette.window_active_text_fg,
                attention_fg: palette.attention_fg,
                divider: palette.divider,
                msg_in_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                msg_in_bg_selected: Color32::from_rgb(0xC2, 0xDC, 0xF2),
                msg_out_bg: Color32::from_rgb(0xEF, 0xFD, 0xDE),
                msg_out_bg_selected: Color32::from_rgb(0xB7, 0xDB, 0xDB),
                msg_in_shadow: Color32::from_rgba_unmultiplied(0x74, 0x8E, 0xA2, 54),
                msg_out_shadow: Color32::from_rgba_unmultiplied(0x56, 0x8E, 0x52, 54),
                msg_in_date_fg: Color32::from_rgb(0x95, 0xA2, 0xAD),
                msg_out_date_fg: Color32::from_rgb(0x5E, 0xA9, 0x56),
                msg_in_reply_bar: Color32::from_rgb(0x40, 0xA7, 0xE3),
                msg_out_reply_bar: Color32::from_rgb(0x55, 0xB3, 0x52),
                msg_in_reply_bg: Color32::from_rgba_unmultiplied(0x40, 0xA7, 0xE3, 24),
                msg_out_reply_bg: Color32::from_rgba_unmultiplied(0x55, 0xB3, 0x52, 26),
                link_in_fg: Color32::from_rgb(0x16, 0x8A, 0xCD),
                link_out_fg: Color32::from_rgb(0x16, 0x8A, 0xCD),
                mono_in_fg: Color32::from_rgb(0x4E, 0x73, 0x91),
                mono_out_fg: Color32::from_rgb(0x3D, 0x84, 0x5A),
                mono_bg: Color32::from_rgba_unmultiplied(0x40, 0x82, 0xB0, 22),
                service_bg: Color32::from_rgba_unmultiplied(0x56, 0x74, 0x8A, 152),
                service_fg: Color32::WHITE,
                date_img_bg: Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 118),
                date_img_fg: Color32::WHITE,
                unread_bar_bg: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 232),
                unread_bar_fg: Color32::from_rgb(0x37, 0x8F, 0xC9),
                select_overlay: Color32::from_rgba_unmultiplied(0x35, 0x8C, 0xD4, 58),
                highlight_tint: Color32::from_rgba_unmultiplied(0x40, 0xA7, 0xE3, 86),
                wave_in_active: Color32::from_rgb(0x40, 0xA7, 0xE3),
                wave_in_inactive: Color32::from_rgb(0xD2, 0xDC, 0xE4),
                wave_out_active: Color32::from_rgb(0x55, 0xB3, 0x52),
                wave_out_inactive: Color32::from_rgb(0xC2, 0xDF, 0xB8),
                reaction_in_bg: Color32::from_rgb(0xE8, 0xF3, 0xFC),
                reaction_out_bg: Color32::from_rgb(0xD8, 0xF2, 0xC8),
                reaction_chosen_bg: Color32::from_rgb(0x40, 0xA7, 0xE3),
                reaction_chosen_fg: Color32::WHITE,
                to_down_bg: Color32::WHITE,
                to_down_bg_over: Color32::from_rgb(0xF2, 0xF5, 0xF8),
                to_down_fg: Color32::from_rgb(0x70, 0x75, 0x79),
                text_select_bg: Color32::from_rgba_unmultiplied(0x40, 0xA7, 0xE3, 72),
            }
        }
    }

    /// Builds a [`tg_ui::stand_in::TextPalette`] for incoming or outgoing message text.
    #[must_use]
    pub fn text_palette(&self, outgoing: bool) -> TextPalette {
        let spoiler_fg = if self.is_night {
            Color32::from_rgb(0x8D, 0xA1, 0xB5)
        } else {
            Color32::from_rgb(0x6D, 0x7C, 0x8A)
        };
        TextPalette {
            link_fg: if outgoing {
                self.link_out_fg
            } else {
                self.link_in_fg
            },
            mono_fg: if outgoing {
                self.mono_out_fg
            } else {
                self.mono_in_fg
            },
            spoiler_fg,
            select_fg: self.text_fg,
            select_bg: self.text_select_bg,
            select_link_fg: if outgoing {
                self.link_out_fg
            } else {
                self.link_in_fg
            },
            select_mono_fg: if outgoing {
                self.mono_out_fg
            } else {
                self.mono_in_fg
            },
            select_spoiler_fg: spoiler_fg,
            select_overlay: self.select_overlay,
            mark_bg: self.mono_bg,
            link_always_active: false,
        }
    }

    /// Builds a [`tg_ui::stand_in::Colors`] adapter reflecting Day or Night palette entries.
    #[must_use]
    pub fn ui_colors(&self) -> HistoryUiColors {
        HistoryUiColors {
            base: DayPalette::new(),
            colors: *self,
        }
    }
}

/// Adapter implementing `tg_ui::stand_in::Colors` so `tg-ui` widgets respect Day & Night themes.
#[derive(Debug, Clone)]
pub struct HistoryUiColors {
    base: DayPalette,
    colors: HistoryColors,
}

impl Colors for HistoryUiColors {
    fn get(&self, name: &str) -> Option<Color32> {
        if self.colors.is_night {
            let mapped = match name {
                "windowBg" | "menuBg" => Some(self.colors.bar_bg),
                "windowBgOver" | "menuBgOver" => Some(self.colors.bar_bg_over),
                "windowBgRipple" | "menuBgRipple" => Some(Color32::from_rgb(0x28, 0x36, 0x45)),
                "windowFg" | "windowFgOver" => Some(self.colors.text_fg),
                "windowBoldFg" => Some(self.colors.bold_fg),
                "windowSubTextFg" | "menuIconFg" => Some(self.colors.sub_text_fg),
                "windowActiveTextFg" => Some(self.colors.active_text_fg),
                "windowShadowFg" => Some(self.colors.divider),
                "windowFgActive" => Some(Color32::WHITE),
                "menuIconFgOver" => Some(self.colors.text_fg),
                "menuSeparatorFg" => Some(self.colors.divider),
                "menuFgDisabled" => Some(Color32::from_rgb(0x55, 0x66, 0x77)),
                "windowBgActive" | "dialogsUnreadBg" | "activeButtonBg" => Some(self.colors.accent),
                "attentionButtonFg" | "attentionButtonFgOver" => Some(self.colors.attention_fg),
                _ => None,
            };
            if mapped.is_some() {
                return mapped;
            }
        }
        self.base.get(name)
    }
}
