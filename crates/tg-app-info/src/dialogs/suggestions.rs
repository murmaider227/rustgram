//! Dialogs top-bar promo suggestions banner (`help.getPromoData` / `help.dismissSuggestion`).
//!
//! Port of `Telegram/SourceFiles/dialogs/suggestions/*`:
//! - `suggestion.h` / `suggestion.cpp` (priority ordering & lifecycle)
//! - `suggestion_userpic_setup.cpp`, `suggestion_premium_offer.cpp`,
//!   `suggestion_birthday_setup.cpp`, `suggestion_birthday_contacts.cpp`,
//!   `suggestion_low_credits_subs.cpp`, `suggestion_premium_grace.cpp`,
//!   `suggestion_custom_promo.cpp`, `suggestion_gift_auctions.cpp`,
//!   `suggestion_unreviewed_auth.cpp`.

use std::collections::BTreeSet;

use egui::{Align2, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_ui::fonts::font_id_from_style;
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::st;
use tg_ui::theme::Colors;

use crate::dialogs::layout::DialogColors;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::misc::{
    PromoDataSnapshot, PromoSuggestion, PromoSuggestionKind, SuggestionAction, SuggestionPriority,
};

/// Stateful container selecting the active top-bar suggestion and tracking local dismissals.
#[derive(Debug, Default, Clone)]
pub struct PromoSuggestionsState {
    items: Vec<PromoSuggestion>,
    dismissed: BTreeSet<String>,
    expires: i32,
}

impl PromoSuggestionsState {
    /// Creates an empty suggestions state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the current suggestions from a server [`PromoDataSnapshot`].
    pub fn apply_snapshot(&mut self, snapshot: PromoDataSnapshot) {
        self.expires = snapshot.expires;
        for key in snapshot.dismissed_suggestions {
            self.dismissed.insert(key);
        }

        let mut next = Vec::new();
        for key in snapshot.pending_suggestions {
            if let Some(built) = suggestion_from_server_key(&key, snapshot.promo_peer) {
                next.push(built);
            }
        }
        if let Some(custom) = snapshot.custom_suggestion {
            next.push(custom);
        }
        next.sort_by_key(|s| s.priority);
        self.items = next;
    }

    /// Adds a custom/local suggestion (useful for tests or client-side offers).
    pub fn push(&mut self, suggestion: PromoSuggestion) {
        self.items.retain(|existing| existing.key != suggestion.key);
        self.items.push(suggestion);
        self.items.sort_by_key(|s| s.priority);
    }

    /// Marks `key` as dismissed locally so it is no longer returned by [`Self::active`].
    pub fn mark_dismissed(&mut self, key: &str) {
        self.dismissed.insert(key.to_owned());
        self.items.retain(|s| s.key != key);
    }

    /// Returns the highest-priority non-dismissed suggestion, if any.
    #[must_use]
    pub fn active(&self) -> Option<&PromoSuggestion> {
        self.items.iter().find(|s| !self.dismissed.contains(&s.key))
    }

    /// Returns expiration timestamp from the last `help.getPromoData` response.
    #[must_use]
    pub const fn expires(&self) -> i32 {
        self.expires
    }

    /// Returns whether `key` has been dismissed.
    #[must_use]
    pub fn is_dismissed(&self, key: &str) -> bool {
        self.dismissed.contains(key)
    }
}

/// Maps well-known Telegram server suggestion keys from `help.promoData.pending_suggestions`
/// to structured [`PromoSuggestion`] entries.
#[must_use]
pub fn suggestion_from_server_key(key: &str, peer_id: Option<PeerId>) -> Option<PromoSuggestion> {
    let (priority, kind, title, description) = match key {
        "USERPIC_SETUP" => (
            SuggestionPriority::UserpicSetup,
            PromoSuggestionKind::UserpicSetup,
            "Add a Profile Photo",
            "Make it easier for friends to recognize you.",
        ),
        "PREMIUM_ANNUAL" | "PREMIUM_UPGRADE" | "PREMIUM_RESTORE" => (
            SuggestionPriority::PremiumOffer,
            PromoSuggestionKind::PremiumOffer,
            "Telegram Premium",
            "Unlock double limits, faster downloads, and more.",
        ),
        "BIRTHDAY_SETUP" => (
            SuggestionPriority::BirthdaySetup,
            PromoSuggestionKind::BirthdaySetup,
            "Add Your Birthday",
            "Let your contacts know when to congratulate you.",
        ),
        "BIRTHDAY_CONTACTS_TODAY" => (
            SuggestionPriority::BirthdayContacts,
            PromoSuggestionKind::BirthdayContacts,
            "Contact Birthday Today",
            "Send a gift or wish them a happy birthday.",
        ),
        "STARS_SUBSCRIPTION_LOW_BALANCE" => (
            SuggestionPriority::LowCreditsSubs,
            PromoSuggestionKind::LowCreditsSubs,
            "Low Telegram Stars Balance",
            "Top up your Stars to keep subscriptions active.",
        ),
        "PREMIUM_GRACE" => (
            SuggestionPriority::PremiumGrace,
            PromoSuggestionKind::PremiumGrace,
            "Premium Subscription Expiring",
            "Update your payment method to keep Premium.",
        ),
        "GIFT_AUCTIONS" => (
            SuggestionPriority::GiftAuctions,
            PromoSuggestionKind::GiftAuctions,
            "Active Gift Auctions",
            "Check your bids on limited Star Gifts.",
        ),
        "AUTOARCHIVE_POPULAR"
        | "VALIDATE_PASSWORD"
        | "VALIDATE_PHONE_NUMBER"
        | "UNREVIEWED_AUTH" => (
            SuggestionPriority::UnreviewedAuth,
            PromoSuggestionKind::UnreviewedAuth,
            "Security Check",
            "Review your account security and active logins.",
        ),
        _ => return None,
    };

    Some(PromoSuggestion {
        key: key.to_owned(),
        priority,
        kind,
        title: title.to_owned(),
        description: description.to_owned(),
        peer_id,
        dismissible: true,
    })
}

/// Renders the active top-bar promo suggestion banner above the dialogs list.
///
/// Mirrors `Dialogs::TopBarSuggestionContent` (`dialogs_top_bar_suggestion_content.cpp`).
pub fn render_top_bar_suggestion(
    ui: &mut Ui,
    state: &mut PromoSuggestionsState,
    palette: &Palette,
) -> Option<SuggestionAction> {
    let active = state.active()?.clone();
    let colors = DialogColors::new(palette);
    let avail_w = ui.available_width().max(160.0);
    let banner_h = 54.0;

    let (outer, _) = ui.allocate_exact_size(Vec2::new(avail_w, banner_h), Sense::hover());
    let card = outer.shrink2(Vec2::new(8.0, 4.0));

    let close_size = 24.0;
    let close_rect = Rect::from_min_size(
        Pos2::new(
            card.right() - close_size - 6.0,
            card.center().y - close_size * 0.5,
        ),
        Vec2::splat(close_size),
    );
    let body_rect = if active.dismissible {
        Rect::from_min_max(card.min, Pos2::new(close_rect.left() - 4.0, card.max.y))
    } else {
        card
    };

    let body_resp = ui.interact(
        body_rect,
        ui.scope_id().with(("promo_suggestion_body", &active.key)),
        Sense::click(),
    );
    body_resp
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, active.title.clone()));
    let close_resp = if active.dismissible {
        let resp = ui.interact(
            close_rect,
            ui.scope_id().with(("promo_suggestion_close", &active.key)),
            Sense::click(),
        );
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                format!("Dismiss {}", active.title),
            )
        });
        Some(resp)
    } else {
        None
    };

    let bg = if body_resp.hovered() {
        colors.color("windowBgOver")
    } else {
        colors.color("windowBgRipple")
    };
    ui.painter().rect_filled(card, CornerRadius::same(8), bg);
    ui.painter().rect_stroke(
        card,
        CornerRadius::same(8),
        Stroke::new(1.0, colors.color("shadowFg")),
        egui::StrokeKind::Inside,
    );

    if body_resp.hovered() || close_resp.as_ref().is_some_and(egui::Response::hovered) {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    ui.painter().text(
        Pos2::new(card.left() + 12.0, card.top() + 15.0),
        Align2::LEFT_CENTER,
        &active.title,
        font_id_from_style(st::semibold_font),
        colors.color("windowFg"),
    );
    ui.painter().text(
        Pos2::new(card.left() + 12.0, card.top() + 33.0),
        Align2::LEFT_CENTER,
        &active.description,
        font_id_from_style(st::normal_font),
        colors.sub_text_fg(),
    );

    if let Some(close_r) = close_resp {
        paint_style_icon(
            ui.painter(),
            close_rect,
            st::box_title_close_icon,
            &colors,
            Some(if close_r.hovered() {
                colors.color("windowFg")
            } else {
                colors.sub_text_fg()
            }),
        );
        if close_r.clicked() {
            state.mark_dismissed(&active.key);
            return Some(SuggestionAction::Dismiss {
                key: active.key,
                peer_id: active.peer_id,
            });
        }
    }

    if body_resp.clicked() {
        return Some(SuggestionAction::Activate(active));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_ordering_and_dismissal_selection() {
        let mut state = PromoSuggestionsState::new();
        state.apply_snapshot(PromoDataSnapshot {
            expires: 1_900_000_000,
            pending_suggestions: vec![
                "GIFT_AUCTIONS".to_owned(),
                "BIRTHDAY_SETUP".to_owned(),
                "USERPIC_SETUP".to_owned(),
            ],
            dismissed_suggestions: vec!["USERPIC_SETUP".to_owned()],
            custom_suggestion: Some(PromoSuggestion {
                key: "CUSTOM_PROMO_1".to_owned(),
                priority: SuggestionPriority::CustomPromo,
                kind: PromoSuggestionKind::CustomPromo {
                    url: "https://t.me/telegram".to_owned(),
                },
                title: "Telegram Update".to_owned(),
                description: "Explore new features".to_owned(),
                peer_id: None,
                dismissible: true,
            }),
            promo_peer: None,
            psa_type: None,
            psa_message: None,
        });

        // USERPIC_SETUP was dismissed, so BIRTHDAY_SETUP (priority 3) wins over CustomPromo (7) and GiftAuctions (8).
        assert_eq!(state.active().unwrap().key, "BIRTHDAY_SETUP");

        state.mark_dismissed("BIRTHDAY_SETUP");
        assert_eq!(state.active().unwrap().key, "CUSTOM_PROMO_1");

        state.mark_dismissed("CUSTOM_PROMO_1");
        assert_eq!(state.active().unwrap().key, "GIFT_AUCTIONS");

        state.mark_dismissed("GIFT_AUCTIONS");
        assert!(state.active().is_none());
    }
}
