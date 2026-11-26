//! Share / Forward to multiple chats modal box.
//!
//! Sharing a link or files uses the 4-column avatar grid with a comment input
//! (`ShareBox`, `_columnCount = 4`); forwarding messages uses the "Forward to..." recipient list
//! (`ChooseRecipientBoxController` in a `PeerListBox`, `window_peer_menu.cpp`).
//!
//! Port origin: `Telegram/SourceFiles/boxes/share_box.h`,
//! `Telegram/SourceFiles/boxes/share_box.cpp`,
//! and `Telegram/SourceFiles/boxes/peer_list_controllers.cpp`.

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

/// One chat folder tab above the "Forward to..." list (`Ui::AddChatFiltersTabsStrip`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareFolderTab {
    /// Folder id; `0` is the "All" tab that lists every chat.
    pub id: i32,
    /// Tab label (`lng_filters_all_short` for the "All" tab).
    pub title: String,
    /// Chats in this folder (unused for the "All" tab).
    pub peers: Vec<PeerId>,
}

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
    /// Chat the forwarded messages come from (set for "Forward to...").
    pub from_peer: Option<PeerId>,
    /// Chat folder tabs shown above the forward list when there is more than one.
    pub folder_tabs: Vec<ShareFolderTab>,
    /// Active folder tab id (`0` = "All").
    pub active_folder: i32,
    /// Chat picked with a plain click while nothing is selected (`singleChosen`): the caller
    /// opens it with the forward bar instead of sending at once.
    pub chosen_single: Option<PeerId>,
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
            from_peer: None,
            folder_tabs: Vec::new(),
            active_folder: 0,
            chosen_single: None,
        }
    }

    /// Creates the "Forward to..." box for `message_ids` from `from_peer` with the chat folder
    /// tabs (`Window::ShowForwardMessagesBox`).
    #[must_use]
    pub fn for_forward(
        from_peer: PeerId,
        message_ids: Vec<i32>,
        folder_tabs: Vec<ShareFolderTab>,
    ) -> Self {
        Self {
            from_peer: Some(from_peer),
            folder_tabs,
            ..Self::new(message_ids, None)
        }
    }

    /// Takes the chat picked with a plain click, if any.
    pub fn take_chosen_single(&mut self) -> Option<PeerId> {
        self.chosen_single.take()
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
            from_peer: None,
            folder_tabs: Vec::new(),
            active_folder: 0,
            chosen_single: None,
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

    /// Whether this box forwards messages through the "Forward to..." recipient list
    /// (no link or files to share).
    #[must_use]
    pub fn is_forward_list(&self) -> bool {
        self.share_link.is_none()
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
        if self.is_forward_list() {
            // Only Cancel until a chat is selected with Ctrl+click (`ShowForwardMessagesBox`
            // adds Send once `hasSelected()`).
            let modal = BoxModal::new(id, tr::lng_forward_choose())
                .width(st::box_wide_width.0 as f32)
                .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()));
            return if self.selected.is_empty() {
                modal
            } else {
                modal.add_button(BoxButton::default_btn("send", send_label))
            };
        }
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
        if self.is_forward_list() {
            self.show_forward_list(ui, peers, colors);
            return;
        }
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
                        .paint_circle_or_photo(ui.painter(), av_rect, peer.id.0);
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

impl ShareBoxState {
    /// Renders the "Forward to..." recipient list: search field, then Saved Messages and chats
    /// as `st::defaultPeerListItem` rows with their status line.
    fn show_forward_list(&mut self, ui: &mut Ui, peers: &[PeerProfile], colors: &dyn Colors) {
        InputField::single_line(
            "share_box_search",
            &mut self.search_query,
            &tr::lng_participant_filter(),
            InputFieldStyle::rounded_search(colors),
        )
        .show(ui);
        ui.add_space(6.0);

        let q = self.search_query.trim().to_lowercase();
        // A query searches every chat, like `applyFilter(box, hasQuery ? 0 : lastFilterId)`.
        if self.folder_tabs.len() > 1 && q.is_empty() {
            if let Some(id) = show_folder_tabs(ui, &self.folder_tabs, self.active_folder, colors) {
                self.active_folder = id;
            }
            ui.add_space(4.0);
        }
        let folder_peers = (q.is_empty() && self.active_folder != 0)
            .then(|| {
                self.folder_tabs
                    .iter()
                    .find(|t| t.id == self.active_folder)
                    .map(|t| t.peers.as_slice())
            })
            .flatten();
        let mut ordered: Vec<&PeerProfile> = peers
            .iter()
            .filter(|p| p.kind == InfoPeerKind::SavedMessages)
            .chain(
                peers
                    .iter()
                    .filter(|p| p.kind != InfoPeerKind::SavedMessages),
            )
            .filter(|p| {
                q.is_empty()
                    || p.title.to_lowercase().contains(&q)
                    || p.primary_username()
                        .is_some_and(|u| u.to_lowercase().contains(&q))
            })
            .filter(|p| folder_peers.is_none_or(|ids| ids.contains(&p.id)))
            .collect();
        ordered.dedup_by_key(|p| p.id);

        let width = ui.available_width().max(260.0);
        let resolver = ColorsResolver(colors);
        for peer in ordered {
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(width, FORWARD_ROW_HEIGHT), Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, peer.title.clone())
            });
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::ZERO,
                    colors.color("windowBgOver"),
                );
            }
            let selected = self.selected.contains(&peer.id);
            let av_rect = Rect::from_min_size(
                Pos2::new(
                    rect.min.x + FORWARD_PHOTO_LEFT,
                    rect.min.y + FORWARD_PHOTO_TOP,
                ),
                Vec2::splat(FORWARD_PHOTO_SIZE),
            );
            if peer.kind == InfoPeerKind::SavedMessages {
                EmptyUserpic::paint_saved_messages(ui.painter(), av_rect, colors);
            } else {
                EmptyUserpic::for_color_index(peer.color_index, &peer.title, colors)
                    .paint_circle_or_photo(ui.painter(), av_rect, peer.id.0);
            }
            if selected {
                let badge_c = Pos2::new(av_rect.max.x - 4.0, av_rect.max.y - 4.0);
                ui.painter()
                    .circle_filled(badge_c, 9.5, colors.color("windowBg"));
                ui.painter()
                    .circle_filled(badge_c, 8.0, colors.color("windowBgActive"));
                paint_style_icon(
                    ui.painter(),
                    Rect::from_center_size(badge_c, Vec2::splat(12.0)),
                    st::default_checkbox_icon,
                    &resolver,
                    Some(Color32::WHITE),
                );
            }
            let text_left = rect.min.x + FORWARD_NAME_LEFT;
            let clip = Rect::from_min_max(
                Pos2::new(text_left, rect.min.y),
                Pos2::new(rect.max.x - 12.0, rect.max.y),
            );
            let painter = ui.painter().with_clip_rect(clip);
            let (title, status, status_active) = forward_row_texts(peer);
            painter.text(
                Pos2::new(text_left, rect.min.y + FORWARD_NAME_TOP),
                Align2::LEFT_TOP,
                title,
                FontId::new(
                    font_id_from_style(st::semibold_font).size,
                    tg_ui::fonts::family_semibold(),
                ),
                colors.color("windowBoldFg"),
            );
            painter.text(
                Pos2::new(text_left, rect.min.y + FORWARD_STATUS_TOP),
                Align2::LEFT_TOP,
                status,
                font_id_from_style(st::normal_font),
                if status_active {
                    colors.color("windowActiveTextFg")
                } else {
                    colors.color("windowSubTextFg")
                },
            );
            if resp.clicked() {
                // A plain click with nothing selected picks that chat (`singleChosen`); Ctrl+click
                // or an existing selection toggles multi-selection for "Send".
                let ctrl = ui.input(|i| i.modifiers.command);
                if self.selected.is_empty() && !ctrl {
                    self.chosen_single = Some(peer.id);
                } else {
                    self.toggle_target(peer.id);
                }
            }
        }

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

/// Paints the `st::chatsFiltersTabs` slider (same metrics as `st::dialogsSearchTabs`) and
/// returns a clicked folder id.
fn show_folder_tabs(
    ui: &mut Ui,
    tabs: &[ShareFolderTab],
    active: i32,
    colors: &dyn Colors,
) -> Option<i32> {
    const HEIGHT: f32 = 33.0;
    const PADDING: f32 = 9.0;
    const SKIP: f32 = 18.0;
    const LABEL_TOP: f32 = 7.0;
    const BAR_TOP: f32 = 30.0;
    let font = FontId::new(
        font_id_from_style(st::semibold_font).size,
        tg_ui::fonts::family_semibold(),
    );
    let width = ui.available_width();
    let (bar, _) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::hover());
    let painter = ui.painter().with_clip_rect(bar);
    let mut clicked = None;
    let mut x = bar.left() + PADDING;
    for tab in tabs {
        let galley = painter.layout_no_wrap(tab.title.clone(), font.clone(), Color32::PLACEHOLDER);
        let w = galley.size().x;
        let hit = Rect::from_min_size(
            Pos2::new(x - SKIP * 0.5, bar.top()),
            Vec2::new(w + SKIP, bar.height()),
        );
        let is_active = tab.id == active;
        let resp = ui.interact(
            hit.intersect(bar),
            ui.scope_id().with(("share_folder_tab", tab.id)),
            Sense::click(),
        );
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Tab, true, is_active, tab.title.clone())
        });
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            clicked = Some(tab.id);
        }
        let fg = if is_active {
            colors.color("lightButtonFg")
        } else {
            colors.color("windowSubTextFg")
        };
        painter.text(
            Pos2::new(x, bar.top() + LABEL_TOP),
            Align2::LEFT_TOP,
            &tab.title,
            font.clone(),
            fg,
        );
        if is_active {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x, bar.top() + BAR_TOP),
                    Pos2::new(x + w, bar.bottom()),
                ),
                egui::CornerRadius {
                    nw: 2,
                    ne: 2,
                    sw: 0,
                    se: 0,
                },
                colors.color("lightButtonFg"),
            );
        }
        x += w + SKIP;
    }
    painter.line_segment(
        [
            Pos2::new(bar.left(), bar.bottom() - 0.5),
            Pos2::new(bar.right(), bar.bottom() - 0.5),
        ],
        Stroke::new(1.0, colors.color("shadowFg")),
    );
    clicked
}

/// Row height of the recipient list (`st::defaultPeerListItem.height`).
const FORWARD_ROW_HEIGHT: f32 = 56.0;
/// Userpic diameter (`st::defaultPeerListItem.photoSize`).
const FORWARD_PHOTO_SIZE: f32 = 42.0;
const FORWARD_PHOTO_LEFT: f32 = 14.0;
const FORWARD_PHOTO_TOP: f32 = 7.0;
const FORWARD_NAME_LEFT: f32 = 70.0;
const FORWARD_NAME_TOP: f32 = 10.0;
const FORWARD_STATUS_TOP: f32 = 30.0;

/// Title, status line and whether the status is drawn in the active (online) color
/// (`ChooseRecipientBoxController::createRow`, `PeerListRow::refreshStatus`).
fn forward_row_texts(peer: &PeerProfile) -> (String, String, bool) {
    match peer.kind {
        InfoPeerKind::SavedMessages => (
            tr::lng_saved_messages(),
            tr::lng_saved_forward_here(),
            false,
        ),
        InfoPeerKind::Bot => (peer.title.clone(), tr::lng_status_bot(), false),
        InfoPeerKind::Channel => (peer.title.clone(), tr::lng_channel_status(), false),
        InfoPeerKind::Group | InfoPeerKind::Supergroup => (
            peer.title.clone(),
            tr::lng_chat_status_members(i64::from(peer.members_count.max(1))),
            false,
        ),
        InfoPeerKind::User => {
            if peer.is_online {
                (peer.title.clone(), tr::lng_status_online(), true)
            } else {
                (peer.title.clone(), peer.status.clone(), false)
            }
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
