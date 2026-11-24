//! History bubble quick actions: configurable double-click Reply/React and hover corner buttons.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/history_view_quick_action.h`
//! - `Telegram/SourceFiles/history/view/history_view_quick_action.cpp`

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use tg_core::settings::DoubleClickQuickAction;
use tg_ui::anim::{SimpleAnimation, Transition, with_alpha};

use crate::history::backend::HistoryMessage;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

/// Default quick reaction emoji used when double-clicking a bubble with
/// [`DoubleClickQuickAction::React`].
pub const DEFAULT_QUICK_REACTION_EMOJI: &str = "❤️";

/// Duration of the hover corner quick-action fade-in (`ms`).
pub const BUBBLE_QUICK_ACTION_FADE_MS: u32 = 120;

/// Action resolved from a double-click or corner quick-action button on a message bubble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryQuickActionOutcome {
    /// Start replying to `msg_id`.
    Reply(MsgId),
    /// Toggle quick reaction `emoji` on `msg_id`.
    React {
        /// Target message ID.
        msg_id: MsgId,
        /// Emoji to toggle.
        emoji: String,
    },
    /// Forward `msg_id`.
    Forward(MsgId),
}

/// Resolves the double-click quick action on `msg` matching
/// `HistoryView::CurrentQuickAction()` (`history_view_quick_action.cpp`).
#[must_use]
pub fn resolve_double_click_action(
    configured: DoubleClickQuickAction,
    msg: &HistoryMessage,
    default_reaction: &str,
) -> Option<HistoryQuickActionOutcome> {
    if msg.is_service() || msg.id.0 <= 0 {
        return None;
    }
    match configured {
        DoubleClickQuickAction::Reply => Some(HistoryQuickActionOutcome::Reply(msg.id)),
        DoubleClickQuickAction::React => {
            let emoji = if default_reaction.trim().is_empty() {
                DEFAULT_QUICK_REACTION_EMOJI.to_owned()
            } else {
                default_reaction.trim().to_owned()
            };
            Some(HistoryQuickActionOutcome::React {
                msg_id: msg.id,
                emoji,
            })
        }
        DoubleClickQuickAction::None => None,
    }
}

/// Tracks hover opacity animation for bubble corner quick-action pills.
#[derive(Debug, Default)]
pub struct BubbleHoverQuickActions {
    hovered_msg: Option<MsgId>,
    fade_anim: SimpleAnimation,
}

impl BubbleHoverQuickActions {
    /// Updates hovered message and returns opacity in `0.0..=1.0` for `msg_id`.
    pub fn opacity_for(
        &mut self,
        msg_id: MsgId,
        is_hovered: bool,
        now_secs: f64,
        reduced_motion: bool,
    ) -> f32 {
        if is_hovered && self.hovered_msg != Some(msg_id) {
            self.hovered_msg = Some(msg_id);
            let dur = if reduced_motion {
                0
            } else {
                BUBBLE_QUICK_ACTION_FADE_MS
            };
            self.fade_anim
                .start(now_secs, 0.0, 1.0, dur, Transition::EaseOutCubic);
        }
        if self.hovered_msg == Some(msg_id) {
            if is_hovered {
                self.fade_anim.sample(now_secs, 1.0) as f32
            } else {
                self.hovered_msg = None;
                0.0
            }
        } else {
            0.0
        }
    }

    /// Returns `true` while the fade animation is running.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.fade_anim.animating()
    }
}

/// Paints compact hover corner quick-action buttons beside `bubble_rect` (Quick Reply, Quick
/// React, Quick Forward) and returns any triggered [`HistoryQuickActionOutcome`].
#[allow(clippy::too_many_arguments)]
pub fn show_bubble_hover_quick_actions(
    ui: &mut Ui,
    bubble_rect: Rect,
    row_rect: Rect,
    msg: &HistoryMessage,
    corner_reply: bool,
    corner_reaction: bool,
    default_reaction: &str,
    opacity: f32,
    colors: &HistoryColors,
) -> Option<HistoryQuickActionOutcome> {
    if msg.is_service() || msg.id.0 <= 0 || opacity <= 0.02 {
        return None;
    }
    let btn_size = 26.0;
    let gap = 4.0;
    let margin_x = 8.0;

    let mut buttons: Vec<(
        tg_ui::tg_style::Icon,
        &'static str,
        HistoryQuickActionOutcome,
    )> = Vec::new();
    if corner_reply {
        buttons.push((
            tg_ui::tg_style::st::menu_icon_reply,
            "Quick Reply",
            HistoryQuickActionOutcome::Reply(msg.id),
        ));
    }
    if corner_reaction {
        let emoji = if default_reaction.trim().is_empty() {
            DEFAULT_QUICK_REACTION_EMOJI.to_owned()
        } else {
            default_reaction.trim().to_owned()
        };
        buttons.push((
            tg_ui::tg_style::st::menu_icon_fave,
            "Quick React",
            HistoryQuickActionOutcome::React {
                msg_id: msg.id,
                emoji,
            },
        ));
    }
    buttons.push((
        tg_ui::tg_style::st::menu_icon_forward,
        "Quick Forward",
        HistoryQuickActionOutcome::Forward(msg.id),
    ));

    let total_w = buttons.len() as f32 * btn_size + buttons.len().saturating_sub(1) as f32 * gap;
    let start_x = if msg.outgoing {
        (bubble_rect.left() - margin_x - total_w).max(row_rect.left() + 6.0)
    } else {
        (bubble_rect.right() + margin_x).min(row_rect.right() - total_w - 6.0)
    };
    let center_y = (bubble_rect.bottom() - 16.0).clamp(
        bubble_rect.top() + btn_size * 0.5,
        bubble_rect.bottom() - btn_size * 0.5,
    );

    let mut triggered = None;
    for (idx, (icon, label, outcome)) in buttons.into_iter().enumerate() {
        let cx = start_x + idx as f32 * (btn_size + gap) + btn_size * 0.5;
        let btn_rect = Rect::from_center_size(Pos2::new(cx, center_y), Vec2::splat(btn_size));
        let resp = ui.interact(
            btn_rect,
            ui.scope_id()
                .with(("tg_bubble_quick_action", msg.id.0, idx)),
            Sense::click(),
        );
        let accessible = format!("{label} (message {})", msg.id.0);
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, accessible.clone())
        });
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        let alpha = f64::from(opacity);
        let fill = if resp.hovered() {
            with_alpha(colors.accent, 0.92 * alpha)
        } else {
            with_alpha(colors.bar_bg, 0.92 * alpha)
        };
        let fg = if resp.hovered() {
            with_alpha(Color32::WHITE, alpha)
        } else {
            with_alpha(colors.sub_text_fg, alpha)
        };
        ui.painter()
            .circle_filled(btn_rect.center(), btn_size * 0.5, fill);
        ui.painter().circle_stroke(
            btn_rect.center(),
            btn_size * 0.5,
            Stroke::new(1.0, with_alpha(colors.divider, 0.55 * alpha)),
        );
        // Subtle crisp outline on hover.
        if resp.hovered() {
            ui.painter().rect_stroke(
                btn_rect,
                egui::CornerRadius::same((btn_size * 0.5) as u8),
                Stroke::new(1.0, with_alpha(Color32::WHITE, 0.25 * alpha)),
                StrokeKind::Inside,
            );
        }
        tg_app_core::ui_icons::paint_style_tinted(
            ui.painter(),
            Rect::from_center_size(btn_rect.center(), Vec2::splat(14.0)),
            icon,
            fg,
        );

        if resp.on_hover_text(label).clicked() {
            triggered = Some(outcome);
        }
    }

    triggered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::mock::MockHistoryStore;
    use tg_app_core::history::backend::HistoryBackend;
    use tg_app_core::stand_in::types::PeerId;

    #[test]
    fn resolve_double_click_respects_configured_action() {
        let mut store = MockHistoryStore::new();
        let peer_id = PeerId(1);
        let msg = store
            .send_message(
                peer_id,
                tg_app_core::history::backend::SendMessageRequest {
                    text: tg_text::TextWithEntities::simple("Quick action sample".to_owned()),
                    reply_to: None,
                    silent: false,
                },
            )
            .expect("send sample message");

        assert_eq!(
            resolve_double_click_action(DoubleClickQuickAction::Reply, &msg, "🔥"),
            Some(HistoryQuickActionOutcome::Reply(msg.id))
        );
        assert_eq!(
            resolve_double_click_action(DoubleClickQuickAction::React, &msg, "🔥"),
            Some(HistoryQuickActionOutcome::React {
                msg_id: msg.id,
                emoji: "🔥".to_owned(),
            })
        );
        assert_eq!(
            resolve_double_click_action(DoubleClickQuickAction::None, &msg, "🔥"),
            None
        );
    }
}
