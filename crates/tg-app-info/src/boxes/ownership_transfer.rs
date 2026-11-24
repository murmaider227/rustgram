//! Channel / Supergroup Ownership Transfer dialog with 2FA SRP verification.
//!
//! Port origin: `Telegram/SourceFiles/boxes/peers/channel_ownership_transfer.cpp`.

use egui::{FontId, Ui};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::FlatLabel;

use crate::info::backend::{InfoBackend, OwnershipTransferCheck, OwnershipTransferError};
use tg_app_core::stand_in::types::PeerId;

/// Multi-step state machine matching `ChannelOwnershipTransfer` (`channel_ownership_transfer.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipTransferStep {
    /// Initial confirmation warning before requesting the 2FA password.
    Confirm,
    /// Blocked by a pre-flight requirement (`PASSWORD_MISSING`, `PASSWORD_TOO_FRESH`, `SESSION_TOO_FRESH`).
    Blocked {
        /// Structured reason.
        reason: OwnershipTransferError,
        /// Human-readable explanation.
        message: String,
    },
    /// Prompting for the account's 2FA cloud password to compute the SRP check.
    EnterPassword,
    /// Transfer succeeded.
    Completed,
}

/// Modal state for transferring ownership of a group or channel (`OwnershipTransferBox`).
#[derive(Debug, Clone)]
pub struct OwnershipTransferBoxState {
    /// Target group or channel peer id.
    pub peer_id: PeerId,
    /// Target group or channel title.
    pub peer_title: String,
    /// `true` for broadcast channel, `false` for group/supergroup.
    pub is_channel: bool,
    /// Selected successor administrator peer id.
    pub new_owner_id: PeerId,
    /// Selected successor administrator display name.
    pub new_owner_name: String,
    /// Active workflow step.
    pub step: OwnershipTransferStep,
    /// Whether the pre-flight check (`check_ownership_transfer`) has run.
    pub precheck_done: bool,
    /// Optional 2FA password hint returned by `account.getPassword`.
    pub password_hint: String,
    /// User-entered 2FA cloud password.
    pub password: String,
    /// Inline error text (wrong password, server limit error, etc.).
    pub error: Option<String>,
}

impl OwnershipTransferBoxState {
    /// Creates an [`OwnershipTransferBoxState`] for transferring `peer_id` to `new_owner_id`.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        is_channel: bool,
        new_owner_id: PeerId,
        new_owner_name: impl Into<String>,
    ) -> Self {
        Self {
            peer_id,
            peer_title: peer_title.into(),
            is_channel,
            new_owner_id,
            new_owner_name: new_owner_name.into(),
            step: OwnershipTransferStep::Confirm,
            precheck_done: false,
            password_hint: String::new(),
            password: String::new(),
            error: None,
        }
    }

    /// Runs the pre-flight eligibility check (`messages.editChatCreator` empty check +
    /// `account.getPassword`).
    pub fn run_precheck(&mut self, backend: &dyn InfoBackend) {
        self.precheck_done = true;
        match backend.check_ownership_transfer(self.peer_id) {
            Ok(OwnershipTransferCheck::Ready { password_hint }) => {
                self.password_hint = password_hint;
                self.step = OwnershipTransferStep::Confirm;
                self.error = None;
            }
            Ok(OwnershipTransferCheck::Blocked { reason, message }) => {
                self.step = OwnershipTransferStep::Blocked { reason, message };
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
    }

    /// Advances from the `Confirm` warning step to the `EnterPassword` SRP step.
    pub fn proceed_to_password(&mut self) {
        if self.step == OwnershipTransferStep::Confirm {
            self.step = OwnershipTransferStep::EnterPassword;
            self.error = None;
        }
    }

    /// Executes the 2FA SRP ownership transfer via `backend.transfer_ownership`.
    pub fn submit_password(
        &mut self,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), OwnershipTransferError> {
        if self.password.is_empty() {
            let err = OwnershipTransferError::WrongPassword;
            self.error = Some("Enter your Two-Step Verification password.".to_owned());
            return Err(err);
        }
        match backend.transfer_ownership(self.peer_id, self.new_owner_id, &self.password) {
            Ok(()) => {
                self.password.clear();
                self.error = None;
                self.step = OwnershipTransferStep::Completed;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.display_message());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor for the active step.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        match &self.step {
            OwnershipTransferStep::Confirm => {
                let title = if self.is_channel {
                    "Transfer Channel Ownership"
                } else {
                    "Transfer Group Ownership"
                };
                BoxModal::new(id, title)
                    .width(st::box_width.0 as f32 + 40.0)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::attention("transfer_confirm", "Change Owner"))
            }
            OwnershipTransferStep::Blocked { .. } => BoxModal::new(id, "Security Check Required")
                .width(st::box_width.0 as f32 + 40.0)
                .add_button(BoxButton::default_btn("close", tr::lng_close())),
            OwnershipTransferStep::EnterPassword => BoxModal::new(id, "Enter Your Password")
                .width(st::box_width.0 as f32 + 40.0)
                .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                .add_button(BoxButton::attention("transfer_submit", "Submit")),
            OwnershipTransferStep::Completed => BoxModal::new(id, "Ownership Transferred")
                .width(st::box_width.0 as f32 + 40.0)
                .add_button(BoxButton::default_btn("close", tr::lng_close())),
        }
    }

    /// Renders the active ownership-transfer step body.
    pub fn show_body(&mut self, ui: &mut Ui, backend: &dyn InfoBackend, colors: &dyn Colors) {
        if !self.precheck_done {
            self.run_precheck(backend);
        }

        let entity_noun = if self.is_channel { "channel" } else { "group" };

        match &self.step {
            OwnershipTransferStep::Confirm => {
                FlatLabel::plain(
                    &format!(
                        "This will transfer full creator rights for \"{}\" to {}. You will remain an administrator, but you will no longer be able to undo this action.",
                        self.peer_title, self.new_owner_name
                    ),
                    FlatLabelStyle::default_label(colors),
                )
                .id_salt("transfer_confirm_text")
                .show(ui);
            }
            OwnershipTransferStep::Blocked { message, .. } => {
                FlatLabel::plain(message, FlatLabelStyle::default_label(colors))
                    .id_salt("transfer_blocked_text")
                    .show(ui);
            }
            OwnershipTransferStep::EnterPassword => {
                FlatLabel::plain(
                    &format!(
                        "Enter your Two-Step Verification password to transfer ownership of this {entity_noun} to {}.",
                        self.new_owner_name
                    ),
                    FlatLabelStyle::default_label(colors),
                )
                .id_salt("transfer_pwd_desc")
                .show(ui);
                ui.add_space(10.0);

                let placeholder = if self.password_hint.is_empty() {
                    "Two-Step Verification password".to_owned()
                } else {
                    format!("Password (hint: {})", self.password_hint)
                };
                InputField::single_line(
                    "transfer_2fa_password",
                    &mut self.password,
                    &placeholder,
                    InputFieldStyle::default_input(colors),
                )
                .password(true)
                .error(self.error.is_some())
                .show(ui);

                if !self.password_hint.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Hint: {}", self.password_hint))
                            .font(FontId::new(12.0, family_regular()))
                            .color(colors.color("windowSubTextFg")),
                    );
                }
            }
            OwnershipTransferStep::Completed => {
                FlatLabel::plain(
                    &format!(
                        "{} is now the owner of \"{}\".",
                        self.new_owner_name, self.peer_title
                    ),
                    FlatLabelStyle::default_label(colors),
                )
                .id_salt("transfer_done_text")
                .show(ui);
            }
        }

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}
