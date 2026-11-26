//! Which tdesktop failure handler a payments deep link maps to (TGB-2977).

use tg_app_core::user_error::PaymentsLinkKind;
use tg_app_info::payments::PaymentsScreen;

/// The [`PaymentsLinkKind`] of the payments screen a deep link opened.
#[must_use]
pub const fn payments_link_kind(screen: PaymentsScreen) -> PaymentsLinkKind {
    match screen {
        PaymentsScreen::Checkout => PaymentsLinkKind::Invoice,
        PaymentsScreen::Passport => PaymentsLinkKind::Passport,
        PaymentsScreen::StarGifts => PaymentsLinkKind::UniqueGift,
        _ => PaymentsLinkKind::Other,
    }
}

/// The forced-logout box text with the server's entities (`Application::forceLogOut` gets a
/// `TextWithEntities`, `api/api_updates.cpp:2370-2377`).
#[must_use]
pub fn forced_logout_text(
    message: String,
    entities: Vec<tg_data::TextEntity>,
) -> tg_ui::text::RichText {
    let data = tg_data::TextWithEntities {
        text: message,
        entities,
    };
    tg_ui::text::RichText::from_text_with_entities(&tg_app_live::convert::data_text_to_ui_text(
        &data,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_logout_text_keeps_the_server_links() {
        let message = "Your session was terminated. Details: telegram.org/faq".to_owned();
        let at = message.find("telegram.org").unwrap_or(0) as i32;
        let entities = vec![tg_data::TextEntity::new(
            at,
            16,
            tg_data::EntityKind::TextUrl {
                url: "https://telegram.org/faq".to_owned(),
            },
        )];
        let rich = forced_logout_text(message.clone(), entities);
        assert_eq!(rich.text, message);
        assert_eq!(
            rich.spans
                .iter()
                .map(|s| (&rich.text[s.start..s.end], s.kind.clone()))
                .collect::<Vec<_>>(),
            vec![(
                "telegram.org/faq",
                tg_ui::text::TextEntityKind::Link("https://telegram.org/faq".to_owned())
            )]
        );
    }

    #[test]
    fn invoice_and_gift_links_use_their_handlers() {
        assert_eq!(
            payments_link_kind(PaymentsScreen::Checkout),
            PaymentsLinkKind::Invoice
        );
        assert_eq!(
            payments_link_kind(PaymentsScreen::StarGifts),
            PaymentsLinkKind::UniqueGift
        );
        assert_eq!(
            payments_link_kind(PaymentsScreen::Premium),
            PaymentsLinkKind::Other
        );
    }
}
