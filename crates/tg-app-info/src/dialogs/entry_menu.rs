//! Chats-list row context menu with tdesktop's outline menu icons and submenus.
//!
//! Port of `Filler::fillContextMenuActions` (`window/window_peer_menu.cpp`), drawn with
//! `st::popupMenuWithIcons` (`lib_ui/ui/widgets/widgets.style`): 24 px icons at
//! `itemIconPosition`, text at `itemPadding.left`, submenu arrow for "Mute notifications" and
//! "Add to folder", and `st::mediaPlayerMenuCheck` on folders that already contain the chat.

use egui::{
    Align2, Area, CornerRadius, CursorIcon, Id, Key, Order, Pos2, Rect, Role, Sense, Ui, Vec2,
    WidgetInfo,
};
use tg_ui::icons::paint_style_icon;
use tg_ui::painter::{BoxShadow, RoundRect};
use tg_ui::stand_in::PopupMenuStyle;
use tg_ui::style::px;
use tg_ui::tg_style::{ColorResolver, Icon, st};

/// One clickable row of an [`EntryMenu`].
#[derive(Debug, Clone)]
pub struct EntryMenuItem {
    /// Action identifier returned when the row is triggered.
    pub id: String,
    /// Visible label.
    pub label: String,
    /// Leading tdesktop menu icon.
    pub icon: Option<Icon>,
    /// Painted with `attentionButtonFg` (e.g. "Delete chat").
    pub attention: bool,
    /// Nested rows shown in a submenu to the right of this row.
    pub submenu: Vec<EntryMenuRow>,
}

/// A row in an [`EntryMenu`].
#[derive(Debug, Clone)]
pub enum EntryMenuRow {
    /// Clickable action (optionally with a submenu).
    Item(EntryMenuItem),
    /// Horizontal separator.
    Separator,
    /// `AddSeparatorAndShiftUp`: the full-width `expandedMenuSeparator` band. The menu opens
    /// shifted up so the middle of this band is at the click point.
    ExpandedSeparator,
}

impl EntryMenuRow {
    /// Builds a plain action row.
    #[must_use]
    pub fn action(id: impl Into<String>, label: impl Into<String>, icon: Option<Icon>) -> Self {
        Self::Item(EntryMenuItem {
            id: id.into(),
            label: label.into(),
            icon,
            attention: false,
            submenu: Vec::new(),
        })
    }

    /// Builds an attention (red) action row.
    #[must_use]
    pub fn attention(id: impl Into<String>, label: impl Into<String>, icon: Option<Icon>) -> Self {
        Self::Item(EntryMenuItem {
            id: id.into(),
            label: label.into(),
            icon,
            attention: true,
            submenu: Vec::new(),
        })
    }

    /// Builds a row that opens `submenu` on hover.
    #[must_use]
    pub fn submenu(
        label: impl Into<String>,
        icon: Option<Icon>,
        submenu: Vec<EntryMenuRow>,
    ) -> Self {
        let label = label.into();
        Self::Item(EntryMenuItem {
            id: format!("submenu:{label}"),
            label,
            icon,
            attention: false,
            submenu,
        })
    }

    /// Visible label of an item row, without its shortcut (`None` for separators).
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Item(item) => Some(split_shortcut(&item.label).0),
            Self::Separator | Self::ExpandedSeparator => None,
        }
    }

    /// Right-aligned shortcut text of an item row ("2h" in "Unmute\t2h").
    #[must_use]
    pub fn shortcut(&self) -> Option<&str> {
        match self {
            Self::Item(item) => split_shortcut(&item.label).1,
            Self::Separator | Self::ExpandedSeparator => None,
        }
    }

    /// Action identifier of an item row (`None` for separators).
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Item(item) => Some(&item.id),
            Self::Separator | Self::ExpandedSeparator => None,
        }
    }
}

/// Open / hover state of the chats-list row context menu.
#[derive(Debug, Clone, Default)]
pub struct EntryMenu {
    open: bool,
    anchor: Pos2,
    hovered: Option<usize>,
    submenu_index: Option<usize>,
    submenu_hovered: Option<usize>,
    opened_frame: u64,
    /// Opened with [`EntryMenu::open_below`]: no `AddSeparatorAndShiftUp` shift.
    unshifted: bool,
}

/// `expandedMenuSeparator` (`ui/chat/chat.style`): 4 px padding above and below a 6 px band.
const EXPANDED_SEPARATOR_PADDING: f32 = 4.0;
const EXPANDED_SEPARATOR_WIDTH: f32 = 6.0;

struct Metrics {
    row_h: f32,
    sep_h: f32,
    width: f32,
    height: f32,
    /// `addTopShift` of `AddSeparatorAndShiftUp` (0 without an expanded separator).
    top_shift: f32,
}

/// `Action::processAction`: the text before a `'\t'` is the label, the text after it is drawn
/// right-aligned like a shortcut (e.g. "Unmute\t2h"). An empty shortcut draws nothing.
#[must_use]
pub fn split_shortcut(label: &str) -> (&str, Option<&str>) {
    match label.split_once('\t') {
        Some((text, shortcut)) => (text, (!shortcut.is_empty()).then_some(shortcut)),
        None => (label, None),
    }
}

/// Width of `menuIconSubmenuArrow` (24 px at 100%).
fn submenu_arrow_width() -> f32 {
    st::menu_icon_submenu_arrow
        .scaled_size()
        .map_or(24.0, |size| size.width as f32)
}

fn measure(ctx: &egui::Context, rows: &[EntryMenuRow], style: &PopupMenuStyle) -> Metrics {
    let menu = &style.menu;
    let font = menu.item_style.font.font_id();
    let row_h =
        menu.item_padding.top + menu.item_style.font.line_height() + menu.item_padding.bottom;
    let sep_h = menu.separator.padding.vertical() + menu.separator.width;
    let painter = ctx.layer_painter(egui::LayerId::background());
    let mut width = menu.width_min;
    let mut y = style.scroll_padding.top;
    let mut top_shift = None;
    for row in rows {
        match row {
            EntryMenuRow::Separator => y += sep_h,
            EntryMenuRow::ExpandedSeparator => {
                top_shift.get_or_insert(
                    y + EXPANDED_SEPARATOR_PADDING + (EXPANDED_SEPARATOR_WIDTH / 2.0).floor(),
                );
                y += 2.0 * EXPANDED_SEPARATOR_PADDING + EXPANDED_SEPARATOR_WIDTH;
            }
            EntryMenuRow::Item(item) => {
                y += row_h;
                let (text, shortcut) = split_shortcut(&item.label);
                let text_w = painter
                    .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x;
                // `Action::setMarkedText`: `itemRightSkip + arrow.width()` for submenu rows,
                // `itemRightSkip + font->width(shortcut)` for rows with a shortcut.
                let arrow = if !item.submenu.is_empty() {
                    menu.item_right_skip + submenu_arrow_width()
                } else if let Some(shortcut) = shortcut {
                    menu.item_right_skip
                        + painter
                            .layout_no_wrap(shortcut.to_owned(), font.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                } else {
                    0.0
                };
                width =
                    width.max(menu.item_padding.left + text_w + arrow + menu.item_padding.right);
            }
        }
    }
    Metrics {
        row_h,
        sep_h,
        width: width.clamp(menu.width_min, menu.width_max).ceil(),
        height: y + style.scroll_padding.bottom,
        top_shift: top_shift.unwrap_or(0.0),
    }
}

struct PaintedRows {
    rect: Rect,
    triggered: Option<String>,
    hovered: Option<usize>,
    item_rects: Vec<(usize, Rect)>,
}

#[allow(clippy::too_many_arguments)]
fn paint_rows(
    ctx: &egui::Context,
    id: Id,
    pos: Pos2,
    rows: &[EntryMenuRow],
    style: &PopupMenuStyle,
    colors: &dyn ColorResolver,
    metrics: &Metrics,
    selected: Option<usize>,
    trigger_selected: bool,
) -> PaintedRows {
    let menu = &style.menu;
    let font = menu.item_style.font.font_id();
    let mut out = PaintedRows {
        rect: Rect::from_min_size(pos, Vec2::new(metrics.width, metrics.height)),
        triggered: None,
        hovered: None,
        item_rects: Vec::new(),
    };
    Area::new(id.with("area"))
        .order(Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui: &mut Ui| {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(metrics.width, metrics.height), Sense::click());
            out.rect = rect;
            BoxShadow::new(style.shadow).paint_with_opacity(
                ctx,
                ui.painter(),
                rect,
                style.radius,
                style.shadow.opacity,
            );
            RoundRect::new(style.radius, menu.item_bg).paint(ui.painter(), rect);
            let painter = ui.painter().with_clip_rect(rect);
            let mut y = rect.min.y + style.scroll_padding.top;
            for (idx, row) in rows.iter().enumerate() {
                match row {
                    EntryMenuRow::Separator => {
                        let sep_y = y + menu.separator.padding.top;
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(rect.min.x + menu.separator.padding.left, sep_y),
                                Vec2::new(
                                    rect.width() - menu.separator.padding.horizontal(),
                                    menu.separator.width,
                                ),
                            ),
                            CornerRadius::ZERO,
                            menu.separator.fg,
                        );
                        y += metrics.sep_h;
                    }
                    EntryMenuRow::ExpandedSeparator => {
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(rect.min.x, y + EXPANDED_SEPARATOR_PADDING),
                                Vec2::new(rect.width(), EXPANDED_SEPARATOR_WIDTH),
                            ),
                            CornerRadius::ZERO,
                            menu.separator.fg,
                        );
                        y += 2.0 * EXPANDED_SEPARATOR_PADDING + EXPANDED_SEPARATOR_WIDTH;
                    }
                    EntryMenuRow::Item(item) => {
                        let item_rect = Rect::from_min_size(
                            Pos2::new(rect.min.x, y),
                            Vec2::new(rect.width(), metrics.row_h),
                        );
                        out.item_rects.push((idx, item_rect));
                        let resp = ui.interact(item_rect, id.with(("row", idx)), Sense::click());
                        let (text, shortcut) = split_shortcut(&item.label);
                        let label = match shortcut {
                            Some(shortcut) => format!("{text} {shortcut}"),
                            None => text.to_owned(),
                        };
                        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
                        if resp.hovered() {
                            out.hovered = Some(idx);
                            ctx.set_cursor_icon(CursorIcon::PointingHand);
                        }
                        let active = resp.hovered() || selected == Some(idx);
                        if active {
                            painter.rect_filled(item_rect, CornerRadius::ZERO, menu.item_bg_over);
                        }
                        let fg = if item.attention {
                            menu.item_fg_attention
                        } else if active {
                            menu.item_fg_over
                        } else {
                            menu.item_fg
                        };
                        if let Some(icon) = item.icon {
                            // `icon->paint(p, itemIconPosition)` in the icon's own colour
                            // (`menuIconColor`, i.e. `windowBoldFg`); attention rows use
                            // `menuIconAttentionColor`.
                            let icon_fg = item.attention.then_some(menu.item_fg_attention);
                            let icon_pos = item_rect.min
                                + Vec2::new(
                                    px(st::menu_with_icons.item_icon_position.x),
                                    px(st::menu_with_icons.item_icon_position.y),
                                );
                            let icon_size = icon.scaled_size().map_or(Vec2::splat(24.0), |sz| {
                                Vec2::new(sz.width as f32, sz.height as f32)
                            });
                            paint_style_icon(
                                &painter,
                                Rect::from_min_size(icon_pos, icon_size),
                                icon,
                                colors,
                                icon_fg,
                            );
                        }
                        painter.text(
                            Pos2::new(
                                item_rect.min.x + menu.item_padding.left,
                                item_rect.center().y,
                            ),
                            Align2::LEFT_CENTER,
                            text,
                            font.clone(),
                            fg,
                        );
                        if !item.submenu.is_empty() {
                            // `Action::paint`: `width() - itemRightSkip - arrow.width()`.
                            let aw = submenu_arrow_width();
                            let arrow = Rect::from_min_size(
                                Pos2::new(
                                    item_rect.max.x - menu.item_right_skip - aw,
                                    item_rect.center().y - aw * 0.5,
                                ),
                                Vec2::splat(aw),
                            );
                            paint_style_icon(
                                &painter,
                                arrow,
                                st::menu_icon_submenu_arrow,
                                colors,
                                Some(menu.item_fg),
                            );
                        } else {
                            if let Some(shortcut) = shortcut {
                                // `p.drawTextRight(itemPadding.right(), ...)` in
                                // `itemFgShortcut` / `itemFgShortcutOver`.
                                painter.text(
                                    Pos2::new(
                                        item_rect.max.x - menu.item_padding.right,
                                        item_rect.center().y,
                                    ),
                                    Align2::RIGHT_CENTER,
                                    shortcut,
                                    font.clone(),
                                    if active {
                                        menu.item_fg_shortcut_over
                                    } else {
                                        menu.item_fg_shortcut
                                    },
                                );
                            }
                            if resp.clicked() || (trigger_selected && selected == Some(idx)) {
                                out.triggered = Some(item.id.clone());
                            }
                        }
                        y += metrics.row_h;
                    }
                }
            }
        });
    out
}

fn next_selectable(rows: &[EntryMenuRow], from: Option<usize>, dir: isize) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let len = rows.len() as isize;
    let mut idx = from.map_or(if dir > 0 { -1 } else { len }, |i| i as isize);
    for _ in 0..rows.len() {
        idx = (idx + dir).rem_euclid(len);
        if matches!(rows[idx as usize], EntryMenuRow::Item(_)) {
            return Some(idx as usize);
        }
    }
    None
}

impl EntryMenu {
    /// Opens the menu with its top-left corner at `anchor`.
    pub fn open_at(&mut self, ctx: &egui::Context, anchor: Pos2) {
        self.open = true;
        self.anchor = anchor;
        self.hovered = None;
        self.submenu_index = None;
        self.submenu_hovered = None;
        self.opened_frame = ctx.cumulative_frame_nr();
        self.unshifted = false;
        ctx.request_repaint();
    }

    /// Opens the menu with its top-right corner at `top_right` (`PanelAnimation::Origin::TopRight`),
    /// full-width separators drawn in place (a `popupMenuExpandedSeparator` menu such as the chat
    /// top bar's) and without the chats-list shift-up.
    pub fn open_below(&mut self, ctx: &egui::Context, top_right: Pos2) {
        self.open_at(ctx, top_right);
        self.unshifted = true;
    }

    /// Closes the menu.
    pub fn close(&mut self) {
        self.open = false;
        self.submenu_index = None;
    }

    /// Whether the menu is visible.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Paints the menu (and the open submenu) and returns the id of a triggered row.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        rows: &[EntryMenuRow],
        style: &PopupMenuStyle,
        colors: &dyn ColorResolver,
    ) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut trigger = false;
        ctx.input(|i| {
            if i.key_pressed(Key::Escape) {
                self.open = false;
            } else if i.key_pressed(Key::ArrowDown) {
                self.hovered = next_selectable(rows, self.hovered, 1);
            } else if i.key_pressed(Key::ArrowUp) {
                self.hovered = next_selectable(rows, self.hovered, -1);
            } else if i.key_pressed(Key::Enter) && self.hovered.is_some() {
                trigger = true;
            }
        });
        if !self.open {
            return None;
        }

        let metrics = measure(ctx, rows, style);
        let screen = ctx.viewport_rect();
        let left = if self.unshifted {
            self.anchor.x - metrics.width
        } else {
            self.anchor.x
        };
        let pos = Pos2::new(
            left.min(screen.max.x - metrics.width - 4.0)
                .max(screen.min.x + 4.0),
            (self.anchor.y
                - if self.unshifted {
                    0.0
                } else {
                    metrics.top_shift
                })
            .min(screen.max.y - metrics.height - 4.0)
            .max(screen.min.y + 4.0),
        );
        let painted = paint_rows(
            ctx,
            id,
            pos,
            rows,
            style,
            colors,
            &metrics,
            self.hovered,
            trigger,
        );
        if let Some(h) = painted.hovered {
            self.hovered = Some(h);
            let has_sub = matches!(&rows[h], EntryMenuRow::Item(item) if !item.submenu.is_empty());
            if has_sub {
                if self.submenu_index != Some(h) {
                    self.submenu_hovered = None;
                }
                self.submenu_index = Some(h);
            } else {
                self.submenu_index = None;
            }
        }
        let mut triggered = painted.triggered;
        let mut sub_rect = None;
        if let Some(sub_idx) = self.submenu_index
            && let Some(EntryMenuRow::Item(parent)) = rows.get(sub_idx)
            && let Some((_, parent_rect)) = painted.item_rects.iter().find(|(i, _)| *i == sub_idx)
        {
            let sub_metrics = measure(ctx, &parent.submenu, style);
            let mut sub_pos = Pos2::new(
                painted.rect.max.x,
                parent_rect.min.y - style.scroll_padding.top,
            );
            if sub_pos.x + sub_metrics.width > screen.max.x - 4.0 {
                sub_pos.x = painted.rect.min.x - sub_metrics.width;
            }
            sub_pos.y = sub_pos
                .y
                .min(screen.max.y - sub_metrics.height - 4.0)
                .max(screen.min.y + 4.0);
            let sub = paint_rows(
                ctx,
                id.with(("sub", sub_idx)),
                sub_pos,
                &parent.submenu,
                style,
                colors,
                &sub_metrics,
                self.submenu_hovered,
                false,
            );
            if sub.hovered.is_some() {
                self.submenu_hovered = sub.hovered;
            }
            if sub.triggered.is_some() {
                triggered = sub.triggered;
            }
            sub_rect = Some(sub.rect);
        }

        if triggered.is_some() {
            self.close();
            return triggered;
        }

        if ctx.cumulative_frame_nr() > self.opened_frame
            && ctx.input(|i| i.pointer.any_pressed())
            && ctx.pointer_interact_pos().is_some_and(|p| {
                !painted.rect.contains(p) && !sub_rect.is_some_and(|r| r.contains(p))
            })
        {
            self.close();
        }
        None
    }
}
