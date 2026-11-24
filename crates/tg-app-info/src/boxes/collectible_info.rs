//! Fragment Collectible Info modal box (`CollectibleInfoBox`).
//!
//! Port origin: `Telegram/SourceFiles/ui/boxes/collectible_info_box.h`,
//! `Telegram/SourceFiles/ui/boxes/collectible_info_box.cpp`,
//! and `Telegram/SourceFiles/window/window_session_controller.cpp` (`resolveCollectible`).
//!
//! Displays ownership, purchase date, TON & fiat price, and Fragment link for collectible
//! usernames and anonymous phone numbers fetched via `fragment.getCollectibleInfo`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::userpic::EmptyUserpic;

use crate::info::values::CollectibleEntityKind;

pub use tg_app_core::core_types::dto::collectible_info::*;

/// Stateful modal box for [`CollectibleInfoData`] (`Ui::CollectibleInfoBox`).
#[derive(Debug, Clone)]
pub struct CollectibleInfoBoxState {
    /// Resolved Fragment collectible metadata.
    pub info: CollectibleInfoData,
    /// Optional transient status feedback shown inside the dialog.
    pub status_message: Option<String>,
}

impl CollectibleInfoBoxState {
    /// Creates a new [`CollectibleInfoBoxState`] from resolved [`CollectibleInfoData`].
    #[must_use]
    pub fn new(info: CollectibleInfoData) -> Self {
        Self {
            info,
            status_message: None,
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor (`boxWideWidth = 390`).
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = match self.info.kind {
            CollectibleEntityKind::Phone => "Anonymous Number",
            CollectibleEntityKind::Username => "Collectible Username",
        };
        BoxModal::new(id, title)
            .width(390.0)
            .add_button(BoxButton::default_btn(
                "copy_collectible",
                self.info.copy_button_label(),
            ))
            .add_button(BoxButton::default_btn("close", "Close"))
    }

    /// Renders the Collectible Info modal body (emblem, clickable headline, owner pill,
    /// purchase summary, and read-only Learn More button).
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
    ) -> Option<CollectibleInfoBodyAction> {
        let mut action = None;
        let active_bg = colors.color("windowBgActive");
        let active_fg = colors.color("windowFgActive");
        let text_fg = colors.color("windowFg");
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let pill_bg = colors.color("windowBgOver");

        ui.vertical_centered(|ui| {
            ui.add_space(6.0);

            // 1. Centered circular Fragment collectible emblem (`collectibleIconDiameter = 80px`).
            let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(72.0), Sense::hover());
            ui.painter()
                .circle_filled(icon_rect.center(), 36.0, active_bg);
            let emblem = match self.info.kind {
                CollectibleEntityKind::Phone => "+888",
                CollectibleEntityKind::Username => "@",
            };
            let emblem_size = match self.info.kind {
                CollectibleEntityKind::Phone => 18.0,
                CollectibleEntityKind::Username => 34.0,
            };
            ui.painter().text(
                icon_rect.center(),
                Align2::CENTER_CENTER,
                emblem,
                FontId::new(emblem_size, family_semibold()),
                active_fg,
            );

            ui.add_space(12.0);

            // 2. Clickable entity headline (`@username is a collectible username`).
            let headline = self.info.headline();
            let headline_resp = ui
                .add(
                    egui::Label::new(
                        egui::RichText::new(&headline)
                            .font(FontId::new(16.0, family_semibold()))
                            .color(bold_fg),
                    )
                    .wrap()
                    .sense(Sense::click()),
                )
                .on_hover_cursor(CursorIcon::PointingHand);
            if headline_resp.clicked() {
                let entity_text = self.info.formatted_entity();
                ui.ctx().copy_text(entity_text.clone());
                self.status_message = Some(format!("Copied {entity_text}"));
                action = Some(CollectibleInfoBodyAction::CopyEntity(entity_text));
            }

            ui.add_space(10.0);

            // 3. Owner pill chip (`MakeOwnerCell` in `collectible_info_box.cpp:73-114`).
            let pill_h = 32.0;
            let name_galley = ui.painter().layout_no_wrap(
                self.info.owner_name.clone(),
                FontId::new(13.5, family_semibold()),
                text_fg,
            );
            let pill_w =
                (pill_h + 10.0 + name_galley.size().x + 14.0).min(ui.available_width().max(120.0));
            let (pill_rect, _) = ui.allocate_exact_size(Vec2::new(pill_w, pill_h), Sense::hover());
            ui.painter()
                .rect_filled(pill_rect, CornerRadius::same((pill_h / 2.0) as u8), pill_bg);
            let avatar_rect = Rect::from_min_size(pill_rect.min, Vec2::splat(pill_h));
            let userpic = EmptyUserpic::for_color_index(
                self.info.owner_color_index,
                &self.info.owner_name,
                colors,
            );
            userpic.paint_circle(ui.painter(), avatar_rect);
            ui.painter().text(
                Pos2::new(pill_rect.left() + pill_h + 10.0, pill_rect.center().y),
                Align2::LEFT_CENTER,
                &self.info.owner_name,
                FontId::new(13.5, family_semibold()),
                text_fg,
            );

            ui.add_space(12.0);

            // 4. Purchase date & price details (`FormatDate` + `FormatPrice`).
            let summary = self.info.summary_line();
            ui.add(
                egui::Label::new(
                    egui::RichText::new(summary)
                        .font(FontId::new(13.5, family_regular()))
                        .color(sub_fg),
                )
                .wrap(),
            );

            ui.add_space(12.0);

            // 5. Read-only "Learn More" Fragment link button.
            if let Some(url) = self.info.validated_fragment_url() {
                let url_owned = url.to_owned();
                let btn = egui::Button::new(
                    egui::RichText::new("Learn More on Fragment")
                        .font(FontId::new(13.5, family_semibold()))
                        .color(active_bg),
                )
                .fill(Color32::TRANSPARENT);
                if ui.add(btn).clicked() {
                    self.status_message = Some(format!("Fragment link: {url_owned}"));
                    action = Some(CollectibleInfoBodyAction::OpenFragmentUrl(url_owned));
                }
            }

            if let Some(ref status) = self.status_message {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(status)
                        .font(FontId::new(12.0, family_regular()))
                        .color(active_bg),
                );
            }
        });

        action
    }
}
