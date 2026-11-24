//! Vertical chat folders sidebar and horizontal folder tabs strip.
//!
//! Port origin: `Telegram/SourceFiles/window/window_filters_menu.h`,
//! `Telegram/SourceFiles/window/window_filters_menu.cpp`,
//! `Telegram/SourceFiles/dialogs/dialogs_widget.cpp` (`toggleFiltersMenu`),
//! and `Telegram/SourceFiles/ui/controls/filter_link_header.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, ScrollArea, Sense,
    Stroke, Ui, Vec2,
};
use tg_ui::fonts::{ensure_tdesktop_fonts, family_regular, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::{PopupMenuStyle, ToggleStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::Colors;
use tg_ui::widgets::buttons::IconKind;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

use crate::dialogs::backend::{ChatFolder, FolderIconKind};
use crate::dialogs::layout::{DialogColors, paint_semibold_text};
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::FilterId;

/// Width of the vertical folders sidebar (`st::windowFiltersWidth = 72px`).
pub const FILTERS_SIDEBAR_WIDTH: f32 = st::window_filters_width.0 as f32;

/// Height of the top main-menu button in the vertical sidebar (`54px`).
pub const SIDEBAR_MENU_BUTTON_HEIGHT: f32 = 54.0;

/// Height of a single folder button in the vertical sidebar (`st::windowFiltersButton = 62px`).
pub const SIDEBAR_FOLDER_BUTTON_HEIGHT: f32 = st::window_filters_button.min_height.0 as f32;

/// Height of the horizontal folder tabs strip (`36px`).
pub const HORIZONTAL_TABS_HEIGHT: f32 = 36.0;

/// Events emitted by [`FiltersMenuWidget`] (vertical sidebar) or [`show_horizontal_folder_tabs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiltersMenuAction {
    /// Hamburger menu button clicked.
    OpenMainMenu,
    /// User selected a chat folder.
    SelectFolder(FilterId),
    /// User clicked the bottom "Edit" button or "Edit Folders" context item.
    OpenFoldersSettings,
    /// User requested editing a specific folder from its context menu.
    EditFolder(FilterId),
    /// User chose "Mark All as Read" on a folder.
    MarkFolderRead(FilterId),
    /// User dragged folders to a new order.
    ReorderFolders(Vec<FilterId>),
}

/// Stateful vertical folders sidebar (`Window::FiltersMenu`).
#[derive(Debug, Default)]
pub struct FiltersMenuWidget {
    context_menu: PopupMenuState,
    context_folder: Option<FilterId>,
    dragging_folder: Option<FilterId>,
    drag_start_y: f32,
    include_muted_in_badges: bool,
}

impl FiltersMenuWidget {
    /// Creates a new vertical filters sidebar widget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            context_menu: PopupMenuState::new(),
            context_folder: None,
            dragging_folder: None,
            drag_start_y: 0.0,
            include_muted_in_badges: true,
        }
    }

    /// Configures whether muted unread chats contribute to folder badge counts.
    pub fn set_include_muted_in_badges(&mut self, include: bool) {
        self.include_muted_in_badges = include;
    }

    /// Renders the vertical folders sidebar inside `ui` (`72px` wide).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        folders: &[ChatFolder],
        active_filter: FilterId,
        palette: &Palette,
    ) -> Option<FiltersMenuAction> {
        ensure_tdesktop_fonts(ui.ctx());
        let colors = DialogColors::new(palette);
        let sidebar_bg = colors.color("sideBarBg");
        let sidebar_bg_active = colors.color("sideBarBgActive");
        let sidebar_fg = colors.color("sideBarTextFg");
        let sidebar_fg_active = colors.color("sideBarTextFgActive");
        let badge_bg_active = colors.color("sideBarBadgeBg");
        let badge_bg_muted = colors.color("sideBarBadgeBgMuted");
        let badge_fg = colors.color("sideBarBadgeFg");
        let label_font = font_id_from_style(st::window_filters_button.style.font);

        let mut action = None;
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, sidebar_bg);
        ui.spacing_mut().item_spacing = Vec2::ZERO;

        // 1. Top hamburger button (`_menu` in `window_filters_menu.cpp`).
        let (menu_rect, menu_resp) = ui.allocate_exact_size(
            Vec2::new(rect.width(), SIDEBAR_MENU_BUTTON_HEIGHT),
            Sense::click(),
        );
        menu_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, tg_lang::tr::lng_main_menu())
        });
        if menu_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(menu_rect, CornerRadius::ZERO, sidebar_bg_active);
        }
        paint_style_icon(
            ui.painter(),
            Rect::from_center_size(menu_rect.center(), Vec2::splat(24.0)),
            st::window_filters_main_menu.icon,
            &colors,
            Some(sidebar_fg),
        );
        if menu_resp
            .on_hover_text(tg_lang::tr::lng_main_menu())
            .clicked()
        {
            action = Some(FiltersMenuAction::OpenMainMenu);
        }

        // 2. Scrollable folder buttons + bottom "Edit" button.
        let mut reorder_request: Option<(usize, usize)> = None;
        ScrollArea::vertical()
            .id_salt("tg_filters_sidebar_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;

                for (idx, folder) in folders.iter().enumerate() {
                    let selected = folder.id == active_filter;
                    let (btn_rect, resp) = ui.allocate_exact_size(
                        Vec2::new(rect.width(), SIDEBAR_FOLDER_BUTTON_HEIGHT),
                        Sense::click_and_drag(),
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let is_dragging_this = self.dragging_folder == Some(folder.id);
                    if selected || resp.hovered() || is_dragging_this {
                        ui.painter()
                            .rect_filled(btn_rect, CornerRadius::ZERO, sidebar_bg_active);
                    }
                    if selected {
                        // Active left indicator pill matching modern tdesktop.
                        let bar = Rect::from_min_size(
                            Pos2::new(btn_rect.left(), btn_rect.top() + 12.0),
                            Vec2::new(3.0, btn_rect.height() - 24.0),
                        );
                        ui.painter()
                            .rect_filled(bar, CornerRadius::same(2), sidebar_fg_active);
                    }

                    let fg = if selected {
                        sidebar_fg_active
                    } else {
                        sidebar_fg
                    };

                    let icon_rect = Rect::from_center_size(
                        Pos2::new(btn_rect.center().x, btn_rect.top() + 23.0),
                        Vec2::splat(30.0),
                    );
                    paint_folder_icon_styled(
                        ui.painter(),
                        icon_rect,
                        folder.icon,
                        selected,
                        &colors,
                    );

                    // Folder title (`st::windowFiltersButton.textTop`).
                    let title = elide_sidebar_label(&folder.title, 10);
                    let text_y = btn_rect.top() + st::window_filters_button.text_top.0 as f32 + 5.0;
                    ui.painter().text(
                        Pos2::new(btn_rect.center().x, text_y),
                        Align2::CENTER_CENTER,
                        title,
                        label_font.clone(),
                        fg,
                    );

                    // Unread badge (`window_filters_menu.cpp:603-613`).
                    let (badge_count, badge_muted) =
                        folder.unread.badge_display(self.include_muted_in_badges);
                    let folder_label = if badge_count > 0 {
                        format!("{} ({badge_count} unread)", folder.title)
                    } else {
                        folder.title.clone()
                    };
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::Button,
                            true,
                            selected,
                            folder_label.clone(),
                        )
                    });
                    if badge_count > 0 {
                        let text = format_sidebar_badge(badge_count);
                        let badge_bg = if badge_muted {
                            badge_bg_muted
                        } else {
                            badge_bg_active
                        };
                        let font = FontId::new(10.0, family_regular());
                        let g = ui.painter().layout_no_wrap(text, font, badge_fg);
                        let bw = (g.size().x + 9.0).max(17.0);
                        let bh = 17.0;
                        let badge_rect = Rect::from_center_size(
                            Pos2::new(btn_rect.right() - 16.0, btn_rect.top() + 14.0),
                            Vec2::new(bw, bh),
                        );
                        ui.painter()
                            .rect_filled(badge_rect, CornerRadius::same(9), badge_bg);
                        ui.painter().galley(
                            Pos2::new(
                                badge_rect.center().x - g.size().x * 0.5,
                                badge_rect.center().y - g.size().y * 0.5,
                            ),
                            g,
                            badge_fg,
                        );
                    }

                    // Drag-and-drop reorder of custom folders (`idx > 0`, All Chats stays pinned at 0).
                    if idx > 0 && resp.drag_started() {
                        self.dragging_folder = Some(folder.id);
                        self.drag_start_y = resp
                            .interact_pointer_pos()
                            .map_or(btn_rect.center().y, |p| p.y);
                    }
                    if is_dragging_this {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let delta = pos.y - self.drag_start_y;
                            if delta.abs() > SIDEBAR_FOLDER_BUTTON_HEIGHT * 0.65 {
                                let target = if delta > 0.0 {
                                    (idx + 1).min(folders.len().saturating_sub(1))
                                } else {
                                    idx.saturating_sub(1).max(1)
                                };
                                if target != idx {
                                    reorder_request = Some((idx, target));
                                    self.drag_start_y = pos.y;
                                }
                            }
                        }
                        if resp.drag_stopped() {
                            self.dragging_folder = None;
                        }
                    }

                    if resp.clicked() && !is_dragging_this {
                        action = Some(FiltersMenuAction::SelectFolder(folder.id));
                    }
                    if resp.secondary_clicked() {
                        let anchor = resp
                            .interact_pointer_pos()
                            .unwrap_or(btn_rect.right_center());
                        let menu_style = PopupMenuStyle::default_popup(&colors);
                        self.context_folder = Some(folder.id);
                        self.context_menu.open_at(ui.ctx(), anchor, &menu_style);
                    }
                }

                // Bottom "Edit" button (`_setup` in `window_filters_menu.cpp:438-442`).
                let (edit_rect, edit_resp) = ui.allocate_exact_size(
                    Vec2::new(rect.width(), SIDEBAR_FOLDER_BUTTON_HEIGHT),
                    Sense::click(),
                );
                edit_resp.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::Button,
                        true,
                        tg_lang::tr::lng_filters_setup(),
                    )
                });
                if edit_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter()
                        .rect_filled(edit_rect, CornerRadius::ZERO, sidebar_bg_active);
                }
                let edit_icon_rect = Rect::from_center_size(
                    Pos2::new(edit_rect.center().x, edit_rect.top() + 23.0),
                    Vec2::splat(30.0),
                );
                paint_style_icon(
                    ui.painter(),
                    edit_icon_rect,
                    st::folders_setup,
                    &colors,
                    Some(sidebar_fg),
                );
                ui.painter().text(
                    Pos2::new(
                        edit_rect.center().x,
                        edit_rect.top() + st::window_filters_button.text_top.0 as f32 + 5.0,
                    ),
                    Align2::CENTER_CENTER,
                    tg_lang::tr::lng_filters_setup(),
                    label_font.clone(),
                    sidebar_fg,
                );
                if edit_resp.clicked() {
                    action = Some(FiltersMenuAction::OpenFoldersSettings);
                }
            });

        if let Some((from, to)) = reorder_request {
            let mut order: Vec<FilterId> = folders.iter().map(|f| f.id).collect();
            let item = order.remove(from);
            order.insert(to, item);
            action = Some(FiltersMenuAction::ReorderFolders(order));
        }

        // 3. Folder right-click context menu (`FiltersMenu::showMenu`).
        if let Some(folder_id) = self.context_folder {
            let menu_style = PopupMenuStyle::default_popup(&colors);
            let toggle_style = ToggleStyle::default_toggle(&colors);
            let items = if folder_id == FilterId::ALL {
                vec![
                    MenuItem::action("edit_all", tg_lang::tr::lng_filters_setup_menu())
                        .with_icon(IconKind::Edit),
                    MenuItem::action("mark_read", tg_lang::tr::lng_context_mark_read_all())
                        .with_icon(IconKind::Check),
                ]
            } else {
                vec![
                    MenuItem::action("edit_folder", tg_lang::tr::lng_filters_context_edit())
                        .with_icon(IconKind::Edit),
                    MenuItem::action("mark_read", tg_lang::tr::lng_context_mark_read_all())
                        .with_icon(IconKind::Check),
                ]
            };
            if let Some(triggered) = self.context_menu.show(
                ui.ctx(),
                Id::unique("tg_filters_sidebar_ctx"),
                &items,
                &menu_style,
                &toggle_style,
            ) {
                match triggered.as_str() {
                    "edit_all" => action = Some(FiltersMenuAction::OpenFoldersSettings),
                    "edit_folder" => action = Some(FiltersMenuAction::EditFolder(folder_id)),
                    "mark_read" => action = Some(FiltersMenuAction::MarkFolderRead(folder_id)),
                    _ => {}
                }
            }
            if !self.context_menu.is_open() {
                self.context_folder = None;
            }
        }

        action
    }
}

/// Renders the horizontal folder tabs strip shown inside the dialogs column when the vertical
/// sidebar is disabled or narrow (`AddChatFiltersTabsStrip` in `dialogs_widget.cpp`).
pub fn show_horizontal_folder_tabs(
    ui: &mut Ui,
    folders: &[ChatFolder],
    active_filter: FilterId,
    palette: &Palette,
) -> Option<FilterId> {
    ensure_tdesktop_fonts(ui.ctx());
    let colors = DialogColors::new(palette);
    let mut selected_out = None;
    let bar_rect = ui
        .allocate_space(Vec2::new(ui.available_width(), HORIZONTAL_TABS_HEIGHT))
        .1;
    ui.painter()
        .rect_filled(bar_rect, CornerRadius::ZERO, colors.window_bg());

    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
    ScrollArea::horizontal()
        .id_salt("tg_horizontal_folder_tabs")
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut child, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(10.0);
                ui.spacing_mut().item_spacing.x = 4.0;

                for folder in folders {
                    let active = folder.id == active_filter;
                    let (badge_count, badge_muted) = folder.unread.badge_display(true);
                    let font = font_id_from_style(st::dialogs_text_font);
                    let title_w = ui
                        .painter()
                        .layout_no_wrap(folder.title.clone(), font.clone(), Color32::WHITE)
                        .size()
                        .x;
                    let badge_text = if badge_count > 0 {
                        format_sidebar_badge(badge_count)
                    } else {
                        String::new()
                    };
                    let badge_w = if badge_text.is_empty() {
                        0.0
                    } else {
                        (badge_text.len() as f32 * 6.5 + 10.0).max(17.0) + 5.0
                    };
                    let tab_w = title_w + badge_w + 18.0;
                    let (tab_rect, resp) = ui.allocate_exact_size(
                        Vec2::new(tab_w, HORIZONTAL_TABS_HEIGHT),
                        Sense::click(),
                    );
                    let tab_label = if badge_count > 0 {
                        format!("{} ({badge_count} unread)", folder.title)
                    } else {
                        folder.title.clone()
                    };
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::Button,
                            true,
                            active,
                            tab_label.clone(),
                        )
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let fg = if active {
                        colors.active_text_fg()
                    } else if resp.hovered() {
                        colors.bold_fg()
                    } else {
                        colors.sub_text_fg()
                    };

                    let text_x = tab_rect.left() + 9.0;
                    if active {
                        paint_semibold_text(
                            ui.painter(),
                            Pos2::new(text_x, tab_rect.center().y - 1.0),
                            Align2::LEFT_CENTER,
                            &folder.title,
                            font,
                            fg,
                        );
                    } else {
                        ui.painter().text(
                            Pos2::new(text_x, tab_rect.center().y - 1.0),
                            Align2::LEFT_CENTER,
                            &folder.title,
                            font,
                            fg,
                        );
                    }

                    if !badge_text.is_empty() {
                        let bw = badge_w - 5.0;
                        let badge_rect = Rect::from_center_size(
                            Pos2::new(text_x + title_w + 5.0 + bw * 0.5, tab_rect.center().y - 1.0),
                            Vec2::new(bw, 17.0),
                        );
                        let bg = if active {
                            colors.color("windowBgActive")
                        } else if badge_muted {
                            colors.color("dialogsUnreadBgMuted")
                        } else {
                            colors.color("dialogsUnreadBg")
                        };
                        ui.painter()
                            .rect_filled(badge_rect, CornerRadius::same(9), bg);
                        ui.painter().text(
                            badge_rect.center(),
                            Align2::CENTER_CENTER,
                            &badge_text,
                            FontId::new(10.0, family_regular()),
                            colors.color("dialogsUnreadFg"),
                        );
                    }

                    // Active bottom underline indicator (`st::dialogsFilterTabs`).
                    if active {
                        let line = Rect::from_min_max(
                            Pos2::new(tab_rect.left() + 6.0, tab_rect.bottom() - 3.0),
                            Pos2::new(tab_rect.right() - 6.0, tab_rect.bottom()),
                        );
                        ui.painter().rect_filled(
                            line,
                            CornerRadius::same(2),
                            colors.color("windowBgActive"),
                        );
                    }

                    if resp.clicked() {
                        selected_out = Some(folder.id);
                    }
                }
                ui.add_space(10.0);
            });
        });

    // Bottom 1px hairline divider.
    ui.painter().line_segment(
        [
            Pos2::new(bar_rect.left(), bar_rect.bottom() - 0.5),
            Pos2::new(bar_rect.right(), bar_rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, colors.divider()),
    );

    selected_out
}

/// Formats folder sidebar badge count (`> 999 -> "99+"`, matching `window_filters_menu.cpp:608-612`).
#[must_use]
pub fn format_sidebar_badge(count: u32) -> String {
    if count == 0 {
        String::new()
    } else if count > 999 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

fn elide_sidebar_label(title: &str, max_chars: usize) -> String {
    if title.chars().count() <= max_chars {
        title.to_owned()
    } else {
        let mut s: String = title.chars().take(max_chars.saturating_sub(1)).collect();
        s.push('\u{2026}');
        s
    }
}

fn folder_style_icon(icon: FolderIconKind, active: bool) -> tg_ui::tg_style::Icon {
    match (icon, active) {
        (FolderIconKind::All, false) => st::folders_all,
        (FolderIconKind::All, true) => st::folders_all_active,
        (FolderIconKind::Unread, false) => st::folders_unread,
        (FolderIconKind::Unread, true) => st::folders_unread_active,
        (FolderIconKind::Unmuted, false) => st::folders_unmuted,
        (FolderIconKind::Unmuted, true) => st::folders_unmuted_active,
        (FolderIconKind::Private, false) => st::folders_private,
        (FolderIconKind::Private, true) => st::folders_private_active,
        (FolderIconKind::Groups, false) => st::folders_groups,
        (FolderIconKind::Groups, true) => st::folders_groups_active,
        (FolderIconKind::Channels, false) => st::folders_channels,
        (FolderIconKind::Channels, true) => st::folders_channels_active,
        (FolderIconKind::Bots, false) => st::folders_bots,
        (FolderIconKind::Bots, true) => st::folders_bots_active,
        (FolderIconKind::Work, false) => st::folders_work,
        (FolderIconKind::Work, true) => st::folders_work_active,
        (FolderIconKind::Star, false) => st::folders_favorite,
        (FolderIconKind::Star, true) => st::folders_favorite_active,
        (FolderIconKind::Book, false) => st::folders_book,
        (FolderIconKind::Book, true) => st::folders_book_active,
        (FolderIconKind::Game, false) => st::folders_game,
        (FolderIconKind::Game, true) => st::folders_game_active,
        (FolderIconKind::Cat, false) => st::folders_cat,
        (FolderIconKind::Cat, true) => st::folders_cat_active,
        (FolderIconKind::Money, false) => st::folders_money,
        (FolderIconKind::Money, true) => st::folders_money_active,
        (FolderIconKind::Light, false) => st::folders_light,
        (FolderIconKind::Light, true) => st::folders_light_active,
        (FolderIconKind::Like, false) => st::folders_like,
        (FolderIconKind::Like, true) => st::folders_like_active,
        (FolderIconKind::Note, false) => st::folders_note,
        (FolderIconKind::Note, true) => st::folders_note_active,
        (FolderIconKind::Palette, false) => st::folders_palette,
        (FolderIconKind::Palette, true) => st::folders_palette_active,
        (FolderIconKind::Travel, false) => st::folders_travel,
        (FolderIconKind::Travel, true) => st::folders_travel_active,
        (FolderIconKind::Sport, false) => st::folders_sport,
        (FolderIconKind::Sport, true) => st::folders_sport_active,
        (FolderIconKind::Study, false) => st::folders_study,
        (FolderIconKind::Study, true) => st::folders_study_active,
        (FolderIconKind::Airplane, false) => st::folders_airplane,
        (FolderIconKind::Airplane, true) => st::folders_airplane_active,
        (FolderIconKind::Crown, false) => st::folders_crown,
        (FolderIconKind::Crown, true) => st::folders_crown_active,
        (FolderIconKind::Flower, false) => st::folders_flower,
        (FolderIconKind::Flower, true) => st::folders_flower_active,
        (FolderIconKind::Home, false) => st::folders_home,
        (FolderIconKind::Home, true) => st::folders_home_active,
        (FolderIconKind::Love, false) => st::folders_love,
        (FolderIconKind::Love, true) => st::folders_love_active,
        (FolderIconKind::Mask, false) => st::folders_mask,
        (FolderIconKind::Mask, true) => st::folders_mask_active,
        (FolderIconKind::Party, false) => st::folders_party,
        (FolderIconKind::Party, true) => st::folders_party_active,
        (FolderIconKind::Trade, false) => st::folders_trade,
        (FolderIconKind::Trade, true) => st::folders_trade_active,
        (FolderIconKind::Setup, false) => st::folders_setup,
        (FolderIconKind::Setup, true) => st::folders_setup_active,
        (FolderIconKind::Custom, false) => st::folders_custom,
        (FolderIconKind::Custom, true) => st::folders_custom_active,
    }
}

fn paint_folder_icon_styled(
    painter: &Painter,
    rect: Rect,
    icon: FolderIconKind,
    active: bool,
    colors: &DialogColors<'_>,
) {
    let style_icon = folder_style_icon(icon, active);
    let fg = if active {
        colors.color("sideBarIconFgActive")
    } else {
        colors.color("sideBarIconFg")
    };
    paint_style_icon(painter, rect, style_icon, colors, Some(fg));
}

/// Paints a real tdesktop folder icon matching `ui/filter_icons.cpp` via `tg-ui`'s icon cache.
pub fn paint_folder_icon(painter: &Painter, rect: Rect, icon: FolderIconKind, fg: Color32) {
    let style_icon = folder_style_icon(icon, false);
    paint_style_icon(
        painter,
        rect,
        style_icon,
        &tg_ui::tg_style::DefaultPaletteResolver,
        Some(fg),
    );
}
