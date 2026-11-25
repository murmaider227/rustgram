//! Rich text labels and section dividers.
//!
//! Port of `Telegram/lib_ui/ui/widgets/labels.h`,
//! `Telegram/lib_ui/ui/widgets/labels.cpp`, and
//! `Telegram/lib_ui/ui/widgets/box_content_divider.cpp`.

use egui::{CornerRadius, Pos2, Rect, Ui, Vec2};
use tg_text::TextWithEntities;

use crate::style::{FlatLabelStyle, constants};
use crate::text::{
    EntityTextResponse, RichText, RichTextOptions, RichTextResponse, render_rich_text,
    render_text_with_entities,
};
use crate::theme::Colors;

/// Multi-line rich text label mirroring `Ui::FlatLabel`.
pub struct FlatLabel<'a> {
    text: RichText,
    entities: Option<TextWithEntities>,
    style: FlatLabelStyle,
    max_width: Option<f32>,
    max_lines: Option<usize>,
    elide: bool,
    selectable: bool,
    id_salt: &'a str,
}

impl<'a> FlatLabel<'a> {
    /// Create a `FlatLabel` from plain text.
    pub fn plain(text: &str, style: FlatLabelStyle) -> Self {
        Self {
            text: RichText::plain(text),
            entities: None,
            style,
            max_width: None,
            max_lines: None,
            elide: true,
            selectable: false,
            id_salt: "flat_label",
        }
    }

    /// Create a `FlatLabel` from [`RichText`].
    pub fn rich(text: RichText, style: FlatLabelStyle) -> Self {
        Self {
            text,
            entities: None,
            style,
            max_width: None,
            max_lines: None,
            elide: true,
            selectable: false,
            id_salt: "flat_label",
        }
    }

    /// Create a `FlatLabel` from `tg_text::TextWithEntities` (full Telegram rich text).
    #[must_use]
    pub fn with_entities(text: TextWithEntities, style: FlatLabelStyle) -> Self {
        let rich = RichText::from_text_with_entities(&text);
        Self {
            text: rich,
            entities: Some(text),
            style,
            max_width: None,
            max_lines: None,
            elide: true,
            selectable: false,
            id_salt: "flat_label",
        }
    }

    /// Set a unique ID salt (important when multiple labels have spoilers/links).
    pub fn id_salt(mut self, salt: &'a str) -> Self {
        self.id_salt = salt;
        self
    }

    /// Override maximum wrapping width.
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }

    /// Limit maximum displayed lines (with ellipsis when [`Self::elide`] is true).
    #[must_use]
    pub fn max_lines(mut self, lines: usize) -> Self {
        self.max_lines = Some(lines);
        self
    }

    /// Enable or disable `"…"` elision when truncated by `max_lines`.
    #[must_use]
    pub fn elide(mut self, elide: bool) -> Self {
        self.elide = elide;
        self
    }

    /// Enable or disable interactive text selection (used by [`Self::show_entities`]).
    #[must_use]
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Show the label in `ui` (legacy [`RichTextResponse`] return type).
    pub fn show(self, ui: &mut Ui) -> RichTextResponse {
        if let Some(entities) = self.entities {
            let avail = self
                .max_width
                .unwrap_or_else(|| ui.available_width())
                .max(self.style.min_width);
            let id = ui.make_persistent_id(self.id_salt);
            let mut opts = RichTextOptions::new(self.style.text_fg, avail)
                .elide(self.elide)
                .selectable(self.selectable);
            if let Some(ml) = self.max_lines {
                opts = opts.max_lines(ml);
            }
            let resp = render_text_with_entities(
                ui,
                id,
                &entities,
                &self.style.style,
                &self.style.palette,
                &opts,
            );
            return RichTextResponse {
                clicked_link: resp.clicked_link.map(|l| l.target),
                spoiler_toggled: resp.spoiler_toggled,
                rect: resp.rect,
            };
        }

        let avail = self
            .max_width
            .unwrap_or_else(|| ui.available_width())
            .max(self.style.min_width);
        let id = ui.make_persistent_id(self.id_salt);
        render_rich_text(
            ui,
            id,
            &self.text,
            &self.style.style,
            &self.style.palette,
            self.style.text_fg,
            avail,
        )
    }

    /// Show the label with the full [`EntityTextResponse`] (links, selection, pre copy, elision).
    pub fn show_entities(self, ui: &mut Ui) -> EntityTextResponse {
        let entities = self
            .entities
            .unwrap_or_else(|| self.text.to_text_with_entities());
        let avail = self
            .max_width
            .unwrap_or_else(|| ui.available_width())
            .max(self.style.min_width);
        let id = ui.make_persistent_id(self.id_salt);
        let mut opts = RichTextOptions::new(self.style.text_fg, avail)
            .elide(self.elide)
            .selectable(self.selectable);
        if let Some(ml) = self.max_lines {
            opts = opts.max_lines(ml);
        }
        render_text_with_entities(
            ui,
            id,
            &entities,
            &self.style.style,
            &self.style.palette,
            &opts,
        )
    }
}

/// Gray section divider bar with subtle top/bottom inner shadows mirroring
/// `Ui::BoxContentDivider` / `DividerBar`.
pub fn divider_bar(ui: &mut Ui, height: f32, colors: &dyn Colors) -> Rect {
    let h = height.max(constants::BOX_DIVIDER_HEIGHT);
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), h), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_box_divider(ui.painter(), rect, colors);
    }
    rect
}

/// Paints `BoxContentDivider` into `rect`: `boxDividerBg` with the soft `boxDividerTop` shadow
/// fading over its first three rows and the faint `boxDividerBottom` row.
pub fn paint_box_divider(painter: &egui::Painter, rect: Rect, colors: &dyn Colors) {
    painter.rect_filled(rect, CornerRadius::ZERO, colors.color("boxDividerBg"));
    let shadow = colors.color("shadowFg");
    let row = |y: f32| {
        Rect::from_min_max(
            Pos2::new(rect.left(), y),
            Pos2::new(rect.right(), (y + 1.0).min(rect.bottom())),
        )
    };
    for (offset, strength) in [(0.0, 0.61), (1.0, 0.36), (2.0, 0.13)] {
        painter.rect_filled(
            row(rect.top() + offset),
            CornerRadius::ZERO,
            shadow.gamma_multiply(strength),
        );
    }
    painter.rect_filled(
        row(rect.bottom() - 1.0),
        CornerRadius::ZERO,
        shadow.gamma_multiply(0.13),
    );
}
