//! Server-forced logout box (`AUTH_KEY_DROP_*`).
//!
//! Port of `Application::forceLogOut` (`core/application.cpp:1080-1096`): the server's
//! explanation in an inform box with one "Log out" button that Escape and an outside click do
//! not close; the account logs out when the box closes (`Account::forcedLogOut`,
//! `main/main_account.cpp:548-553`).

use super::WindowController;
use crate::window::layer_stack::ModalBox;

impl WindowController {
    /// Shows the forced-logout box with the server's `text`, once: a second drop notice while
    /// the box is open changes nothing.
    pub fn show_forced_logout_box(&mut self, text: String) {
        self.show_forced_logout_box_rich(tg_ui::text::RichText::plain(text));
    }

    /// [`Self::show_forced_logout_box`] with the server's entities: its links stay clickable
    /// (`Ui::MakeInformBox(TextWithEntities)` in `Application::forceLogOut`).
    pub fn show_forced_logout_box_rich(&mut self, text: tg_ui::text::RichText) {
        if !self.layer_stack.is_forced_logout_shown_for(None) {
            self.layer_stack.show_box(ModalBox::ForcedLogout {
                text,
                account: None,
            });
        }
    }

    /// The forced-logout box for a background account (`slot`): tdesktop shows it in the
    /// window whatever account the server dropped (`Ui::show` in `Application::forceLogOut`),
    /// and that account logs out when it closes ([`Self::take_background_logouts`]).
    pub fn show_background_forced_logout_box(&mut self, slot: i32, text: tg_ui::text::RichText) {
        if !self.layer_stack.is_forced_logout_shown_for(Some(slot)) {
            self.layer_stack.show_box(ModalBox::ForcedLogout {
                text,
                account: Some(slot),
            });
        }
    }

    /// Drains the background accounts whose forced-logout box was closed.
    pub fn take_background_logouts(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.background_logouts)
    }

    /// Returns `true` while the forced-logout box waits for the user; the account stays signed
    /// in under it until the box closes.
    #[must_use]
    pub fn is_forced_logout_shown(&self) -> bool {
        self.layer_stack.is_forced_logout_shown()
    }

    pub(super) fn finish_forced_logout(&mut self, account: Option<i32>) {
        match account {
            Some(slot) => self.background_logouts.push(slot),
            None => {
                self.media_overlay.close();
                self.logout_requested = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WindowController;
    use crate::mock_session::MockSession;
    use crate::window::layer_stack::{LayerAction, LayerStack, ModalBox};
    use tg_app_core::stand_in::settings::ShellSettings;

    #[test]
    fn forced_logout_shows_server_text_and_logs_out_only_when_closed() {
        let mut ctrl = WindowController::with_session(
            MockSession::new(true),
            ShellSettings::default(),
            std::env::temp_dir().join("rustgram_forced_logout_test.json"),
        );
        ctrl.show_forced_logout_box("Your session was terminated.".to_owned());
        ctrl.show_forced_logout_box("second notice".to_owned());
        assert!(ctrl.is_forced_logout_shown());
        assert_eq!(
            ctrl.layer_stack.toast_text(),
            None,
            "no toast with the raw type"
        );
        assert!(
            !ctrl.take_logout_requested(),
            "still signed in while the box is up"
        );
        assert!(!ctrl.layer_stack.close_top_by_back());
        ctrl.handle_layer_action(LayerAction::ForcedLogout(None));
        assert!(ctrl.take_logout_requested());
    }

    #[test]
    fn forced_logout_box_survives_back_and_hide_all() {
        let mut stack = LayerStack::new();
        stack.show_box(ModalBox::ForcedLogout {
            text: tg_ui::text::RichText::plain("Your session was terminated."),
            account: None,
        });
        assert!(!stack.close_top_by_back(), "Escape/back must not close it");
        stack.hide_all();
        assert!(stack.is_forced_logout_shown());
        assert!(
            !ModalBox::ForcedLogout {
                text: tg_ui::text::RichText::default(),
                account: None,
            }
            .closes_by_escape()
        );
        assert!(
            ModalBox::Inform {
                title: String::new(),
                body: String::new(),
            }
            .closes_by_escape()
        );
    }
}
