//! Floating popup menus with 9-slice shadows, icons, shortcuts, toggles, and ripples.
//!
//! Port of `Telegram/lib_ui/ui/widgets/popup_menu.h`,
//! `Telegram/lib_ui/ui/widgets/popup_menu.cpp`, and
//! `Telegram/lib_ui/ui/widgets/menu/menu_action.cpp`.

use egui::{
    Align2, Area, CornerRadius, CursorIcon, Id, Key, Order, Pos2, Rect, Role, Sense, Ui, Vec2,
    WidgetInfo,
};

use tg_style::{DefaultPaletteResolver, Icon};

use crate::anim::{SimpleAnimation, Transition, interpolate_f32, with_alpha};
use crate::effects::{RippleMask, update_and_paint_ripple};
use crate::icons::paint_style_icon;
use crate::painter::{BoxShadow, RoundRect};
use crate::style::{PopupMenuStyle, ToggleStyle};
use crate::widgets::buttons::{IconKind, paint_icon};
use crate::widgets::checkbox::paint_toggle_view;

/// `menuWithIcons.itemPadding.left`: label inset when any item has an icon.
const MENU_WITH_ICONS_LEFT: f32 = 54.0;
/// `menuWithIcons.itemPadding.bottom`.
const MENU_WITH_ICONS_BOTTOM: f32 = 8.0;
/// `menuWithIcons.itemIconPosition`: top-left of the 24 px icon inside the row.
const MENU_ICON_POSITION: Vec2 = Vec2::new(15.0, 5.0);
/// Size of the `menu/*` tdesktop icons.
const MENU_ICON_SIZE: f32 = 24.0;

/// Rect of the leading icon inside a menu row (`menuWithIcons.itemIconPosition`).
fn menu_icon_rect(item_rect: Rect) -> Rect {
    Rect::from_min_size(
        item_rect.min + MENU_ICON_POSITION,
        Vec2::splat(MENU_ICON_SIZE),
    )
}

/// Single item inside a [`PopupMenu`].
#[derive(Debug, Clone)]
pub enum MenuItem {
    /// Standard clickable menu action.
    Action {
        id: String,
        label: String,
        shortcut: Option<String>,
        icon: Option<IconKind>,
        /// tdesktop vector icon (`st::menu_icon_*`), painted instead of `icon` when set.
        style_icon: Option<Icon>,
        enabled: bool,
        attention: bool,
        /// Painted in `boxTextFgGood` (label and icon), like the "Unmute" row of tdesktop's
        /// mute menu (`MuteItem`, `menu/menu_mute.cpp`).
        good: bool,
        has_submenu: bool,
    },
    /// Toggleable menu item with an inline tdesktop switch.
    Toggle {
        id: String,
        label: String,
        checked: bool,
        icon: Option<IconKind>,
    },
    /// Horizontal separator rule.
    Separator,
}

impl MenuItem {
    /// Create an enabled standard menu action.
    pub fn action(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::Action {
            id: id.into(),
            label: label.into(),
            shortcut: None,
            icon: None,
            style_icon: None,
            enabled: true,
            attention: false,
            good: false,
            has_submenu: false,
        }
    }

    /// Attach a tdesktop vector icon (`st::menu_icon_*` from `menu_icons.style`) to an action,
    /// drawn at `menuWithIcons.itemIconPosition` and tinted like the label. Ignored on toggles
    /// and separators.
    pub fn with_style_icon(mut self, icon: Icon) -> Self {
        if let Self::Action { style_icon, .. } = &mut self {
            *style_icon = Some(icon);
        }
        self
    }

    /// The tdesktop vector icon attached with [`Self::with_style_icon`], if any.
    #[must_use]
    pub const fn style_icon(&self) -> Option<Icon> {
        match self {
            Self::Action { style_icon, .. } => *style_icon,
            Self::Toggle { .. } | Self::Separator => None,
        }
    }

    /// Attach a leading icon to an action or toggle.
    pub fn with_icon(mut self, icon: IconKind) -> Self {
        match &mut self {
            Self::Action { icon: slot, .. } | Self::Toggle { icon: slot, .. } => {
                *slot = Some(icon);
            }
            Self::Separator => {}
        }
        self
    }

    /// Attach a right-aligned keyboard shortcut hint.
    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        if let Self::Action { shortcut: slot, .. } = &mut self {
            *slot = Some(shortcut.into());
        }
        self
    }

    /// Mark an action as destructive/attention (red label & icon).
    pub fn attention(mut self) -> Self {
        if let Self::Action { attention, .. } = &mut self {
            *attention = true;
        }
        self
    }

    /// Mark an action as positive (green `boxTextFgGood` label & icon, `MuteItem`'s "Unmute").
    pub fn good(mut self) -> Self {
        if let Self::Action { good, .. } = &mut self {
            *good = true;
        }
        self
    }

    /// Disable an action.
    pub fn disabled(mut self) -> Self {
        if let Self::Action { enabled, .. } = &mut self {
            *enabled = false;
        }
        self
    }

    /// Show a submenu chevron arrow on the right.
    pub fn with_submenu(mut self) -> Self {
        if let Self::Action { has_submenu, .. } = &mut self {
            *has_submenu = true;
        }
        self
    }
}

/// Persistent open/animation state for a [`PopupMenu`].
#[derive(Debug, Clone, Default)]
pub struct PopupMenuState {
    open: bool,
    anchor: Pos2,
    selected_index: Option<usize>,
    show_anim: SimpleAnimation,
    opened_frame: u64,
}

impl PopupMenuState {
    /// Create a closed popup menu state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the popup anchored at `anchor` (in screen coordinates).
    pub fn open_at(&mut self, ctx: &egui::Context, anchor: Pos2, style: &PopupMenuStyle) {
        let now = ctx.input(|i| i.time);
        self.open = true;
        self.anchor = anchor;
        self.selected_index = None;
        self.opened_frame = ctx.cumulative_frame_nr();
        self.show_anim.start(
            now,
            0.0,
            1.0,
            style.show_duration_ms,
            Transition::EaseOutCirc,
        );
        ctx.request_repaint();
    }

    /// Close the popup menu immediately.
    pub fn close(&mut self) {
        self.open = false;
        self.show_anim.stop();
    }

    /// Whether the popup menu is currently visible.
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Render the popup menu in a foreground [`Area`] when open.
    /// Returns the triggered item `id`, if one was clicked or activated via Enter.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        items: &[MenuItem],
        style: &PopupMenuStyle,
        toggle_style: &ToggleStyle,
    ) -> Option<String> {
        if !self.open {
            return None;
        }

        let now = ctx.input(|i| i.time);
        let progress = self.show_anim.sample(now, 1.0);
        if self.show_anim.animating() {
            ctx.request_repaint();
        }

        // Escape dismisses menu; Up/Down navigates enabled items.
        let mut trigger_selected = false;
        ctx.input(|i| {
            if i.key_pressed(Key::Escape) {
                self.open = false;
            } else if i.key_pressed(Key::ArrowDown) {
                self.move_selection(items, 1);
            } else if i.key_pressed(Key::ArrowUp) {
                self.move_selection(items, -1);
            } else if i.key_pressed(Key::Enter) && self.selected_index.is_some() {
                trigger_selected = true;
            }
        });
        if !self.open {
            return None;
        }

        let menu_st = &style.menu;
        let has_any_icon = items.iter().any(|it| match it {
            MenuItem::Action {
                icon, style_icon, ..
            } => icon.is_some() || style_icon.is_some(),
            MenuItem::Toggle { icon, .. } => icon.is_some(),
            MenuItem::Separator => false,
        });
        // `menuWithIcons`: itemPadding margins(54px, 8px, 17px, 8px).
        let (left_pad, bottom_pad) = if has_any_icon {
            (
                menu_st.item_padding.left.max(MENU_WITH_ICONS_LEFT),
                menu_st.item_padding.bottom.max(MENU_WITH_ICONS_BOTTOM),
            )
        } else {
            (menu_st.item_padding.left, menu_st.item_padding.bottom)
        };
        let font_id = menu_st.item_style.font.font_id();
        let row_h = menu_st.item_padding.top + menu_st.item_style.font.line_height() + bottom_pad;
        let sep_h = menu_st.separator.padding.vertical() + menu_st.separator.width;

        // Compute natural width clamped to `[width_min, width_max]`.
        let mut width = menu_st.width_min;
        let mut total_h = style.scroll_padding.vertical();
        for item in items {
            match item {
                MenuItem::Separator => total_h += sep_h,
                MenuItem::Action {
                    label,
                    shortcut,
                    has_submenu,
                    ..
                } => {
                    total_h += row_h;
                    let approx_label = label.chars().count() as f32 * 7.2;
                    let approx_extra = if *has_submenu {
                        menu_st.item_right_skip + 16.0
                    } else if let Some(sc) = shortcut {
                        menu_st.item_right_skip + sc.chars().count() as f32 * 6.8 + 16.0
                    } else {
                        0.0
                    };
                    width = width
                        .max(left_pad + approx_label + approx_extra + menu_st.item_padding.right);
                }
                MenuItem::Toggle { label, .. } => {
                    total_h += row_h;
                    let approx_label = label.chars().count() as f32 * 7.2;
                    width = width.max(left_pad + approx_label + 48.0 + menu_st.item_padding.right);
                }
            }
        }
        width = width.clamp(menu_st.width_min, menu_st.width_max);

        // PanelAnimation height/opacity reveal (`defaultPanelAnimation` in widgets.style).
        let anim_h = interpolate_f32(total_h * 0.35, total_h, progress as f32);
        let anim_opacity = ((progress / 0.35).clamp(0.0, 1.0)) as f32;

        // Clamp position inside viewport.
        let screen = ctx.viewport_rect();
        let x = self
            .anchor
            .x
            .min(screen.max.x - width - 8.0)
            .max(screen.min.x + 8.0);
        let y = self
            .anchor
            .y
            .min(screen.max.y - total_h - 8.0)
            .max(screen.min.y + 8.0);

        let mut triggered: Option<String> = None;
        let mut menu_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, anim_h));

        let area_resp = Area::new(id.with("_tg_popup_area"))
            .order(Order::Foreground)
            .fixed_pos(Pos2::new(x, y))
            .show(ctx, |ui: &mut Ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(width, anim_h), Sense::click());
                menu_rect = rect;

                // 9-slice blurred BoxShadow behind popup card.
                BoxShadow::new(style.shadow).paint_with_opacity(
                    ctx,
                    ui.painter(),
                    rect,
                    style.radius,
                    style.shadow.opacity * anim_opacity,
                );
                RoundRect::new(
                    style.radius,
                    with_alpha(menu_st.item_bg, f64::from(anim_opacity)),
                )
                .paint(ui.painter(), rect);

                let clipped = ui.painter().with_clip_rect(rect);
                let mut cursor_y = rect.min.y + style.scroll_padding.top;

                for (idx, item) in items.iter().enumerate() {
                    match item {
                        MenuItem::Separator => {
                            let sep_y = cursor_y + menu_st.separator.padding.top;
                            clipped.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(rect.min.x, sep_y),
                                    Vec2::new(rect.width(), menu_st.separator.width),
                                ),
                                CornerRadius::ZERO,
                                with_alpha(menu_st.separator.fg, f64::from(anim_opacity)),
                            );
                            cursor_y += sep_h;
                        }
                        MenuItem::Action {
                            id: action_id,
                            label,
                            shortcut,
                            icon,
                            style_icon,
                            enabled,
                            attention,
                            good,
                            has_submenu,
                        } => {
                            let item_rect = Rect::from_min_size(
                                Pos2::new(rect.min.x, cursor_y),
                                Vec2::new(rect.width(), row_h),
                            );
                            let resp = ui.interact(
                                item_rect,
                                id.with(("item", idx)),
                                if *enabled {
                                    Sense::click()
                                } else {
                                    Sense::hover()
                                },
                            );
                            resp.widget_info(|| WidgetInfo::labeled(Role::Button, *enabled, label));
                            if *enabled && resp.hovered() {
                                self.selected_index = Some(idx);
                                ctx.set_cursor_icon(CursorIcon::PointingHand);
                            }
                            let selected = *enabled && self.selected_index == Some(idx);
                            if selected {
                                clipped.rect_filled(
                                    item_rect,
                                    CornerRadius::ZERO,
                                    with_alpha(menu_st.item_bg_over, f64::from(anim_opacity)),
                                );
                            }
                            if *enabled {
                                update_and_paint_ripple(
                                    &clipped,
                                    id.with(("ripple", idx)),
                                    item_rect,
                                    RippleMask::Rect,
                                    &menu_st.ripple,
                                    resp.is_pointer_button_down_on(),
                                    None,
                                );
                            }
                            let fg = if !*enabled {
                                menu_st.item_fg_disabled
                            } else if *attention {
                                menu_st.item_fg_attention
                            } else if *good {
                                menu_st.item_fg_good
                            } else if selected {
                                menu_st.item_fg_over
                            } else {
                                menu_st.item_fg
                            };
                            let fg = with_alpha(fg, f64::from(anim_opacity));

                            let icon_fg = if !*enabled {
                                menu_st.item_fg_disabled
                            } else if *attention {
                                menu_st.item_fg_attention
                            } else if *good {
                                menu_st.item_fg_good
                            } else if selected {
                                menu_st.item_icon_fg_over
                            } else {
                                menu_st.item_icon_fg
                            };
                            let icon_fg = with_alpha(icon_fg, f64::from(anim_opacity));
                            if let Some(st_icon) = style_icon {
                                paint_style_icon(
                                    &clipped,
                                    menu_icon_rect(item_rect),
                                    *st_icon,
                                    &DefaultPaletteResolver,
                                    Some(icon_fg),
                                );
                            } else if let Some(ic) = icon {
                                let icon_rect = Rect::from_center_size(
                                    menu_icon_rect(item_rect).center(),
                                    Vec2::splat(16.0),
                                );
                                paint_icon(&clipped, icon_rect, *ic, icon_fg);
                            }

                            clipped.text(
                                Pos2::new(item_rect.min.x + left_pad, item_rect.center().y),
                                Align2::LEFT_CENTER,
                                label,
                                font_id.clone(),
                                fg,
                            );

                            if *has_submenu {
                                let arrow_rect = Rect::from_center_size(
                                    Pos2::new(
                                        item_rect.max.x - menu_st.item_padding.right,
                                        item_rect.center().y,
                                    ),
                                    Vec2::splat(12.0),
                                );
                                paint_icon(&clipped, arrow_rect, IconKind::ChevronRight, fg);
                            } else if let Some(sc) = shortcut {
                                let sc_fg = if !*enabled {
                                    menu_st.item_fg_shortcut_disabled
                                } else if selected {
                                    menu_st.item_fg_shortcut_over
                                } else {
                                    menu_st.item_fg_shortcut
                                };
                                clipped.text(
                                    Pos2::new(
                                        item_rect.max.x - menu_st.item_padding.right,
                                        item_rect.center().y,
                                    ),
                                    Align2::RIGHT_CENTER,
                                    sc,
                                    font_id.clone(),
                                    with_alpha(sc_fg, f64::from(anim_opacity)),
                                );
                            }

                            if *enabled && (resp.clicked() || (trigger_selected && selected)) {
                                triggered = Some(action_id.clone());
                                self.open = false;
                            }
                            cursor_y += row_h;
                        }
                        MenuItem::Toggle {
                            id: toggle_id,
                            label,
                            checked,
                            icon,
                        } => {
                            let item_rect = Rect::from_min_size(
                                Pos2::new(rect.min.x, cursor_y),
                                Vec2::new(rect.width(), row_h),
                            );
                            let resp =
                                ui.interact(item_rect, id.with(("toggle", idx)), Sense::click());
                            resp.widget_info(|| {
                                WidgetInfo::selected(Role::CheckBox, true, *checked, label)
                            });
                            if resp.hovered() {
                                self.selected_index = Some(idx);
                                ctx.set_cursor_icon(CursorIcon::PointingHand);
                            }
                            let selected = self.selected_index == Some(idx);
                            if selected {
                                clipped.rect_filled(
                                    item_rect,
                                    CornerRadius::ZERO,
                                    with_alpha(menu_st.item_bg_over, f64::from(anim_opacity)),
                                );
                            }
                            if let Some(ic) = icon {
                                let icon_rect = Rect::from_center_size(
                                    menu_icon_rect(item_rect).center(),
                                    Vec2::splat(16.0),
                                );
                                paint_icon(
                                    &clipped,
                                    icon_rect,
                                    *ic,
                                    with_alpha(
                                        if selected {
                                            menu_st.item_icon_fg_over
                                        } else {
                                            menu_st.item_icon_fg
                                        },
                                        f64::from(anim_opacity),
                                    ),
                                );
                            }
                            clipped.text(
                                Pos2::new(item_rect.min.x + left_pad, item_rect.center().y),
                                Align2::LEFT_CENTER,
                                label,
                                font_id.clone(),
                                with_alpha(menu_st.item_fg, f64::from(anim_opacity)),
                            );
                            let tsize = crate::widgets::checkbox::Toggle::toggle_size(toggle_style);
                            let trect = Rect::from_min_size(
                                Pos2::new(
                                    item_rect.max.x - menu_st.item_padding.right - tsize.x,
                                    item_rect.center().y - tsize.y * 0.5,
                                ),
                                tsize,
                            );
                            paint_toggle_view(
                                &clipped,
                                trect,
                                toggle_style,
                                if *checked { 1.0 } else { 0.0 },
                                f64::from(anim_opacity),
                            );
                            if resp.clicked() || (trigger_selected && selected) {
                                triggered = Some(toggle_id.clone());
                            }
                            cursor_y += row_h;
                        }
                    }
                }
            });

        // Close on click outside (ignore the exact frame it was opened).
        let current_frame = ctx.cumulative_frame_nr();
        if current_frame > self.opened_frame
            && ctx.input(|i| i.pointer.any_pressed())
            && ctx
                .pointer_interact_pos()
                .is_some_and(|p| !menu_rect.contains(p) && !area_resp.response.rect.contains(p))
        {
            self.open = false;
        }

        triggered
    }

    fn move_selection(&mut self, items: &[MenuItem], dir: isize) {
        if items.is_empty() {
            return;
        }
        let len = items.len() as isize;
        let mut idx = self
            .selected_index
            .map_or(if dir > 0 { -1 } else { len }, |i| i as isize);
        for _ in 0..items.len() {
            idx = (idx + dir).rem_euclid(len);
            let selectable = match &items[idx as usize] {
                MenuItem::Action { enabled, .. } => *enabled,
                MenuItem::Toggle { .. } => true,
                MenuItem::Separator => false,
            };
            if selectable {
                self.selected_index = Some(idx as usize);
                return;
            }
        }
    }
}
