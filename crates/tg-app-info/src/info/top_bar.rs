//! Top header bar for Info/Profile and Shared Media views.
//!
//! Port origin: `Telegram/SourceFiles/info/info_top_bar.h`,
//! `Telegram/SourceFiles/info/info_top_bar.cpp`,
//! and `Telegram/SourceFiles/info/info.style` (`infoTopBarHeight = 54px`).

use egui::{
    Align2, CornerRadius, CursorIcon, FontId, Id, Key, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::{Colors, InputFieldStyle, PopupMenuStyle, ToggleStyle};
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::{ColorsResolver, resolve_color_ref};
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

use crate::info::strings;

/// Fixed pixel height of the Info top bar (`st::infoTopBarHeight` in `info.style`).
pub const INFO_TOP_BAR_HEIGHT: f32 = st::info_top_bar_height.0 as f32;

/// Events emitted by [`InfoTopBar::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopBarAction {
    /// Navigate back within the Info stack or parent section stack.
    Back,
    /// Close the third-column Info panel.
    Close,
    /// Open the Edit Peer Info / Edit Contact box.
    Edit,
    /// Forward currently selected shared media items.
    ForwardSelected,
    /// Delete currently selected shared media items.
    DeleteSelected,
    /// Cancel media multi-selection mode.
    CancelSelection,
    /// Triggered an item from the 3-dot overflow popup menu.
    MenuCommand(String),
}

/// Configuration passed into [`InfoTopBar::show`].
pub struct TopBarConfig<'a> {
    /// Primary title (e.g. `"User Info"`, `"Group Info"`, `"Shared Media"`).
    pub title: &'a str,
    /// Optional subtitle shown beneath the title (e.g. `"9 members"`).
    pub subtitle: Option<&'a str>,
    /// Show left back arrow button.
    pub show_back: bool,
    /// Show right close `X` button (typically when hosted in the third column).
    pub show_close: bool,
    /// Show inline search toggle button.
    pub show_search: bool,
    /// Show edit pencil button (`can_edit`).
    pub show_edit: bool,
    /// Overflow popup menu entries (if non-empty, shows the `⋮` button).
    pub menu_items: Vec<MenuItem>,
    /// Number of currently selected shared media items (`> 0` activates selection bar).
    pub selected_count: usize,
    /// Draw only the buttons over the profile cover (no background, title or shadow), like
    /// tdesktop's transparent `TopBar` over `Info::Profile::TopBar`.
    pub overlay: bool,
}

/// Stateful controller for the Info section top bar (`Info::TopBar`).
#[derive(Debug, Default)]
pub struct InfoTopBar {
    search_open: bool,
    search_query: String,
    menu_state: PopupMenuState,
}

impl InfoTopBar {
    /// Creates a new top bar with closed search and menu.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the inline search bar is currently expanded.
    #[must_use]
    pub const fn is_search_open(&self) -> bool {
        self.search_open
    }

    /// Returns the active search query string (empty when search is closed).
    #[must_use]
    pub fn search_query(&self) -> &str {
        if self.search_open {
            &self.search_query
        } else {
            ""
        }
    }

    /// Opens inline search mode.
    pub fn open_search(&mut self) {
        self.search_open = true;
    }

    /// Clears and closes inline search mode.
    pub fn close_search(&mut self) {
        self.search_open = false;
        self.search_query.clear();
    }

    /// Handles Escape priority (`TopBar::checkBeforeCloseByEscape`):
    /// clears search text first, then exits search mode; returns `true` if consumed.
    pub fn handle_escape(&mut self) -> bool {
        if !self.search_open {
            return false;
        }
        if !self.search_query.is_empty() {
            self.search_query.clear();
        } else {
            self.search_open = false;
        }
        true
    }

    /// Renders the `54px` top bar inside `ui` and returns any emitted [`TopBarAction`].
    pub fn show(
        &mut self,
        ui: &mut Ui,
        id_salt: &str,
        cfg: &TopBarConfig<'_>,
        colors: &dyn Colors,
    ) -> Option<TopBarAction> {
        let mut action = None;
        let width = ui.available_width().max(180.0);
        let (bar_rect, _) =
            ui.allocate_exact_size(Vec2::new(width, INFO_TOP_BAR_HEIGHT), Sense::hover());

        if !cfg.overlay {
            ui.painter()
                .rect_filled(bar_rect, CornerRadius::ZERO, colors.color("windowBg"));
        }

        let center_y = bar_rect.center().y;
        let title_font = font_id_from_style(st::info_top_bar_title.style.font);
        let title_fg = resolve_color_ref(st::info_top_bar_title.text_fg, colors);

        if cfg.selected_count > 0 {
            // Media multi-selection bar (`Info::TopBar::selectionMode`).
            let cancel_rect = Rect::from_center_size(
                Pos2::new(bar_rect.min.x + 26.0, center_y),
                Vec2::splat(40.0),
            );
            if top_bar_icon_btn(
                ui,
                (id_salt, "sel_cancel"),
                cancel_rect,
                &st::info_top_bar_media_cancel.icon,
                &st::info_top_bar_media_cancel.icon_over,
                strings::TOOLTIP_CANCEL_SELECTION,
                colors,
            ) {
                action = Some(TopBarAction::CancelSelection);
            }

            let count_text = if cfg.selected_count == 1 {
                strings::TOP_BAR_SELECTED_ONE.to_owned()
            } else {
                format!(
                    "{} {}",
                    cfg.selected_count,
                    strings::TOP_BAR_SELECTED_SUFFIX
                )
            };
            ui.painter().text(
                Pos2::new(bar_rect.min.x + 56.0, center_y),
                Align2::LEFT_CENTER,
                count_text,
                title_font,
                title_fg,
            );

            let del_rect = Rect::from_center_size(
                Pos2::new(bar_rect.max.x - 26.0, center_y),
                Vec2::splat(40.0),
            );
            if top_bar_icon_btn(
                ui,
                (id_salt, "sel_delete"),
                del_rect,
                &st::info_top_bar_delete.icon,
                &st::info_top_bar_delete.icon_over,
                &tr::lng_selected_delete(),
                colors,
            ) {
                action = Some(TopBarAction::DeleteSelected);
            }

            let fwd_rect = Rect::from_center_size(
                Pos2::new(bar_rect.max.x - 70.0, center_y),
                Vec2::splat(40.0),
            );
            if top_bar_icon_btn(
                ui,
                (id_salt, "sel_forward"),
                fwd_rect,
                &st::info_top_bar_forward.icon,
                &st::info_top_bar_forward.icon_over,
                &tr::lng_selected_forward(),
                colors,
            ) {
                action = Some(TopBarAction::ForwardSelected);
            }
        } else if self.search_open && cfg.show_search {
            if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
                self.handle_escape();
            }

            let back_rect = Rect::from_center_size(
                Pos2::new(bar_rect.min.x + 26.0, center_y),
                Vec2::splat(40.0),
            );
            if top_bar_icon_btn(
                ui,
                (id_salt, "search_back"),
                back_rect,
                &st::info_top_bar_back_icon,
                &st::info_top_bar_back_icon_over,
                strings::TOOLTIP_EXIT_SEARCH,
                colors,
            ) {
                self.close_search();
            }

            let clear_w = if self.search_query.is_empty() {
                12.0
            } else {
                44.0
            };
            let field_rect = Rect::from_min_max(
                Pos2::new(bar_rect.min.x + 52.0, bar_rect.min.y + 10.0),
                Pos2::new(
                    (bar_rect.max.x - clear_w).max(bar_rect.min.x + 110.0),
                    bar_rect.max.y - 10.0,
                ),
            );
            let mut f_ui = ui.new_child(egui::UiBuilder::new().max_rect(field_rect));
            let search_ph = tr::lng_dlg_filter();
            InputField::single_line(
                &format!("{id_salt}_topbar_search"),
                &mut self.search_query,
                &search_ph,
                InputFieldStyle::rounded_search(colors),
            )
            .width(field_rect.width())
            .show(&mut f_ui);

            if !self.search_query.is_empty() {
                let clr_rect = Rect::from_center_size(
                    Pos2::new(bar_rect.max.x - 24.0, center_y),
                    Vec2::splat(36.0),
                );
                if top_bar_icon_btn(
                    ui,
                    (id_salt, "search_clear"),
                    clr_rect,
                    &st::info_top_bar_close.icon,
                    &st::info_top_bar_close.icon_over,
                    strings::TOOLTIP_CLEAR_SEARCH,
                    colors,
                ) {
                    self.search_query.clear();
                }
            }
        } else {
            let mut left_x = bar_rect.min.x + 18.0;
            if cfg.show_back {
                let back_rect = Rect::from_center_size(
                    Pos2::new(bar_rect.min.x + 26.0, center_y),
                    Vec2::splat(40.0),
                );
                if top_bar_icon_btn(
                    ui,
                    (id_salt, "back"),
                    back_rect,
                    &st::info_top_bar_back_icon,
                    &st::info_top_bar_back_icon_over,
                    &tr::lng_go_back(),
                    colors,
                ) {
                    action = Some(TopBarAction::Back);
                }
                left_x = bar_rect.min.x + 54.0;
            }

            // Right-side action buttons from right to left.
            let mut right_x = bar_rect.max.x - 6.0;
            if cfg.show_close {
                let r =
                    Rect::from_center_size(Pos2::new(right_x - 20.0, center_y), Vec2::splat(40.0));
                if top_bar_icon_btn(
                    ui,
                    (id_salt, "close"),
                    r,
                    &st::info_top_bar_close.icon,
                    &st::info_top_bar_close.icon_over,
                    &tr::lng_close(),
                    colors,
                ) {
                    action = Some(TopBarAction::Close);
                }
                right_x -= 40.0;
            }

            if !cfg.menu_items.is_empty() {
                let r =
                    Rect::from_center_size(Pos2::new(right_x - 20.0, center_y), Vec2::splat(40.0));
                if top_bar_icon_btn(
                    ui,
                    (id_salt, "menu"),
                    r,
                    &st::info_top_bar_menu.icon,
                    &st::info_top_bar_menu.icon_over,
                    strings::TOOLTIP_MORE_ACTIONS,
                    colors,
                ) {
                    let popup_st = PopupMenuStyle::default_popup(colors);
                    self.menu_state.open_at(
                        ui.ctx(),
                        Pos2::new(r.min.x - 120.0, r.max.y + 2.0),
                        &popup_st,
                    );
                }
                right_x -= 40.0;
            }

            if cfg.show_edit {
                let r =
                    Rect::from_center_size(Pos2::new(right_x - 20.0, center_y), Vec2::splat(40.0));
                if top_bar_icon_btn(
                    ui,
                    (id_salt, "edit"),
                    r,
                    &st::info_top_bar_edit.icon,
                    &st::info_top_bar_edit.icon_over,
                    strings::TOOLTIP_EDIT,
                    colors,
                ) {
                    action = Some(TopBarAction::Edit);
                }
                right_x -= 40.0;
            }

            if cfg.show_search {
                let r =
                    Rect::from_center_size(Pos2::new(right_x - 20.0, center_y), Vec2::splat(40.0));
                if top_bar_icon_btn(
                    ui,
                    (id_salt, "search"),
                    r,
                    &st::info_top_bar_search.icon,
                    &st::info_top_bar_search.icon_over,
                    strings::TOOLTIP_SEARCH,
                    colors,
                ) {
                    self.search_open = true;
                }
            }

            // Title (+ optional subtitle) clipped safely to available center width.
            let title_text = if cfg.overlay { "" } else { cfg.title };
            let title_clip = Rect::from_min_max(
                Pos2::new(left_x, bar_rect.min.y),
                Pos2::new(right_x.max(left_x + 40.0), bar_rect.max.y),
            );
            let p = ui.painter().with_clip_rect(title_clip);
            if let Some(sub) = cfg.subtitle.filter(|_| !cfg.overlay) {
                p.text(
                    Pos2::new(left_x, bar_rect.min.y + 10.0),
                    Align2::LEFT_TOP,
                    cfg.title,
                    title_font,
                    title_fg,
                );
                p.text(
                    Pos2::new(left_x, bar_rect.min.y + 30.0),
                    Align2::LEFT_TOP,
                    sub,
                    FontId::new(12.0, family_regular()),
                    colors.color("windowSubTextFg"),
                );
            } else {
                p.text(
                    Pos2::new(left_x, center_y),
                    Align2::LEFT_CENTER,
                    title_text,
                    title_font,
                    title_fg,
                );
            }
        }

        // Bottom 1px hairline shadow (`shadowFg`).
        if !cfg.overlay {
            ui.painter().line_segment(
                [
                    Pos2::new(bar_rect.left(), bar_rect.bottom()),
                    Pos2::new(bar_rect.right(), bar_rect.bottom()),
                ],
                Stroke::new(1.0, colors.color("shadowFg")),
            );
        }

        // Render floating popup menu when open.
        if !cfg.menu_items.is_empty() {
            let popup_st = PopupMenuStyle::default_popup(colors);
            let toggle_st = ToggleStyle::default_toggle(colors);
            if let Some(cmd) = self.menu_state.show(
                ui.ctx(),
                Id::unique((id_salt, "info_topbar_popup")),
                &cfg.menu_items,
                &popup_st,
                &toggle_st,
            ) {
                action = Some(TopBarAction::MenuCommand(cmd));
            }
        }

        action
    }
}

/// Paints a circular hoverable top-bar icon button backed by real `tg_style` icons.
fn top_bar_icon_btn(
    ui: &mut Ui,
    id_salt: (&str, &str),
    rect: Rect,
    icon: &Icon,
    icon_over: &Icon,
    tooltip: &str,
    colors: &dyn Colors,
) -> bool {
    let resp = ui.interact(rect, ui.make_persistent_id(id_salt), Sense::click());
    if !tooltip.is_empty() {
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, tooltip)
        });
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().circle_filled(
            rect.center(),
            (rect.width().min(rect.height()) * 0.45).min(18.0),
            colors.color("windowBgOver"),
        );
    }
    let active_icon = if resp.hovered() && !icon_over.is_empty() {
        icon_over
    } else {
        icon
    };
    let ic_rect = Rect::from_center_size(rect.center(), Vec2::splat(22.0));
    paint_style_icon(
        ui.painter(),
        ic_rect,
        *active_icon,
        &ColorsResolver(colors),
        None,
    );
    let resp = if !tooltip.is_empty() {
        resp.on_hover_text(tooltip)
    } else {
        resp
    };
    resp.clicked()
}
