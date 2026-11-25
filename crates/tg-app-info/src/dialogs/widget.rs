//! Full stateful dialogs column widget: top search bar, Archive subsection bar, horizontal
//! folder tabs, search view, virtualized list, scroll-to-top button, and keyboard navigation.
//!
//! Port origin: `Telegram/SourceFiles/dialogs/dialogs_widget.h` and
//! `Telegram/SourceFiles/dialogs/dialogs_widget.cpp`.

use egui::{
    Align2, CornerRadius, CursorIcon, Frame, Id, Key, Margin, Pos2, Rect, Sense, Stroke, TextEdit,
    Ui, Vec2,
};
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::st;
use tg_ui::theme::Colors;

use crate::dialogs::backend::{DialogListQuery, DialogsBackend};
use crate::dialogs::filters_menu::{
    FiltersMenuAction, FiltersMenuWidget, show_horizontal_folder_tabs,
};
use crate::dialogs::inner::{DialogsInnerWidget, InnerAction};
use crate::dialogs::layout::{DialogColors, paint_semibold_text};
use crate::dialogs::search::{
    DialogsSearchState, HashOrCashtag, SearchKeyboardItem, SearchViewAction,
    collect_search_keyboard_items_with_sponsored, fix_hashtag_search_query,
    is_hash_or_cashtag_search_query, paint_search_scope_chip, show_search_body,
    show_search_header_tabs,
};
use crate::dialogs::strings;
use crate::dialogs::suggestions::{
    KEY_BIRTHDAY_CONTACTS_TODAY, PromoDataSnapshot, PromoSuggestion, PromoSuggestionsState,
    SuggestionAction, render_top_bar_suggestion,
};
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{FilterId, MsgId, PeerId};

/// High-level events emitted by [`DialogsPanel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogsPanelAction {
    /// Open the main navigation drawer.
    OpenMainMenu,
    /// Open a conversation at its unread / latest message.
    SelectPeer(PeerId),
    /// Open a conversation and jump to a specific message (e.g. from message search).
    SelectMessage {
        /// Target peer.
        peer_id: PeerId,
        /// Target message ID.
        msg_id: MsgId,
    },
    /// Active folder filter changed (via click, tabs, or `Ctrl+1..9`).
    SelectFolder(FilterId),
    /// User clicked the "Edit" button in the vertical folders sidebar.
    OpenFoldersSettings,
    /// User clicked a bot's inline "Open" action button.
    OpenBotApp(PeerId),
    /// User clicked a top-bar promo suggestion banner (`dialogs/suggestions/*`).
    ActivateSuggestion(PromoSuggestion),
    /// Informational toast message.
    Toast(String),
    /// Open a conversation in a separate window (`"Open in new window"`).
    OpenInNewWindow(PeerId),
    /// Block or unblock a user / bot from the chat-list menu (`Filler::addBlockUser`).
    ToggleBlock(PeerId),
    /// Mute submenu "Mute for..." (`PickMuteBox`).
    OpenMuteFor(PeerId),
    /// Mute submenu "Select tone" (`RingtonesBox`).
    OpenSelectTone(PeerId),
    /// Open the `DeleteMessagesBox` confirmation for "Clear history" (`just_clear`) or
    /// "Delete chat" / "Delete and leave" / "Leave" from the chat-list menu.
    ConfirmDelete {
        /// Chat to clear, delete or leave.
        peer_id: PeerId,
        /// `true` for "Clear history".
        just_clear: bool,
    },
}

/// Keyboard command mapped from tdesktop's `Shortcuts::Command` (`dialogs_inner_widget.cpp:6484-6620`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogsShortcut {
    /// Move selection up (`Up`).
    MoveUp,
    /// Move selection down (`Down`).
    MoveDown,
    /// Jump one page up (`PageUp`).
    PageUp,
    /// Jump one page down (`PageDown`).
    PageDown,
    /// Jump to first dialog (`Home` / `Ctrl+Home`).
    FirstChat,
    /// Jump to last dialog (`End` / `Ctrl+End`).
    LastChat,
    /// Open highlighted row (`Enter`).
    ActivateSelected,
    /// Switch to next chat (`Ctrl+Tab` or `Alt+Down`).
    NextChat,
    /// Switch to previous chat (`Ctrl+Shift+Tab` or `Alt+Up`).
    PrevChat,
    /// Switch to folder slot `0..=8` (`Ctrl+1`..`Ctrl+9`; slot 8 selects the last folder).
    SelectFolderSlot(usize),
    /// Jump to Saved Messages (`Ctrl+0`).
    OpenSavedMessages,
    /// Focus the search input (`Ctrl+F`).
    FocusSearch,
    /// Cancel search / exit archive / close context menu (`Escape`).
    Escape,
}

/// Translates egui key state into a [`DialogsShortcut`] when dialogs have keyboard focus.
#[must_use]
pub fn detect_dialogs_shortcut(
    ctx: &egui::Context,
    search_has_focus: bool,
) -> Option<DialogsShortcut> {
    ctx.input(|i| {
        let ctrl = i.modifiers.ctrl || i.modifiers.command;
        let alt = i.modifiers.alt;
        let shift = i.modifiers.shift;

        if ctrl && i.key_pressed(Key::F) {
            return Some(DialogsShortcut::FocusSearch);
        }
        if ctrl && i.key_pressed(Key::Num0) {
            return Some(DialogsShortcut::OpenSavedMessages);
        }
        if ctrl && !alt {
            let folder_keys = [
                (Key::Num1, 0),
                (Key::Num2, 1),
                (Key::Num3, 2),
                (Key::Num4, 3),
                (Key::Num5, 4),
                (Key::Num6, 5),
                (Key::Num7, 6),
                (Key::Num8, 7),
                (Key::Num9, 8),
            ];
            for (k, slot) in folder_keys {
                if i.key_pressed(k) {
                    return Some(DialogsShortcut::SelectFolderSlot(slot));
                }
            }
        }
        if (ctrl && i.key_pressed(Key::Tab)) || (alt && i.key_pressed(Key::ArrowDown)) {
            return Some(if shift {
                DialogsShortcut::PrevChat
            } else {
                DialogsShortcut::NextChat
            });
        }
        if alt && i.key_pressed(Key::ArrowUp) {
            return Some(DialogsShortcut::PrevChat);
        }
        if i.key_pressed(Key::Escape) {
            return Some(DialogsShortcut::Escape);
        }
        if i.key_pressed(Key::ArrowDown) {
            return Some(DialogsShortcut::MoveDown);
        }
        if i.key_pressed(Key::ArrowUp) {
            return Some(DialogsShortcut::MoveUp);
        }
        if i.key_pressed(Key::PageDown) {
            return Some(DialogsShortcut::PageDown);
        }
        if i.key_pressed(Key::PageUp) {
            return Some(DialogsShortcut::PageUp);
        }
        if i.key_pressed(Key::Enter) {
            return Some(DialogsShortcut::ActivateSelected);
        }
        if !search_has_focus {
            if i.key_pressed(Key::Home) {
                return Some(DialogsShortcut::FirstChat);
            }
            if i.key_pressed(Key::End) {
                return Some(DialogsShortcut::LastChat);
            }
        }
        None
    })
}

/// Maps a 0-based `Ctrl+1..9` slot to the corresponding folder ID, matching
/// `dialogs_inner_widget.cpp:6565-6572` (slot 8 / `Ctrl+9` selects the last folder).
#[must_use]
pub fn resolve_folder_slot(
    folders: &[crate::dialogs::backend::ChatFolder],
    slot: usize,
) -> Option<FilterId> {
    if folders.is_empty() {
        return None;
    }
    let idx = if slot >= 8 {
        folders.len() - 1
    } else {
        slot.min(folders.len() - 1)
    };
    Some(folders[idx].id)
}

/// Complete stateful dialogs widget (`Dialogs::Widget`) integrating search, folders, archive,
/// virtualized rows, drag-and-drop pinned reorder, context menu, and keyboard shortcuts.
#[derive(Debug)]
pub struct DialogsPanel {
    /// Active chat folder filter (`FilterId::ALL` by default).
    pub active_filter: FilterId,
    /// `true` when viewing inside the Archived Chats subsection.
    pub opened_archive: bool,
    /// Current search query string.
    pub search_query: String,
    /// `true` while search mode (focused suggestions or active query) is displayed.
    pub search_active: bool,
    /// Show folder filter tag pills beneath dialog previews (`DialogRowStyle::TAGGED`).
    pub show_folder_tags: bool,
    /// Search tab, type filter, and suggestion state.
    pub search_state: DialogsSearchState,
    /// Vertical folders sidebar widget (`Window::FiltersMenu`).
    pub filters_sidebar: FiltersMenuWidget,
    /// Virtualized dialogs inner list (`Dialogs::InnerWidget`).
    pub inner: DialogsInnerWidget,
    /// Top-bar promo suggestions banner state (`dialogs/suggestions/*`).
    pub promo_suggestions: PromoSuggestionsState,
    /// Configured quick dialog swipe/hover/middle-click action (`CoreSettings::quick_dialog_action`).
    pub quick_dialog_action: tg_core::settings::QuickDialogAction,
    /// Reduced-motion flag (`PowerSavingFlags::ANIMATIONS`).
    pub reduced_motion: bool,
    last_promo_snapshot: Option<PromoDataSnapshot>,
    last_birthdays_date: Option<(i32, u8, u8)>,
    request_search_focus: bool,
}

/// `dialogsMenuToggle` width / height.
const MENU_TOGGLE_SIZE: f32 = 40.0;
/// `dialogsFilter.heightMin`.
const SEARCH_FIELD_HEIGHT: f32 = 35.0;

impl Default for DialogsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl DialogsPanel {
    /// Creates a new [`DialogsPanel`] initialized to `All Chats`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_filter: FilterId::ALL,
            opened_archive: false,
            search_query: String::new(),
            search_active: false,
            show_folder_tags: false,
            search_state: DialogsSearchState::default(),
            filters_sidebar: FiltersMenuWidget::new(),
            inner: DialogsInnerWidget::new(),
            promo_suggestions: PromoSuggestionsState::new(),
            quick_dialog_action: tg_core::settings::QuickDialogAction::Disabled,
            reduced_motion: false,
            last_promo_snapshot: None,
            last_birthdays_date: None,
            request_search_focus: false,
        }
    }

    /// Forces a fresh `DialogsBackend::contact_birthdays` query on the next render frame.
    pub fn invalidate_contact_birthdays_cache(&mut self) {
        self.last_birthdays_date = None;
    }

    /// Applies relevant [`tg_core::settings::CoreSettings`] fields (quick dialog action and
    /// power-saving reduced motion) to the dialogs column.
    pub fn apply_core_settings(&mut self, settings: &tg_core::settings::CoreSettings) {
        self.quick_dialog_action = settings.quick_dialog_action;
        self.reduced_motion =
            settings.is_power_saving_on(tg_core::settings::PowerSavingFlags::ANIMATIONS);
        self.inner.set_quick_dialog_action(self.quick_dialog_action);
        self.inner.set_reduced_motion(self.reduced_motion);
    }

    /// Returns the currently displayed [`DialogListQuery`] (`Archive` or `Filter(active_filter)`).
    #[must_use]
    pub const fn current_list(&self) -> DialogListQuery {
        if self.opened_archive {
            DialogListQuery::Archive
        } else {
            DialogListQuery::Filter(self.active_filter)
        }
    }

    /// Cancels search mode and clears the query, returning `true` if search was active.
    pub fn cancel_search(&mut self) -> bool {
        if !self.search_active
            && self.search_query.is_empty()
            && self.search_state.in_peer.is_none()
        {
            return false;
        }
        self.search_query.clear();
        self.search_active = false;
        self.search_state.reset();
        true
    }

    /// Handles a [`DialogsShortcut`] against `backend` and returns any resulting [`DialogsPanelAction`].
    pub fn apply_shortcut(
        &mut self,
        shortcut: DialogsShortcut,
        backend: &mut dyn DialogsBackend,
        active_peer: PeerId,
    ) -> Option<DialogsPanelAction> {
        match shortcut {
            DialogsShortcut::FocusSearch => {
                self.search_active = true;
                self.request_search_focus = true;
                None
            }
            DialogsShortcut::Escape => {
                if self.inner.is_context_menu_open() {
                    self.inner.close_context_menu();
                    return None;
                }
                if self.cancel_search() {
                    return None;
                }
                if self.opened_archive {
                    self.opened_archive = false;
                }
                None
            }
            DialogsShortcut::OpenSavedMessages => {
                let saved = backend.saved_messages_peer_id();
                self.inner.set_keyboard_selected_peer(Some(saved));
                Some(DialogsPanelAction::SelectPeer(saved))
            }
            DialogsShortcut::SelectFolderSlot(slot) => {
                let folders = backend.chat_folders();
                let fid = resolve_folder_slot(&folders, slot)?;
                self.opened_archive = false;
                self.active_filter = fid;
                Some(DialogsPanelAction::SelectFolder(fid))
            }
            DialogsShortcut::NextChat => {
                let next = self.inner.resolve_adjacent_peer(
                    backend,
                    self.current_list(),
                    active_peer,
                    1,
                )?;
                self.inner.set_keyboard_selected_peer(Some(next));
                Some(DialogsPanelAction::SelectPeer(next))
            }
            DialogsShortcut::PrevChat => {
                let prev = self.inner.resolve_adjacent_peer(
                    backend,
                    self.current_list(),
                    active_peer,
                    -1,
                )?;
                self.inner.set_keyboard_selected_peer(Some(prev));
                Some(DialogsPanelAction::SelectPeer(prev))
            }
            DialogsShortcut::MoveUp
            | DialogsShortcut::MoveDown
            | DialogsShortcut::PageUp
            | DialogsShortcut::PageDown
            | DialogsShortcut::FirstChat
            | DialogsShortcut::LastChat
            | DialogsShortcut::ActivateSelected => {
                if self.search_active {
                    self.apply_search_keyboard(shortcut, backend)
                } else {
                    self.apply_list_keyboard(shortcut, backend, active_peer)
                }
            }
        }
    }

    fn apply_search_keyboard(
        &mut self,
        shortcut: DialogsShortcut,
        backend: &mut dyn DialogsBackend,
    ) -> Option<DialogsPanelAction> {
        let req = self.search_state.build_request(&self.search_query);
        let results = backend.search_dialogs(&req);
        let recent = backend.recent_search_peers();
        let items = collect_search_keyboard_items_with_sponsored(
            &self.search_query,
            &results,
            &self.search_state.sponsored_peers,
            &recent,
        );

        match shortcut {
            DialogsShortcut::MoveDown => {
                self.search_state.move_keyboard_selection(1, items.len());
                None
            }
            DialogsShortcut::MoveUp => {
                self.search_state.move_keyboard_selection(-1, items.len());
                None
            }
            DialogsShortcut::PageDown => {
                self.search_state.move_keyboard_selection(6, items.len());
                None
            }
            DialogsShortcut::PageUp => {
                self.search_state.move_keyboard_selection(-6, items.len());
                None
            }
            DialogsShortcut::FirstChat => {
                if !items.is_empty() {
                    self.search_state.keyboard_index = Some(0);
                }
                None
            }
            DialogsShortcut::LastChat => {
                if !items.is_empty() {
                    self.search_state.keyboard_index = Some(items.len() - 1);
                }
                None
            }
            DialogsShortcut::ActivateSelected => {
                let idx = self.search_state.keyboard_index.unwrap_or(0);
                let chosen = items.get(idx)?.clone();
                match chosen {
                    SearchKeyboardItem::Hashtag(tag) => {
                        self.search_query = format!("{tag} ");
                        self.search_state.keyboard_index = None;
                        None
                    }
                    SearchKeyboardItem::LocalChat(pid)
                    | SearchKeyboardItem::SponsoredPeer(pid)
                    | SearchKeyboardItem::GlobalPeer(pid)
                    | SearchKeyboardItem::RecentPeer(pid) => {
                        backend.add_recent_search_peer(pid);
                        Some(DialogsPanelAction::SelectPeer(pid))
                    }
                    SearchKeyboardItem::Message { peer_id, msg_id } => {
                        backend.add_recent_search_peer(peer_id);
                        Some(DialogsPanelAction::SelectMessage { peer_id, msg_id })
                    }
                }
            }
            _ => None,
        }
    }

    fn apply_list_keyboard(
        &mut self,
        shortcut: DialogsShortcut,
        backend: &dyn DialogsBackend,
        active_peer: PeerId,
    ) -> Option<DialogsPanelAction> {
        let slice = backend.dialog_slice(self.current_list(), 0, self.inner.loaded_limit());
        if slice.entries.is_empty() {
            return None;
        }
        let current_peer = self.inner.keyboard_selected_peer().unwrap_or(active_peer);
        let cur_idx = slice.entries.iter().position(|d| d.peer_id == current_peer);

        match shortcut {
            DialogsShortcut::MoveDown => {
                let next = cur_idx.map_or(0, |i| (i + 1).min(slice.entries.len() - 1));
                self.inner
                    .set_keyboard_selected_peer(Some(slice.entries[next].peer_id));
                None
            }
            DialogsShortcut::MoveUp => {
                let prev = cur_idx.map_or(0, |i| i.saturating_sub(1));
                self.inner
                    .set_keyboard_selected_peer(Some(slice.entries[prev].peer_id));
                None
            }
            DialogsShortcut::PageDown => {
                let next = cur_idx.map_or(0, |i| (i + 8).min(slice.entries.len() - 1));
                self.inner
                    .set_keyboard_selected_peer(Some(slice.entries[next].peer_id));
                None
            }
            DialogsShortcut::PageUp => {
                let prev = cur_idx.map_or(0, |i| i.saturating_sub(8));
                self.inner
                    .set_keyboard_selected_peer(Some(slice.entries[prev].peer_id));
                None
            }
            DialogsShortcut::FirstChat => {
                self.inner
                    .set_keyboard_selected_peer(Some(slice.entries[0].peer_id));
                None
            }
            DialogsShortcut::LastChat => {
                let last = slice.entries.len() - 1;
                self.inner
                    .set_keyboard_selected_peer(Some(slice.entries[last].peer_id));
                None
            }
            DialogsShortcut::ActivateSelected => {
                let pid = self
                    .inner
                    .keyboard_selected_peer()
                    .or_else(|| slice.entries.first().map(|d| d.peer_id))?;
                Some(DialogsPanelAction::SelectPeer(pid))
            }
            _ => None,
        }
    }

    /// Renders the vertical folders sidebar (`Window::FiltersMenu`) using `backend`.
    pub fn show_vertical_sidebar(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn DialogsBackend,
        palette: &Palette,
    ) -> Option<DialogsPanelAction> {
        let folders = backend.chat_folders();
        let act = self
            .filters_sidebar
            .show(ui, &folders, self.active_filter, palette)?;
        match act {
            FiltersMenuAction::OpenMainMenu => Some(DialogsPanelAction::OpenMainMenu),
            FiltersMenuAction::SelectFolder(fid) => {
                self.opened_archive = false;
                if self.active_filter == fid {
                    self.inner.scroll_to_top();
                } else {
                    self.active_filter = fid;
                }
                Some(DialogsPanelAction::SelectFolder(fid))
            }
            FiltersMenuAction::OpenFoldersSettings | FiltersMenuAction::EditFolder(_) => {
                Some(DialogsPanelAction::OpenFoldersSettings)
            }
            FiltersMenuAction::MarkFolderRead(fid) => {
                backend.mark_folder_read(fid);
                Some(DialogsPanelAction::Toast(
                    strings::TOAST_FOLDER_MARKED_READ.to_owned(),
                ))
            }
            FiltersMenuAction::ReorderFolders(order) => {
                backend.reorder_folders(&order);
                None
            }
        }
    }

    /// Renders the dialogs column (search/archive top bar, optional horizontal tabs, search or
    /// virtualized list, and floating scroll-to-top button).
    #[allow(clippy::too_many_arguments)]
    pub fn show_column(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn DialogsBackend,
        active_peer: PeerId,
        show_horizontal_folders: bool,
        compact_small_column: bool,
        show_hamburger: bool,
        enable_shortcuts: bool,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<DialogsPanelAction> {
        ensure_tdesktop_fonts(ui.ctx());
        let colors = DialogColors::new(palette);
        let mut action = None;
        let full_rect = ui.available_rect_before_wrap();
        ui.painter()
            .rect_filled(full_rect, CornerRadius::ZERO, colors.window_bg());
        ui.spacing_mut().item_spacing = Vec2::ZERO;

        let search_input_id = ui.scope_id().with("tg_dialogs_search_input");
        let search_has_focus = ui.ctx().memory(|m| m.has_focus(search_input_id));

        if enable_shortcuts
            && !compact_small_column
            && let Some(sc) = detect_dialogs_shortcut(ui.ctx(), search_has_focus)
            && let Some(a) = self.apply_shortcut(sc, backend, active_peer)
        {
            action = Some(a);
        }

        // 1. Top bar: Archive subsection header OR compact rail controls OR hamburger + search input.
        if self.opened_archive && !self.search_active {
            if self.show_archive_top_bar(ui, compact_small_column, palette) {
                self.opened_archive = false;
            }
        } else if compact_small_column {
            if let Some(a) = self.show_compact_top_bar(ui, show_hamburger, palette) {
                action = Some(a);
            }
        } else if let Some(a) = self.show_search_top_bar(
            ui,
            search_input_id,
            show_hamburger,
            backend,
            palette,
            strings,
        ) {
            action = Some(a);
        }

        // 2. Horizontal folder tabs when vertical sidebar is hidden and not searching/in archive.
        if show_horizontal_folders
            && !compact_small_column
            && !self.search_active
            && !self.opened_archive
        {
            let folders = backend.chat_folders();
            if folders.len() > 1
                && let Some(fid) =
                    show_horizontal_folder_tabs(ui, &folders, self.active_filter, palette)
            {
                if self.active_filter == fid {
                    self.inner.scroll_to_top();
                } else {
                    self.active_filter = fid;
                }
                action = Some(DialogsPanelAction::SelectFolder(fid));
            }
        }

        // 3. Body: Search view OR Virtualized dialogs list.
        if self.search_active && !compact_small_column {
            if let Some(act) = show_search_header_tabs(ui, &mut self.search_state, backend, palette)
            {
                self.handle_search_action(act, backend, &mut action);
            }

            let req = self.search_state.build_request(&self.search_query);
            let results = backend.search_dialogs(&req);
            self.search_state
                .sync_sponsored_peers(&self.search_query, backend);
            let now = backend.current_time();
            egui::ScrollArea::vertical()
                .id_salt("tg_dialogs_search_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    if let Some(act) = show_search_body(
                        ui,
                        &self.search_query,
                        &mut self.search_state,
                        &results,
                        backend,
                        active_peer,
                        now,
                        palette,
                    ) {
                        self.handle_search_action(act, backend, &mut action);
                    }
                });
        } else {
            if !compact_small_column && !self.opened_archive {
                if let Some(snap) = backend.promo_data()
                    && self.last_promo_snapshot.as_ref() != Some(&snap)
                {
                    self.promo_suggestions.apply_snapshot(snap.clone());
                    self.last_promo_snapshot = Some(snap);
                }
                let today_date = backend.current_time().date;
                let today_key = (today_date.year, today_date.month, today_date.day);
                if self.last_birthdays_date != Some(today_key)
                    && let Some(result) = backend.poll_contact_birthdays()
                {
                    if let Ok(entries) = result {
                        self.promo_suggestions.sync_contact_birthdays(
                            today_date.month,
                            today_date.day,
                            entries,
                        );
                    }
                    self.last_birthdays_date = Some(today_key);
                }
                if let Some(sug_act) =
                    render_top_bar_suggestion(ui, &mut self.promo_suggestions, palette)
                {
                    match sug_act {
                        SuggestionAction::Activate(s) => {
                            action = Some(DialogsPanelAction::ActivateSuggestion(s));
                        }
                        SuggestionAction::Dismiss { key, peer_id } => {
                            match backend.dismiss_promo_suggestion(peer_id, &key) {
                                Ok(_) if key == KEY_BIRTHDAY_CONTACTS_TODAY => {
                                    action = Some(DialogsPanelAction::Toast(
                                        tg_lang::tr::lng_dialogs_suggestions_birthday_contact_dismiss(
                                        ),
                                    ));
                                }
                                Ok(_) => {}
                                Err(err) => {
                                    action = Some(DialogsPanelAction::Toast(err));
                                }
                            }
                        }
                    }
                }
            }

            let list = self.current_list();
            let show_folder_tags = self.show_folder_tags || backend.folder_tags_enabled();
            self.inner.set_quick_dialog_action(self.quick_dialog_action);
            self.inner.set_reduced_motion(self.reduced_motion);
            if let Some(inner_act) = self.inner.show(
                ui,
                backend,
                list,
                active_peer,
                compact_small_column,
                show_folder_tags,
                palette,
            ) {
                match inner_act {
                    InnerAction::SelectPeer(pid) => {
                        action = Some(DialogsPanelAction::SelectPeer(pid));
                    }
                    InnerAction::OpenArchive => {
                        self.opened_archive = true;
                        self.inner.scroll_to_top();
                    }
                    InnerAction::OpenBotApp(pid) => {
                        action = Some(DialogsPanelAction::OpenBotApp(pid));
                    }
                    InnerAction::Toast(msg) => {
                        action = Some(DialogsPanelAction::Toast(msg));
                    }
                    InnerAction::OpenInNewWindow(pid) => {
                        action = Some(DialogsPanelAction::OpenInNewWindow(pid));
                    }
                    InnerAction::ToggleBlock(pid) => {
                        action = Some(DialogsPanelAction::ToggleBlock(pid));
                    }
                    InnerAction::OpenMuteFor(pid) => {
                        action = Some(DialogsPanelAction::OpenMuteFor(pid));
                    }
                    InnerAction::OpenSelectTone(pid) => {
                        action = Some(DialogsPanelAction::OpenSelectTone(pid));
                    }
                    InnerAction::ConfirmDelete {
                        peer_id,
                        just_clear,
                    } => {
                        action = Some(DialogsPanelAction::ConfirmDelete {
                            peer_id,
                            just_clear,
                        });
                    }
                }
            }

            // Floating scroll-to-top button when scrolled far down (`_scrollToTop`).
            if !compact_small_column && self.inner.scroll_offset_y() > 420.0 {
                let btn_center = Pos2::new(full_rect.right() - 28.0, full_rect.bottom() - 28.0);
                let btn_rect = Rect::from_center_size(btn_center, Vec2::splat(36.0));
                let resp = ui.interact(
                    btn_rect,
                    ui.scope_id().with("tg_dialogs_scroll_top_btn"),
                    Sense::click(),
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().circle_filled(
                    btn_center,
                    18.0,
                    if resp.hovered() {
                        colors.color("windowBgOver")
                    } else {
                        colors.window_bg()
                    },
                );
                ui.painter()
                    .circle_stroke(btn_center, 18.0, Stroke::new(1.0, colors.divider()));
                let icon = if resp.hovered() {
                    st::dialogs_to_up_above_over
                } else {
                    st::dialogs_to_up_above
                };
                paint_style_icon(
                    ui.painter(),
                    btn_rect,
                    icon,
                    &colors,
                    Some(colors.sub_text_fg()),
                );
                if resp
                    .on_hover_text(tg_lang::tr::lng_sr_scroll_to_top())
                    .clicked()
                {
                    self.inner.scroll_to_top();
                }
            }
        }

        action
    }

    fn handle_search_action(
        &mut self,
        act: SearchViewAction,
        backend: &mut dyn DialogsBackend,
        out: &mut Option<DialogsPanelAction>,
    ) {
        match act {
            SearchViewAction::OpenPeer(pid) => {
                backend.add_recent_search_peer(pid);
                *out = Some(DialogsPanelAction::SelectPeer(pid));
            }
            SearchViewAction::OpenMessage { peer_id, msg_id } => {
                backend.add_recent_search_peer(peer_id);
                *out = Some(DialogsPanelAction::SelectMessage { peer_id, msg_id });
            }
            SearchViewAction::CompleteHashtag(tag) => {
                let kind = is_hash_or_cashtag_search_query(&tag);
                let normalized = if matches!(kind, HashOrCashtag::None) {
                    tag
                } else {
                    fix_hashtag_search_query(&tag, kind)
                };
                self.search_query = normalized;
                self.search_state.keyboard_index = None;
            }
            SearchViewAction::RemoveHashtag(tag) => {
                backend.remove_recent_hashtag(&tag);
            }
            SearchViewAction::RemoveRecentPeer(pid) => {
                backend.remove_recent_search_peer(pid);
            }
            SearchViewAction::ClearRecentPeers => {
                backend.clear_recent_search_peers();
            }
            SearchViewAction::RemoveTopPeer(pid) => {
                backend.remove_top_peer(pid);
            }
            SearchViewAction::SetTab(tab) => {
                self.search_state.tab = tab;
                self.search_state.keyboard_index = None;
            }
            SearchViewAction::CycleChatTypeFilter => {
                self.search_state.chat_type_filter = self.search_state.chat_type_filter.next();
            }
            SearchViewAction::ToggleTag(tag_id) => {
                if self.search_state.selected_tag.as_deref() == Some(tag_id.as_str()) {
                    self.search_state.selected_tag = None;
                } else {
                    self.search_state.selected_tag = Some(tag_id);
                }
            }
            SearchViewAction::LoadMoreMessages => {
                self.search_state.messages_limit =
                    self.search_state.messages_limit.saturating_add(40);
            }
            SearchViewAction::ToggleSponsoredInfo(pid) => {
                if self.search_state.sponsored_info_open == Some(pid) {
                    self.search_state.sponsored_info_open = None;
                } else {
                    self.search_state.sponsored_info_open = Some(pid);
                }
            }
        }
    }

    fn show_compact_top_bar(
        &mut self,
        ui: &mut Ui,
        show_hamburger: bool,
        palette: &Palette,
    ) -> Option<DialogsPanelAction> {
        let mut action = None;
        let colors = DialogColors::new(palette);
        Frame::NONE
            .fill(colors.window_bg())
            .inner_margin(Margin::symmetric(
                st::dialogs_filter_padding.x.0 as i8,
                st::dialogs_filter_padding.y.0 as i8,
            ))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    if show_hamburger && hamburger_button(ui, palette) {
                        action = Some(DialogsPanelAction::OpenMainMenu);
                    }
                });
            });
        action
    }

    fn show_archive_top_bar(
        &mut self,
        ui: &mut Ui,
        compact_small_column: bool,
        palette: &Palette,
    ) -> bool {
        let colors = DialogColors::new(palette);
        let mut go_back = false;
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.window_bg());

        let back_rect = Rect::from_center_size(
            Pos2::new(
                if compact_small_column {
                    rect.center().x
                } else {
                    rect.left() + 26.0
                },
                rect.center().y,
            ),
            Vec2::splat(34.0),
        );
        let back_resp = ui.interact(
            back_rect,
            ui.scope_id().with("tg_archive_back"),
            Sense::click(),
        );
        if back_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(back_rect.center(), 17.0, colors.color("windowBgOver"));
        }
        paint_style_icon(
            ui.painter(),
            back_rect,
            st::back_button_icon,
            &colors,
            Some(colors.sub_text_fg()),
        );
        if back_resp
            .on_hover_text(tg_lang::tr::lng_filters_all())
            .clicked()
        {
            go_back = true;
        }

        if !compact_small_column {
            paint_semibold_text(
                ui.painter(),
                Pos2::new(rect.left() + 54.0, rect.center().y),
                Align2::LEFT_CENTER,
                &tg_lang::tr::lng_archived_name(),
                font_id_from_style(st::semibold_font),
                colors.bold_fg(),
            );

            // Search inside archive button.
            let search_rect = Rect::from_center_size(
                Pos2::new(rect.right() - 24.0, rect.center().y),
                Vec2::splat(32.0),
            );
            let search_resp = ui.interact(
                search_rect,
                ui.scope_id().with("tg_archive_search"),
                Sense::click(),
            );
            if search_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().circle_filled(
                    search_rect.center(),
                    16.0,
                    colors.color("windowBgOver"),
                );
            }
            paint_style_icon(
                ui.painter(),
                search_rect,
                st::box_field_search_icon,
                &colors,
                Some(colors.sub_text_fg()),
            );
            if search_resp
                .on_hover_text(strings::TOOLTIP_SEARCH_IN_ARCHIVE)
                .clicked()
            {
                self.search_active = true;
                self.search_state.in_archive = true;
                self.request_search_focus = true;
            }
        }

        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom() - 0.5),
                Pos2::new(rect.right(), rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, colors.divider()),
        );
        go_back
    }

    fn show_search_top_bar(
        &mut self,
        ui: &mut Ui,
        input_id: Id,
        show_hamburger: bool,
        backend: &dyn DialogsBackend,
        palette: &Palette,
        strings_provider: &dyn Strings,
    ) -> Option<DialogsPanelAction> {
        let _ = strings_provider;
        let colors = DialogColors::new(palette);
        let mut action = None;
        Frame::NONE
            .fill(colors.window_bg())
            .inner_margin(Margin::symmetric(
                st::dialogs_filter_padding.x.0 as i8,
                st::dialogs_filter_padding.y.0 as i8,
            ))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    // The bar is `topBarHeight` (54 px) with or without the menu toggle: the
                    // row is as tall as `dialogsMenuToggle` (40 px) inside the 7 px padding.
                    ui.set_min_height(MENU_TOGGLE_SIZE);
                    // 7.4.6 keeps the main menu button while searching and cancels with the ×
                    // inside the field (`_cancelSearch`); there is no extra back arrow.
                    if show_hamburger && hamburger_button(ui, palette) {
                        action = Some(DialogsPanelAction::OpenMainMenu);
                    }

                    let show_clear = !self.search_query.is_empty() || self.search_active;
                    let pill_w = ui.available_width().max(80.0);
                    // `filterTop = (topBarHeight - dialogsFilter.heightMin) / 2` = 9 px.
                    let pill_top = ui.max_rect().top()
                        + ((MENU_TOGGLE_SIZE - SEARCH_FIELD_HEIGHT) / 2.0).floor();
                    let pill_rect = Rect::from_min_size(
                        Pos2::new(ui.cursor().min.x, pill_top),
                        Vec2::new(pill_w, SEARCH_FIELD_HEIGHT),
                    );
                    let focused = ui.ctx().memory(|m| m.has_focus(input_id));
                    ui.painter().rect_filled(
                        pill_rect,
                        CornerRadius::same(18),
                        if focused {
                            colors.color("filterInputActiveBg")
                        } else {
                            colors.color("filterInputInactiveBg")
                        },
                    );
                    if focused {
                        // `st::dialogsFilter`: borderActive 2px / denominator 2 in `windowBgRipple`.
                        ui.painter().rect_stroke(
                            pill_rect,
                            CornerRadius::same(18),
                            Stroke::new(1.0, colors.color("windowBgRipple")),
                            egui::StrokeKind::Inside,
                        );
                    }

                    let inner_rect = pill_rect.shrink2(Vec2::new(12.0, 4.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner_rect));
                    child.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let clear_w = if show_clear { 20.0 } else { 0.0 };
                        let edit_w = (ui.available_width() - clear_w).max(40.0);
                        let resp = ui.add_sized(
                            Vec2::new(edit_w, 24.0),
                            TextEdit::singleline(&mut self.search_query)
                                .id(input_id)
                                .frame(Frame::NONE)
                                .hint_text(tg_lang::tr::lng_dlg_filter()),
                        );
                        if self.request_search_focus {
                            resp.request_focus();
                            self.request_search_focus = false;
                        }
                        if resp.gained_focus() || !self.search_query.is_empty() {
                            self.search_active = true;
                        }
                        if resp.changed() {
                            self.search_state.keyboard_index = None;
                        }

                        if show_clear {
                            let (x_rect, x_resp) =
                                ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                            if x_resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            paint_style_icon(
                                ui.painter(),
                                x_rect,
                                st::box_title_close_icon,
                                &colors,
                                Some(colors.sub_text_fg()),
                            );
                            if x_resp
                                .on_hover_text(tg_lang::tr::lng_sr_clear_search())
                                .clicked()
                            {
                                if self.search_query.is_empty() {
                                    self.cancel_search();
                                } else {
                                    self.search_query.clear();
                                    self.search_state.keyboard_index = None;
                                }
                            }
                        }
                    });
                    // Advance parent cursor past the rounded pill.
                    ui.advance_cursor_after_rect(pill_rect);
                });

                // Optional search-in-archive / search-in-peer scope chip.
                if self.search_active
                    && (self.search_state.in_archive || self.search_state.in_peer.is_some())
                {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if self.search_state.in_archive
                            && paint_search_scope_chip(
                                ui,
                                &strings::search_in_scope(&tg_lang::tr::lng_archived_name()),
                                palette,
                            )
                        {
                            self.search_state.in_archive = false;
                        }
                        if let Some(pid) = self.search_state.in_peer {
                            let title = backend
                                .dialog_by_peer(pid)
                                .map_or_else(tg_lang::tr::lng_search_tab_this_chat, |d| {
                                    strings::search_in_scope(&d.title)
                                });
                            if paint_search_scope_chip(ui, &title, palette) {
                                self.search_state.in_peer = None;
                            }
                        }
                    });
                }
            });
        action
    }
}

/// Paints the circular-hover hamburger menu icon button (`st::dialogsMenuToggle`).
pub fn hamburger_button(ui: &mut Ui, palette: &Palette) -> bool {
    let colors = DialogColors::new(palette);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(rect.center(), 16.0, colors.color("windowBgOver"));
    }
    paint_style_icon(
        ui.painter(),
        rect,
        st::dialogs_menu_toggle.icon,
        &colors,
        Some(colors.sub_text_fg()),
    );
    resp.on_hover_text(tg_lang::tr::lng_main_menu()).clicked()
}
