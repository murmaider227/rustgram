//! Dialogs list column, vertical folders sidebar, horizontal folder tabs, Archive row,
//! drag-and-drop pinned chat reordering, context menu, and local/global/message/hashtag search.
//!
//! Port origin: `Telegram/SourceFiles/dialogs/dialogs_widget.cpp`,
//! `Telegram/SourceFiles/dialogs/dialogs_inner_widget.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_layout.cpp`,
//! `Telegram/SourceFiles/dialogs/dialogs_search_*.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/top_peers_strip.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/chats_filter_tag.cpp`,
//! and `Telegram/SourceFiles/window/window_filters_menu.cpp`.

pub use tg_app_core::dialogs::backend;
pub mod community;
pub mod emoji_status;
pub mod entry_menu;
pub mod filters_menu;
pub mod inner;
pub mod layout;
pub mod list_cache;
pub mod mini_preview;
pub mod mock;
pub mod mute_label;
pub mod quick_action;
pub mod search;
pub mod search_snippet;
pub mod strings;
pub mod suggestions;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_frame_cache;
#[cfg(test)]
mod tests_tgb2972_2946;
pub mod video_userpic;
pub mod widget;

use egui::{
    CornerRadius, CursorIcon, Frame, Margin, Pos2, Rect, ScrollArea, Sense, TextEdit, Ui, Vec2,
};
use tg_history::{DialogBadgesState, DialogSendState};

use crate::dialogs::backend::{
    ChatFolder, DialogItem, DialogPeerKind, FolderIconKind, FolderUnreadState, SpecialRowKind,
};
use crate::dialogs::filters_menu::show_horizontal_folder_tabs;
use crate::dialogs::inner::visible_row_range;
use crate::dialogs::layout::{DialogColors, RowPaintState, paint_dialog_row};
use crate::dialogs::mock::MOCK_NOW;
use crate::dialogs::widget::hamburger_button;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{DialogEntry, FilterId, FolderFilter, MsgId, PeerId, PeerKind};

pub use backend::{
    ArchiveSummary, ChatTypeFilter, DialogDraft, DialogListQuery, DialogSlice, DialogsBackend,
    FolderTagBadge, GlobalPeerResult, HashtagSuggestion, MessageSearchResult, RecentPeerItem,
    SearchRequest, SearchResults, SearchTab, SearchTagChip, TopPeerItem, TypingAction, TypingState,
};
pub use filters_menu::{
    FILTERS_SIDEBAR_WIDTH, FiltersMenuAction, FiltersMenuWidget, HORIZONTAL_TABS_HEIGHT,
};
pub use inner::{
    DIALOGS_PAGE_SIZE, DialogsInnerWidget, InnerAction, PinnedDragState, variable_visible_row_range,
};
pub use layout::{COLLAPSED_ARCHIVE_HEIGHT, SEARCHED_BAR_HEIGHT, find_highlight_ranges};
pub use mock::{MockDialogsStore, reset_mock_backend_dialogs_state};
pub use quick_action::{
    DialogHoverQuickAction, DialogSwipeState, QUICK_ACTION_ICON_SIZE, QUICK_ACTION_STRIP_WIDTH,
    QUICK_ACTION_SWIPE_THRESHOLD, QuickDialogActionLabel, paint_quick_action_strip,
    perform_quick_dialog_action, quick_action_bg_color, quick_action_style_icon,
    quick_dialog_label_text, quick_dialog_lottie_icon_name, resolve_quick_dialog_label,
    show_hover_quick_action_pill,
};
pub use search::{
    DialogsSearchState, HashOrCashtag, SearchKeyboardItem, SearchViewAction,
    fix_hashtag_search_query, is_hash_or_cashtag_search_query,
};
pub use video_userpic::{
    DialogVideoUserpic, DialogVideoUserpicCache, VideoUserpicFetch, VideoUserpicFrame,
    VideoUserpicSource, VideoUserpicState, apply_ellipse_mask, paint_dialog_userpic,
};
pub use widget::{
    DialogsPanel, DialogsPanelAction, DialogsSearchRow, DialogsShortcut, detect_dialogs_shortcut,
    resolve_folder_slot,
};

/// Standard dialog row height (`st::dialogsRowHeight = 62px`).
pub const DIALOG_ROW_HEIGHT: f32 = tg_ui::tg_style::st::dialogs_row_height.0 as f32;
/// Top search bar height (`st::dialogsFilterPadding` + input).
pub const DIALOGS_TOP_BAR_HEIGHT: f32 = 54.0;

/// Events emitted by the stateless shell entry point [`DialogsWidget::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogsAction {
    /// User clicked the hamburger menu icon.
    OpenMainMenu,
    /// User clicked a dialog row.
    SelectPeer(PeerId),
    /// User picked a folder tab in horizontal (narrow) folder bar.
    SelectFolder(FilterId),
}

/// Stateless shell entry point compatible with `window::main_widget::MainWidget`.
///
/// Upgraded to render every row using `tg-history`'s [`tg_history::layout_dialog_row`]
/// and `tg-ui`'s userpics/badges with virtualized viewport culling. For the complete
/// stateful experience (Archive, drag-and-drop pin reorder, context menu, and rich search),
/// use [`DialogsPanel`] backed by [`DialogsBackend`].
pub struct DialogsWidget;

impl DialogsWidget {
    /// Renders the dialogs column inside `ui`.
    ///
    /// When `compact_small_column` is `true` (width == 66px), draws the avatar-only rail
    /// just like `tdesktop` when the left splitter is dragged below `columnMinimalWidthLeft / 2`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        dialogs: &[DialogEntry],
        folders: &[FolderFilter],
        active_filter: FilterId,
        show_horizontal_folders: bool,
        active_peer: PeerId,
        search_query: &mut String,
        compact_small_column: bool,
        show_hamburger: bool,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<DialogsAction> {
        let mut action = None;
        tg_ui::fonts::ensure_tdesktop_fonts(ui.ctx());
        let colors = DialogColors::new(palette);
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.window_bg());
        ui.spacing_mut().item_spacing = Vec2::ZERO;

        // Top bar: hamburger + rounded search pill (or centered hamburger in compact 66px rail).
        Frame::NONE
            .fill(colors.window_bg())
            .inner_margin(Margin::symmetric(
                tg_ui::tg_style::st::dialogs_filter_padding.x.0 as i8,
                tg_ui::tg_style::st::dialogs_filter_padding.y.0 as i8,
            ))
            .show(ui, |ui| {
                if compact_small_column {
                    if show_hamburger {
                        ui.vertical_centered(|ui| {
                            if hamburger_button(ui, palette) {
                                action = Some(DialogsAction::OpenMainMenu);
                            }
                        });
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if show_hamburger && hamburger_button(ui, palette) {
                            action = Some(DialogsAction::OpenMainMenu);
                        }

                        let has_clear = !search_query.is_empty();
                        let input_width =
                            (ui.available_width() - if has_clear { 24.0 } else { 0.0 }).max(80.0);
                        ui.add_sized(
                            Vec2::new(input_width, 32.0),
                            TextEdit::singleline(search_query)
                                .hint_text(tg_lang::tr::lng_dlg_filter()),
                        );

                        if has_clear {
                            let (x_rect, x_resp) =
                                ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                            if x_resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            tg_ui::icons::paint_style_icon(
                                ui.painter(),
                                x_rect,
                                tg_ui::tg_style::st::box_title_close_icon,
                                &colors,
                                Some(colors.sub_text_fg()),
                            );
                            if x_resp.clicked() {
                                search_query.clear();
                            }
                        }
                    });
                }
            });

        // Horizontal folder tabs when the vertical sidebar is hidden.
        if show_horizontal_folders && !compact_small_column && folders.len() > 1 {
            let chat_folders: Vec<ChatFolder> = folders
                .iter()
                .map(|f| ChatFolder {
                    id: f.id,
                    title: f.title.clone(),
                    icon: FolderIconKind::All,
                    color_index: 5,
                    unread: FolderUnreadState {
                        unread_chats: f.unread_count,
                        muted_chats: 0,
                    },
                    locked: false,
                })
                .collect();
            if let Some(fid) =
                show_horizontal_folder_tabs(ui, &chat_folders, active_filter, palette)
            {
                action = Some(DialogsAction::SelectFolder(fid));
            }
        }

        // Virtualized scrollable dialogs list painted via `tg-history` + `tg-ui`.
        let _ = strings;
        let list_width = ui.available_width();
        let anim_time = ui.ctx().input(|i| i.time);

        ScrollArea::vertical()
            .id_salt("tg_dialogs_scroll")
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if dialogs.is_empty() && !compact_small_column {
                    ui.add_space(28.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(tg_lang::tr::lng_no_chats())
                                .font(tg_ui::fonts::font_id_from_style(
                                    tg_ui::tg_style::st::dialogs_text_font,
                                ))
                                .color(colors.sub_text_fg()),
                        );
                    });
                    return;
                }

                let total_h = dialogs.len() as f32 * DIALOG_ROW_HEIGHT;
                let (content_rect, _) =
                    ui.allocate_exact_size(Vec2::new(list_width, total_h.max(1.0)), Sense::hover());
                let vis = visible_row_range(
                    viewport.min.y,
                    viewport.max.y,
                    DIALOG_ROW_HEIGHT,
                    dialogs.len(),
                );
                for idx in vis {
                    let entry = &dialogs[idx];
                    let row_rect = Rect::from_min_size(
                        Pos2::new(
                            content_rect.left(),
                            content_rect.top() + idx as f32 * DIALOG_ROW_HEIGHT,
                        ),
                        Vec2::new(list_width, DIALOG_ROW_HEIGHT),
                    );
                    let resp = ui.interact(
                        row_rect,
                        ui.scope_id().with(("tg_shell_dialog_row", entry.peer.id.0)),
                        Sense::click(),
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let item = dialog_item_from_stand_in(entry);
                    let state = RowPaintState {
                        active: entry.peer.id == active_peer,
                        selected: resp.hovered(),
                        narrow: compact_small_column,
                        dragging: false,
                        show_folder_tags: false,
                        anim_time_secs: anim_time,
                        use_24h: true,
                    };
                    paint_dialog_row(
                        ui.painter(),
                        row_rect,
                        &item,
                        MOCK_NOW,
                        state,
                        palette,
                        &colors,
                    );
                    if resp.clicked() {
                        action = Some(DialogsAction::SelectPeer(entry.peer.id));
                    }
                    if compact_small_column {
                        resp.on_hover_text(&entry.peer.title);
                    }
                }
            });

        action
    }
}

fn dialog_item_from_stand_in(entry: &DialogEntry) -> DialogItem {
    let kind = match entry.peer.kind {
        PeerKind::User => DialogPeerKind::User,
        PeerKind::Group => DialogPeerKind::Group,
        PeerKind::Channel => DialogPeerKind::Channel,
    };
    DialogItem {
        peer_id: entry.peer.id,
        kind,
        special: if entry.peer.is_saved_messages {
            SpecialRowKind::SavedMessages
        } else {
            SpecialRowKind::None
        },
        title: entry.peer.title.clone(),
        username: entry.peer.username.clone(),
        color_index: tg_ui::userpic::decide_color_index(entry.peer.id.bare()),
        verified: false,
        premium: false,
        emoji_status_id: None,
        online: entry.peer.status == "online",
        active_call: false,
        muted: entry.peer.muted,
        pinned: entry.pinned,
        pinned_order: None,
        archived: false,
        folders: entry.folders.clone(),
        folder_tags: Vec::new(),
        last_message_id: MsgId(1),
        last_message: entry.last_message.clone(),
        last_sender: entry.last_sender.clone(),
        last_message_time: MOCK_NOW,
        send_state: if entry.peer.is_saved_messages {
            DialogSendState::Received
        } else {
            DialogSendState::None
        },
        draft: None,
        typing: None,
        badges: DialogBadgesState {
            unread_counter: entry.unread_count as i32,
            unread: entry.unread_count > 0,
            unread_muted: entry.peer.muted,
            ..Default::default()
        },
        is_contact: false,
        media_preview: None,
        has_bot_app_button: false,
    }
}
