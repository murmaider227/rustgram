//! Request failures in the chat that tdesktop shows as a box or a screen, not a toast
//! (TGB-2977, TGB-2978). The decision lives in [`tg_app_core::user_error`]; a toast stays in the
//! chat's own toast, everything else goes up to the window through [`HistoryWidget::take_user_errors`].

use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::{PeerId, PeerKind};
use tg_app_core::user_error::{self, SendTarget, UserFacingError};

use super::HistoryWidget;

/// The [`SendTarget`] of a chat kind (`peer->isBroadcast()` / `isUser()` in `sendMessageFail`).
#[must_use]
pub const fn send_target(kind: PeerKind) -> SendTarget {
    match kind {
        PeerKind::User => SendTarget::User,
        PeerKind::Group => SendTarget::Group,
        PeerKind::Channel => SendTarget::Channel,
    }
}

impl HistoryWidget {
    /// Shows a failed request: a toast in the chat, or a box/screen queued for the window.
    pub(crate) fn show_user_error(&mut self, error: UserFacingError, now_secs: f64) {
        match error {
            UserFacingError::Toast(text) => self.show_toast(text, now_secs),
            UserFacingError::Silent => {}
            other => self.pending_user_errors.push(other),
        }
    }

    /// A message send failed (`ApiWrap::sendMessageFail`, `apiwrap.cpp:561-663`).
    pub(crate) fn show_send_failure(&mut self, error: &str, kind: PeerKind, now_secs: f64) {
        self.show_user_error(user_error::send_message(error, send_target(kind)), now_secs);
    }

    /// A send to `peer_id` failed synchronously: the chat kind comes from `backend`.
    pub(crate) fn show_send_failure_to(
        &mut self,
        error: &str,
        peer_id: PeerId,
        backend: &dyn HistoryBackend,
        now_secs: f64,
    ) {
        let kind = backend
            .peer_info(peer_id)
            .map_or(PeerKind::User, |info| info.summary.kind);
        self.show_send_failure(error, kind, now_secs);
    }

    /// Drains the failures that need a box or a screen (inform boxes, the Premium screen).
    pub fn take_user_errors(&mut self) -> Vec<UserFacingError> {
        std::mem::take(&mut self.pending_user_errors)
    }
}
