//! Virtualized dialogs inner list, Archive row, pinned drag-and-drop reorder, and context menu.
//!
//! Port origin: `Telegram/SourceFiles/dialogs/dialogs_inner_widget.h`,
//! `Telegram/SourceFiles/dialogs/dialogs_inner_widget.cpp`,
//! and `Telegram/SourceFiles/window/window_peer_menu.cpp` (`FillDialogsEntryMenu`).

use std::ops::Range;

use egui::{CursorIcon, Pos2, Rect, ScrollArea, Sense, Ui, Vec2};
use tg_ui::stand_in::PopupMenuStyle;
use tg_ui::tg_style::st;
use tg_ui::widgets::menu::MenuItem;

use crate::dialogs::backend::{
    ArchiveSummary, DialogItem, DialogListQuery, DialogPeerKind, DialogsBackend, SpecialRowKind,
};
use crate::dialogs::entry_menu::{EntryMenu, EntryMenuRow};
use crate::dialogs::layout::{
    COLLAPSED_ARCHIVE_HEIGHT, DialogColors, RowPaintState, paint_archive_row, paint_dialog_row,
    row_style_for,
};
use crate::dialogs::quick_action::{
    DialogHoverQuickAction, DialogSwipeState, QuickDialogActionLabel, paint_quick_action_strip,
    perform_quick_dialog_action, resolve_quick_dialog_label, show_hover_quick_action_pill,
};
use crate::dialogs::strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{FilterId, PeerId};
use tg_core::settings::QuickDialogAction;

/// Number of dialogs requested per pagination page.
pub const DIALOGS_PAGE_SIZE: usize = 80;
/// Threshold in pixels from the bottom edge that triggers loading the next page.
pub const PRELOAD_BOTTOM_MARGIN: f32 = 320.0;

/// Events emitted by [`DialogsInnerWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InnerAction {
    /// User clicked or activated a peer conversation.
    SelectPeer(PeerId),
    /// User clicked the Archive row to enter the Archived Chats view.
    OpenArchive,
    /// User clicked a bot's inline `"Open"` button.
    OpenBotApp(PeerId),
    /// Informational toast emitted after a context menu operation (e.g. Archive / Delete).
    Toast(String),
    /// `"Open in new window"` (`SessionController::showInNewWindow`).
    OpenInNewWindow(PeerId),
    /// `"Block user"` / `"Unblock user"` / `"Block bot"` / `"Restart bot"` (`Filler::addBlockUser`).
    ToggleBlock(PeerId),
    /// Mute submenu "Mute for..." (`PickMuteBox`).
    OpenMuteFor(PeerId),
    /// Mute submenu "Select tone" (`RingtonesBox`).
    OpenSelectTone(PeerId),
    /// "Clear history" (`just_clear`) or "Delete chat" / "Delete and leave" / "Leave": opens
    /// `DeleteMessagesBox` (`ClearHistoryHandler` / `DeleteAndLeaveHandler`); nothing is sent
    /// before the box is confirmed.
    ConfirmDelete {
        /// Chat to clear, delete or leave.
        peer_id: PeerId,
        /// `true` for "Clear history".
        just_clear: bool,
    },
}

/// Runs the chat-list block row for `peer_id`: blocks or unblocks through the same
/// `InfoBackend::toggle_block_user` call as the profile's Block row and returns tdesktop's
/// `"{user} is now blocked"` toast after blocking a user (bots are blocked without a toast,
/// unblocking shows none).
pub fn toggle_block_from_chat_list<B>(backend: &mut B, peer_id: PeerId) -> Option<String>
where
    B: DialogsBackend + tg_app_core::info::backend::InfoBackend + ?Sized,
{
    let item = DialogsBackend::dialog_by_peer(backend, peer_id);
    let blocked = tg_app_core::info::backend::InfoBackend::toggle_block_user(backend, peer_id);
    let user = item.filter(|i| i.kind == DialogPeerKind::User)?;
    let short_name = user.title.split_whitespace().next().unwrap_or(&user.title);
    blocked.then(|| tg_lang::tr::lng_new_contact_block_done(short_name))
}

/// Menu ids of the Mute submenu rows (`FillMuteMenu` in `menu/menu_mute.cpp`).
pub const MUTE_SELECT_TONE_ACTION: &str = "mute_select_tone";
/// "Disable sound" / "Enable sound".
pub const MUTE_SOUND_TOGGLE_ACTION: &str = "mute_sound_toggle";
/// "Mute for..." (opens `PickMuteBox`).
pub const MUTE_FOR_ACTION: &str = "mute_for";
/// "Mute forever".
pub const MUTE_FOREVER_ACTION: &str = "mute_forever";

/// The Mute submenu of an unmuted chat (`FillMuteMenu`): "Select tone", "Disable sound" (or
/// "Enable sound" when the chat is silent), "Mute for...", and the red "Mute forever".
#[must_use]
pub fn mute_submenu_rows(silent: bool) -> Vec<EntryMenuRow> {
    vec![
        EntryMenuRow::action(
            MUTE_SELECT_TONE_ACTION,
            tg_lang::tr::lng_mute_menu_sound_select(),
            Some(st::menu_icon_sound_select),
        ),
        if silent {
            EntryMenuRow::action(
                MUTE_SOUND_TOGGLE_ACTION,
                tg_lang::tr::lng_mute_menu_sound_on(),
                Some(st::menu_icon_sound_on),
            )
        } else {
            EntryMenuRow::action(
                MUTE_SOUND_TOGGLE_ACTION,
                tg_lang::tr::lng_mute_menu_sound_off(),
                Some(st::menu_icon_sound_off),
            )
        },
        EntryMenuRow::action(
            MUTE_FOR_ACTION,
            tg_lang::tr::lng_mute_menu_duration(),
            Some(st::menu_icon_mute_for),
        ),
        EntryMenuRow::attention(
            MUTE_FOREVER_ACTION,
            tg_lang::tr::lng_mute_menu_duration_forever(),
            Some(st::menu_icon_mute),
        ),
    ]
}

/// Menu id of the chat-list block / unblock row.
pub const BLOCK_USER_ACTION: &str = "block_user";
/// Menu id of "Clear history" (opens the confirmation box).
pub const CLEAR_HISTORY_ACTION: &str = "clear_history";
/// Menu id of "Delete chat" / "Delete and leave" / "Leave group" / "Leave channel" (opens the
/// confirmation box).
pub const DELETE_CHAT_ACTION: &str = "delete_chat";

/// The chat-list `"Block user"` row (`Filler::fillContextMenuActions` adds it only for users who
/// are not contacts; `Filler::addBlockUser` skips self, Replies and Verification Codes). The label
/// and icon follow `blockText` and the `menuIconBlock` / `menuIconUnblock` /
/// `menuIconRestartBot` choice.
#[must_use]
pub fn block_user_row(item: &DialogItem, blocked: bool) -> Option<EntryMenuRow> {
    let is_user = matches!(item.kind, DialogPeerKind::User | DialogPeerKind::Bot);
    if !is_user || item.is_contact || item.special != SpecialRowKind::None {
        return None;
    }
    let bot = item.kind == DialogPeerKind::Bot;
    let (label, icon) = match (bot, blocked) {
        (false, false) => (tg_lang::tr::lng_profile_block_user(), st::menu_icon_block),
        (true, false) => (tg_lang::tr::lng_profile_block_bot(), st::menu_icon_block),
        (false, true) => (
            tg_lang::tr::lng_profile_unblock_user(),
            st::menu_icon_unblock,
        ),
        (true, true) => (
            tg_lang::tr::lng_profile_restart_bot(),
            st::menu_icon_restart_bot,
        ),
    };
    Some(EntryMenuRow::action(BLOCK_USER_ACTION, label, Some(icon)))
}

/// Computes the visible `[from, to)` row range for uniform-height virtualized scrolling.
///
/// Exact port of `Ui::RowsInRange` (`dialogs_inner_widget.cpp`).
#[must_use]
pub fn visible_row_range(
    viewport_top: f32,
    viewport_bottom: f32,
    row_height: f32,
    total_rows: usize,
) -> Range<usize> {
    if total_rows == 0 || row_height <= 0.0 || viewport_bottom <= viewport_top {
        return 0..0;
    }
    let from = ((viewport_top / row_height).floor().max(0.0) as usize).min(total_rows);
    let to = ((viewport_bottom / row_height).ceil().max(0.0) as usize).min(total_rows);
    from..to.max(from)
}

/// Computes the visible `[from, to)` range and starting Y offset for variable-height rows
/// (e.g. mixed 62px `DEFAULT` and 72px `TAGGED` dialog rows).
#[must_use]
pub fn variable_visible_row_range(
    viewport_top: f32,
    viewport_bottom: f32,
    heights: &[f32],
) -> (Range<usize>, f32) {
    if heights.is_empty() || viewport_bottom <= viewport_top {
        return (0..0, 0.0);
    }
    let mut y = 0.0_f32;
    let mut from = heights.len();
    let mut start_y = 0.0_f32;
    let mut to = heights.len();
    for (idx, &h) in heights.iter().enumerate() {
        let next_y = y + h;
        if from == heights.len() && next_y > viewport_top {
            from = idx;
            start_y = y;
        }
        if y >= viewport_bottom {
            to = idx;
            break;
        }
        y = next_y;
    }
    if from >= to {
        (0..0, 0.0)
    } else {
        (from..to, start_y)
    }
}

/// Pure-logic state machine for drag-and-drop reordering of pinned chats.
///
/// Exact port of `InnerWidget::updateReorderPinned` (`dialogs_inner_widget.cpp:2978-3064`).
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedDragState {
    /// Peer being dragged.
    pub peer_id: PeerId,
    /// Current logical index inside the pinned slice.
    pub dragging_index: usize,
    /// Pointer Y coordinate at the last swap anchor.
    pub anchor_y: f32,
    /// Visual vertical offset of the dragged row relative to its slot.
    pub visual_offset_y: f32,
    /// Height of the dragged row in logical pixels.
    pub dragging_height: f32,
    /// Whether the pointer moved far enough to count as a drag (suppressing click).
    pub moved: bool,
}

impl PinnedDragState {
    /// Starts a pinned drag gesture at `index` with pointer Y `start_y`.
    #[must_use]
    pub const fn start(peer_id: PeerId, index: usize, start_y: f32, dragging_height: f32) -> Self {
        Self {
            peer_id,
            dragging_index: index,
            anchor_y: start_y,
            visual_offset_y: 0.0,
            dragging_height,
            moved: false,
        }
    }

    /// Crossing threshold matching `dialogs_inner_widget.cpp:2991-2993`:
    /// `max(draggingHeight / 2, neighborHeight - draggingHeight / 2)`.
    #[must_use]
    pub fn cross_threshold(dragging_height: f32, neighbor_height: f32) -> f32 {
        (dragging_height * 0.5).max(neighbor_height - dragging_height * 0.5)
    }

    /// Advances drag position to `pointer_y`, returning all `(from, to)` swaps to commit.
    pub fn update(&mut self, pointer_y: f32, pinned_heights: &[f32]) -> Vec<(usize, usize)> {
        let mut swaps = Vec::new();
        let pinned_count = pinned_heights.len();
        if pinned_count < 2 || self.dragging_index >= pinned_count {
            return swaps;
        }
        if (pointer_y - self.anchor_y).abs() >= 4.0 {
            self.moved = true;
        }

        if pointer_y < self.anchor_y && self.dragging_index > 0 {
            let mut delta = self.anchor_y - pointer_y;
            while self.dragging_index > 0 {
                let target_idx = self.dragging_index - 1;
                let h = pinned_heights[target_idx];
                if delta < Self::cross_threshold(self.dragging_height, h) {
                    break;
                }
                delta -= h;
                swaps.push((self.dragging_index, target_idx));
                self.dragging_index = target_idx;
                self.anchor_y -= h;
            }
        } else if pointer_y > self.anchor_y && self.dragging_index + 1 < pinned_count {
            let mut delta = pointer_y - self.anchor_y;
            while self.dragging_index + 1 < pinned_count {
                let target_idx = self.dragging_index + 1;
                let h = pinned_heights[target_idx];
                if delta < Self::cross_threshold(self.dragging_height, h) {
                    break;
                }
                delta -= h;
                swaps.push((self.dragging_index, target_idx));
                self.dragging_index = target_idx;
                self.anchor_y += h;
            }
        }

        self.visual_offset_y = pointer_y - self.anchor_y;
        swaps
    }
}

/// Target of the active right-click context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextTarget {
    Archive,
    Dialog(PeerId),
}

/// Stateful virtualized dialogs list widget (`Dialogs::InnerWidget`).
#[derive(Debug)]
pub struct DialogsInnerWidget {
    loaded_limit: usize,
    last_list: DialogListQuery,
    keyboard_selected_peer: Option<PeerId>,
    pinned_drag: Option<PinnedDragState>,
    context_menu: EntryMenu,
    context_target: Option<ContextTarget>,
    scroll_offset_y: f32,
    pending_scroll_to_y: Option<f32>,
    /// Animated video avatars of visible rows (`InnerWidget::_videoUserpics`).
    video_userpics: crate::dialogs::video_userpic::DialogVideoUserpicCache,
    /// Configured swipe / middle-click / hover quick action (`CoreSettings::quick_dialog_action`).
    quick_dialog_action: QuickDialogAction,
    /// Active horizontal swipe-to-act gesture on a dialog row.
    swipe_state: Option<DialogSwipeState>,
    /// Drag start pointer position to disambiguate horizontal swipe vs vertical pinned reorder.
    swipe_drag_origin: Option<Pos2>,
    /// Hover quick-action pill fade tracker.
    hover_quick_action: DialogHoverQuickAction,
    /// Reduced-motion flag (`PowerSavingFlags::ANIMATIONS`).
    reduced_motion: bool,
}

impl Default for DialogsInnerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DialogsInnerWidget {
    /// Creates a new virtualized dialogs inner list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            loaded_limit: DIALOGS_PAGE_SIZE,
            last_list: DialogListQuery::Filter(FilterId::ALL),
            keyboard_selected_peer: None,
            pinned_drag: None,
            context_menu: EntryMenu::default(),
            context_target: None,
            scroll_offset_y: 0.0,
            pending_scroll_to_y: None,
            video_userpics: crate::dialogs::video_userpic::DialogVideoUserpicCache::new(),
            quick_dialog_action: QuickDialogAction::Disabled,
            swipe_state: None,
            swipe_drag_origin: None,
            hover_quick_action: DialogHoverQuickAction::default(),
            reduced_motion: false,
        }
    }

    /// Sets the configured dialog quick action (`CoreSettings::quick_dialog_action`).
    pub fn set_quick_dialog_action(&mut self, action: QuickDialogAction) {
        self.quick_dialog_action = action;
        if action == QuickDialogAction::Disabled {
            self.swipe_state = None;
        }
    }

    /// Returns the currently configured dialog quick action.
    #[must_use]
    pub const fn quick_dialog_action(&self) -> QuickDialogAction {
        self.quick_dialog_action
    }

    /// Sets reduced-motion mode (`PowerSavingFlags::ANIMATIONS`).
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
    }

    /// Sets an active swipe preview distance on `peer_id` (used for tests & visual snapshots).
    pub fn set_swipe_preview(&mut self, peer_id: PeerId, reveal_px: f32) {
        let mut s = DialogSwipeState::start(peer_id);
        s.update_drag(reveal_px);
        self.swipe_state = Some(s);
    }

    /// Animated video avatars tracked for visible rows.
    #[must_use]
    pub const fn video_userpics(&self) -> &crate::dialogs::video_userpic::DialogVideoUserpicCache {
        &self.video_userpics
    }

    /// Returns the currently tracked vertical scroll offset.
    #[must_use]
    pub const fn scroll_offset_y(&self) -> f32 {
        self.scroll_offset_y
    }

    /// Requests scrolling smoothly/immediately to the top of the list.
    pub fn scroll_to_top(&mut self) {
        self.pending_scroll_to_y = Some(0.0);
    }

    /// Returns the current keyboard-highlighted peer, if any.
    #[must_use]
    pub const fn keyboard_selected_peer(&self) -> Option<PeerId> {
        self.keyboard_selected_peer
    }

    /// Sets the keyboard-highlighted peer and ensures it is scrolled into view.
    pub fn set_keyboard_selected_peer(&mut self, peer_id: Option<PeerId>) {
        self.keyboard_selected_peer = peer_id;
    }

    /// Returns `true` if a context menu is currently open.
    #[must_use]
    pub const fn is_context_menu_open(&self) -> bool {
        self.context_menu.is_open()
    }

    /// Closes any open context menu.
    pub fn close_context_menu(&mut self) {
        self.context_menu.close();
        self.context_target = None;
    }

    /// Returns the currently loaded pagination window size.
    #[must_use]
    pub const fn loaded_limit(&self) -> usize {
        self.loaded_limit
    }

    /// Resets pagination when switching folders or entering/exiting Archive.
    pub fn ensure_list(&mut self, list: DialogListQuery) {
        if self.last_list != list {
            self.last_list = list;
            self.loaded_limit = DIALOGS_PAGE_SIZE;
            self.pinned_drag = None;
            self.pending_scroll_to_y = Some(0.0);
        }
    }

    /// Resolves the next or previous peer relative to `current` in `list` (`resolveChatNext` / `resolveChatPrevious`).
    #[must_use]
    pub fn resolve_adjacent_peer(
        &self,
        backend: &dyn DialogsBackend,
        list: DialogListQuery,
        current: PeerId,
        delta: isize,
    ) -> Option<PeerId> {
        let slice = backend.dialog_slice(list, 0, self.loaded_limit.max(DIALOGS_PAGE_SIZE));
        if slice.entries.is_empty() {
            return None;
        }
        let pos = slice.entries.iter().position(|d| d.peer_id == current);
        let next_idx = match pos {
            Some(i) => {
                let len = slice.entries.len() as isize;
                (i as isize + delta).rem_euclid(len) as usize
            }
            None => {
                if delta >= 0 {
                    0
                } else {
                    slice.entries.len() - 1
                }
            }
        };
        Some(slice.entries[next_idx].peer_id)
    }

    /// Renders the virtualized dialogs list (Archive row + paginated dialog rows + drag reorder + context menu).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn DialogsBackend,
        list: DialogListQuery,
        active_peer: PeerId,
        narrow: bool,
        show_folder_tags: bool,
        palette: &Palette,
    ) -> Option<InnerAction> {
        self.ensure_list(list);
        let mut action = None;
        let colors = DialogColors::new(palette);
        let now = backend.current_time();
        let anim_time = ui.ctx().input(|i| i.time);

        let show_archive_row = matches!(list, DialogListQuery::Filter(FilterId::ALL));
        let archive_summary: Option<ArchiveSummary> = if show_archive_row {
            backend.archive_summary()
        } else {
            None
        };
        let archive_h = archive_summary.as_ref().map_or(0.0, |a| {
            if a.collapsed {
                COLLAPSED_ARCHIVE_HEIGHT
            } else {
                tg_history::consts::DIALOGS_ROW_HEIGHT as f32
            }
        });

        let mut slice = backend.dialog_slice(list, 0, self.loaded_limit);
        let chats_loading = matches!(list, DialogListQuery::Filter(FilterId::ALL))
            && slice.entries.is_empty()
            && !backend.chats_list_loaded();
        if matches!(list, DialogListQuery::Filter(FilterId::ALL)) {
            let groups = backend.community_groups();
            if groups.iter().any(|g| g.collapsed_in_dialogs) {
                let grouped = crate::dialogs::community::group_dialogs_with_communities(
                    &slice.entries,
                    &groups,
                );
                slice.entries = grouped
                    .into_iter()
                    .map(|row| row.as_dialog_item().clone())
                    .collect();
                slice.pinned_count = slice.entries.iter().take_while(|d| d.pinned).count();
            }
        }
        let row_heights: Vec<f32> = slice
            .entries
            .iter()
            .map(|item| row_style_for(item, show_folder_tags, narrow).height as f32)
            .collect();
        let total_dialogs_h: f32 = row_heights.iter().sum();
        let total_content_h = archive_h + total_dialogs_h;

        let any_typing = slice.entries.iter().any(|d| d.typing.is_some());
        if any_typing {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }

        let width = ui.available_width();
        let mut scroll = ScrollArea::vertical()
            .id_salt(("tg_dialogs_inner_scroll", list))
            .auto_shrink([false, false]);
        if let Some(target_y) = self.pending_scroll_to_y.take() {
            scroll = scroll.vertical_scroll_offset(target_y);
        }

        let mut pending_swaps: Vec<(usize, usize)> = Vec::new();
        let mut pending_quick_action: Option<PeerId> = None;
        let mut open_menu: Option<(ContextTarget, Pos2)> = None;
        let mut should_load_more = false;
        let quick_action_enabled =
            !narrow && self.quick_dialog_action != QuickDialogAction::Disabled;
        if let Some(ref mut swipe) = self.swipe_state {
            if !swipe.sample(anim_time) {
                self.swipe_state = None;
            } else if !swipe.dragging {
                ui.ctx().request_repaint();
            }
        }

        let output = scroll.show_viewport(ui, |ui, viewport| {
            self.scroll_offset_y = viewport.min.y;
            if slice.has_more && viewport.max.y + PRELOAD_BOTTOM_MARGIN >= total_content_h {
                should_load_more = true;
            }

            if chats_loading {
                if !narrow {
                    paint_chats_list_loading(ui, width, &colors);
                }
                return;
            }
            if slice.entries.is_empty() && archive_summary.is_none() {
                if !narrow {
                    ui.add_space(36.0);
                    ui.vertical_centered(|ui| {
                        let empty_msg = match list {
                            DialogListQuery::Archive => strings::EMPTY_ARCHIVED_CHATS.to_owned(),
                            DialogListQuery::Filter(_) => tg_lang::tr::lng_no_chats_filter(),
                        };
                        ui.label(
                            egui::RichText::new(empty_msg)
                                .font(tg_ui::fonts::font_id_from_style(
                                    tg_ui::tg_style::st::dialogs_text_font,
                                ))
                                .color(colors.sub_text_fg()),
                        );
                    });
                }
                return;
            }

            let (content_rect, _) =
                ui.allocate_exact_size(Vec2::new(width, total_content_h.max(1.0)), Sense::hover());
            let origin = content_rect.min;

            // 1. Archive row at top of All Chats if visible in viewport.
            if let Some(summary) = &archive_summary
                && viewport.min.y < archive_h
                && viewport.max.y > 0.0
            {
                let arch_rect = Rect::from_min_size(origin, Vec2::new(width, archive_h));
                let resp = ui.interact(
                    arch_rect,
                    ui.scope_id().with("tg_archive_row"),
                    Sense::click(),
                );
                let arch_label = if summary.unread_count > 0 {
                    format!(
                        "{} ({} unread)",
                        tg_lang::tr::lng_archived_name(),
                        summary.unread_count
                    )
                } else {
                    tg_lang::tr::lng_archived_name()
                };
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, arch_label.clone())
                });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let selected =
                    resp.hovered() || self.context_target == Some(ContextTarget::Archive);
                paint_archive_row(
                    ui.painter(),
                    arch_rect,
                    summary,
                    selected,
                    narrow,
                    palette,
                    &colors,
                );
                if resp.clicked() {
                    action = Some(InnerAction::OpenArchive);
                }
                if resp.secondary_clicked() {
                    let pos = resp.interact_pointer_pos().unwrap_or(arch_rect.center());
                    open_menu = Some((ContextTarget::Archive, pos));
                }
                if narrow {
                    resp.on_hover_text(tg_lang::tr::lng_archived_name());
                }
            }

            // 2. Virtualized visible dialog rows.
            let dialogs_vp_top = (viewport.min.y - archive_h).max(0.0);
            let dialogs_vp_bottom = (viewport.max.y - archive_h).max(0.0);
            let (vis_range, vis_start_y) =
                variable_visible_row_range(dialogs_vp_top, dialogs_vp_bottom, &row_heights);

            let mut deferred_drag_paint: Option<(Rect, usize)> = None;
            let mut cursor_y = archive_h + vis_start_y;

            for idx in vis_range {
                let item = &slice.entries[idx];
                let h = row_heights[idx];
                let row_rect = Rect::from_min_size(
                    Pos2::new(origin.x, origin.y + cursor_y),
                    Vec2::new(width, h),
                );
                cursor_y += h;

                let is_pinned_draggable =
                    item.pinned && slice.pinned_count >= 2 && idx < slice.pinned_count;
                let row_quick_label = if quick_action_enabled {
                    resolve_quick_dialog_label(
                        item,
                        self.quick_dialog_action,
                        item.pinned && idx < slice.pinned_count,
                    )
                } else {
                    QuickDialogActionLabel::Disabled
                };
                let can_swipe_row =
                    quick_action_enabled && row_quick_label != QuickDialogActionLabel::Disabled;
                let sense = if is_pinned_draggable || can_swipe_row {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                };
                let resp = ui.interact(
                    row_rect,
                    ui.scope_id().with(("tg_dialog_row", item.peer_id.0)),
                    sense,
                );
                let row_label = if item.badges.unread_counter > 0 {
                    format!("{} ({} unread)", item.title, item.badges.unread_counter)
                } else if item.badges.unread {
                    format!("{} (unread)", item.title)
                } else {
                    item.title.clone()
                };
                resp.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::Role::Button,
                        true,
                        item.peer_id == active_peer,
                        row_label.clone(),
                    )
                });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }

                // Disambiguate horizontal swipe quick-action vs vertical pinned reorder.
                if resp.drag_started()
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    self.swipe_drag_origin = Some(p);
                    if is_pinned_draggable && !can_swipe_row {
                        self.pinned_drag = Some(PinnedDragState::start(item.peer_id, idx, p.y, h));
                    }
                }
                if resp.dragged()
                    && let (Some(origin_p), Some(cur_p)) =
                        (self.swipe_drag_origin, resp.interact_pointer_pos())
                {
                    let dx = cur_p.x - origin_p.x;
                    let dy = cur_p.y - origin_p.y;
                    let swiping_this = self
                        .swipe_state
                        .as_ref()
                        .is_some_and(|s| s.peer_id == item.peer_id && s.dragging);
                    let pinned_this = self
                        .pinned_drag
                        .as_ref()
                        .is_some_and(|d| d.peer_id == item.peer_id);
                    if !swiping_this && !pinned_this {
                        if can_swipe_row && dx < -6.0 && dx.abs() > dy.abs() {
                            let mut sw = DialogSwipeState::start(item.peer_id);
                            sw.update_drag(-dx);
                            self.swipe_state = Some(sw);
                        } else if is_pinned_draggable && dy.abs() > 4.0 {
                            self.pinned_drag =
                                Some(PinnedDragState::start(item.peer_id, idx, origin_p.y, h));
                        }
                    } else if swiping_this && let Some(sw) = self.swipe_state.as_mut() {
                        sw.update_drag(-dx);
                    }
                }
                if resp.drag_stopped() {
                    self.swipe_drag_origin = None;
                    if let Some(sw) = self.swipe_state.as_mut()
                        && sw.peer_id == item.peer_id
                        && sw.dragging
                        && sw.finish_drag(anim_time, self.reduced_motion)
                    {
                        pending_quick_action = Some(item.peer_id);
                    }
                }
                let is_being_dragged = self
                    .pinned_drag
                    .as_ref()
                    .is_some_and(|d| d.peer_id == item.peer_id);
                if is_being_dragged {
                    if let Some(p) = resp.interact_pointer_pos()
                        && let Some(drag) = self.pinned_drag.as_mut()
                    {
                        let swaps = drag.update(p.y, &row_heights[..slice.pinned_count]);
                        pending_swaps.extend(swaps);
                    }
                    let offset_y = self.pinned_drag.as_ref().map_or(0.0, |d| d.visual_offset_y);
                    deferred_drag_paint = Some((row_rect.translate(Vec2::new(0.0, offset_y)), idx));
                    if resp.drag_stopped() {
                        self.pinned_drag = None;
                    }
                } else {
                    let row_swipe = self
                        .swipe_state
                        .as_ref()
                        .filter(|s| s.peer_id == item.peer_id && s.reveal_px > 0.5)
                        .map(|s| (s.reveal_px, s.armed));
                    let paint_row_rect = if let Some((rev, _)) = row_swipe {
                        Rect::from_min_max(
                            Pos2::new(row_rect.left() - rev, row_rect.top()),
                            Pos2::new(row_rect.right() - rev, row_rect.bottom()),
                        )
                    } else {
                        row_rect
                    };
                    let row_pointer_over = ui.rect_contains_pointer(row_rect);
                    let paint_state = RowPaintState {
                        active: item.peer_id == active_peer,
                        selected: resp.hovered()
                            || row_pointer_over
                            || self.keyboard_selected_peer == Some(item.peer_id)
                            || self.context_target == Some(ContextTarget::Dialog(item.peer_id)),
                        narrow,
                        dragging: false,
                        show_folder_tags,
                        anim_time_secs: anim_time,
                        use_24h: true,
                    };
                    let bot_btn_rect = paint_dialog_row(
                        &ui.painter().with_clip_rect(row_rect),
                        paint_row_rect,
                        item,
                        now,
                        paint_state,
                        palette,
                        &colors,
                    );
                    crate::dialogs::video_userpic::paint_row_video_userpic(
                        ui,
                        &mut self.video_userpics,
                        &*backend,
                        item.peer_id,
                        crate::dialogs::video_userpic::dialog_row_photo_rect(
                            paint_row_rect,
                            item,
                            narrow,
                            show_folder_tags,
                        ),
                    );

                    // Paint swipe strip when row is swiped left, or floating hover pill when hovered.
                    let mut quick_pill_clicked = false;
                    if let Some((rev, armed)) = row_swipe {
                        paint_quick_action_strip(
                            ui.painter(),
                            row_rect,
                            rev,
                            row_quick_label,
                            armed,
                            &colors,
                        );
                    } else if can_swipe_row && !item.has_bot_app_button {
                        let hover_opacity = self.hover_quick_action.opacity_for(
                            item.peer_id,
                            row_pointer_over || resp.hovered(),
                            anim_time,
                            self.reduced_motion,
                        );
                        if self.hover_quick_action.is_animating() {
                            ui.ctx().request_repaint();
                        }
                        if hover_opacity > 0.02
                            && show_hover_quick_action_pill(
                                ui,
                                row_rect,
                                item.peer_id,
                                &item.title,
                                row_quick_label,
                                hover_opacity,
                                &colors,
                            )
                        {
                            quick_pill_clicked = true;
                            pending_quick_action = Some(item.peer_id);
                        }
                    }

                    // Middle-click performs configured quick action immediately (tdesktop parity).
                    if can_swipe_row && resp.middle_clicked() {
                        pending_quick_action = Some(item.peer_id);
                    }

                    let was_dragging =
                        self.pinned_drag.as_ref().is_some_and(|d| d.moved) || row_swipe.is_some();
                    if resp.clicked() && !was_dragging && !quick_pill_clicked {
                        if let Some(btn_r) = bot_btn_rect
                            && resp
                                .interact_pointer_pos()
                                .is_some_and(|p| btn_r.contains(p))
                        {
                            action = Some(InnerAction::OpenBotApp(item.peer_id));
                        } else {
                            self.keyboard_selected_peer = Some(item.peer_id);
                            action = Some(InnerAction::SelectPeer(item.peer_id));
                        }
                    }
                    if resp.secondary_clicked() {
                        let pos = resp.interact_pointer_pos().unwrap_or(row_rect.center());
                        open_menu = Some((ContextTarget::Dialog(item.peer_id), pos));
                    }
                }

                if narrow {
                    resp.on_hover_text(&item.title);
                }
            }

            // Paint the dragged pinned row last so it floats above neighbors (`_aboveIndex`).
            if let Some((drag_rect, idx)) = deferred_drag_paint {
                let item = &slice.entries[idx];
                let paint_state = RowPaintState {
                    active: item.peer_id == active_peer,
                    selected: true,
                    narrow,
                    dragging: true,
                    show_folder_tags,
                    anim_time_secs: anim_time,
                    use_24h: true,
                };
                paint_dialog_row(
                    ui.painter(),
                    drag_rect,
                    item,
                    now,
                    paint_state,
                    palette,
                    &colors,
                );
                crate::dialogs::video_userpic::paint_row_video_userpic(
                    ui,
                    &mut self.video_userpics,
                    &*backend,
                    item.peer_id,
                    crate::dialogs::video_userpic::dialog_row_photo_rect(
                        drag_rect,
                        item,
                        narrow,
                        show_folder_tags,
                    ),
                );
            }
        });
        self.scroll_offset_y = output.state.clamped_offset().y;

        // Commit any pinned swaps produced during drag.
        for (from, to) in pending_swaps {
            backend.reorder_pinned(list, from, to);
        }

        // Execute any swipe / hover-pill / middle-click dialog quick action. "Delete" opens the
        // same confirmation box as the menu (`DeleteAndLeaveHandler`).
        if let Some(peer_id) = pending_quick_action {
            if self.quick_dialog_action == QuickDialogAction::Delete {
                action = Some(InnerAction::ConfirmDelete {
                    peer_id,
                    just_clear: false,
                });
            } else if let Some(toast) =
                perform_quick_dialog_action(backend, peer_id, self.quick_dialog_action, list)
            {
                action = Some(InnerAction::Toast(toast));
            }
        }

        // Grow pagination window when user scrolls near the bottom.
        if should_load_more {
            self.loaded_limit = self.loaded_limit.saturating_add(DIALOGS_PAGE_SIZE);
        }

        // Open right-click context menu if requested.
        if let Some((target, pos)) = open_menu {
            self.context_target = Some(target);
            self.context_menu.open_at(ui.ctx(), pos);
        }

        // Render active context menu (`Filler::fillContextMenuActions` in `window_peer_menu.cpp`).
        if let Some(target) = self.context_target {
            let menu_style = PopupMenuStyle::from_tg(&st::popup_menu_with_icons, &colors);
            let rows = build_context_menu_rows(target, backend, list);
            if let Some(triggered) = self.context_menu.show(
                ui.ctx(),
                ui.scope_id().with("tg_dialogs_row_ctx_menu"),
                &rows,
                &menu_style,
                &colors,
            ) {
                if let (ContextTarget::Dialog(peer_id), "new_window") = (target, triggered.as_str())
                {
                    action = Some(InnerAction::OpenInNewWindow(peer_id));
                } else if let (ContextTarget::Dialog(peer_id), BLOCK_USER_ACTION) =
                    (target, triggered.as_str())
                {
                    action = Some(InnerAction::ToggleBlock(peer_id));
                } else if let (ContextTarget::Dialog(peer_id), MUTE_FOR_ACTION) =
                    (target, triggered.as_str())
                {
                    action = Some(InnerAction::OpenMuteFor(peer_id));
                } else if let (ContextTarget::Dialog(peer_id), MUTE_SELECT_TONE_ACTION) =
                    (target, triggered.as_str())
                {
                    action = Some(InnerAction::OpenSelectTone(peer_id));
                } else if let (
                    ContextTarget::Dialog(peer_id),
                    CLEAR_HISTORY_ACTION | DELETE_CHAT_ACTION,
                ) = (target, triggered.as_str())
                {
                    action = Some(InnerAction::ConfirmDelete {
                        peer_id,
                        just_clear: triggered == CLEAR_HISTORY_ACTION,
                    });
                } else if let Some(toast) =
                    execute_context_action(&triggered, target, backend, list)
                {
                    action = Some(InnerAction::Toast(toast));
                }
            }
            if !self.context_menu.is_open() {
                self.context_target = None;
            }
        }

        action
    }
}

/// Draws tdesktop's chat-list loading state: `lng_contacts_loading` in `st::dialogsEmptyLabel`,
/// `dialogsEmptySkip` from each side and centred in the first `dialogsEmptyHeight` pixels
/// (`InnerWidget::refreshEmpty` / `resizeEmpty`, `dialogs_inner_widget.cpp:5282,5372-5377`).
fn paint_chats_list_loading(ui: &mut Ui, width: f32, colors: &DialogColors<'_>) {
    let skip = st::dialogs_empty_skip.0 as f32;
    let area_h = st::dialogs_empty_height.0 as f32;
    let label = &st::dialogs_empty_label;
    let galley = ui.painter().layout(
        tg_lang::tr::lng_contacts_loading(),
        tg_ui::fonts::font_id_from_style(label.style.font),
        colors.sub_text_fg(),
        (width - 2.0 * skip).max(label.min_width.0 as f32),
    );
    let (area, _) = ui.allocate_exact_size(Vec2::new(width, area_h), Sense::hover());
    let pos = Pos2::new(
        (area.center().x - galley.size().x / 2.0).round(),
        (area.top() + (area_h - galley.size().y) / 2.0).round(),
    );
    ui.painter().galley(pos, galley, colors.sub_text_fg());
}

/// Builds the dialog row context menu items for `peer_id` in `list`
/// (`Filler::fillContextMenuActions` in `window_peer_menu.cpp`), flattened to
/// [`MenuItem`]s (submenu rows are listed after their parent).
#[must_use]
pub fn dialog_context_menu_items(
    peer_id: PeerId,
    backend: &dyn DialogsBackend,
    list: DialogListQuery,
) -> Vec<MenuItem> {
    fn flatten(rows: &[EntryMenuRow], out: &mut Vec<MenuItem>) {
        for row in rows {
            match row {
                EntryMenuRow::Separator | EntryMenuRow::ExpandedSeparator => {
                    out.push(MenuItem::Separator);
                }
                EntryMenuRow::Item(item) => {
                    let mut entry = MenuItem::action(item.id.clone(), item.label.clone());
                    if let Some(icon) = item.icon {
                        entry = entry.with_style_icon(icon);
                    }
                    if item.attention {
                        entry = entry.attention();
                    }
                    if !item.submenu.is_empty() {
                        entry = entry.with_submenu();
                    }
                    out.push(entry);
                    flatten(&item.submenu, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    flatten(
        &build_context_menu_rows(ContextTarget::Dialog(peer_id), backend, list),
        &mut out,
    );
    out
}

/// Builds the chats-list context menu rows for `peer_id` in `list`
/// (`Filler::fillContextMenuActions` in `window_peer_menu.cpp`).
#[must_use]
pub fn dialog_context_menu_rows(
    peer_id: PeerId,
    backend: &dyn DialogsBackend,
    list: DialogListQuery,
) -> Vec<EntryMenuRow> {
    build_context_menu_rows(ContextTarget::Dialog(peer_id), backend, list)
}

fn build_context_menu_rows(
    target: ContextTarget,
    backend: &dyn DialogsBackend,
    list: DialogListQuery,
) -> Vec<EntryMenuRow> {
    match target {
        ContextTarget::Archive => {
            let collapsed = backend.archive_collapsed();
            vec![
                EntryMenuRow::action(
                    "archive_toggle_collapse",
                    if collapsed {
                        tg_lang::tr::lng_context_archive_expand()
                    } else {
                        tg_lang::tr::lng_context_archive_collapse()
                    },
                    Some(if collapsed {
                        st::menu_icon_expand
                    } else {
                        st::menu_icon_collapse
                    }),
                ),
                EntryMenuRow::action(
                    "archive_mark_read",
                    tg_lang::tr::lng_context_mark_read_all(),
                    Some(st::menu_icon_mark_read),
                ),
            ]
        }
        ContextTarget::Dialog(peer_id) => {
            let Some(item) = backend.dialog_by_peer(peer_id) else {
                return Vec::new();
            };
            let is_saved = item.special == SpecialRowKind::SavedMessages;
            let is_promoted = backend
                .promo_data()
                .is_some_and(|promo| promo.promo_peer == Some(peer_id));
            let mut rows = Vec::new();

            // `addNewWindow` + `AddSeparatorAndShiftUp`.
            rows.push(EntryMenuRow::action(
                "new_window",
                tg_lang::tr::lng_context_new_window(),
                Some(st::menu_icon_new_window),
            ));
            rows.push(EntryMenuRow::ExpandedSeparator);

            if is_promoted {
                rows.push(EntryMenuRow::action(
                    "hide_promo",
                    tg_lang::tr::lng_context_hide_psa(),
                    Some(st::menu_icon_remove),
                ));
            }

            // `addToggleArchive` (`CanArchive`: never Saved Messages).
            if !is_saved {
                rows.push(EntryMenuRow::action(
                    "toggle_archive",
                    if item.archived {
                        tg_lang::tr::lng_archived_remove()
                    } else {
                        tg_lang::tr::lng_archived_add()
                    },
                    Some(if item.archived {
                        st::menu_icon_unarchive
                    } else {
                        st::menu_icon_archive
                    }),
                ));
            }

            // `addTogglePin`.
            let slice = backend.dialog_slice(list, 0, 20);
            let is_pinned_here = slice
                .entries
                .iter()
                .take(slice.pinned_count)
                .any(|d| d.peer_id == peer_id);
            rows.push(EntryMenuRow::action(
                "toggle_pin",
                if is_pinned_here {
                    tg_lang::tr::lng_context_unpin_from_top()
                } else {
                    tg_lang::tr::lng_context_pin_to_top()
                },
                Some(if is_pinned_here {
                    st::menu_icon_unpin
                } else {
                    st::menu_icon_pin
                }),
            ));

            // `addToggleMuteSubmenu` (`PeerMenuAddMuteSubmenuAction`; never for self).
            if !is_saved {
                if item.muted {
                    rows.push(EntryMenuRow::action(
                        "unmute",
                        tg_lang::tr::lng_context_unmute(),
                        Some(st::menu_icon_unmute),
                    ));
                } else {
                    let silent = backend.dialog_notify_sound(peer_id).none;
                    rows.push(EntryMenuRow::submenu(
                        tg_lang::tr::lng_context_mute(),
                        Some(if silent {
                            st::menu_icon_silent
                        } else {
                            st::menu_icon_mute
                        }),
                        mute_submenu_rows(silent),
                    ));
                }
            }

            // `addToggleUnreadMark`.
            rows.push(EntryMenuRow::action(
                "toggle_read",
                if item.badges.unread {
                    tg_lang::tr::lng_context_mark_read()
                } else {
                    tg_lang::tr::lng_context_mark_unread()
                },
                Some(if item.badges.unread {
                    st::menu_icon_mark_read
                } else {
                    st::menu_icon_mark_unread
                }),
            ));

            // `addToggleFolder` -> `FillChooseFilterMenu`.
            let folders: Vec<_> = backend
                .chat_folders()
                .into_iter()
                .filter(|f| f.id != FilterId::ALL)
                .collect();
            if !folders.is_empty() {
                let sub = folders
                    .iter()
                    .map(|f| {
                        EntryMenuRow::action(
                            format!("folder_toggle:{}", f.id.0),
                            f.title.clone(),
                            item.folders
                                .contains(&f.id)
                                .then_some(st::media_player_menu_check),
                        )
                    })
                    .collect();
                rows.push(EntryMenuRow::submenu(
                    tg_lang::tr::lng_filters_menu_add(),
                    Some(st::menu_icon_add_to_folder),
                    sub,
                ));
            }

            // `if (!user->isContact()) addBlockUser();` (never self, Replies or Verification Codes).
            if let Some(row) = block_user_row(&item, backend.is_peer_blocked(peer_id)) {
                rows.push(row);
            }

            // `addClearHistory` (channels only when the user can delete messages).
            if item.kind != DialogPeerKind::Channel {
                rows.push(EntryMenuRow::action(
                    CLEAR_HISTORY_ACTION,
                    tg_lang::tr::lng_profile_clear_history(),
                    Some(st::menu_icon_clear),
                ));
            }
            // `addDeleteChat` (users: "Delete chat", basic groups: "Delete and leave") /
            // `addLeaveChat` (supergroups and channels).
            let basic_group = peer_id.kind() == tg_app_core::stand_in::types::PeerKind::Group;
            let (label, icon) = match item.kind {
                DialogPeerKind::User | DialogPeerKind::Bot => (
                    tg_lang::tr::lng_profile_delete_conversation(),
                    st::menu_icon_delete_attention,
                ),
                DialogPeerKind::Group | DialogPeerKind::Forum if basic_group => (
                    tg_lang::tr::lng_profile_clear_and_exit(),
                    st::menu_icon_delete_attention,
                ),
                DialogPeerKind::Group | DialogPeerKind::Forum => (
                    tg_lang::tr::lng_profile_leave_group(),
                    st::menu_icon_leave_attention,
                ),
                DialogPeerKind::Channel => (
                    tg_lang::tr::lng_profile_leave_channel(),
                    st::menu_icon_leave_attention,
                ),
            };
            rows.push(EntryMenuRow::attention(
                DELETE_CHAT_ACTION,
                label,
                Some(icon),
            ));
            rows
        }
    }
}

fn execute_context_action(
    action_id: &str,
    target: ContextTarget,
    backend: &mut dyn DialogsBackend,
    list: DialogListQuery,
) -> Option<String> {
    match target {
        ContextTarget::Archive => match action_id {
            "archive_toggle_collapse" => {
                let next = !backend.archive_collapsed();
                backend.set_archive_collapsed(next);
                Some(if next {
                    strings::TOAST_ARCHIVE_COLLAPSED.to_owned()
                } else {
                    strings::TOAST_ARCHIVE_EXPANDED.to_owned()
                })
            }
            "archive_mark_read" => {
                backend.mark_archive_read();
                Some(strings::TOAST_ARCHIVE_MARKED_READ.to_owned())
            }
            _ => None,
        },
        ContextTarget::Dialog(peer_id) => {
            if let Some(fid_str) = action_id.strip_prefix("folder_toggle:")
                && let Ok(fid_num) = fid_str.parse::<i32>()
            {
                let fid = FilterId(fid_num);
                let already = backend
                    .dialog_by_peer(peer_id)
                    .is_some_and(|d| d.folders.contains(&fid));
                if already {
                    backend.remove_peer_from_folder(peer_id, fid);
                    return Some(strings::TOAST_REMOVED_FROM_FOLDER.to_owned());
                }
                backend.add_peer_to_folder(peer_id, fid);
                return Some(strings::TOAST_ADDED_TO_FOLDER.to_owned());
            }
            match action_id {
                MUTE_FOREVER_ACTION | "unmute" => {
                    let seconds = if action_id == "unmute" { 0 } else { i32::MAX };
                    match backend.mute_dialog_for(peer_id, seconds) {
                        Ok(true) => Some(strings::TOAST_MUTED.to_owned()),
                        Ok(false) => Some(strings::TOAST_UNMUTED.to_owned()),
                        Err(_) => {
                            let muted = backend.toggle_mute_dialog(peer_id);
                            Some(if muted {
                                strings::TOAST_MUTED.to_owned()
                            } else {
                                strings::TOAST_UNMUTED.to_owned()
                            })
                        }
                    }
                }
                MUTE_SOUND_TOGGLE_ACTION => {
                    let silent = backend.dialog_notify_sound(peer_id).none;
                    backend.set_dialog_sound_enabled(peer_id, silent).err()
                }
                "hide_promo" => match backend.hide_promo_data(peer_id) {
                    Ok(_) => Some("Announcement hidden".to_owned()),
                    Err(err) => Some(err),
                },
                "toggle_archive" => {
                    let archived = backend.toggle_archive(peer_id);
                    Some(if archived {
                        tg_lang::tr::lng_archived_added()
                    } else {
                        tg_lang::tr::lng_archived_removed()
                    })
                }
                "toggle_pin" => {
                    let pinned = backend.toggle_pin(peer_id, list);
                    Some(if pinned {
                        strings::TOAST_PINNED_TO_TOP.to_owned()
                    } else {
                        strings::TOAST_UNPINNED.to_owned()
                    })
                }
                "toggle_mute" => {
                    let muted = backend.toggle_mute_dialog(peer_id);
                    Some(if muted {
                        strings::TOAST_MUTED.to_owned()
                    } else {
                        strings::TOAST_UNMUTED.to_owned()
                    })
                }
                "toggle_read" => {
                    let was_unread = backend
                        .dialog_by_peer(peer_id)
                        .is_some_and(|d| d.badges.unread);
                    if was_unread {
                        backend.mark_dialog_read(peer_id);
                        Some(strings::TOAST_MARKED_READ.to_owned())
                    } else {
                        backend.mark_dialog_unread(peer_id);
                        Some(strings::TOAST_MARKED_UNREAD.to_owned())
                    }
                }
                _ => None,
            }
        }
    }
}
