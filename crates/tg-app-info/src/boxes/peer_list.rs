//! Reusable searchable peer picker modal box (multi-select or single-select).
//!
//! Port origin: `Telegram/SourceFiles/boxes/peer_list_box.h`,
//! `Telegram/SourceFiles/boxes/peer_list_box.cpp`,
//! and `Telegram/SourceFiles/boxes/peer_list_controllers.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::input_field::InputField;

use crate::boxes::strings;
use crate::info::backend::{InfoPeerKind, PeerProfile};
use tg_app_core::stand_in::types::PeerId;

/// Selection cardinality for [`PeerListBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerSelectionMode {
    /// Only one peer can be selected at a time (e.g. linked discussion group).
    Single,
    /// Multiple peers can be selected (e.g. add members, create group, share).
    Multi,
}

/// Purpose tag explaining what happens when a [`PeerListBoxState`] confirms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerListPurpose {
    /// Adding members into an existing group.
    AddMembers {
        /// Target group peer id.
        group_id: PeerId,
    },
    /// Selecting a discussion group to link to a broadcast channel.
    LinkDiscussion {
        /// Target broadcast channel id.
        channel_id: PeerId,
    },
    /// Picking a member to promote to administrator.
    PickAdmin {
        /// Target group/channel id.
        peer_id: PeerId,
    },
    /// Picking a destination chat for a bot `switch_inline` button (`api/api_bot.cpp`).
    SwitchInline {
        /// Target bot peer id (`PeerId::from_user(...)`).
        bot_id: PeerId,
        /// Target bot username without leading `@`.
        bot_username: String,
        /// Pre-filled inline query string.
        query: String,
        /// Allowed chat types filter (`PeerTypes::empty()` allows any sendable chat).
        peer_types: tg_bots::markup::PeerTypes,
    },
    /// Generic peer selection.
    Generic,
}

/// Filters `all_peers` to chats eligible for a `switch_inline` recipient picker
/// (`InlineBots::PeerMatchesTypes` in `inline_bots/bot_attach_web_view.cpp` + send permission).
#[must_use]
pub fn filter_switch_inline_peers(
    all_peers: &[PeerProfile],
    bot_id: PeerId,
    peer_types: tg_bots::markup::PeerTypes,
) -> Vec<&PeerProfile> {
    let bot_peer = tg_bots::stand_in::PeerId(bot_id.0);
    all_peers
        .iter()
        .filter(|p| !p.is_blocked && (!p.kind.is_channel() || p.can_edit))
        .filter(|p| {
            if peer_types.is_empty() {
                return true;
            }
            let kind = match p.kind {
                InfoPeerKind::User | InfoPeerKind::SavedMessages => {
                    tg_bots::stand_in::PeerKind::User
                }
                InfoPeerKind::Bot => tg_bots::stand_in::PeerKind::Bot,
                InfoPeerKind::Group | InfoPeerKind::Supergroup => {
                    tg_bots::stand_in::PeerKind::Group
                }
                InfoPeerKind::Channel => tg_bots::stand_in::PeerKind::Broadcast,
            };
            tg_bots::markup::peer_matches_types(
                tg_bots::stand_in::PeerId(p.id.0),
                kind,
                bot_peer,
                peer_types,
            )
        })
        .collect()
}

/// Searchable peer picker state (`PeerListBox`).
#[derive(Debug, Clone)]
pub struct PeerListBoxState {
    /// Dialog title (e.g. `"Add Members"`).
    pub title: String,
    /// Confirm button label (e.g. `"Add"`, `"Select"`).
    pub confirm_label: String,
    /// Selection mode (`Single` or `Multi`).
    pub mode: PeerSelectionMode,
    /// Purpose for submission handling.
    pub purpose: PeerListPurpose,
    /// Live search query string.
    pub search_query: String,
    /// Ordered list of selected peer IDs.
    pub selected: Vec<PeerId>,
    /// Peer IDs that are already members and should be shown as disabled/excluded.
    pub excluded: Vec<PeerId>,
}

impl PeerListBoxState {
    /// Creates a new peer list picker state.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        confirm_label: impl Into<String>,
        mode: PeerSelectionMode,
        purpose: PeerListPurpose,
    ) -> Self {
        Self {
            title: title.into(),
            confirm_label: confirm_label.into(),
            mode,
            purpose,
            search_query: String::new(),
            selected: Vec::new(),
            excluded: Vec::new(),
        }
    }

    /// Pre-populates selected peers.
    #[must_use]
    pub fn with_selected(mut self, selected: Vec<PeerId>) -> Self {
        self.selected = selected;
        self
    }

    /// Excludes already-present peer IDs.
    #[must_use]
    pub fn with_excluded(mut self, excluded: Vec<PeerId>) -> Self {
        self.excluded = excluded;
        self
    }

    /// Toggles selection of `peer_id` respecting [`PeerSelectionMode`].
    pub fn toggle_peer(&mut self, peer_id: PeerId) {
        if self.excluded.contains(&peer_id) {
            return;
        }
        if let Some(pos) = self.selected.iter().position(|&id| id == peer_id) {
            self.selected.remove(pos);
        } else {
            if self.mode == PeerSelectionMode::Single {
                self.selected.clear();
            }
            self.selected.push(peer_id);
        }
    }

    /// Filters `candidates` by the current search query (matching title or username).
    #[must_use]
    pub fn filter_candidates<'a>(&self, candidates: &'a [PeerProfile]) -> Vec<&'a PeerProfile> {
        let q = self.search_query.trim().to_lowercase();
        candidates
            .iter()
            .filter(|p| !self.excluded.contains(&p.id))
            .filter(|p| {
                if q.is_empty() {
                    return true;
                }
                p.title.to_lowercase().contains(&q)
                    || p.primary_username()
                        .is_some_and(|u| u.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, &self.title)
            .width(st::box_wide_width.0 as f32 + 16.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("confirm", &self.confirm_label));
        if self.mode == PeerSelectionMode::Multi && !self.selected.is_empty() {
            modal = modal.additional_title(format!("{} / 200", self.selected.len()));
        }
        modal
    }

    /// Renders the search input, selected chips strip, and scrollable peer rows.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        id_salt: &str,
        candidates: &[PeerProfile],
        colors: &dyn Colors,
    ) {
        let search_ph = tr::lng_dlg_filter();
        InputField::single_line(
            &format!("{id_salt}_peer_search"),
            &mut self.search_query,
            &search_ph,
            InputFieldStyle::rounded_search(colors),
        )
        .show(ui);

        // Selected chips bar (`Ui::MultiSelect` equivalent)
        if !self.selected.is_empty() {
            ui.add_space(8.0);
            let mut to_remove: Option<PeerId> = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for &pid in &self.selected {
                    let title = candidates
                        .iter()
                        .find(|p| p.id == pid)
                        .map_or(strings::PEER_FALLBACK_LABEL, |p| p.title.as_str());
                    let short = title.split_whitespace().next().unwrap_or(title);
                    if selected_chip(ui, short, colors) {
                        to_remove = Some(pid);
                    }
                }
            });
            if let Some(rm) = to_remove {
                self.selected.retain(|&id| id != rm);
            }
        }

        ui.add_space(8.0);
        let filtered: Vec<PeerProfile> = self
            .filter_candidates(candidates)
            .into_iter()
            .cloned()
            .collect();

        if filtered.is_empty() {
            let (r, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), Sense::hover());
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                strings::NO_MATCHING_PEERS,
                font_id_from_style(st::normal_font),
                colors.color("windowSubTextFg"),
            );
            return;
        }

        for peer in &filtered {
            let is_selected = self.selected.contains(&peer.id);
            if peer_picker_row(ui, peer, is_selected, colors) {
                self.toggle_peer(peer.id);
            }
        }
    }
}

fn selected_chip(ui: &mut Ui, label: &str, colors: &dyn Colors) -> bool {
    let text = format!("{label} ×");
    let chip_font = FontId::new(12.0, family_semibold());
    let galley = ui.painter().layout_no_wrap(
        text.clone(),
        chip_font.clone(),
        colors.color("windowActiveTextFg"),
    );
    let size = Vec2::new(galley.size().x + 16.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(12),
        if resp.hovered() {
            colors.color("lightButtonBgRipple")
        } else {
            colors.color("lightButtonBgOver")
        },
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        chip_font,
        colors.color("windowActiveTextFg"),
    );
    resp.clicked()
}

fn peer_picker_row(ui: &mut Ui, peer: &PeerProfile, selected: bool, colors: &dyn Colors) -> bool {
    let width = ui.available_width().max(200.0);
    let row_h = (st::peer_list_box_item.height.0 as f32).min(54.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, peer.title.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
    }

    let av_size = (st::peer_list_box_item.photo_size.0 as f32).min(38.0);
    let av_rect = Rect::from_min_size(
        Pos2::new(rect.min.x + 6.0, rect.center().y - av_size * 0.5),
        Vec2::splat(av_size),
    );
    if peer.kind == InfoPeerKind::SavedMessages {
        EmptyUserpic::paint_saved_messages(ui.painter(), av_rect, colors);
    } else {
        EmptyUserpic::for_color_index(peer.color_index, &peer.title, colors)
            .paint_circle(ui.painter(), av_rect);
    }

    let resolver = ColorsResolver(colors);

    // Checkmark badge overlay on avatar when selected (`RoundImageCheckbox` style)
    if selected {
        let badge_c = Pos2::new(av_rect.max.x - 4.0, av_rect.max.y - 4.0);
        ui.painter()
            .circle_filled(badge_c, 8.5, colors.color("windowBg"));
        ui.painter()
            .circle_filled(badge_c, 7.0, colors.color("windowBgActive"));
        let chk_rect = Rect::from_center_size(badge_c, Vec2::splat(11.0));
        paint_style_icon(
            ui.painter(),
            chk_rect,
            st::default_checkbox_icon,
            &resolver,
            Some(Color32::WHITE),
        );
    }

    let text_x = av_rect.max.x + 12.0;
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 8.0),
        Align2::LEFT_TOP,
        &peer.title,
        font_id_from_style(st::peer_list_box_item.name_style.font),
        colors.color("windowBoldFg"),
    );
    let st_fg = if peer.is_online {
        colors.color("windowActiveTextFg")
    } else {
        colors.color("windowSubTextFg")
    };
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 27.0),
        Align2::LEFT_TOP,
        &peer.status,
        FontId::new(12.0, family_regular()),
        st_fg,
    );

    // Selection circle indicator on the right
    let ind_c = Pos2::new(rect.max.x - 16.0, rect.center().y);
    if selected {
        ui.painter()
            .circle_filled(ind_c, 9.0, colors.color("windowBgActive"));
        let chk_rect = Rect::from_center_size(ind_c, Vec2::splat(13.0));
        paint_style_icon(
            ui.painter(),
            chk_rect,
            st::default_checkbox_icon,
            &resolver,
            Some(Color32::WHITE),
        );
    } else {
        ui.painter()
            .circle_stroke(ind_c, 9.0, Stroke::new(1.5, colors.color("checkboxFg")));
    }

    resp.clicked()
}

#[cfg(test)]
mod tests {
    use tg_bots::markup::PeerTypes;

    use super::*;
    use crate::info::InfoBackend;
    use crate::info::mock::{MockBackend, mock_peers};

    #[test]
    fn switch_inline_peer_filter_respects_peer_types_and_can_send() {
        let session = MockBackend::new();
        let all = session.all_peers();
        let bot_id = mock_peers::BOT_BUILD;

        // USER | GROUP includes Mira (User) and Rustgram Core (Group), excludes Channel & Bot.
        let ug = filter_switch_inline_peers(&all, bot_id, PeerTypes::USER | PeerTypes::GROUP);
        assert!(ug.iter().any(|p| p.id == mock_peers::USER_MIRA));
        assert!(ug.iter().any(|p| p.id == mock_peers::GROUP_CORE));
        assert!(!ug.iter().any(|p| p.id == mock_peers::BOT_BUILD));
        assert!(!ug.iter().any(|p| p.id == mock_peers::CHANNEL_RELEASES));

        // SAME_BOT includes only the bot itself.
        let same = filter_switch_inline_peers(&all, bot_id, PeerTypes::SAME_BOT);
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].id, bot_id);

        // Empty peer_types permits every non-blocked sendable chat regardless of kind.
        let any = filter_switch_inline_peers(&all, bot_id, PeerTypes::empty());
        assert!(any.iter().all(|p| !p.is_blocked));
        assert!(any.iter().any(|p| p.id == mock_peers::USER_MIRA));
        assert!(any.iter().any(|p| p.id == mock_peers::GROUP_CORE));
        assert!(any.iter().any(|p| p.id == mock_peers::BOT_BUILD));
    }
}
