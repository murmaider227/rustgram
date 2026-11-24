//! Share / Forward to multiple chats modal box with 4-column avatar grid and comment input.
//!
//! Port origin: `Telegram/SourceFiles/boxes/share_box.h`
//! and `Telegram/SourceFiles/boxes/share_box.cpp` (`ShareBox`, `_columnCount = 4`).

use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::input_field::InputField;

use crate::boxes::strings;
use crate::info::backend::{InfoBackend, InfoPeerKind, PeerProfile};
use tg_app_core::media::backend::MediaItemId;
use tg_app_core::stand_in::types::PeerId;

/// State for the Share / Forward modal (`ShareBox`).
#[derive(Debug, Clone)]
pub struct ShareBoxState {
    /// Message IDs being forwarded (may be empty when sharing a link).
    pub message_ids: Vec<i32>,
    /// Optional media viewer item ID when opened from the fullscreen media overlay
    /// (forwarded via [`tg_app_core::media::backend::MediaBackend::forward_item_to`]).
    pub media_item_id: Option<MediaItemId>,
    /// Optional shareable link (`Copy Link` button).
    pub share_link: Option<String>,
    /// Live search query filtering chat tiles.
    pub search_query: String,
    /// Selected destination peer IDs.
    pub selected: Vec<PeerId>,
    /// Optional accompanying comment sent with forwarded messages.
    pub comment: String,
    /// Validation error message.
    pub error: Option<String>,
}

impl ShareBoxState {
    /// Creates a new [`ShareBoxState`].
    #[must_use]
    pub fn new(message_ids: Vec<i32>, share_link: Option<String>) -> Self {
        Self {
            message_ids,
            media_item_id: None,
            share_link,
            search_query: String::new(),
            selected: Vec::new(),
            comment: String::new(),
            error: None,
        }
    }

    /// Creates a [`ShareBoxState`] for forwarding a media viewer item to chosen chats via
    /// [`tg_app_core::media::backend::MediaBackend::forward_item_to`].
    #[must_use]
    pub fn for_media_item(media_item_id: MediaItemId, msg_id: i32) -> Self {
        Self {
            message_ids: vec![msg_id],
            media_item_id: Some(media_item_id),
            share_link: None,
            search_query: String::new(),
            selected: Vec::new(),
            comment: String::new(),
            error: None,
        }
    }

    /// Toggles selection of a target chat.
    pub fn toggle_target(&mut self, peer_id: PeerId) {
        if let Some(pos) = self.selected.iter().position(|&id| id == peer_id) {
            self.selected.remove(pos);
        } else {
            self.selected.push(peer_id);
        }
        self.error = None;
    }

    /// Submits the forward operation to `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<usize, String> {
        if self.selected.is_empty() {
            let msg = strings::SHARE_SELECT_ONE_ERROR.to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        match backend.forward_messages(&self.selected, &self.message_ids, &self.comment) {
            Ok(n) => {
                self.error = None;
                Ok(n)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let send_base = tr::lng_share_confirm();
        let send_label = if self.selected.is_empty() {
            send_base
        } else {
            format!("{send_base} ({})", self.selected.len())
        };
        let mut modal = BoxModal::new(id, tr::lng_share_title())
            .width(st::box_wide_width.0 as f32 + 48.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("send", send_label));
        if self.share_link.is_some() {
            modal = modal.with_left_button(BoxButton::default_btn(
                "copy_link",
                tr::lng_share_copy_link(),
            ));
        }
        modal
    }

    /// Renders the search bar, 4-column chat avatar grid (`ShareBox::Inner`), and comment input.
    pub fn show_body(&mut self, ui: &mut Ui, peers: &[PeerProfile], colors: &dyn Colors) {
        InputField::single_line(
            "share_box_search",
            &mut self.search_query,
            strings::SHARE_SEARCH_PH,
            InputFieldStyle::rounded_search(colors),
        )
        .show(ui);
        ui.add_space(10.0);

        let q = self.search_query.trim().to_lowercase();
        let filtered: Vec<&PeerProfile> = peers
            .iter()
            .filter(|p| {
                q.is_empty()
                    || p.title.to_lowercase().contains(&q)
                    || p.primary_username()
                        .is_some_and(|u| u.to_lowercase().contains(&q))
            })
            .collect();

        let width = ui.available_width().max(260.0);
        let cols = 4usize;
        let cell_w = width / cols as f32;
        let cell_h = 84.0;
        let resolver = ColorsResolver(colors);

        for row in filtered.chunks(cols) {
            let (row_rect, _) = ui.allocate_exact_size(Vec2::new(width, cell_h), Sense::hover());
            for (idx, peer) in row.iter().enumerate() {
                let cell_rect = Rect::from_min_size(
                    Pos2::new(row_rect.min.x + idx as f32 * cell_w, row_rect.min.y),
                    Vec2::new(cell_w, cell_h),
                );
                let resp = ui.interact(
                    cell_rect,
                    ui.make_persistent_id(("share_tile", peer.id.0)),
                    Sense::click(),
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }

                let selected = self.selected.contains(&peer.id);
                let av_size = 50.0;
                let av_rect = Rect::from_center_size(
                    Pos2::new(cell_rect.center().x, cell_rect.min.y + 30.0),
                    Vec2::splat(av_size),
                );

                if selected {
                    ui.painter().circle_stroke(
                        av_rect.center(),
                        av_size * 0.5 + 3.0,
                        Stroke::new(2.0, colors.color("windowBgActive")),
                    );
                }

                if peer.kind == InfoPeerKind::SavedMessages {
                    EmptyUserpic::paint_saved_messages(ui.painter(), av_rect, colors);
                } else {
                    EmptyUserpic::for_color_index(peer.color_index, &peer.title, colors)
                        .paint_circle(ui.painter(), av_rect);
                }

                if selected {
                    let badge_c = Pos2::new(av_rect.max.x - 4.0, av_rect.max.y - 4.0);
                    ui.painter()
                        .circle_filled(badge_c, 9.5, colors.color("windowBg"));
                    ui.painter()
                        .circle_filled(badge_c, 8.0, colors.color("windowBgActive"));
                    let chk_rect = Rect::from_center_size(badge_c, Vec2::splat(12.0));
                    paint_style_icon(
                        ui.painter(),
                        chk_rect,
                        st::default_checkbox_icon,
                        &resolver,
                        Some(Color32::WHITE),
                    );
                }

                let short_title = truncate_label(&peer.title, 11);
                let label_fg = if selected {
                    colors.color("windowActiveTextFg")
                } else {
                    colors.color("windowFg")
                };
                ui.painter().text(
                    Pos2::new(cell_rect.center().x, av_rect.max.y + 12.0),
                    Align2::CENTER_CENTER,
                    short_title,
                    FontId::new(11.5, family_regular()),
                    label_fg,
                );

                if resp.clicked() {
                    self.toggle_target(peer.id);
                }
            }
        }

        ui.add_space(8.0);
        InputField::multi_line(
            "share_box_comment",
            &mut self.comment,
            strings::SHARE_COMMENT_PH,
            InputFieldStyle::default_input(colors),
        )
        .show(ui);

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_owned()
    } else {
        let mut s: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}
