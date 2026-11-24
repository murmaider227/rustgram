//! Generic confirmation modal box.
//!
//! Port origin: `Telegram/SourceFiles/ui/boxes/confirm_box.h`
//! and `Telegram/SourceFiles/ui/boxes/confirm_box.cpp`.

use egui::Ui;
use tg_lang::tr;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::FlatLabel;

pub use crate::boxes::confirm_phone::{
    ConfirmPhoneBodyAction, ConfirmPhoneCodeBoxState, format_display_phone,
};

/// Semantic action tag carried by a [`ConfirmBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmTag {
    /// Generic confirmation with caller-chosen key.
    Custom(String),
    /// Confirmed declining Terms of Service.
    DeclineTerms,
    /// Confirmed leaving or deleting a peer.
    LeavePeer(tg_app_core::stand_in::types::PeerId),
    /// Confirmed adding and connecting to a proxy from a `tg://proxy` / `t.me/socks` link.
    ApplyProxy(tg_core::ProxyData),
    /// Confirmed joining a shareable chat folder (`tg://addlist?slug=...`).
    JoinChatlist {
        /// Folder invite slug.
        slug: String,
        /// Target peers to add to the folder.
        peers: Vec<tg_app_core::stand_in::types::PeerId>,
    },
    /// Confirmed applying a language pack from `tg://setlanguage?lang=...`.
    ApplyLanguage(String),
    /// Confirmed redeeming a Telegram Premium gift code (`payments.applyGiftCode`).
    ApplyGiftCode {
        /// Gift code slug.
        slug: String,
        /// Subscription duration in days.
        days: i32,
    },
}

/// State for a reusable confirmation modal (`Ui::ConfirmBox`).
#[derive(Debug, Clone)]
pub struct ConfirmBoxState {
    /// Modal title.
    pub title: String,
    /// Body message.
    pub body: String,
    /// Label of the primary confirmation button (e.g. `"Confirm"`, `"Delete"`, `"Leave"`).
    pub confirm_label: String,
    /// Label of the cancel button (e.g. `"Cancel"`).
    pub cancel_label: String,
    /// Whether the confirmation button should use red destructive styling (`attentionBoxButton`).
    pub is_attention: bool,
    /// Optional checkbox label (e.g. `"Also block user"`).
    pub checkbox_label: Option<String>,
    /// Current state of the optional checkbox.
    pub checkbox_checked: bool,
    /// Tag identifying what to execute on confirmation.
    pub tag: ConfirmTag,
}

impl ConfirmBoxState {
    /// Creates a standard confirmation dialog.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        confirm_label: impl Into<String>,
        tag: ConfirmTag,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            confirm_label: confirm_label.into(),
            cancel_label: tr::lng_cancel(),
            is_attention: false,
            checkbox_label: None,
            checkbox_checked: false,
            tag,
        }
    }

    /// Marks the confirm action as destructive (`attentionBoxButton`).
    #[must_use]
    pub const fn attention(mut self, attention: bool) -> Self {
        self.is_attention = attention;
        self
    }

    /// Adds an optional confirmation checkbox.
    #[must_use]
    pub fn with_checkbox(mut self, label: impl Into<String>, initial: bool) -> Self {
        self.checkbox_label = Some(label.into());
        self.checkbox_checked = initial;
        self
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor for this confirmation box.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let confirm_btn = if self.is_attention {
            BoxButton::attention("confirm", &self.confirm_label)
        } else {
            BoxButton::default_btn("confirm", &self.confirm_label)
        };
        BoxModal::new(id, &self.title)
            .width(st::box_width.0 as f32 + 20.0)
            .add_button(BoxButton::default_btn("cancel", &self.cancel_label))
            .add_button(confirm_btn)
    }

    /// Renders the modal body inside `ui`.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        FlatLabel::plain(&self.body, FlatLabelStyle::box_label(colors))
            .id_salt("confirm_body")
            .show(ui);

        if let Some(ref label) = self.checkbox_label {
            ui.add_space(12.0);
            Checkbox::new(
                &mut self.checkbox_checked,
                label,
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);
        }
    }
}
