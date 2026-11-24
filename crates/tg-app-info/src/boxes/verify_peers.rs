//! Bot custom peer verification box (`bots.setCustomVerification`).
//!
//! Port origin: `Telegram/SourceFiles/boxes/peers/verify_peers_box.h`
//! and `Telegram/SourceFiles/boxes/peers/verify_peers_box.cpp`.

use egui::{
    Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::info::backend::{InfoBackend, InfoPeerKind, PeerProfile};
use tg_app_core::stand_in::types::PeerId;

/// Default custom verification description limit matching `verify_peers_box.cpp:141`.
pub const BOT_VERIFY_DESCRIPTION_LIMIT: usize = 70;

/// Active view inside [`VerifyPeersBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyPeersStep {
    /// Searchable list of verifiable users, bots, groups, and channels.
    List,
    /// Confirmation screen for granting custom verification to a peer.
    ConfirmAdd {
        /// Target peer ID.
        peer_id: PeerId,
        /// Target peer title.
        peer_title: String,
        /// Target peer category.
        peer_kind: InfoPeerKind,
        /// Editable custom verification description (`<= 70` chars).
        description: String,
    },
    /// Confirmation screen for revoking custom verification from a peer.
    ConfirmRemove {
        /// Target peer ID.
        peer_id: PeerId,
        /// Target peer title.
        peer_title: String,
    },
}

/// Actions emitted by [`VerifyPeersBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyPeersAction {
    /// Modal buttons changed (e.g. switched between List and Confirm steps).
    RefreshModal,
}

/// Stateful `VerifyPeersBox` modal allowing an authorized verifier bot to grant or revoke
/// third-party custom verification badges (`bots.setCustomVerification`).
#[derive(Debug, Clone)]
pub struct VerifyPeersBoxState {
    /// Verifier bot peer ID.
    pub bot_id: PeerId,
    /// Verifier bot display name.
    pub bot_title: String,
    /// Whether the verifier bot permits customizing the description per peer.
    pub can_modify_description: bool,
    /// Default verification description template.
    pub default_description: String,
    /// Search query filtering the peer list.
    pub search_query: String,
    /// Active step (List / ConfirmAdd / ConfirmRemove).
    pub step: VerifyPeersStep,
    /// Optional inline validation or RPC error.
    pub error: Option<String>,
}

impl VerifyPeersBoxState {
    /// Creates a new [`VerifyPeersBoxState`] anchored on verifier `bot_id`.
    #[must_use]
    pub fn new(bot_id: PeerId, bot_title: impl Into<String>) -> Self {
        Self {
            bot_id,
            bot_title: bot_title.into(),
            can_modify_description: true,
            default_description: String::new(),
            search_query: String::new(),
            step: VerifyPeersStep::List,
            error: None,
        }
    }

    /// Returns whether `peer` is eligible for bot verification (`verify_peers_box.cpp:282`).
    #[must_use]
    pub fn is_verifiable_peer(peer: &PeerProfile) -> bool {
        !matches!(peer.kind, InfoPeerKind::SavedMessages)
    }

    /// Opens the add/remove confirmation step for `peer`.
    pub fn select_peer(&mut self, peer: &PeerProfile, backend: &dyn InfoBackend) {
        self.error = None;
        let status = backend.peer_verification_status(self.bot_id, peer.id);
        if status.verified {
            self.step = VerifyPeersStep::ConfirmRemove {
                peer_id: peer.id,
                peer_title: peer.title.clone(),
            };
        } else {
            self.step = VerifyPeersStep::ConfirmAdd {
                peer_id: peer.id,
                peer_title: peer.title.clone(),
                peer_kind: peer.kind,
                description: self.default_description.clone(),
            };
        }
    }

    /// Returns to the peer list view.
    pub fn back_to_list(&mut self) {
        self.step = VerifyPeersStep::List;
        self.error = None;
    }

    /// Confirms the active add or remove operation via `bots.setCustomVerification`.
    pub fn submit_current(&mut self, backend: &mut dyn InfoBackend) -> Result<String, String> {
        match self.step.clone() {
            VerifyPeersStep::ConfirmAdd {
                peer_id,
                peer_title,
                description,
                ..
            } => {
                let trimmed = description.trim();
                if self.can_modify_description
                    && trimmed.chars().count() > BOT_VERIFY_DESCRIPTION_LIMIT
                {
                    let msg = format!(
                        "Description must be at most {BOT_VERIFY_DESCRIPTION_LIMIT} characters."
                    );
                    self.error = Some(msg.clone());
                    return Err(msg);
                }
                let desc_opt = if self.can_modify_description && !trimmed.is_empty() {
                    Some(trimmed)
                } else {
                    None
                };
                backend
                    .set_peer_custom_verification(self.bot_id, peer_id, true, desc_opt)
                    .inspect_err(|e| {
                        self.error = Some(e.clone());
                    })?;
                self.step = VerifyPeersStep::List;
                self.error = None;
                Ok(format!("Verified {peer_title}"))
            }
            VerifyPeersStep::ConfirmRemove {
                peer_id,
                peer_title,
            } => {
                backend
                    .set_peer_custom_verification(self.bot_id, peer_id, false, None)
                    .inspect_err(|e| {
                        self.error = Some(e.clone());
                    })?;
                self.step = VerifyPeersStep::List;
                self.error = None;
                Ok(format!("Removed verification for {peer_title}"))
            }
            VerifyPeersStep::List => Ok(String::new()),
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor for the current step.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        match &self.step {
            VerifyPeersStep::List => BoxModal::new(id, "Verify Peers")
                .width(st::box_wide_width.0 as f32)
                .add_button(BoxButton::default_btn("close", tr::lng_box_done())),
            VerifyPeersStep::ConfirmAdd { peer_kind, .. } => {
                let title = match peer_kind {
                    InfoPeerKind::Channel => "Verify Channel",
                    InfoPeerKind::Group | InfoPeerKind::Supergroup => "Verify Group",
                    InfoPeerKind::Bot => "Verify Bot",
                    _ => "Verify User",
                };
                BoxModal::new(id, title)
                    .width(st::box_wide_width.0 as f32)
                    .with_left_button(BoxButton::default_btn("verify_back", "Back"))
                    .add_button(BoxButton::default_btn("verify_submit", "Verify"))
            }
            VerifyPeersStep::ConfirmRemove { .. } => BoxModal::new(id, "Remove Verification")
                .width(st::box_wide_width.0 as f32)
                .with_left_button(BoxButton::default_btn("verify_back", "Back"))
                .add_button(BoxButton::attention("verify_submit", "Remove")),
        }
    }

    /// Renders the peer picker or confirm-add/remove view.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        all_peers: &[PeerProfile],
        backend: &dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<VerifyPeersAction> {
        let mut action = None;
        match &mut self.step {
            VerifyPeersStep::List => {
                FlatLabel::plain(
                    &format!("Verifier: {}", self.bot_title),
                    FlatLabelStyle::sub_text(colors),
                )
                .id_salt("verify_bot_caption")
                .show(ui);
                ui.add_space(6.0);

                InputField::single_line(
                    "verify_peers_search",
                    &mut self.search_query,
                    "Search users, groups, or channels",
                    InputFieldStyle::default_input(colors),
                )
                .show(ui);
                ui.add_space(8.0);

                let q = self.search_query.trim().to_lowercase();
                let mut clicked_peer: Option<PeerProfile> = None;
                let mut shown = 0usize;
                for peer in all_peers.iter().filter(|p| Self::is_verifiable_peer(p)) {
                    if !q.is_empty()
                        && !peer.title.to_lowercase().contains(&q)
                        && !peer
                            .primary_username()
                            .is_some_and(|u| u.to_lowercase().contains(&q))
                    {
                        continue;
                    }
                    shown += 1;
                    let status = backend.peer_verification_status(self.bot_id, peer.id);
                    let width = ui.available_width().max(220.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 48.0), Sense::click());
                    resp.widget_info(|| {
                        WidgetInfo::labeled(
                            Role::Button,
                            true,
                            format!(
                                "{}{}",
                                peer.title,
                                if status.verified { " (Verified)" } else { "" }
                            ),
                        )
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            colors.color("windowBgOver"),
                        );
                    }
                    let av_rect = Rect::from_center_size(
                        Pos2::new(rect.left() + 24.0, rect.center().y),
                        Vec2::splat(34.0),
                    );
                    EmptyUserpic::for_color_index(peer.color_index, &peer.title, colors)
                        .paint_circle(ui.painter(), av_rect);

                    let text_x = av_rect.right() + 10.0;
                    ui.painter().text(
                        Pos2::new(text_x, rect.top() + 8.0),
                        Align2::LEFT_TOP,
                        &peer.title,
                        font_id_from_style(st::semibold_font),
                        colors.color("windowFg"),
                    );
                    let subtitle = if status.verified {
                        if status.custom_description.is_empty() {
                            "Verified by this bot".to_owned()
                        } else {
                            format!("Verified · {}", status.custom_description)
                        }
                    } else {
                        peer.primary_username()
                            .map(|u| format!("@{u}"))
                            .unwrap_or_else(|| peer.status.clone())
                    };
                    ui.painter().text(
                        Pos2::new(text_x, rect.top() + 27.0),
                        Align2::LEFT_TOP,
                        subtitle,
                        FontId::new(11.5, family_regular()),
                        if status.verified {
                            colors.color("windowActiveTextFg")
                        } else {
                            colors.color("windowSubTextFg")
                        },
                    );
                    if status.verified {
                        let verified_fg = colors.color("windowActiveTextFg");
                        let galley = ui.painter().layout_no_wrap(
                            "Verified".to_owned(),
                            FontId::new(11.5, family_semibold()),
                            verified_fg,
                        );
                        let text_x = rect.right() - 10.0 - galley.size().x;
                        tg_app_core::ui_icons::paint_select_check(
                            ui.painter(),
                            Pos2::new(text_x - 10.0, rect.center().y),
                            13.0,
                            verified_fg,
                        );
                        ui.painter().galley(
                            Pos2::new(text_x, rect.center().y - galley.size().y * 0.5),
                            galley,
                            verified_fg,
                        );
                    }
                    if resp.clicked() {
                        clicked_peer = Some(peer.clone());
                    }
                }
                if shown == 0 {
                    ui.label(
                        egui::RichText::new("No matching peers found.")
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowSubTextFg")),
                    );
                }
                if let Some(peer) = clicked_peer {
                    self.select_peer(&peer, backend);
                    action = Some(VerifyPeersAction::RefreshModal);
                }
            }
            VerifyPeersStep::ConfirmAdd {
                peer_title,
                description,
                ..
            } => {
                ui.label(
                    egui::RichText::new(format!(
                        "Assign custom verification badge to {peer_title}?"
                    ))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowFg")),
                );
                if self.can_modify_description {
                    ui.add_space(10.0);
                    divider_bar(ui, 6.0, colors);
                    ui.add_space(6.0);
                    let len = description.chars().count();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Custom Description (optional)")
                                .font(FontId::new(12.5, family_semibold()))
                                .color(colors.color("windowSubTextFg")),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{len}/{BOT_VERIFY_DESCRIPTION_LIMIT}"
                                ))
                                .font(FontId::new(11.5, family_regular()))
                                .color(
                                    if len > BOT_VERIFY_DESCRIPTION_LIMIT {
                                        colors.color("boxTextFgError")
                                    } else {
                                        colors.color("windowSubTextFg")
                                    },
                                ),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    InputField::single_line(
                        "verify_peer_description_input",
                        description,
                        "Shown on the verified profile",
                        InputFieldStyle::default_input(colors),
                    )
                    .error(len > BOT_VERIFY_DESCRIPTION_LIMIT)
                    .show(ui);
                }
            }
            VerifyPeersStep::ConfirmRemove { peer_title, .. } => {
                ui.label(
                    egui::RichText::new(format!(
                        "Remove custom verification from {peer_title}? The verification badge and custom description will no longer appear on their profile."
                    ))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowFg")),
                );
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

        action
    }
}
