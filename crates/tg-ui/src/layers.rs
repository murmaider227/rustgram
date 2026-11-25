//! Stacked modal layer and dialog box system with fade/slide transitions.
//!
//! Port of `Telegram/lib_ui/ui/layers/layer_widget.h`,
//! `Telegram/lib_ui/ui/layers/layer_widget.cpp`,
//! `Telegram/lib_ui/ui/layers/box_layer_widget.cpp`, and
//! `Telegram/lib_ui/ui/layers/box_content.cpp`.

use egui::{Align2, Area, CornerRadius, Id, Key, Order, Pos2, Rect, Sense, Ui, Vec2};

use crate::anim::{SimpleAnimation, Transition, interpolate_f32, with_alpha};
use crate::effects::InfiniteRadialAnimation;
use crate::painter::{BoxShadow, RoundRect};
use crate::style::{
    BoxStyle, IconButtonStyle, InfiniteRadialAnimationStyle, ScrollAreaStyle, constants,
};
use crate::theme::Colors;
use crate::widgets::buttons::{IconButton, IconKind, RoundButton};
use crate::widgets::scroll_area::TgScrollArea;

/// Visual variant of a box footer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxButtonKind {
    /// Standard light blue text button (`defaultBoxButton`).
    #[default]
    Default,
    /// Destructive red button (`attentionBoxButton`).
    Attention,
}

/// Button descriptor in a [`BoxModal`] footer.
#[derive(Debug, Clone)]
pub struct BoxButton {
    pub id: String,
    pub label: String,
    pub kind: BoxButtonKind,
}

impl BoxButton {
    /// Standard box action button (`defaultBoxButton`).
    pub fn default_btn(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: BoxButtonKind::Default,
        }
    }

    /// Destructive/attention box button (`attentionBoxButton`).
    pub fn attention(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: BoxButtonKind::Attention,
        }
    }
}

/// Specification for a single modal box on a [`LayerStack`].
#[derive(Debug, Clone)]
pub struct BoxModal {
    pub id: String,
    pub title: String,
    pub additional_title: Option<String>,
    pub width: f32,
    pub max_content_height: f32,
    pub close_by_outside_click: bool,
    pub show_close_button: bool,
    pub show_loading: bool,
    pub left_button: Option<BoxButton>,
    pub buttons: Vec<BoxButton>,
}

impl BoxModal {
    /// Create a modal box with default tdesktop width (`boxWidth = 320px`).
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            additional_title: None,
            width: constants::BOX_WIDE_WIDTH,
            max_content_height: 360.0,
            close_by_outside_click: true,
            show_close_button: true,
            show_loading: false,
            left_button: None,
            buttons: Vec::new(),
        }
    }

    /// Set explicit box width.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Set secondary muted title text displayed next to the main title.
    pub fn additional_title(mut self, text: impl Into<String>) -> Self {
        self.additional_title = Some(text.into());
        self
    }

    /// Add a right-aligned footer button.
    pub fn add_button(mut self, button: BoxButton) -> Self {
        self.buttons.push(button);
        self
    }

    /// Set a left-aligned footer button (e.g. "Reset" or "Help").
    pub fn with_left_button(mut self, button: BoxButton) -> Self {
        self.left_button = Some(button);
        self
    }

    /// Show the radial loading indicator in the bottom-left corner.
    pub fn loading(mut self, loading: bool) -> Self {
        self.show_loading = loading;
        self
    }
}

/// Event emitted by [`LayerStack::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerEvent {
    /// ID of the box that produced the event.
    pub box_id: String,
    /// Clicked button ID, or `"__close__"` if closed via X / Escape / outside click.
    pub action_id: String,
}

struct StackEntry {
    modal: BoxModal,
    enter_anim: SimpleAnimation,
    opened_frame: u64,
}

/// Manages a stack of modal boxes with dimmed backdrop (`layerBg`) and
/// fade/slide transitions (`boxDuration = 200ms`, `easeOutCirc` on show, `linear` on hide).
#[derive(Default)]
pub struct LayerStack {
    entries: Vec<StackEntry>,
    backdrop_anim: SimpleAnimation,
    was_shown: bool,
}

impl LayerStack {
    /// Create an empty layer stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new modal box onto the top of the stack.
    pub fn push(&mut self, ctx: &egui::Context, modal: BoxModal, style: &BoxStyle) {
        let now = ctx.input(|i| i.time);
        let mut enter_anim = SimpleAnimation::new();
        enter_anim.start(now, 0.0, 1.0, style.duration_ms, Transition::EaseOutCirc);
        if self.entries.is_empty() {
            self.backdrop_anim
                .change(now, 1.0, style.duration_ms, Transition::EaseOutCirc);
            self.was_shown = true;
        }
        self.entries.push(StackEntry {
            modal,
            enter_anim,
            opened_frame: ctx.cumulative_frame_nr(),
        });
        ctx.request_repaint();
    }

    /// Pop the topmost modal box.
    pub fn pop(&mut self, ctx: &egui::Context, style: &BoxStyle) -> Option<BoxModal> {
        let popped = self.entries.pop().map(|e| e.modal);
        if self.entries.is_empty() && self.was_shown {
            let now = ctx.input(|i| i.time);
            self.was_shown = false;
            self.backdrop_anim
                .change(now, 0.0, style.duration_ms, Transition::Linear);
            ctx.request_repaint();
        }
        popped
    }

    /// Close all stacked modal boxes.
    pub fn clear(&mut self, ctx: &egui::Context, style: &BoxStyle) {
        if !self.entries.is_empty() {
            self.entries.clear();
            let now = ctx.input(|i| i.time);
            self.was_shown = false;
            self.backdrop_anim
                .change(now, 0.0, style.duration_ms, Transition::Linear);
            ctx.request_repaint();
        }
    }

    /// Number of currently stacked modal boxes.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no modal boxes are currently open.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// ID of the topmost open modal box, if any.
    pub fn top_id(&self) -> Option<&str> {
        self.entries.last().map(|e| e.modal.id.as_str())
    }

    /// Render the backdrop and active modal box(es). `render_body` is invoked with
    /// `(box_id, ui)` to render the interior content of the active box.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        bounds: Rect,
        style: &BoxStyle,
        colors: &dyn Colors,
        mut render_body: impl FnMut(&str, &mut Ui),
    ) -> Option<LayerEvent> {
        let now = ctx.input(|i| i.time);
        let backdrop_opacity = self
            .backdrop_anim
            .sample(now, if self.entries.is_empty() { 0.0 } else { 1.0 });
        if self.backdrop_anim.animating() {
            ctx.request_repaint();
        }

        if self.entries.is_empty() && backdrop_opacity <= 0.01 {
            return None;
        }

        let mut event: Option<LayerEvent> = None;

        // Paint full-bounds dimmed backdrop (`layerBg` = #0000007f) and block clicks behind it.
        let mut outside_clicked = false;
        Area::new(Id::unique("tg_layer_backdrop"))
            .order(Order::Middle)
            .fixed_pos(bounds.min)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(bounds.size(), Sense::click());
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::ZERO,
                    with_alpha(style.layer_bg, backdrop_opacity),
                );
                if resp.clicked() {
                    outside_clicked = true;
                }
            });

        if self.entries.is_empty() {
            return None;
        }

        // Escape closes top modal.
        let escape_pressed = ctx.input(|i| i.key_pressed(Key::Escape));

        let top_idx = self.entries.len() - 1;
        let current_frame = ctx.cumulative_frame_nr();
        let mut box_rect = Rect::NOTHING;

        // Render stacked boxes from bottom to top; only top is interactive.
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            let is_top = idx == top_idx;
            let progress = entry.enter_anim.sample(now, 1.0);
            if entry.enter_anim.animating() {
                ctx.request_repaint();
            }

            let box_w = entry.modal.width.min((bounds.width() - 32.0).max(240.0));
            // Subtle upward slide on entry (`12px -> 0px`).
            let slide_y = interpolate_f32(12.0, 0.0, progress as f32);
            let opacity = progress as f32;
            let est_h = (style.title_height
                + 180.0
                + style.button_padding.vertical()
                + style.button_height)
                .min(bounds.height() - style.margin.vertical());
            let center_pos = Pos2::new(
                bounds.center().x - box_w * 0.5,
                (bounds.center().y - est_h * 0.5 + slide_y).max(bounds.min.y + style.margin.top),
            );

            let area_id = Id::unique(("tg_layer_box", idx, &entry.modal.id));
            Area::new(area_id)
                .order(Order::Foreground)
                .interactable(is_top)
                .fixed_pos(center_pos)
                .show(ctx, |ui| {
                    let frame_start = ui.cursor().min;

                    // Reserve shadow/bg draw index behind content using a painter layer.
                    let bg_idx = ui.painter().add(egui::Shape::Noop);
                    let shadow_painter = ui.painter().clone();

                    let inner_resp = ui
                        .allocate_ui_with_layout(
                            Vec2::new(box_w, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_min_width(box_w);
                                ui.set_max_width(box_w);

                                // A box with an empty title and no close button has no title
                                // row; its content starts at `boxPadding.top` (untitled
                                // `Ui::BoxContent`, e.g. `DeleteMessagesBox`).
                                let has_title =
                                    !entry.modal.title.is_empty() || entry.modal.show_close_button;
                                if has_title {
                                    // Title row (`boxTitleHeight = 48px`)
                                    let (title_rect, _) = ui.allocate_exact_size(
                                        Vec2::new(box_w, style.title_height),
                                        Sense::hover(),
                                    );
                                    let title_font = style.title.style.font.font_id();
                                    let title_fg =
                                        with_alpha(style.title.text_fg, f64::from(opacity));
                                    let title_galley = ui.painter().layout_no_wrap(
                                        entry.modal.title.clone(),
                                        title_font,
                                        title_fg,
                                    );
                                    let title_pos = Pos2::new(
                                        title_rect.min.x + 24.0,
                                        title_rect.center().y - title_galley.size().y * 0.5,
                                    );
                                    ui.painter()
                                        .galley(title_pos, title_galley.clone(), title_fg);

                                    if let Some(add) = &entry.modal.additional_title {
                                        ui.painter().text(
                                            Pos2::new(
                                                title_pos.x + title_galley.size().x + 9.0,
                                                title_rect.center().y,
                                            ),
                                            Align2::LEFT_CENTER,
                                            add,
                                            egui::FontId::proportional(constants::FSIZE),
                                            with_alpha(
                                                style.title_additional_fg,
                                                f64::from(opacity),
                                            ),
                                        );
                                    }

                                    if entry.modal.show_close_button {
                                        let close_st = IconButtonStyle::box_title_close(colors);
                                        let close_rect = Rect::from_min_size(
                                            Pos2::new(
                                                title_rect.max.x - close_st.width,
                                                title_rect.min.y,
                                            ),
                                            Vec2::new(close_st.width, close_st.height),
                                        );
                                        let mut close_ui = ui
                                            .new_child(egui::UiBuilder::new().max_rect(close_rect));
                                        if IconButton::new(IconKind::Close, close_st)
                                            .show(&mut close_ui)
                                            .clicked()
                                            && is_top
                                        {
                                            event = Some(LayerEvent {
                                                box_id: entry.modal.id.clone(),
                                                action_id: "__close__".to_owned(),
                                            });
                                        }
                                    }
                                } else {
                                    ui.add_space(style.content_padding.top);
                                }

                                // Scrollable body area with `boxPadding`.
                                let scroll_st = ScrollAreaStyle::solid(colors);
                                let body_w = (box_w - style.content_padding.horizontal()).max(80.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(style.content_padding.left);
                                    ui.vertical(|ui| {
                                        ui.set_min_width(body_w);
                                        ui.set_max_width(body_w);
                                        TgScrollArea::new(&entry.modal.id, scroll_st)
                                            .max_height(entry.modal.max_content_height)
                                            .show_shadows(true)
                                            .show(ui, |ui| {
                                                render_body(&entry.modal.id, ui);
                                            });
                                    });
                                });

                                ui.add_space(style.content_padding.bottom);

                                // Footer buttons row
                                if !entry.modal.buttons.is_empty()
                                    || entry.modal.left_button.is_some()
                                    || entry.modal.show_loading
                                {
                                    ui.add_space(style.button_padding.top);
                                    ui.horizontal(|ui| {
                                        ui.set_min_width(box_w);
                                        ui.add_space(14.0);
                                        if entry.modal.show_loading {
                                            let l_st =
                                                InfiniteRadialAnimationStyle::box_loading(colors);
                                            let (lrect, _) = ui.allocate_exact_size(
                                                l_st.size.to_vec2(),
                                                Sense::hover(),
                                            );
                                            InfiniteRadialAnimation::new(l_st).paint(
                                                ctx,
                                                ui.painter(),
                                                lrect,
                                            );
                                        } else if let Some(lb) = &entry.modal.left_button {
                                            let bst = match lb.kind {
                                                BoxButtonKind::Default => style.button,
                                                BoxButtonKind::Attention => style.attention_button,
                                            };
                                            if RoundButton::new(&lb.label, bst).show(ui).clicked()
                                                && is_top
                                            {
                                                event = Some(LayerEvent {
                                                    box_id: entry.modal.id.clone(),
                                                    action_id: lb.id.clone(),
                                                });
                                            }
                                        }

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.add_space(style.button_padding.right);
                                                for btn in entry.modal.buttons.iter().rev() {
                                                    let bst = match btn.kind {
                                                        BoxButtonKind::Default => style.button,
                                                        BoxButtonKind::Attention => {
                                                            style.attention_button
                                                        }
                                                    };
                                                    if RoundButton::new(&btn.label, bst)
                                                        .show(ui)
                                                        .clicked()
                                                        && is_top
                                                    {
                                                        event = Some(LayerEvent {
                                                            box_id: entry.modal.id.clone(),
                                                            action_id: btn.id.clone(),
                                                        });
                                                    }
                                                    ui.add_space(style.button_padding.left);
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(style.button_padding.bottom);
                                }
                            },
                        )
                        .response;

                    let final_rect = Rect::from_min_size(
                        frame_start,
                        Vec2::new(box_w, inner_resp.rect.height()),
                    );
                    if is_top {
                        box_rect = final_rect;
                    }

                    // Draw rounded white card & blurred shadow behind the widgets.
                    let bg_shape = egui::Shape::rect_filled(
                        final_rect,
                        CornerRadius::same(style.radius.round() as u8),
                        with_alpha(style.bg, f64::from(opacity)),
                    );
                    ui.painter().set(bg_idx, bg_shape);

                    // Paint subtle border + shadow on lower stacked card dim if not top.
                    BoxShadow::new(style.shadow).paint_with_opacity(
                        ctx,
                        &shadow_painter,
                        final_rect,
                        style.radius,
                        style.shadow.opacity * opacity * 0.45,
                    );
                    if !is_top {
                        RoundRect::new(style.radius, with_alpha(style.layer_bg, 0.35))
                            .paint(ui.painter(), final_rect);
                    }
                });
        }

        // Handle outside click / Escape on the topmost modal.
        if event.is_none()
            && let Some(top) = self.entries.last()
            && current_frame > top.opened_frame
        {
            let should_close = escape_pressed
                || (outside_clicked
                    && top.modal.close_by_outside_click
                    && ctx
                        .pointer_interact_pos()
                        .is_none_or(|p| !box_rect.contains(p)));
            if should_close {
                event = Some(LayerEvent {
                    box_id: top.modal.id.clone(),
                    action_id: "__close__".to_owned(),
                });
            }
        }

        if event.as_ref().is_some_and(|e| e.action_id == "__close__") {
            self.pop(ctx, style);
        }

        event
    }
}
