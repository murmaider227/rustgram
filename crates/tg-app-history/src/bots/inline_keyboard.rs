//! Inline keyboard widget rendered beneath a chat message bubble.
//!
//! Port origin: `Telegram/SourceFiles/history/history_item_components.cpp` (`ReplyKeyboard`),
//! `Telegram/SourceFiles/history/view/history_view_element.cpp` (`KeyboardStyle`), and
//! `Telegram/SourceFiles/history/view/history_view_message.cpp` (reply markup painting).

use std::collections::HashMap;

use egui::{
    Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use tg_bots::callback::{BotButtonAction, ButtonActivationContext, activate_button};
use tg_bots::keyboard_layout::{BotKeyboardMetrics, ButtonRounding, ButtonsLayout};
use tg_bots::markup::{ButtonType, MarkupButton, ReplyMarkupData};
use tg_bots::stand_in::{FullMsgId, MsgId, PeerId, UserId};

use crate::bots::ui_helpers::{
    BotButtonColors, paint_inline_button_icon, paint_loading_spinner, to_corner_radius, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Gap between the bottom of a message bubble and its attached inline keyboard (`2px`).
pub const INLINE_KEYBOARD_TOP_SKIP: f32 = 2.0;

/// Event emitted when the user clicks an active inline keyboard button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineKeyboardActivation {
    /// Owning message id.
    pub msg_id: FullMsgId,
    /// Zero-based row index.
    pub row: usize,
    /// Zero-based column index.
    pub col: usize,
    /// Copy of the clicked [`MarkupButton`].
    pub button: MarkupButton,
    /// Computed [`BotButtonAction`] from `tg_bots::callback::activate_button`.
    pub action: BotButtonAction,
}

/// Identifies a specific button coordinate on a specific message for tracking in-flight RPCs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InlineButtonKey {
    /// Owning message id.
    pub msg_id: FullMsgId,
    /// Row index.
    pub row: usize,
    /// Column index.
    pub col: usize,
}

/// Per-chat state tracking pressed / loading inline keyboard buttons (callback spinners).
#[derive(Debug, Default, Clone)]
pub struct InlineKeyboardState {
    /// Active loading buttons mapped to their start timestamp (seconds).
    loading: HashMap<InlineButtonKey, f64>,
    /// Currently pressed button coordinate (for visual pressed feedback).
    pressed: Option<InlineButtonKey>,
}

impl InlineKeyboardState {
    /// Creates an empty [`InlineKeyboardState`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `(msg_id, row, col)` as loading (shows spinner & blocks duplicate clicks).
    pub fn set_loading(&mut self, msg_id: FullMsgId, row: usize, col: usize, started_at_secs: f64) {
        self.loading
            .insert(InlineButtonKey { msg_id, row, col }, started_at_secs);
    }

    /// Clears the loading spinner for `(msg_id, row, col)`.
    pub fn clear_loading(&mut self, msg_id: FullMsgId, row: usize, col: usize) -> bool {
        self.loading
            .remove(&InlineButtonKey { msg_id, row, col })
            .is_some()
    }

    /// Returns `true` if `(msg_id, row, col)` currently has an in-flight request.
    #[must_use]
    pub fn is_loading(&self, msg_id: FullMsgId, row: usize, col: usize) -> bool {
        self.loading
            .contains_key(&InlineButtonKey { msg_id, row, col })
    }

    /// Returns the number of currently loading inline buttons.
    #[must_use]
    pub fn loading_count(&self) -> usize {
        self.loading.len()
    }

    /// Returns the currently pressed button key, if any.
    #[must_use]
    pub const fn pressed_button(&self) -> Option<InlineButtonKey> {
        self.pressed
    }
}

/// Parameters controlling inline keyboard rendering and activation context.
#[derive(Debug, Clone, Copy)]
pub struct InlineKeyboardParams {
    /// Owning message id.
    pub msg_id: FullMsgId,
    /// Optional forum topic root id.
    pub topic_root_id: Option<MsgId>,
    /// Bot user id that sent or owns the message.
    pub bot_id: Option<UserId>,
    /// Whether the bot is verified (skips URL confirmation when `true`).
    pub bot_verified: bool,
    /// Optional peer to return to on `SwitchInline`.
    pub bot_inline_return_to: Option<PeerId>,
    /// Whether the message is a regular server message.
    pub is_regular: bool,
    /// Whether this view is an admin log entry (restricts interactive buttons to URL/Callback).
    pub is_admin_log_entry: bool,
    /// Target keyboard width in logical pixels (typically the bubble width).
    pub width: f32,
    /// Current animation clock in seconds (deterministic in tests/screenshots).
    pub time_secs: f64,
    /// Optional outer corner rounding (top corners are always forced `Small` for inline keyboards).
    pub outer_rounding: ButtonRounding,
}

impl InlineKeyboardParams {
    /// Creates standard parameters for a regular bot message bubble of `width` pixels.
    #[must_use]
    pub const fn for_message(
        msg_id: FullMsgId,
        bot_id: Option<UserId>,
        bot_verified: bool,
        width: f32,
        time_secs: f64,
    ) -> Self {
        Self {
            msg_id,
            topic_root_id: None,
            bot_id,
            bot_verified,
            bot_inline_return_to: None,
            is_regular: true,
            is_admin_log_entry: false,
            width,
            time_secs,
            outer_rounding: ButtonRounding::ALL_LARGE,
        }
    }
}

/// Computes the exact [`ButtonsLayout`] for an inline keyboard using egui's font metrics.
#[must_use]
pub fn compute_inline_layout(ui: &mut Ui, markup: &ReplyMarkupData, width: f32) -> ButtonsLayout {
    let font = FontId::proportional(13.0);
    let mut layout = ButtonsLayout::from_markup(markup, BotKeyboardMetrics::INLINE, |text| {
        let w = ui.fonts_mut(|f| {
            f.layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        });
        w.ceil() as i32
    });
    let natural_h = layout.natural_height();
    let target_w = width.round().max(64.0) as i32;
    layout.resize(target_w, natural_h);
    layout
}

/// Paints an inline keyboard directly below a message bubble and returns any activated button.
///
/// Uses `tg_bots::keyboard_layout::ButtonsLayout` for width distribution, corner rounding,
/// and `tg_bots::callback::activate_button` for action resolution.
pub fn show_inline_keyboard(
    ui: &mut Ui,
    markup: &ReplyMarkupData,
    params: InlineKeyboardParams,
    state: &mut InlineKeyboardState,
    palette: &Palette,
) -> Option<InlineKeyboardActivation> {
    if !markup.is_inline() || markup.rows.is_empty() {
        return None;
    }

    let layout = compute_inline_layout(ui, markup, params.width);
    let total_w = layout.width() as f32;
    let total_h = layout.height().max(0) as f32;
    if total_w <= 0.0 || total_h <= 0.0 {
        return None;
    }

    let (origin_rect, _) = ui.allocate_exact_size(Vec2::new(total_w, total_h), Sense::hover());
    let colors = BotButtonColors::from_palette(palette);
    let font = FontId::proportional(13.0);
    let mut activation = None;
    let mut any_pressed = None;

    for laid_row in layout.rows() {
        for laid in laid_row {
            let Some(button) = markup.button(laid.row, laid.column) else {
                continue;
            };
            if laid.rect.x + laid.rect.width > layout.width() {
                break;
            }
            let btn_rect = Rect::from_min_size(
                Pos2::new(
                    origin_rect.left() + laid.rect.x as f32,
                    origin_rect.top() + laid.rect.y as f32,
                ),
                Vec2::new(laid.rect.width as f32, laid.rect.height as f32),
            );
            let key = InlineButtonKey {
                msg_id: params.msg_id,
                row: laid.row,
                col: laid.column,
            };
            let is_loading = state.loading.contains_key(&key);
            let is_disabled = button.button_type() == ButtonType::Disabled
                || (params.is_admin_log_entry
                    && button.button_type() != ButtonType::Url
                    && button.button_type() != ButtonType::Callback);

            let id = Id::unique((
                "tg_inline_kb_btn",
                params.msg_id.peer.0,
                params.msg_id.msg.0,
                laid.row,
                laid.column,
            ));
            let sense = if is_disabled {
                Sense::hover()
            } else {
                Sense::click()
            };
            let resp = ui.interact(btn_rect, id, sense);
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, !is_disabled, button.text.clone())
            });
            let hovered = !is_disabled && resp.hovered();
            let pressed = !is_disabled && resp.is_pointer_button_down_on();
            if hovered {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if pressed {
                any_pressed = Some(key);
            }

            let rounding =
                layout.button_rounding(laid.row, laid.column, params.outer_rounding, true);
            let corner_radius = to_corner_radius(rounding);
            let (mut bg, mut fg) = colors.resolve(button.visual.color, true, hovered, pressed);
            if is_disabled {
                bg = with_alpha(bg, 0.50);
                fg = with_alpha(fg, 0.60);
            }

            let pad = BotKeyboardMetrics::INLINE.button_padding as f32;
            let text_max_w = (btn_rect.width() - pad * 2.0).max(8.0);
            let display_text = truncate_single_line(ui, &button.text, &font, text_max_w);

            let painter = ui.painter();
            painter.rect_filled(btn_rect, corner_radius, bg);

            // Subtle glare shimmer band while callback request is in flight
            // (matches `KeyboardStyle::paintButtonLoading` in `history_view_element.cpp`).
            if is_loading {
                let phase = ((params.time_secs * 0.9) % 1.0) as f32;
                let band_w = (btn_rect.width() * 0.35).max(24.0);
                let band_x = btn_rect.left() - band_w + (btn_rect.width() + band_w * 2.0) * phase;
                let band_rect = Rect::from_min_max(
                    Pos2::new(band_x.max(btn_rect.left()), btn_rect.top()),
                    Pos2::new((band_x + band_w).min(btn_rect.right()), btn_rect.bottom()),
                );
                if band_rect.width() > 0.5 {
                    painter.rect_filled(
                        band_rect,
                        corner_radius,
                        Color32::from_rgba_unmultiplied(255, 255, 255, 34),
                    );
                }
                painter.rect_stroke(
                    btn_rect,
                    corner_radius,
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
                    StrokeKind::Inside,
                );
            }

            // Label (elided if narrower than button)
            painter.text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                display_text,
                font.clone(),
                fg,
            );

            // Corner icon or loading spinner
            if is_loading {
                let spinner_center = Pos2::new(btn_rect.right() - 10.0, btn_rect.bottom() - 10.0);
                paint_loading_spinner(painter, spinner_center, 4.5, fg, params.time_secs);
            } else {
                paint_inline_button_icon(
                    painter,
                    btn_rect,
                    button.button_type(),
                    button.corner_icon(),
                    with_alpha(fg, 0.90),
                );
            }

            if !is_disabled && resp.clicked() {
                let ctx = ButtonActivationContext {
                    item_id: params.msg_id,
                    topic_root_id: params.topic_root_id,
                    bot_id: params.bot_id,
                    bot_verified: params.bot_verified,
                    bot_inline_return_to: params.bot_inline_return_to,
                    is_regular: params.is_regular,
                    ephemeral_id: None,
                    request_in_flight: is_loading,
                };
                let action = activate_button(button, &ctx);
                if !matches!(action, BotButtonAction::NoOp) {
                    activation = Some(InlineKeyboardActivation {
                        msg_id: params.msg_id,
                        row: laid.row,
                        col: laid.column,
                        button: button.clone(),
                        action,
                    });
                }
            }
        }
    }

    state.pressed = any_pressed;
    activation
}

fn truncate_single_line(ui: &mut Ui, text: &str, font: &FontId, max_width: f32) -> String {
    let full_w = ui.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    });
    if full_w <= max_width {
        return text.to_owned();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let candidate = format!("{out}{ch}…");
        let w = ui.fonts_mut(|f| {
            f.layout_no_wrap(candidate, font.clone(), Color32::WHITE)
                .size()
                .x
        });
        if w > max_width {
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        "…".to_owned()
    } else {
        format!("{out}…")
    }
}
