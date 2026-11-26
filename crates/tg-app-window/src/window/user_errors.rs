//! Shows a failed request the way tdesktop does at the call site (TGB-2957, TGB-2978): a toast,
//! an inform box (with links when the text has them), the Premium screen, or nothing. The
//! decision itself lives in [`tg_app_core::user_error`].

use tg_app_core::user_error::{LimitBoxKind, UserFacingError, limit_box_content};
use tg_app_info::payments::PaymentsScreen;

use super::WindowController;
use crate::window::deep_links::{DeepLinkOutcome, DeepLinkOverlayRequest};
use crate::window::layer_stack::ModalBox;

/// The inform box for [`UserFacingError::Inform`].
#[must_use]
pub fn inform_box(title: Option<String>, text: String) -> ModalBox {
    ModalBox::Inform {
        title: title.unwrap_or_default(),
        body: text,
    }
}

/// The limit box `kind` for a free or Premium account.
#[must_use]
pub fn limit_box(kind: LimitBoxKind, is_premium: bool) -> ModalBox {
    let content = limit_box_content(kind, is_premium);
    ModalBox::PremiumLimit {
        title: content.title,
        body: content.text,
        increase_ref: content.increase_ref,
    }
}

/// `Settings::ShowPremium(session, ref)`: the Premium screen with its `ref`.
#[must_use]
pub fn premium_screen(referral: String) -> DeepLinkOverlayRequest {
    DeepLinkOverlayRequest::OpenPayments {
        screen: PaymentsScreen::Premium,
        slug: Some(referral),
    }
}

impl DeepLinkOutcome {
    /// The outcome of a deep link whose request failed.
    #[must_use]
    pub fn from_user_error(error: UserFacingError) -> Self {
        match error {
            UserFacingError::Toast(text) => Self::Toast(text),
            UserFacingError::Inform { title, text } => Self::ShowModal(inform_box(title, text)),
            UserFacingError::InformRich { title, text } => Self::ShowModal(ModalBox::InformRich {
                title: title.unwrap_or_default(),
                body: text,
            }),
            UserFacingError::PremiumScreen { referral } => {
                Self::OpenOverlay(premium_screen(referral))
            }
            error @ UserFacingError::LimitBox(_) => Self::UserError(error),
            UserFacingError::Silent => Self::Handled,
        }
    }
}

impl WindowController {
    /// Shows `error` as a toast, an inform box or the Premium screen;
    /// [`UserFacingError::Silent`] shows nothing.
    pub fn show_user_error(&mut self, error: UserFacingError) {
        match error {
            UserFacingError::Toast(text) => self.layer_stack.show_toast(text),
            UserFacingError::Inform { title, text } => {
                self.layer_stack.show_box(inform_box(title, text));
            }
            UserFacingError::InformRich { title, text } => {
                self.layer_stack.show_box(ModalBox::InformRich {
                    title: title.unwrap_or_default(),
                    body: text,
                });
            }
            UserFacingError::LimitBox(kind) => {
                let is_premium = tg_app_core::chat_helpers::backend::ChatHelpersBackend::is_premium(
                    &*self.session_backend,
                );
                self.layer_stack.show_box(limit_box(kind, is_premium));
            }
            UserFacingError::PremiumScreen { referral } => {
                self.pending_overlay_requests.push(premium_screen(referral));
            }
            UserFacingError::Silent => {}
        }
    }
}
